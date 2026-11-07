//! One ignored test against a real Xwayland.
//!
//! It spawns `/usr/bin/Xwayland :N -rootless -wm <fd> -displayfd <fd>`,
//! handshakes on the socketpair, runs startup (extensions and the atom
//! batch), and reads `WM_NAME` on the root window. I/O is `poll`, `read`,
//! and `write`.
//!
//! Run it under a Wayland compositor:
//!
//! ```text
//! WAYLAND_DISPLAY=<socket> cargo test --offline --bin tema x11::live:: -- --ignored --test-threads=1
//! ```
//!
//! With `WAYLAND_DISPLAY` unset the test returns without spawning.

use std::{
    cell::RefCell,
    env, fs,
    io::{Error, ErrorKind},
    os::fd::{AsFd, AsRawFd, OwnedFd},
    path::Path,
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    fcntl::{self, FcntlArg, OFlag},
    poll::{poll, PollFd, PollFlags},
    unistd,
};

use crate::{
    process::{dup_above, pipe_cloexec, set_cloexec, socketpair_stream},
    runner::{AnyAction, Dispatcher, Redispatcher},
};

use super::{
    generated::Atom,
    property::{get_property, Property, PropertyError},
    startup::{Startup, StartupEvent, Status},
    State, XConnection,
};

const LIMIT: Duration = Duration::from_secs(20);
const PROGRAM: &str = "/usr/bin/Xwayland";

struct Live {
    child: Option<Child>,
    number: u32,
    wm: OwnedFd,
    displayfd: OwnedFd,
    stderr: OwnedFd,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(format!("/tmp/.X11-unix/X{}", self.number));
        let _ = fs::remove_file(format!("/tmp/.X{}-lock", self.number));
    }
}

#[test]
#[ignore = "needs a Wayland compositor; run: WAYLAND_DISPLAY=<socket> cargo test --offline --bin tema x11::live:: -- --ignored --test-threads=1"]
fn live_xwayland() {
    if env::var_os("WAYLAND_DISPLAY").is_none_or(|value| value.is_empty()) {
        eprintln!("WAYLAND_DISPLAY is unset; skipping");
        return;
    }
    let number = free_display();
    let mut live = spawn(number).unwrap_or_else(|error| panic!("spawn {PROGRAM}: {error}"));
    let deadline = Instant::now() + LIMIT;
    match session(&mut live, deadline) {
        Ok(()) => {}
        Err(error) => panic!("{error}\n{}", stderr_text(&live.stderr)),
    }
}

