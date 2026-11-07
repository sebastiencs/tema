//! Blocking wrappers around the syscalls the forker and the reaper use.
//!
//! Every descriptor created here has `CLOEXEC` set. A blocked signal mask and
//! a custom handler survive `exec`, so a child must call [`reset_signals`]
//! before it execs.
//!
//! `nix` 0.31 has no wrapper for `pidfd_open` or `close_range`. Those two
//! stay on `libc`.

use std::{
    ffi::CStr,
    fs::File,
    io::{Error, ErrorKind, IoSlice, IoSliceMut, Read},
    mem,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
    ptr,
};

use nix::{
    errno::Errno,
    fcntl::{fcntl, FcntlArg, FdFlag, OFlag},
    sys::{
        prctl,
        signal::{sigaction, SaFlags, SigHandler, SigSet, SigmaskHow, Signal},
        socket::{
            cmsg_space, recvmsg, sendmsg, AddressFamily, ControlMessage, ControlMessageOwned,
            MsgFlags, SockFlag, SockType,
        },
        wait::{waitid, Id, WaitPidFlag, WaitStatus},
    },
    unistd::{self, pipe2},
};

/// How a waited-for child stopped running.
///
/// Variant order is the wire discriminant inside a forker `Exited` message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ChildExit {
    /// `exit` status. A normal exit is in `0..=255`.
    Code(i32),
    /// Terminated by this signal. A core dump is still reported as the signal.
    Signal(i32),
}

/// Most file descriptors one message may carry. Matches the protocol limit.
pub(crate) const MAX_ATTACHED_FDS: usize = 32;

pub(crate) fn socketpair_seqpacket() -> std::io::Result<(OwnedFd, OwnedFd)> {
    socketpair(SockType::SeqPacket)
}

pub(crate) fn socketpair_dgram() -> std::io::Result<(OwnedFd, OwnedFd)> {
    socketpair(SockType::Datagram)
}

pub(crate) fn pipe_cloexec() -> std::io::Result<(OwnedFd, OwnedFd)> {
    Ok(pipe2(OFlag::O_CLOEXEC)?)
}

pub(crate) fn pidfd_open(pid: i32) -> std::io::Result<OwnedFd> {
    // `pidfd_open` is not wrapped by nix 0.31, nor by this crate's libc.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Collect the exit of the process `pidfd` refers to, without blocking.
///
/// `Ok(None)` means the process is still running. The descriptor stays open
/// either way; the caller closes it.
pub(crate) fn waitid_pidfd(pidfd: &impl AsRawFd) -> std::io::Result<Option<ChildExit>> {
    // `BorrowedFd::borrow_raw` is safe: `pidfd` owns the descriptor for this call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(pidfd.as_raw_fd()) };
    let status = waitid(Id::PIDFd(borrowed), WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG)?;
    match status {
        WaitStatus::StillAlive => Ok(None),
        WaitStatus::Exited(_, code) => Ok(Some(ChildExit::Code(code))),
        WaitStatus::Signaled(_, signal, _) => Ok(Some(ChildExit::Signal(signal as i32))),
        other => {
            Err(Error::new(ErrorKind::InvalidData, format!("unexpected waitid status {other:?}")))
        }
    }
}

/// Close every descriptor numbered `first` or higher.
pub(crate) fn close_range_from(first: i32) -> std::io::Result<()> {
    if first < 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "negative file descriptor"));
    }
    // nix 0.31 does not wrap `close_range`.
    if unsafe { libc::close_range(first as libc::c_uint, u32::MAX, 0) } < 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn set_cloexec(fd: &impl AsFd, on: bool) -> std::io::Result<()> {
    let flags = if on { FdFlag::FD_CLOEXEC } else { FdFlag::empty() };
    fcntl(fd, FcntlArg::F_SETFD(flags))?;
    Ok(())
}

