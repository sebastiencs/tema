//! The process that forks programs for the compositor.
//!
//! It is a separate process with a blocking `poll` loop. The compositor talks
//! to it over one seqpacket socket. Children get `/dev/null` on standard
//! streams unless a spawn puts something else there.

use std::{
    collections::BTreeMap,
    env,
    ffi::{CString, OsString},
    io::{Error, ErrorKind},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStringExt, fs::OpenOptionsExt},
    },
    thread,
};

use nix::{
    errno::Errno,
    poll::{poll, PollFd, PollFlags, PollTimeout},
    sys::wait::{waitpid, WaitStatus},
    unistd::{self, execvpe, fork, getppid, setpgid, setsid, ForkResult, Pid},
};

use super::{
    fd_map::{apply, plan, FdOp},
    protocol::{decode, encode, FromForker, ToForker, MAX_MESSAGE},
    sys::{
        close_range_from, dup_to, pidfd_open, pipe_cloexec, recv_with_fds, reset_signals,
        send_with_fds, set_cloexec, set_pdeathsig, set_process_name, socketpair_dgram,
        socketpair_seqpacket, waitid_pidfd, MAX_ATTACHED_FDS,
    },
};

/// Socket the compositor keeps, plus a pidfd for the forker itself.
pub(crate) struct ForkerHandle {
    pub(crate) socket: OwnedFd,
    pub(crate) pidfd: OwnedFd,
    pub(crate) pid: i32,
}

const SOCKET_SLOT: RawFd = 3;

/// Start the forker.
///
/// `reaper_pid` is the process the forker is reparented to. `None` skips that
/// check and forks once, which is what tests use. Production passes the reaper
/// and double-forks so the forker is not a child of the compositor.
pub(crate) fn spawn_forker(reaper_pid: Option<i32>) -> std::io::Result<ForkerHandle> {
    let (parent_socket, child_socket) = socketpair_seqpacket()?;
    match reaper_pid {
        None => fork_once(parent_socket, child_socket),
        Some(reaper) => fork_under_reaper(parent_socket, child_socket, reaper),
    }
}

fn fork_once(parent_socket: OwnedFd, child_socket: OwnedFd) -> std::io::Result<ForkerHandle> {
    match unsafe { fork() }.map_err(Error::from)? {
        ForkResult::Child => {
            // Drop the compositor's end. Keeping it would hide the peer close.
            drop(parent_socket);
            run_forker(child_socket, None);
        }
        ForkResult::Parent { child } => {
            drop(child_socket);
            Ok(ForkerHandle {
                pidfd: pidfd_open(child.as_raw())?,
                pid: child.as_raw(),
                socket: parent_socket,
            })
        }
    }
}

fn fork_under_reaper(
    parent_socket: OwnedFd,
    child_socket: OwnedFd,
    reaper: i32,
) -> std::io::Result<ForkerHandle> {
    let (report_read, report_write) = socketpair_dgram()?;
    match unsafe { fork() }.map_err(Error::from)? {
        ForkResult::Child => {
            drop(parent_socket);
            drop(report_read);
            intermediate(child_socket, report_write, reaper);
        }
        ForkResult::Parent { child } => {
            drop(child_socket);
            drop(report_write);
            let mut buf = [0u8; 4];
            let (len, fds) = recv_with_fds(&report_read, &mut buf)?;
            match waitpid(child, None).map_err(Error::from)? {
                WaitStatus::Exited(_, 0) => {}
                other => {
                    return Err(Error::other(format!("forker intermediate exited {other:?}")));
                }
            }
            let [pidfd] = fds
                .try_into()
                .map_err(|_| Error::new(ErrorKind::InvalidData, "forker did not send its pidfd"))?;
            if len != 4 {
                return Err(Error::new(ErrorKind::InvalidData, "forker pid was truncated"));
            }
            Ok(ForkerHandle { socket: parent_socket, pidfd, pid: i32::from_ne_bytes(buf) })
        }
    }
}

