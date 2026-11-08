//! Spawning programs outside the compositor.
//!
//! The compositor itself does not fork. The original process becomes a reaper,
//! and [`manager::ProcessManager`] sends spawn requests to a forker process.

mod activation;
mod fd_map;
mod forker;
mod manager;
mod protocol;
mod reaper;
mod sys;

#[allow(unused_imports)]
pub(crate) use fd_map::{apply, plan, FdOp};
#[cfg(test)]
pub(crate) use forker::lock_forks;
#[allow(unused_imports)]
pub(crate) use forker::{spawn_forker, ForkerHandle};
#[allow(unused_imports)]
pub(crate) use manager::{ProcessEvent, ProcessManager, ProcessManagerAction, SpawnRequest};
#[allow(unused_imports)]
pub(crate) use protocol::{decode, encode, FromForker, ToForker, MAX_MESSAGE};
#[allow(unused_imports)]
pub(crate) use reaper::become_reaped;
#[allow(unused_imports)]
pub(crate) use sys::{
    close_range_from, count_threads, dup_above, dup_to, pidfd_open, pidfd_send_signal,
    pipe_cloexec, recv_with_fds, reset_signals, send_now, send_with_fds, set_child_subreaper,
    set_cloexec, set_pdeathsig, set_process_name, socketpair_dgram, socketpair_seqpacket,
    socketpair_stream, waitid_pidfd, ChildExit, MAX_ATTACHED_FDS,
};

/// `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP`, shared by the forker environment
/// and the activation environment.
fn session_variables() -> Vec<(String, String)> {
    vec![
        ("WAYLAND_DISPLAY".into(), crate::clients::WAYLAND_DISPLAY_NAME.into()),
        ("XDG_CURRENT_DESKTOP".into(), "tema".into()),
    ]
}

/// Environment every spawn inherits. `DISPLAY` is absent on purpose: the
/// forker strips it at startup, and nothing here puts it back.
pub(crate) fn default_environment() -> Vec<(String, String)> {
    let mut env = session_variables();
    env.push(("_JAVA_AWT_WM_NONREPARENTING".into(), "1".into()));
    env
}

/// Variables published to D-Bus activation and the systemd user manager.
pub(crate) fn activation_environment() -> Vec<(String, String)> {
    let mut env = session_variables();
    env.push(("XDG_SESSION_TYPE".into(), "wayland".into()));
    env
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_environment_is_wayland_and_leaves_display_unset() {
        let env = super::default_environment();
        assert_eq!(
            env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            vec!["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "_JAVA_AWT_WM_NONREPARENTING"]
        );
        assert_eq!(env[0].1, crate::clients::WAYLAND_DISPLAY_NAME);
        assert_eq!(env[1].1, "tema");
        assert_eq!(env[2].1, "1");
        assert!(env.iter().all(|(key, _)| key != "DISPLAY"));
    }

    #[test]
    fn activation_environment_matches_the_session_variables() {
        let env = super::activation_environment();
        let defaults = super::default_environment();
        assert_eq!(
            env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            vec!["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"]
        );
        assert_eq!(env[0], defaults[0]);
        assert_eq!(env[1], defaults[1]);
        assert_eq!(env[0].1, crate::clients::WAYLAND_DISPLAY_NAME);
        assert_eq!(env[1].1, "tema");
        assert_eq!(env[2].1, "wayland");
    }
}
