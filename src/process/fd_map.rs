//! Move file descriptors onto the slots a spawned program should inherit.
//!
//! [`plan`] only looks at descriptor numbers. [`apply`] runs the plan in the
//! forked child, before `exec`. Descriptors that must survive `exec` are left
//! without `CLOEXEC`. The exec-error pipe is parked above every slot and keeps
//! `CLOEXEC`, so a successful exec closes it.

use std::{
    collections::BTreeSet,
    io::{Error, ErrorKind},
    mem,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
};

use nix::{fcntl::OFlag, unistd};

use super::sys::{dup_above, set_cloexec};

/// One step of a plan.
///
/// `DupAbove` runs before any `DupTo`, and its `from` is the number the
/// descriptor has when the plan starts. Later steps keep using that original
/// number; [`apply`] follows the copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FdOp {
    /// Duplicate `from` onto the lowest free number greater than or equal to `min`.
    /// The copy has `CLOEXEC` set.
    DupAbove { from: RawFd, min: i32 },
    /// Install `from` on `to` with `CLOEXEC` clear.
    DupTo { from: RawFd, to: i32 },
    /// `fd` is already on its slot. `dup2(fd, fd)` would leave `CLOEXEC` set.
    ClearCloexec { fd: RawFd },
}

/// `entries` are `(current number, slot)`. `extra_keep` stays open across the
/// shuffle and is not itself a slot, typically the exec-error pipe.
///
/// A descriptor that currently occupies a slot claimed by a different entry is
/// copied above the highest slot first. Every `extra_keep` descriptor is copied
/// there too. Copies happen in ascending number order, then each entry is
/// placed in the order given.
pub(crate) fn plan(entries: &[(RawFd, i32)], extra_keep: &[RawFd]) -> Vec<FdOp> {
    let Some(highest) = entries.iter().map(|(_, slot)| *slot).max() else {
        return Vec::new();
    };
    let min = highest.saturating_add(1);
    let mut evacuate = BTreeSet::new();
    for &(fd, _) in entries {
        if claimed_by_another(fd, entries) {
            evacuate.insert(fd);
        }
    }
    evacuate.extend(extra_keep.iter().copied());

    let mut ops = Vec::with_capacity(evacuate.len() + entries.len());
    for fd in &evacuate {
        ops.push(FdOp::DupAbove { from: *fd, min });
    }
    for &(fd, slot) in entries {
        if fd == slot && !evacuate.contains(&fd) {
            ops.push(FdOp::ClearCloexec { fd });
        } else {
            ops.push(FdOp::DupTo { from: fd, to: slot });
        }
    }
    ops
}

/// Run `ops`. The returned descriptors are the `DupAbove` copies. They have to
/// stay open until `exec`: an earlier number may already have been overwritten,
/// and dropping the copy would close the last reference.
pub(crate) fn apply(ops: &[FdOp]) -> std::io::Result<Vec<OwnedFd>> {
    let mut where_now: Vec<(RawFd, RawFd)> = Vec::new();
    let mut kept = Vec::new();
    for op in ops {
        match *op {
            FdOp::DupAbove { from, min } => {
                let source = borrow(current(&where_now, from))?;
                let copy = dup_above(&source, min)?;
                where_now.push((from, copy.as_raw_fd()));
                kept.push(copy);
            }
            FdOp::DupTo { from, to } => dup_onto(current(&where_now, from), to)?,
            FdOp::ClearCloexec { fd } => {
                let fd = borrow(current(&where_now, fd))?;
                set_cloexec(&fd, false)?;
            }
        }
    }
    Ok(kept)
}

fn claimed_by_another(fd: RawFd, entries: &[(RawFd, i32)]) -> bool {
    entries
        .iter()
        .any(|&(source, slot)| slot == fd && source != fd)
}

fn current(where_now: &[(RawFd, RawFd)], fd: RawFd) -> RawFd {
    where_now
        .iter()
        .rev()
        .find(|(from, _)| *from == fd)
        .map(|(_, now)| *now)
        .unwrap_or(fd)
}

fn borrow(fd: RawFd) -> std::io::Result<BorrowedFd<'static>> {
    if fd < 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "negative file descriptor"));
    }
    Ok(unsafe { BorrowedFd::borrow_raw(fd) })
}