fn intermediate(child_socket: OwnedFd, report_write: OwnedFd, reaper: i32) -> ! {
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            drop(report_write);
            run_forker(child_socket, Some(reaper));
        }
        Ok(ForkResult::Parent { child }) => {
            drop(child_socket);
            let pid = child.as_raw();
            if let Ok(pidfd) = pidfd_open(pid) {
                let _ = send_with_fds(&report_write, &pid.to_ne_bytes(), &[pidfd.as_raw_fd()]);
            }
            unsafe { libc::_exit(0) }
        }
        Err(_) => unsafe { libc::_exit(1) },
    }
}

fn run_forker(socket: OwnedFd, reaper: Option<i32>) -> ! {
    set_process_name("tema forker");
    if let Some(reaper) = reaper {
        if bind_to_reaper(reaper).is_err() {
            unsafe { libc::_exit(1) }
        }
    }
    if setsid().is_err() || reset_signals().is_err() {
        unsafe { libc::_exit(1) }
    }
    let socket = match park_socket(socket).and_then(|socket| {
        null_stdio()?;
        close_range_from(SOCKET_SLOT + 1)?;
        Ok(socket)
    }) {
        Ok(socket) => socket,
        Err(_) => unsafe { libc::_exit(1) },
    };
    prepare_environment();
    install_panic_hook();
    event_loop(socket);
}

/// `PDEATHSIG` is armed only once `reaper` is our parent. Arming it earlier
/// would kill us when the intermediate process exits.
fn bind_to_reaper(reaper: i32) -> std::io::Result<()> {
    let initial = getppid().as_raw();
    if initial != reaper {
        for _ in 0..1_000_000 {
            if getppid().as_raw() != initial {
                break;
            }
            thread::yield_now();
        }
        if getppid().as_raw() == initial {
            unsafe { libc::_exit(1) }
        }
    }
    set_pdeathsig(libc::SIGKILL)?;
    if getppid().as_raw() != reaper {
        unsafe { libc::_exit(0) }
    }
    Ok(())
}

fn park_socket(socket: OwnedFd) -> std::io::Result<OwnedFd> {
    if socket.as_raw_fd() == SOCKET_SLOT {
        set_cloexec(&socket, true)?;
        return Ok(socket);
    }
    dup_to(&socket, SOCKET_SLOT)?;
    drop(socket);
    Ok(unsafe { OwnedFd::from_raw_fd(SOCKET_SLOT) })
}

fn null_stdio() -> std::io::Result<()> {
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open("/dev/null")?;
    for slot in 0..3 {
        dup_no_cloexec(null.as_raw_fd(), slot)?;
    }
    Ok(())
}

fn dup_no_cloexec(from: RawFd, to: RawFd) -> std::io::Result<()> {
    let from = unsafe { BorrowedFd::borrow_raw(from) };
    let mut installed = unsafe { OwnedFd::from_raw_fd(to) };
    let result = unistd::dup3(&from, &mut installed, nix::fcntl::OFlag::empty());
    std::mem::forget(installed);
    result.map_err(Error::from)
}

fn prepare_environment() {
    // The forker is a single thread. `set_var` is unsafe because of that rule.
    unsafe {
        env::remove_var("DISPLAY");
        env::remove_var("WAYLAND_DISPLAY");
        env::set_var("XDG_SESSION_TYPE", "wayland");
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let socket = unsafe { BorrowedFd::borrow_raw(SOCKET_SLOT) };
        let _ = send(
            &socket,
            &FromForker::Log {
                level: log::Level::Error as u8,
                message: format!("forker panic: {info}"),
            },
            &[],
        );
    }));
}