fn session(live: &mut Live, deadline: Instant) -> Result<(), String> {
    let mut stderr = Vec::new();
    let announced = read_display_number(live, &mut stderr, deadline)
        .map_err(|error| format!("{error}\n{}", String::from_utf8_lossy(&stderr)))?;
    if announced.trim() != live.number.to_string() {
        return Err(format!(
            "displayfd said {announced:?}, display is :{}\n{}",
            live.number,
            String::from_utf8_lossy(&stderr)
        ));
    }

    let (mut connection, setup) = XConnection::new();
    write_all(&live.wm, &setup, deadline).map_err(|error| error.to_string())?;
    let mut dispatcher = Dispatcher::default();
    let startup_events = Rc::new(RefCell::new(Vec::<StartupEvent>::new()));
    let property_events = Rc::new(RefCell::new(Vec::<Result<Property, PropertyError>>::new()));
    let mut startup = Startup::new();
    let mut started = false;
    let mut asked = false;
    let mut property = None;
    let mut stderr_open = true;
    let mut buffer = [0u8; 64 * 1024];

    let outcome = 'drive: loop {
        if Instant::now() >= deadline {
            break Err(format!("timed out in {:?}", connection.state()));
        }
        let outgoing = connection.take_outgoing();
        if !outgoing.is_empty() {
            if let Err(error) = write_all(&live.wm, &outgoing, deadline) {
                break Err(error.to_string());
            }
        }
        if connection.state() == State::Ready && !started {
            let queue = Rc::clone(&startup_events);
            let status = startup.start(
                &mut connection,
                Redispatcher::to_any(move |event| {
                    queue.borrow_mut().push(event);
                    AnyAction::empty()
                }),
            );
            started = true;
            if let Status::Failed(error) = status {
                break Err(error.to_string());
            }
            continue;
        }
        if matches!(startup.status(), Status::Ready(_)) && !asked {
            let Some(root) = connection.root() else {
                break Err("ready connection has no root".to_string());
            };
            let queue = Rc::clone(&property_events);
            if let Err(error) = get_property(
                &mut connection,
                root,
                Atom::WM_NAME,
                Atom::Any,
                256,
                Redispatcher::to_any(move |result| {
                    queue.borrow_mut().push(result);
                    AnyAction::empty()
                }),
            ) {
                break Err(error.to_string());
            }
            asked = true;
            continue;
        }
        if let Some(result) = property.take() {
            break finish(&startup, &connection, result);
        }

        let mut polls = vec![PollFd::new(live.wm.as_fd(), PollFlags::POLLIN)];
        if stderr_open {
            polls.push(PollFd::new(live.stderr.as_fd(), PollFlags::POLLIN));
        }
        if let Err(error) = wait_io(&mut polls, deadline) {
            break Err(error.to_string());
        }
        if stderr_open && readable(&polls[1]) {
            stderr_open = match drain(&live.stderr, &mut buffer, &mut stderr) {
                Ok(open) => open,
                Err(error) => break Err(error.to_string()),
            };
        }
        if readable(&polls[0]) {
            let count = match read_once(&live.wm, &mut buffer, deadline) {
                Ok(count) => count,
                Err(error) => break Err(error.to_string()),
            };
            if count == 0 {
                break Err("wm socket closed".to_string());
            }
            let read = connection.on_read(&buffer[..count], &mut dispatcher);
            if let Some(error) = read.dead {
                break Err(error.to_string());
            }
            let pending: Vec<_> = startup_events.borrow_mut().drain(..).collect();
            for event in pending {
                if let Status::Failed(error) = startup.handle(event, &mut connection) {
                    break 'drive Err(error.to_string());
                }
            }
            if let Some(result) = property_events.borrow_mut().drain(..).next() {
                property = Some(result);
            }
        }
    };
    outcome.map_err(|error| format!("{error}\n{}", String::from_utf8_lossy(&stderr)))
}

fn finish(
    startup: &Startup,
    connection: &XConnection,
    result: Result<Property, PropertyError>,
) -> Result<(), String> {
    let Status::Ready(bringup) = startup.status() else {
        return Err("property arrived before startup finished".to_string());
    };
    if bringup.atoms.WM_NAME != Atom::WM_NAME || bringup.atoms.WM_S0.0 == 0 {
        return Err(format!("atoms {:?} {:?}", bringup.atoms.WM_NAME, bringup.atoms.WM_S0));
    }
    if bringup.extensions.xfixes.major_version < 6 {
        return Err(format!("xfixes {}", bringup.extensions.xfixes.major_version));
    }
    result.map_err(|error| error.to_string())?;
    if connection.state() != State::Ready {
        return Err(format!("connection {:?} after GetProperty", connection.state()));
    }
    Ok(())
}

fn read_display_number(
    live: &Live,
    stderr: &mut Vec<u8>,
    deadline: Instant,
) -> Result<String, String> {
    let mut text = Vec::new();
    let mut buffer = [0u8; 256];
    let mut stderr_open = true;
    loop {
        if text.contains(&b'\n') || text.len() > 32 {
            break;
        }
        let mut polls = vec![PollFd::new(live.displayfd.as_fd(), PollFlags::POLLIN)];
        if stderr_open {
            polls.push(PollFd::new(live.stderr.as_fd(), PollFlags::POLLIN));
        }
        wait_io(&mut polls, deadline).map_err(|error| error.to_string())?;
        if stderr_open && readable(&polls[1]) {
            stderr_open =
                drain(&live.stderr, &mut buffer, stderr).map_err(|error| error.to_string())?;
        }
        if readable(&polls[0]) {
            let count = read_once(&live.displayfd, &mut buffer, deadline)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            text.extend_from_slice(&buffer[..count]);
        }
    }
    Ok(String::from_utf8_lossy(&text).into_owned())
}

