//! The original process, after it forks the compositor.
//!
//! It is a child subreaper: orphans, including the forker and double-forked
//! daemons, are reparented here and waited on. It stays alive until the
//! compositor exits, then exits with the compositor's status. Other children
//! are left running and are reparented again. The forker dies with this
//! process because its death signal is `SIGKILL`.
//!
//! `SIGINT` is caught and forwarded. Leaving it at the default would kill
//! this process, and the compositor's death signal would then `SIGKILL` it
//! before DRM and logind cleanup.

use std::{
    io::{self, Write},
    sync::atomic::{AtomicI32, Ordering},
};

use nix::{
    sys::{
        signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal},
        wait::{wait, WaitStatus},
    },
    unistd::{fork, getppid, ForkResult, Pid},
};

use super::{reset_signals, set_child_subreaper, set_pdeathsig, set_process_name, ChildExit};

/// Compositor pid, read from the signal handler. `0` until the fork returns.
static COMPOSITOR: AtomicI32 = AtomicI32::new(0);

/// Fork the compositor off this process and become its reaper.
///
/// Returns in the compositor. The reaper does not return: it waits, then
/// exits with the compositor's status.
pub(crate) fn become_reaped() -> i32 {
    if let Err(error) = reset_signals() {
        fail("reset signals", &error);
    }
    // Not inherited across fork, so the compositor does not become a reaper.
    if let Err(error) = set_child_subreaper() {
        fail("subreaper", &error);
    }
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => run_reaper(child),
        Ok(ForkResult::Child) => adopt_reaper(),
        Err(error) => fail("fork", &error),
    }
}

/// Status this process exits with after the compositor stops.
///
/// A normal exit passes through. A signal becomes `128 + signal`, as a shell
/// does. A status that does not fit in a byte becomes 1.
pub(crate) fn exit_code(exit: ChildExit) -> u8 {
    match exit {
        ChildExit::Code(code) => u8::try_from(code).unwrap_or(1),
        ChildExit::Signal(signal) => match u8::try_from(signal) {
            Ok(signal) => 128u8.saturating_add(signal),
            Err(_) => 1,
        },
    }
}

fn adopt_reaper() -> i32 {
    let reaper = getppid();
    if let Err(error) = set_pdeathsig(libc::SIGKILL) {
        fail("death signal", &error);
    }
    // The reaper can die between the fork and this check. The death signal
    // covers a death after it is armed; this check covers one before.
    if getppid() != reaper {
        unsafe { libc::_exit(0) }
    }
    reaper.as_raw()
}

fn run_reaper(compositor: Pid) -> ! {
    set_process_name("tema reaper");
    if let Err(error) = install_forwarders(compositor) {
        eprintln!("reaper: signal handlers: {error}");
    }
    let status = loop {
        match wait() {
            Ok(WaitStatus::Exited(pid, code)) if pid == compositor => {
                break ChildExit::Code(code);
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) if pid == compositor => {
                break ChildExit::Signal(signal as i32);
            }
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => {}
            Err(error) => {
                eprintln!("reaper: wait: {error}");
                std::process::exit(1);
            }
        }
    };
    std::process::exit(exit_code(status).into());
}

fn install_forwarders(compositor: Pid) -> io::Result<()> {
    COMPOSITOR.store(compositor.as_raw(), Ordering::Relaxed);
    let action = SigAction::new(
        SigHandler::Handler(forward_to_compositor),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        unsafe { sigaction(signal, &action) }?;
    }
    Ok(())
}

/// Async-signal-safe: an atomic load and `kill`.
extern "C" fn forward_to_compositor(signal: libc::c_int) {
    let pid = COMPOSITOR.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe { libc::kill(pid, signal) };
    }
}

fn fail(what: &str, error: &impl std::fmt::Display) -> ! {
    eprintln!("reaper: {what}: {error}");
    let _ = io::stderr().flush();
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::{exit_code, ChildExit};

    #[test]
    fn exit_status_passes_a_code_through_and_maps_a_signal() {
        assert_eq!(exit_code(ChildExit::Code(0)), 0);
        assert_eq!(exit_code(ChildExit::Code(7)), 7);
        assert_eq!(exit_code(ChildExit::Code(255)), 255);
        assert_eq!(exit_code(ChildExit::Code(-1)), 1);
        assert_eq!(exit_code(ChildExit::Code(256)), 1);
        assert_eq!(exit_code(ChildExit::Signal(libc::SIGHUP)), 129);
        assert_eq!(exit_code(ChildExit::Signal(libc::SIGINT)), 130);
        assert_eq!(exit_code(ChildExit::Signal(libc::SIGKILL)), 137);
        assert_eq!(exit_code(ChildExit::Signal(libc::SIGTERM)), 143);
        assert_eq!(exit_code(ChildExit::Signal(-1)), 1);
    }
}