fn event_loop(socket: OwnedFd) -> ! {
    let mut children: Vec<Running> = Vec::new();
    let mut buffer = vec![0u8; MAX_MESSAGE];
    loop {
        let mut polls = Vec::with_capacity(1 + children.len());
        polls.push(PollFd::new(socket.as_fd(), PollFlags::POLLIN));
        for child in &children {
            polls.push(PollFd::new(child.pidfd.as_fd(), PollFlags::POLLIN));
        }
        match poll(&mut polls, PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(_) => unsafe { libc::_exit(1) },
        }

        let socket_ready = happened(&polls[0]);
        let child_ready: Vec<bool> = polls[1..].iter().map(happened).collect();
        drop(polls);
        if socket_ready {
            match recv_with_fds(&socket, &mut buffer) {
                Ok((0, fds)) if fds.is_empty() => unsafe { libc::_exit(0) },
                Ok((len, fds)) => dispatch(&socket, &buffer[..len], fds, &mut children),
                Err(_) => unsafe { libc::_exit(0) },
            }
        }

        let mut finished = Vec::new();
        for index in 0..child_ready.len() {
            if !child_ready[index] {
                continue;
            }
            match waitid_pidfd(&children[index].pidfd) {
                Ok(Some(exit)) => finished.push((index, Ok(exit))),
                Ok(None) => {}
                Err(error) => finished.push((index, Err(error))),
            }
        }
        for (index, result) in finished.into_iter().rev() {
            let child = children.swap_remove(index);
            let message = match result {
                Ok(exit) => FromForker::Exited { token: child.token, exit },
                Err(error) => FromForker::Log {
                    level: log::Level::Error as u8,
                    message: format!("waitid for pid {} failed: {error}", child.pid),
                },
            };
            if send(&socket, &message, &[]).is_err() {
                unsafe { libc::_exit(0) }
            }
        }
    }
}

struct Running {
    token: u64,
    pid: i32,
    pidfd: OwnedFd,
}

fn happened(fd: &PollFd) -> bool {
    fd.revents().is_some_and(|events| {
        events.intersects(
            PollFlags::POLLIN | PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL,
        )
    })
}

fn dispatch(socket: &OwnedFd, bytes: &[u8], fds: Vec<OwnedFd>, children: &mut Vec<Running>) {
    let message = match decode::<ToForker>(bytes) {
        Ok(message) => message,
        Err(error) => {
            let _ = send(socket, &log_error(format!("bad message: {error}")), &[]);
            return;
        }
    };
    match message {
        ToForker::SetEnv { key, value } => {
            // Single-threaded process; see [`prepare_environment`].
            let key = OsString::from_vec(key);
            unsafe {
                match value {
                    Some(value) => env::set_var(key, OsString::from_vec(value)),
                    None => env::remove_var(key),
                }
            }
        }
        ToForker::Spawn { token, program, args, env, slots } => {
            spawn(socket, token, &program, &args, &env, &slots, fds, children);
        }
    }
}

fn spawn(
    socket: &OwnedFd,
    token: u64,
    program: &[u8],
    args: &[Vec<u8>],
    env_overrides: &[(Vec<u8>, Option<Vec<u8>>)],
    slots: &[i32],
    fds: Vec<OwnedFd>,
    children: &mut Vec<Running>,
) {
    let fail = |error: String| {
        let _ = send(socket, &FromForker::SpawnFailed { token, error }, &[]);
    };
    if slots.len() != fds.len() || fds.len() > MAX_ATTACHED_FDS {
        fail("file descriptors do not match slots".into());
        return;
    }
    let argv = match argv_of(program, args) {
        Ok(argv) => argv,
        Err(error) => return fail(error.to_string()),
    };
    let envp = match envp_of(env_overrides) {
        Ok(envp) => envp,
        Err(error) => return fail(error.to_string()),
    };
    let (read, write) = match pipe_cloexec() {
        Ok(pipe) => pipe,
        Err(error) => return fail(error.to_string()),
    };
    let child = match unsafe { fork() } {
        Ok(ForkResult::Child) => child_exec(&fds, slots, &write, &argv, &envp),
        Ok(ForkResult::Parent { child }) => child,
        Err(error) => return fail(error.to_string()),
    };
    drop(write);
    match exec_error(&read) {
        Ok(()) => {}
        Err(error) => {
            let _ = waitpid(child, None);
            fail(error);
            return;
        }
    }
    let pidfd = match pidfd_open(child.as_raw()) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            let _ = waitpid(child, None);
            fail(error.to_string());
            return;
        }
    };
    let reported = match pidfd.try_clone() {
        Ok(reported) => reported,
        Err(error) => {
            let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
            let _ = waitpid(child, None);
            return fail(error.to_string());
        }
    };
    if send(socket, &FromForker::Spawned { token, pid: child.as_raw() }, &[reported.as_raw_fd()])
        .is_err()
    {
        unsafe { libc::_exit(0) }
    }
    children.push(Running { token, pid: child.as_raw(), pidfd });
}