/// Put a duplicate of `fd` on `slot`, with `CLOEXEC` set.
///
/// It fails when `fd` is already `slot`; clear `CLOEXEC` with [`set_cloexec`]
/// in that case. This does not return an [`OwnedFd`]: that owner would close
/// `slot` on drop, and the caller asked for a descriptor at a specific number.
/// `dup3` still writes through an [`OwnedFd`], so that owner is forgotten.
pub(crate) fn dup_to(fd: &impl AsFd, slot: i32) -> std::io::Result<()> {
    let mut installed = unsafe { OwnedFd::from_raw_fd(slot) };
    let result = unistd::dup3(fd, &mut installed, OFlag::O_CLOEXEC);
    mem::forget(installed);
    result.map_err(Error::from)
}

/// Duplicate `fd` onto the lowest free number greater than or equal to `min`.
pub(crate) fn dup_above(fd: &impl AsFd, min: i32) -> std::io::Result<OwnedFd> {
    let fd = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(min))?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) fn set_pdeathsig(signal: i32) -> std::io::Result<()> {
    let signal = Signal::try_from(signal)?;
    prctl::set_pdeathsig(Some(signal))?;
    Ok(())
}

pub(crate) fn set_child_subreaper() -> std::io::Result<()> {
    prctl::set_child_subreaper(true)?;
    Ok(())
}

/// `PR_SET_NAME` keeps 15 bytes plus a trailing NUL.
pub(crate) fn set_process_name(name: &str) {
    let mut buf = [0u8; 16];
    let bytes = name.as_bytes();
    let len = bytes.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    let Ok(name) = CStr::from_bytes_with_nul(&buf[..=len]) else {
        return;
    };
    let _ = prctl::set_name(name);
}

/// Restore `SIG_DFL` for every signal and clear the blocked mask.
pub(crate) fn reset_signals() -> std::io::Result<()> {
    let action =
        nix::sys::signal::SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    // nix names the standard signals only. 1..=64 also covers the real-time set.
    for signo in 1..=64 {
        match Signal::try_from(signo) {
            Ok(signal) => {
                // `SIGKILL` and `SIGSTOP` cannot be changed.
                let _ = unsafe { sigaction(signal, &action) };
            }
            Err(_) => reset_unnamed_signal(signo)?,
        }
    }
    nix::sys::signal::pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&SigSet::empty()), None)?;
    Ok(())
}

