//! The program, arguments, environment and file descriptors for one Xwayland.
//!
//! The child receives the write end of each pipe and one end of each
//! socketpair. This side keeps the read ends and the other socketpair ends.
//! The listening X socket stays open here as well: the child gets a duplicate.

use std::{os::fd::OwnedFd, rc::Rc};

use crate::{
    process::{pipe_cloexec, socketpair_stream, ProcessEvent, SpawnRequest},
    runner::Redispatcher,
};

pub(crate) const PROGRAM: &str = "Xwayland";
/// Seconds Xwayland waits after the last application disconnects.
pub(crate) const TERMINATE_DELAY_SECS: &str = "10";
pub(crate) const STDERR_SLOT: i32 = 2;
pub(crate) const DISPLAYFD_SLOT: i32 = 3;
pub(crate) const LISTEN_SLOT: i32 = 4;
pub(crate) const WM_SLOT: i32 = 5;
pub(crate) const WAYLAND_SLOT: i32 = 6;

pub(crate) struct Launch {
    pub(crate) request: SpawnRequest,
    pub(crate) ours: OurEnds,
}

pub(crate) struct OurEnds {
    pub(crate) stderr: OwnedFd,
    pub(crate) displayfd: OwnedFd,
    pub(crate) wm: OwnedFd,
    pub(crate) wayland: OwnedFd,
}

impl Launch {
    pub(crate) fn prepare(
        listener: &Rc<OwnedFd>,
        notify: Redispatcher<ProcessEvent>,
    ) -> std::io::Result<Self> {
        let (stderr, stderr_child) = pipe_cloexec()?;
        let (displayfd, displayfd_child) = pipe_cloexec()?;
        let (wm, wm_child) = socketpair_stream()?;
        let (wayland, wayland_child) = socketpair_stream()?;
        let request = SpawnRequest {
            program: PROGRAM.to_string(),
            args: args(),
            env: env(),
            fds: vec![
                (STDERR_SLOT, Rc::new(stderr_child)),
                (DISPLAYFD_SLOT, Rc::new(displayfd_child)),
                (LISTEN_SLOT, Rc::clone(listener)),
                (WM_SLOT, Rc::new(wm_child)),
                (WAYLAND_SLOT, Rc::new(wayland_child)),
            ],
            notify: Some(notify),
        };
        Ok(Self { request, ours: OurEnds { stderr, displayfd, wm, wayland } })
    }
}

fn args() -> Vec<String> {
    vec![
        "-terminate".to_string(),
        TERMINATE_DELAY_SECS.to_string(),
        "-rootless".to_string(),
        "-verbose".to_string(),
        "10".to_string(),
        "-displayfd".to_string(),
        DISPLAYFD_SLOT.to_string(),
        "-listenfd".to_string(),
        LISTEN_SLOT.to_string(),
        "-wm".to_string(),
        WM_SLOT.to_string(),
    ]
}

fn env() -> Vec<(String, Option<String>)> {
    vec![
        ("WAYLAND_SOCKET".to_string(), Some(WAYLAND_SLOT.to_string())),
        ("LIBEI_SOCKET".to_string(), None),
    ]
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        os::fd::{AsFd, AsRawFd},
        rc::Rc,
    };

    use nix::{
        fcntl::{fcntl, FcntlArg, FdFlag},
        unistd,
    };

    use super::*;

    fn is_cloexec(fd: &impl AsFd) -> bool {
        let flags = fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD");
        FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC)
    }

    #[test]
    fn arguments_name_the_descriptor_slots() {
        assert_eq!(
            args(),
            [
                "-terminate",
                TERMINATE_DELAY_SECS,
                "-rootless",
                "-verbose",
                "10",
                "-displayfd",
                "3",
                "-listenfd",
                "4",
                "-wm",
                "5",
            ]
            .map(str::to_string)
        );
        assert_eq!(args()[6], DISPLAYFD_SLOT.to_string());
        assert_eq!(args()[8], LISTEN_SLOT.to_string());
        assert_eq!(args()[10], WM_SLOT.to_string());
        assert_eq!(
            env(),
            vec![
                ("WAYLAND_SOCKET".to_string(), Some(WAYLAND_SLOT.to_string())),
                ("LIBEI_SOCKET".to_string(), None),
            ]
        );
    }

    #[test]
    fn prepare_assigns_slots_2_through_6_and_keeps_readable_pipes() {
        let (listener, _peer) = socketpair_stream().unwrap();
        let listener = Rc::new(listener);
        let launch = Launch::prepare(&listener, Redispatcher::None).unwrap();

        assert_eq!(launch.request.program, PROGRAM);
        assert_eq!(launch.request.args, args());
        assert_eq!(launch.request.env, env());
        let slots: Vec<i32> = launch.request.fds.iter().map(|(slot, _)| *slot).collect();
        assert_eq!(slots, vec![STDERR_SLOT, DISPLAYFD_SLOT, LISTEN_SLOT, WM_SLOT, WAYLAND_SLOT]);
        assert_eq!(slots.iter().copied().collect::<BTreeSet<_>>().len(), slots.len());
        assert_eq!(launch.request.fds[2].1.as_raw_fd(), listener.as_raw_fd());

        assert!(is_cloexec(&launch.ours.stderr));
        assert!(is_cloexec(&launch.ours.displayfd));
        assert!(is_cloexec(&launch.ours.wm));
        assert!(is_cloexec(&launch.ours.wayland));

        let stderr_child = &launch.request.fds[0].1;
        let display_child = &launch.request.fds[1].1;
        assert_eq!(unistd::write(stderr_child, b"e").unwrap(), 1);
        assert_eq!(unistd::write(display_child, b"d").unwrap(), 1);
        let mut byte = [0u8; 1];
        assert_eq!(unistd::read(&launch.ours.stderr, &mut byte).unwrap(), 1);
        assert_eq!(&byte, b"e");
        assert_eq!(unistd::read(&launch.ours.displayfd, &mut byte).unwrap(), 1);
        assert_eq!(&byte, b"d");
    }
}