fn child_exec(
    fds: &[OwnedFd],
    slots: &[i32],
    write: &OwnedFd,
    argv: &[CString],
    envp: &[CString],
) -> ! {
    let pipe_fd = write.as_raw_fd();
    let entries: Vec<(RawFd, i32)> = fds
        .iter()
        .zip(slots)
        .map(|(fd, slot)| (fd.as_raw_fd(), *slot))
        .collect();
    let ops = plan(&entries, &[pipe_fd]);
    let kept = match apply(&ops) {
        Ok(kept) => kept,
        Err(error) => die(pipe_fd, &error.to_string()),
    };
    let pipe_fd = parked_pipe(&ops, pipe_fd, &kept).unwrap_or(pipe_fd);
    if let Err(error) = reset_signals() {
        die(pipe_fd, &error.to_string());
    }
    if let Err(error) = setpgid(Pid::from_raw(0), Pid::from_raw(0)) {
        die(pipe_fd, &error.to_string());
    }
    let Err(error) = execvpe(&argv[0], argv, envp);
    die(pipe_fd, &error.to_string());
}

fn parked_pipe(ops: &[FdOp], pipe_fd: RawFd, kept: &[OwnedFd]) -> Option<RawFd> {
    let mut copies = 0;
    for op in ops {
        let FdOp::DupAbove { from, .. } = op else { continue };
        if *from == pipe_fd {
            return Some(kept.get(copies)?.as_raw_fd());
        }
        copies += 1;
    }
    None
}

fn die(pipe_fd: RawFd, message: &str) -> ! {
    let pipe = unsafe { BorrowedFd::borrow_raw(pipe_fd) };
    let _ = unistd::write(&pipe, message.as_bytes());
    unsafe { libc::_exit(127) }
}

fn exec_error(read: &OwnedFd) -> Result<(), String> {
    let mut buffer = [0u8; 256];
    let mut message = Vec::new();
    loop {
        match unistd::read(read, &mut buffer) {
            Ok(0) => break,
            Ok(len) => {
                message.extend_from_slice(&buffer[..len]);
                if message.len() > 4096 {
                    break;
                }
            }
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    if message.is_empty() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&message).into_owned())
    }
}

fn argv_of(program: &[u8], args: &[Vec<u8>]) -> std::io::Result<Vec<CString>> {
    let mut argv = Vec::with_capacity(1 + args.len());
    argv.push(c_bytes(program)?);
    for arg in args {
        argv.push(c_bytes(arg)?);
    }
    Ok(argv)
}

fn envp_of(overrides: &[(Vec<u8>, Option<Vec<u8>>)]) -> std::io::Result<Vec<CString>> {
    let mut values = BTreeMap::new();
    for (key, value) in env::vars_os() {
        values.insert(key.into_vec(), value.into_vec());
    }
    for (key, value) in overrides {
        match value {
            Some(value) => {
                values.insert(key.clone(), value.clone());
            }
            None => {
                values.remove(key);
            }
        }
    }
    let mut envp = Vec::with_capacity(values.len());
    for (key, value) in values {
        if key.contains(&0) || value.contains(&0) {
            return Err(Error::new(ErrorKind::InvalidInput, "interior nul in environment"));
        }
        let mut entry = key;
        entry.push(b'=');
        entry.extend(value);
        envp.push(c_bytes(&entry)?);
    }
    Ok(envp)
}

fn c_bytes(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes.to_vec()).map_err(|_| Error::new(ErrorKind::InvalidInput, "interior nul"))
}

fn log_error(message: String) -> FromForker {
    FromForker::Log { level: log::Level::Error as u8, message }
}