/// `dup3` onto `to` without `CLOEXEC`. `to` is not owned here: dropping the
/// temporary owner would close the slot the caller just asked to fill.
fn dup_onto(from: RawFd, to: i32) -> std::io::Result<()> {
    let from = borrow(from)?;
    let mut installed = unsafe { OwnedFd::from_raw_fd(to) };
    let result = unistd::dup3(&from, &mut installed, OFlag::empty());
    mem::forget(installed);
    result.map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use nix::{
        fcntl::{fcntl, FcntlArg, FdFlag},
        unistd,
    };

    use {
        super::{plan, FdOp},
        crate::process::sys::pipe_cloexec,
    };

    fn is_cloexec(fd: &impl std::os::fd::AsFd) -> bool {
        let flags = fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD");
        FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC)
    }

    #[test]
    fn swap_parks_both_numbers_before_either_move() {
        assert_eq!(
            plan(&[(3, 4), (4, 3)], &[]),
            vec![
                FdOp::DupAbove { from: 3, min: 5 },
                FdOp::DupAbove { from: 4, min: 5 },
                FdOp::DupTo { from: 3, to: 4 },
                FdOp::DupTo { from: 4, to: 3 },
            ]
        );
    }

    #[test]
    fn chain_parks_only_the_numbers_later_entries_need() {
        assert_eq!(
            plan(&[(5, 6), (6, 7), (7, 8)], &[]),
            vec![
                FdOp::DupAbove { from: 6, min: 9 },
                FdOp::DupAbove { from: 7, min: 9 },
                FdOp::DupTo { from: 5, to: 6 },
                FdOp::DupTo { from: 6, to: 7 },
                FdOp::DupTo { from: 7, to: 8 },
            ]
        );
    }

    #[test]
    fn a_descriptor_already_on_its_slot_keeps_the_number() {
        assert_eq!(plan(&[(4, 4)], &[]), vec![FdOp::ClearCloexec { fd: 4 }]);
        assert_eq!(
            plan(&[(4, 4), (8, 9)], &[]),
            vec![FdOp::ClearCloexec { fd: 4 }, FdOp::DupTo { from: 8, to: 9 }]
        );
    }

    #[test]
    fn the_error_pipe_is_parked_above_every_slot() {
        assert_eq!(
            plan(&[(7, 4)], &[4]),
            vec![FdOp::DupAbove { from: 4, min: 5 }, FdOp::DupTo { from: 7, to: 4 }]
        );
        assert_eq!(
            plan(&[(7, 5)], &[4]),
            vec![FdOp::DupAbove { from: 4, min: 6 }, FdOp::DupTo { from: 7, to: 5 }]
        );
    }

    #[test]
    fn stdio_slots_are_plain_moves() {
        assert_eq!(
            plan(&[(10, 0), (11, 1), (12, 2)], &[]),
            vec![
                FdOp::DupTo { from: 10, to: 0 },
                FdOp::DupTo { from: 11, to: 1 },
                FdOp::DupTo { from: 12, to: 2 },
            ]
        );
    }

    #[test]
    fn apply_swaps_two_pipes_and_clears_cloexec() {
        let (a_read, a_write) = pipe_cloexec().unwrap();
        let (b_read, b_write) = pipe_cloexec().unwrap();
        let a_slot = dup_above_owned(&a_write);
        let b_slot = dup_above_owned(&b_write);
        drop(a_write);
        drop(b_write);
        let a_num = a_slot.as_raw_fd();
        let b_num = b_slot.as_raw_fd();
        let ops = plan(&[(a_num, b_num), (b_num, a_num)], &[]);
        mem_forget(a_slot);
        mem_forget(b_slot);

        let kept = super::apply(&ops).unwrap();
        let a_now = unsafe { OwnedFd::from_raw_fd(a_num) };
        let b_now = unsafe { OwnedFd::from_raw_fd(b_num) };
        assert!(kept.iter().all(|fd| is_cloexec(fd)));
        assert!(!is_cloexec(&a_now));
        assert!(!is_cloexec(&b_now));
        drop(kept);

        assert_eq!(unistd::write(&a_now, b"B").unwrap(), 1);
        let mut buf = [0u8; 1];
        assert_eq!(unistd::read(&b_read, &mut buf).unwrap(), 1);
        assert_eq!(&buf, b"B");
        assert_eq!(unistd::write(&b_now, b"A").unwrap(), 1);
        assert_eq!(unistd::read(&a_read, &mut buf).unwrap(), 1);
        assert_eq!(&buf, b"A");
    }

    fn dup_above_owned(fd: &OwnedFd) -> OwnedFd {
        super::dup_above(fd, 100).unwrap()
    }

    fn mem_forget(fd: OwnedFd) {
        std::mem::forget(fd);
    }
}