/// Send one datagram, plus `fds`, on a socket the caller owns.
///
/// A seqpacket socket delivers this as a single message. `MSG_NOSIGNAL` keeps
/// a closed peer from raising `SIGPIPE` in the forker, which runs with the
/// default signal dispositions.
pub(crate) fn send_with_fds(
    sock: &impl AsRawFd,
    bytes: &[u8],
    fds: &[RawFd],
) -> std::io::Result<()> {
    if fds.len() > MAX_ATTACHED_FDS {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("at most {MAX_ATTACHED_FDS} file descriptors per message"),
        ));
    }

    let iov = [IoSlice::new(bytes)];
    let rights = ControlMessage::ScmRights(fds);
    let cmsgs: &[ControlMessage] = if fds.is_empty() { &[] } else { &[rights] };
    loop {
        match sendmsg::<()>(sock.as_raw_fd(), &iov, cmsgs, MsgFlags::MSG_NOSIGNAL, None) {
            Ok(sent) if sent == bytes.len() => return Ok(()),
            Ok(_) => {
                return Err(Error::new(ErrorKind::WriteZero, "short send on a datagram socket"));
            }
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Receive one datagram and the descriptors attached to it.
///
/// A length of zero and no descriptors means the peer closed a seqpacket
/// socket. Received descriptors have `CLOEXEC` set. `MSG_TRUNC` or
/// `MSG_CTRUNC` is an error; any descriptors already received are closed.
pub(crate) fn recv_with_fds(
    sock: &impl AsRawFd,
    buf: &mut [u8],
) -> std::io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = [IoSliceMut::new(buf)];
    let mut control = vec![0u8; cmsg_space::<[RawFd; MAX_ATTACHED_FDS]>()];
    let message = loop {
        match recvmsg::<()>(
            sock.as_raw_fd(),
            &mut iov,
            Some(&mut control),
            MsgFlags::MSG_CMSG_CLOEXEC,
        ) {
            Ok(message) => break message,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
    };

    // `cmsgs` refuses to iterate when the ancillary data was truncated, which
    // would leak any descriptors the kernel did deliver.
    let delivered = match message.cmsgs() {
        Ok(cmsgs) => cmsgs,
        Err(_) => {
            close_truncated_rights(&control);
            return Err(Error::new(ErrorKind::InvalidData, "message or ancillary data truncated"));
        }
    };
    let mut fds = Vec::new();
    for cmsg in delivered {
        if let ControlMessageOwned::ScmRights(raw) = cmsg {
            fds.extend(
                raw.into_iter()
                    .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
            );
        }
    }
    if message
        .flags
        .intersects(MsgFlags::MSG_TRUNC | MsgFlags::MSG_CTRUNC)
    {
        return Err(Error::new(ErrorKind::InvalidData, "message or ancillary data truncated"));
    }
    Ok((message.bytes, fds))
}

/// `Threads:` from `/proc/self/status`.
pub(crate) fn count_threads() -> std::io::Result<usize> {
    let mut status = String::new();
    File::open("/proc/self/status")?.read_to_string(&mut status)?;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("Threads:") else {
            continue;
        };
        return rest.trim().parse().map_err(|error| {
            Error::new(ErrorKind::InvalidData, format!("Threads field: {error}"))
        });
    }
    Err(Error::new(ErrorKind::InvalidData, "Threads field missing"))
}

fn socketpair(kind: SockType) -> std::io::Result<(OwnedFd, OwnedFd)> {
    Ok(nix::sys::socket::socketpair(AddressFamily::Unix, kind, None, SockFlag::SOCK_CLOEXEC)?)
}

/// Close `SCM_RIGHTS` descriptors that `RecvMsg::cmsgs` will not yield.
///
/// A truncated control message is skipped. Complete ones are taken so their
/// descriptors are closed with the returned [`OwnedFd`].
fn close_truncated_rights(control: &[u8]) {
    unsafe {
        let mut header: libc::msghdr = mem::zeroed();
        header.msg_control = control.as_ptr() as *mut libc::c_void;
        header.msg_controllen = control.len();
        let mut current = libc::CMSG_FIRSTHDR(&header);
        while !current.is_null() {
            let cmsg = &*current;
            let start = current as usize;
            let buffer_end = control.as_ptr() as usize + control.len();
            let claimed = cmsg.cmsg_len as usize;
            let data = libc::CMSG_DATA(current) as usize;
            let inside = start
                .checked_add(claimed)
                .is_some_and(|end| end <= buffer_end);
            if inside
                && cmsg.cmsg_level == libc::SOL_SOCKET
                && cmsg.cmsg_type == libc::SCM_RIGHTS
                && data >= start
                && data <= buffer_end
            {
                let count = claimed.saturating_sub(data - start) / mem::size_of::<RawFd>();
                for index in 0..count {
                    let fd = ptr::read_unaligned((data as *const RawFd).add(index));
                    if fd >= 0 {
                        drop(OwnedFd::from_raw_fd(fd));
                    }
                }
            }
            current = libc::CMSG_NXTHDR(&header, current);
        }
    }
}

fn reset_unnamed_signal(signo: i32) -> std::io::Result<()> {
    unsafe {
        let mut action: libc::sigaction = mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(signo, &action, ptr::null_mut()) < 0 {
            let error = Error::last_os_error();
            // Numbers in 1..=64 that are not signals on this kernel.
            if error.raw_os_error() != Some(libc::EINVAL) {
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, AsRawFd, IntoRawFd};

    use nix::{
        fcntl::{fcntl, FcntlArg, FdFlag},
        sys::{
            signal::{kill, Signal},
            wait::{waitid, Id, WaitPidFlag},
        },
        unistd::{self, ForkResult, Pid},
    };

    use super::*;

    fn is_cloexec(fd: &impl AsFd) -> bool {
        let flags = fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD");
        FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC)
    }

    /// Kills `pid` and collects it through `pidfd`, so a recycled pid cannot be reaped.
    struct KillOnDrop {
        pid: Pid,
        pidfd: OwnedFd,
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = kill(self.pid, Signal::SIGKILL);
            let borrowed = unsafe { BorrowedFd::borrow_raw(self.pidfd.as_raw_fd()) };
            let _ = waitid(Id::PIDFd(borrowed), WaitPidFlag::WEXITED);
        }
    }

    fn fork_child(child: impl FnOnce() -> !) -> (Pid, OwnedFd, KillOnDrop) {
        let ForkResult::Parent { child: pid } = unsafe { unistd::fork() }.expect("fork") else {
            child();
        };
        let pidfd = pidfd_open(pid.as_raw()).expect("pidfd_open");
        let reap = KillOnDrop { pid, pidfd: pidfd.try_clone().expect("dup pidfd") };
        (pid, pidfd, reap)
    }

    fn wait_ready(pidfd: &OwnedFd) -> ChildExit {
        for _ in 0..10_000 {
            if let Some(exit) = waitid_pidfd(pidfd).expect("waitid") {
                return exit;
            }
            std::thread::yield_now();
        }
        panic!("child did not exit");
    }

    #[test]
    fn seqpacket_round_trip_carries_a_cloexec_pipe() {
        let (ours, theirs) = socketpair_seqpacket().unwrap();
        assert!(is_cloexec(&ours));
        let (read, write) = pipe_cloexec().unwrap();
        assert!(is_cloexec(&read));

        send_with_fds(&ours, b"pipe", &[write.as_raw_fd()]).unwrap();
        drop(write);

        let mut buf = [0u8; 16];
        let (len, fds) = recv_with_fds(&theirs, &mut buf).unwrap();
        assert_eq!(&buf[..len], b"pipe");
        assert_eq!(fds.len(), 1);
        assert!(is_cloexec(&fds[0]));

        assert_eq!(unistd::write(&fds[0], b"hi").unwrap(), 2);
        drop(fds);
        let mut got = [0u8; 2];
        assert_eq!(unistd::read(&read, &mut got).unwrap(), 2);
        assert_eq!(&got, b"hi");
    }

    #[test]
    fn closed_seqpacket_peer_reads_as_empty() {
        let (ours, theirs) = socketpair_seqpacket().unwrap();
        drop(ours);
        let mut buf = [0u8; 8];
        let (len, fds) = recv_with_fds(&theirs, &mut buf).unwrap();
        assert_eq!(len, 0);
        assert!(fds.is_empty());
    }

    #[test]
    fn more_than_the_fd_limit_is_rejected() {
        let (ours, _theirs) = socketpair_seqpacket().unwrap();
        let fds = [ours.as_raw_fd(); MAX_ATTACHED_FDS + 1];
        let error = send_with_fds(&ours, b"no", &fds).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn waitid_reports_exit_code_3() {
        let (_pid, pidfd, _reap) = fork_child(|| unsafe { libc::_exit(3) });
        assert_eq!(wait_ready(&pidfd), ChildExit::Code(3));
    }

    #[test]
    fn waitid_reports_sigkill() {
        let (pid, pidfd, _reap) = fork_child(|| loop {
            unistd::pause();
        });
        kill(pid, Signal::SIGKILL).unwrap();
        assert_eq!(wait_ready(&pidfd), ChildExit::Signal(libc::SIGKILL));
    }

    #[test]
    fn dup_to_replaces_a_held_slot_and_sets_cloexec() {
        let (read, _write) = pipe_cloexec().unwrap();
        let slot = dup_above(&read, 100).unwrap().into_raw_fd();
        dup_to(&read, slot).unwrap();
        let slot = unsafe { OwnedFd::from_raw_fd(slot) };
        assert!(slot.as_raw_fd() >= 100);
        assert!(is_cloexec(&slot));
    }

    #[test]
    fn clearing_cloexec_is_visible_to_fcntl() {
        let (read, _write) = pipe_cloexec().unwrap();
        let copy = dup_above(&read, 100).unwrap();
        assert!(is_cloexec(&copy));
        set_cloexec(&copy, false).unwrap();
        assert!(!is_cloexec(&copy));
    }

    #[test]
    fn this_process_has_a_thread() {
        assert!(count_threads().unwrap() >= 1);
    }
}