/// Hold this across `spawn_forker` in tests. Cargo runs tests on many threads,
/// and the forker keeps running Rust after `fork`.
#[cfg(test)]
pub(crate) fn lock_forks() -> std::sync::MutexGuard<'static, ()> {
    tests::FORK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn send(socket: &impl AsRawFd, message: &FromForker, fds: &[RawFd]) -> std::io::Result<()> {
    send_with_fds(socket, &encode(message)?, fds)
}

#[cfg(test)]
mod tests {
    use std::{
        os::fd::{AsFd, OwnedFd},
        sync::Mutex,
        thread,
    };

    use nix::{
        poll::{poll, PollFd, PollFlags},
        sys::{
            signal::{pthread_sigmask, SigSet, SigmaskHow, Signal},
            wait::{waitid, Id, WaitPidFlag},
        },
        unistd::{self, Pid},
    };

    use {
        super::*,
        crate::process::sys::{waitid_pidfd, ChildExit},
    };

    pub(super) static FORK: Mutex<()> = Mutex::new(());

    struct Guard(Option<ForkerHandle>);

    impl Guard {
        fn start() -> Self {
            Self(Some(spawn_forker(None).expect("forker")))
        }

        fn socket(&self) -> &OwnedFd {
            &self.0.as_ref().unwrap().socket
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let Some(handle) = self.0.take() else { return };
            drop(handle.socket);
            for _ in 0..10_000 {
                if waitid_pidfd(&handle.pidfd).ok().flatten().is_some() {
                    return;
                }
                thread::yield_now();
            }
            let _ = nix::sys::signal::kill(Pid::from_raw(handle.pid), Signal::SIGKILL);
            let pidfd = unsafe { BorrowedFd::borrow_raw(handle.pidfd.as_raw_fd()) };
            let _ = waitid(Id::PIDFd(pidfd), WaitPidFlag::WEXITED);
        }
    }

    fn request(socket: &OwnedFd, message: &ToForker, fds: &[RawFd]) {
        send_with_fds(socket, &encode(message).unwrap(), fds).unwrap();
    }

    fn response(socket: &OwnedFd) -> (FromForker, Vec<OwnedFd>) {
        let mut polls = [PollFd::new(socket.as_fd(), PollFlags::POLLIN)];
        assert!(poll(&mut polls, 5_000u16).unwrap() > 0, "forker did not answer");
        let mut buffer = vec![0u8; MAX_MESSAGE];
        let (len, fds) = recv_with_fds(socket, &mut buffer).unwrap();
        (decode(&buffer[..len]).unwrap(), fds)
    }

    fn read_pipe(read: &OwnedFd) -> Vec<u8> {
        let mut polls = [PollFd::new(read.as_fd(), PollFlags::POLLIN)];
        let mut buffer = [0u8; 256];
        let mut output = Vec::new();
        loop {
            assert!(poll(&mut polls, 5_000u16).unwrap() > 0, "child output timed out");
            match unistd::read(read, &mut buffer).unwrap() {
                0 => break,
                len => output.extend_from_slice(&buffer[..len]),
            }
        }
        output
    }

    fn spawn_sh(script: &str, slots: Vec<i32>) -> ToForker {
        ToForker::Spawn {
            token: 1,
            program: b"sh".to_vec(),
            args: vec![b"-c".to_vec(), script.as_bytes().to_vec()],
            env: Vec::new(),
            slots,
        }
    }