fn spawn(number: u32) -> Result<Live, Error> {
    let (display_parent, display_child) = pipe_cloexec()?;
    let (wm_parent, wm_child) = socketpair_stream()?;
    let (stderr_parent, stderr_child) = pipe_cloexec()?;
    let display_child = dup_above(&display_child, 100)?;
    let wm_child = dup_above(&wm_child, 100)?;
    set_cloexec(&display_child, false)?;
    set_cloexec(&wm_child, false)?;
    set_nonblock(&display_parent)?;
    set_nonblock(&wm_parent)?;
    set_nonblock(&stderr_parent)?;

    let child = Command::new(PROGRAM)
        .arg(format!(":{number}"))
        .arg("-rootless")
        .arg("-wm")
        .arg(wm_child.as_raw_fd().to_string())
        .arg("-displayfd")
        .arg(display_child.as_raw_fd().to_string())
        .env_remove("WAYLAND_SOCKET")
        .env_remove("LIBEI_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_child))
        .spawn()?;
    drop(display_child);
    drop(wm_child);
    Ok(Live {
        child: Some(child),
        number,
        wm: wm_parent,
        displayfd: display_parent,
        stderr: stderr_parent,
    })
}

fn free_display() -> u32 {
    for number in 80..400 {
        let socket = format!("/tmp/.X11-unix/X{number}");
        let lock = format!("/tmp/.X{number}-lock");
        if !Path::new(&socket).exists() && !Path::new(&lock).exists() {
            return number;
        }
    }
    panic!("no free X display in 80..400");
}

fn set_nonblock(fd: &impl AsFd) -> Result<(), Error> {
    let flags = OFlag::from_bits_retain(fcntl::fcntl(fd, FcntlArg::F_GETFL)?);
    if !flags.contains(OFlag::O_NONBLOCK) {
        fcntl::fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    }
    Ok(())
}

fn write_all(fd: &OwnedFd, mut bytes: &[u8], deadline: Instant) -> Result<(), Error> {
    while !bytes.is_empty() {
        match unistd::write(fd, bytes) {
            Ok(0) => return Err(Error::new(ErrorKind::WriteZero, "short write")),
            Ok(count) => bytes = &bytes[count..],
            Err(Errno::EINTR) => {}
            Err(Errno::EAGAIN) => {
                let mut polls = [PollFd::new(fd.as_fd(), PollFlags::POLLOUT)];
                wait_io(&mut polls, deadline)?;
            }
            Err(error) => return Err(Error::from(error)),
        }
    }
    Ok(())
}

fn read_once(fd: &OwnedFd, buffer: &mut [u8], deadline: Instant) -> Result<usize, Error> {
    loop {
        match unistd::read(fd, buffer) {
            Ok(count) => return Ok(count),
            Err(Errno::EINTR) => {}
            Err(Errno::EAGAIN) => {
                let mut polls = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
                wait_io(&mut polls, deadline)?;
            }
            Err(error) => return Err(Error::from(error)),
        }
    }
}

/// Reads what is already buffered. Returns whether the pipe is still open.
fn drain(fd: &OwnedFd, buffer: &mut [u8], tail: &mut Vec<u8>) -> Result<bool, Error> {
    loop {
        match unistd::read(fd, buffer) {
            Ok(0) => return Ok(false),
            Err(Errno::EAGAIN) => return Ok(true),
            Ok(count) => append_tail(tail, &buffer[..count]),
            Err(Errno::EINTR) => {}
            Err(error) => return Err(Error::from(error)),
        }
    }
}

fn append_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    tail.extend_from_slice(chunk);
    if tail.len() > 4096 {
        let extra = tail.len() - 4096;
        tail.drain(..extra);
    }
}

fn wait_io(fds: &mut [PollFd], deadline: Instant) -> Result<(), Error> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Error::new(ErrorKind::TimedOut, "Xwayland"));
    }
    let millis = u16::try_from(left.as_millis()).unwrap_or(u16::MAX);
    if poll(fds, millis).map_err(Error::from)? == 0 {
        return Err(Error::new(ErrorKind::TimedOut, "Xwayland"));
    }
    Ok(())
}

fn readable(fd: &PollFd) -> bool {
    fd.revents().is_some_and(|flags| {
        flags.contains(PollFlags::POLLIN)
            || flags.contains(PollFlags::POLLHUP)
            || flags.contains(PollFlags::POLLERR)
    })
}

fn stderr_text(fd: &OwnedFd) -> String {
    let mut buffer = [0u8; 4096];
    let mut text = Vec::new();
    loop {
        match unistd::read(fd, &mut buffer) {
            Ok(0) | Err(Errno::EAGAIN) => break,
            Ok(count) => text.extend_from_slice(&buffer[..count]),
            Err(Errno::EINTR) => {}
            Err(_) => break,
        }
    }
    if text.len() > 4096 {
        text.drain(..text.len() - 4096);
    }
    String::from_utf8_lossy(&text).into_owned()
}