    #[test]
    fn exit_status_is_reported() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let forker = Guard::start();
        request(forker.socket(), &spawn_sh("exit 3", Vec::new()), &[]);
        let (message, fds) = response(forker.socket());
        assert!(matches!(message, FromForker::Spawned { token: 1, .. }));
        assert_eq!(fds.len(), 1);
        let (message, fds) = response(forker.socket());
        assert_eq!(message, FromForker::Exited { token: 1, exit: ChildExit::Code(3) });
        assert!(fds.is_empty());
    }

    #[test]
    fn a_missing_program_is_enoent() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let forker = Guard::start();
        request(
            forker.socket(),
            &ToForker::Spawn {
                token: 4,
                program: b"tema-no-such-program".to_vec(),
                args: Vec::new(),
                env: Vec::new(),
                slots: Vec::new(),
            },
            &[],
        );
        let (message, _) = response(forker.socket());
        match message {
            FromForker::SpawnFailed { token: 4, error } => {
                assert!(error.contains("ENOENT"), "{error}")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_passed_pipe_receives_the_child_output() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let forker = Guard::start();
        let (read, write) = pipe_cloexec().unwrap();
        request(forker.socket(), &spawn_sh("echo hi >&5", vec![5]), &[write.as_raw_fd()]);
        drop(write);
        let (message, _) = response(forker.socket());
        assert!(matches!(message, FromForker::Spawned { token: 1, .. }));
        assert_eq!(read_pipe(&read), b"hi\n");
        let (message, _) = response(forker.socket());
        assert!(matches!(message, FromForker::Exited { token: 1, exit: ChildExit::Code(0) }));
    }

    #[test]
    fn setenv_reaches_later_spawns_and_can_be_cleared() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let forker = Guard::start();
        request(
            forker.socket(),
            &ToForker::SetEnv { key: b"FOO".to_vec(), value: Some(b"bar".to_vec()) },
            &[],
        );
        let output = spawn_and_read(&forker, "printf %s \"$FOO\" >&5");
        assert_eq!(output, b"bar");

        request(forker.socket(), &ToForker::SetEnv { key: b"FOO".to_vec(), value: None }, &[]);
        let output = spawn_and_read(&forker, "printf %s \"$FOO\" >&5");
        assert_eq!(output, b"");
    }

    fn spawn_and_read(forker: &Guard, script: &str) -> Vec<u8> {
        let (read, write) = pipe_cloexec().unwrap();
        request(forker.socket(), &spawn_sh(script, vec![5]), &[write.as_raw_fd()]);
        drop(write);
        let (message, _) = response(forker.socket());
        assert!(matches!(message, FromForker::Spawned { .. }), "{message:?}");
        let output = read_pipe(&read);
        let (message, _) = response(forker.socket());
        assert!(
            matches!(message, FromForker::Exited { exit: ChildExit::Code(0), .. }),
            "{message:?}"
        );
        output
    }

    #[test]
    fn a_child_does_not_inherit_a_blocked_sigterm() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let mut blocked = SigSet::empty();
        blocked.add(Signal::SIGTERM);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous)).unwrap();
        let restore = RestoreMask(previous);
        let forker = Guard::start();
        let output = spawn_and_read(&forker, "cat /proc/self/status >&5");
        drop(restore);
        let text = String::from_utf8_lossy(&output);
        let line = text
            .lines()
            .find(|line| line.starts_with("SigBlk"))
            .unwrap_or_else(|| panic!("{text}"));
        let hex: String = line.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let low = u64::from_str_radix(&hex[hex.len() - 16..], 16).unwrap();
        assert_eq!(low & (1 << (libc::SIGTERM - 1)), 0, "{line}");
    }

    struct RestoreMask(SigSet);

    impl Drop for RestoreMask {
        fn drop(&mut self) {
            let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.0), None);
        }
    }

    #[test]
    fn closing_the_socket_reaps_the_forker() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        let mut forker = Guard::start();
        let handle = forker.0.take().unwrap();
        drop(handle.socket);
        for _ in 0..10_000 {
            if let Some(exit) = waitid_pidfd(&handle.pidfd).unwrap() {
                assert_eq!(exit, ChildExit::Code(0));
                return;
            }
            thread::yield_now();
        }
        panic!("forker stayed alive after the socket closed");
    }

    #[test]
    fn a_reaper_mismatch_exits() {
        let _lock = FORK.lock().unwrap_or_else(|error| error.into_inner());
        // The forker is reparented away from this process, so it cannot be
        // waited on. The pidfd still becomes readable when it exits.
        let handle = spawn_forker(Some(std::process::id() as i32)).unwrap();
        let mut polls = [PollFd::new(handle.pidfd.as_fd(), PollFlags::POLLIN)];
        assert!(
            poll(&mut polls, 5_000u16).unwrap() > 0,
            "forker stayed alive under the wrong reaper"
        );
    }
}
