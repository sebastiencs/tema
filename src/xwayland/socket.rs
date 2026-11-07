//! One X display: a lock file plus a listening filesystem socket.
//!
//! The lock file holds this process's pid, because the compositor keeps the
//! display across Xwayland restarts. A lock whose pid is dead is removed and
//! claimed. A live pid, or a file that is not a pid, is left alone.

use std::{
    io::{Error, ErrorKind},
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    rc::Rc,
};

use nix::{
    errno::Errno,
    fcntl::{open, OFlag},
    sys::{
        signal::kill,
        socket::{bind, listen, socket, AddressFamily, Backlog, SockFlag, SockType, UnixAddr},
        stat::{fchmod, stat, Mode, SFlag},
    },
    unistd::{self, AccessFlags, Pid},
};

const LISTEN_BACKLOG: i32 = 4096;

#[derive(Debug)]
pub(crate) struct XDisplay {
    number: u32,
    listener: Rc<OwnedFd>,
    socket_path: PathBuf,
    lock_path: PathBuf,
}

impl XDisplay {
    pub(crate) fn number(&self) -> u32 {
        self.number
    }

    pub(crate) fn listener(&self) -> &Rc<OwnedFd> {
        &self.listener
    }
}

impl Drop for XDisplay {
    fn drop(&mut self) {
        unlink_path(&self.socket_path);
        unlink_path(&self.lock_path);
    }
}

#[derive(Debug)]
pub(crate) enum XDisplayError {
    MissingDirectory(PathBuf),
    NotADirectory(PathBuf),
    NotWritable(PathBuf),
    AddressesInUse,
    Socket(Error),
    Bind { path: PathBuf, source: Error },
}

impl std::fmt::Display for XDisplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDirectory(path) => {
                write!(formatter, "X socket directory {} does not exist", path.display())
            }
            Self::NotADirectory(path) => {
                write!(formatter, "X socket directory {} is not a directory", path.display())
            }
            Self::NotWritable(path) => {
                write!(formatter, "X socket directory {} is not writable", path.display())
            }
            Self::AddressesInUse => {
                formatter.write_str("every X display in the requested range is in use")
            }
            Self::Socket(source) => write!(formatter, "could not create an X socket: {source}"),
            Self::Bind { path, source } => {
                write!(formatter, "could not bind X socket {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for XDisplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Socket(source) | Self::Bind { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn allocate() -> Result<XDisplay, XDisplayError> {
    allocate_in(Path::new("/tmp/.X11-unix"), Path::new("/tmp"), 500..1500)
}

pub(crate) fn allocate_in(
    socket_dir: &Path,
    lock_dir: &Path,
    numbers: std::ops::Range<u32>,
) -> Result<XDisplay, XDisplayError> {
    check_socket_dir(socket_dir)?;
    // A bind failure is kept. It is reported only when no number was merely
    // busy: otherwise the range was in use, which is the useful error.
    let mut bind_failure = None;
    let mut skipped = false;
    for number in numbers {
        match claim(socket_dir, lock_dir, number) {
            Ok(display) => return Ok(display),
            Err(ClaimError::Skip(reason)) => {
                skipped = true;
                log::debug!("skipping X display :{number}: {reason}");
            }
            Err(ClaimError::Socket(source)) => return Err(XDisplayError::Socket(source)),
            Err(ClaimError::Bind { path, source }) => {
                log::debug!(
                    "skipping X display :{number}: could not bind {}: {source}",
                    path.display()
                );
                bind_failure = Some(XDisplayError::Bind { path, source });
            }
        }
    }
    match bind_failure {
        Some(error) if !skipped => Err(error),
        _ => Err(XDisplayError::AddressesInUse),
    }
}

enum ClaimError {
    Skip(String),
    Socket(Error),
    Bind { path: PathBuf, source: Error },
}

fn check_socket_dir(path: &Path) -> Result<(), XDisplayError> {
    let info = match stat(path) {
        Ok(info) => info,
        Err(Errno::ENOENT) => return Err(XDisplayError::MissingDirectory(path.to_path_buf())),
        Err(error) => return Err(XDisplayError::Socket(error.into())),
    };
    let kind = SFlag::from_bits_truncate(info.st_mode) & SFlag::S_IFMT;
    if kind != SFlag::S_IFDIR {
        return Err(XDisplayError::NotADirectory(path.to_path_buf()));
    }
    match unistd::access(path, AccessFlags::W_OK) {
        Ok(()) => Ok(()),
        Err(Errno::EACCES | Errno::EROFS) => Err(XDisplayError::NotWritable(path.to_path_buf())),
        Err(error) => Err(XDisplayError::Socket(error.into())),
    }
}

fn claim(socket_dir: &Path, lock_dir: &Path, number: u32) -> Result<XDisplay, ClaimError> {
    let lock_path = lock_dir.join(format!(".X{number}-lock"));
    let socket_path = socket_dir.join(format!("X{number}"));
    let lock = match open_lock(&lock_path) {
        Ok(lock) => lock,
        Err(reason) => return Err(ClaimError::Skip(reason)),
    };
    match create_listener(number, &lock, &lock_path, &socket_path) {
        Ok(display) => Ok(display),
        Err(error) => {
            let _ = unistd::unlink(&lock_path);
            let _ = unistd::unlink(&socket_path);
            Err(error)
        }
    }
}

/// `Ok` is a lock file this process created. `Err` means this number is taken.
fn open_lock(path: &Path) -> Result<OwnedFd, String> {
    let flags = OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_CLOEXEC;
    let mode = Mode::S_IRUSR | Mode::S_IRGRP | Mode::S_IROTH;
    for attempt in 0..2 {
        match open(path, flags, mode) {
            Ok(fd) => {
                if let Err(error) = fchmod(&fd, mode) {
                    drop(fd);
                    let _ = unistd::unlink(path);
                    return Err(format!("could not set the lock mode: {error}"));
                }
                return Ok(fd);
            }
            Err(Errno::EEXIST) if attempt == 0 => match lock_owner(path) {
                Owner::Dead => match unistd::unlink(path) {
                    Ok(()) | Err(Errno::ENOENT) => continue,
                    Err(error) => return Err(format!("could not remove the stale lock: {error}")),
                },
                Owner::Held(pid) => return Err(format!("lock is held by pid {pid}")),
                Owner::NotAPid => return Err("lock file is not a pid".to_string()),
            },
            Err(Errno::EEXIST) => return Err("lock file is still present".to_string()),
            Err(error) => return Err(format!("could not create the lock: {error}")),
        }
    }
    Err("lock file is still present".to_string())
}

enum Owner {
    Dead,
    Held(i32),
    NotAPid,
}

fn lock_owner(path: &Path) -> Owner {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Owner::NotAPid,
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        return Owner::NotAPid;
    };
    match kill(Pid::from_raw(pid), None) {
        Err(Errno::ESRCH) => Owner::Dead,
        _ => Owner::Held(pid),
    }
}

fn create_listener(
    number: u32,
    lock: &OwnedFd,
    lock_path: &Path,
    socket_path: &Path,
) -> Result<XDisplay, ClaimError> {
    match unistd::unlink(socket_path) {
        Ok(()) | Err(Errno::ENOENT) => {}
        Err(error) => {
            return Err(ClaimError::Bind { path: socket_path.to_path_buf(), source: error.into() });
        }
    }
    let listener = socket(AddressFamily::Unix, SockType::Stream, SockFlag::SOCK_CLOEXEC, None)
        .map_err(|error| ClaimError::Socket(error.into()))?;
    let address = UnixAddr::new(socket_path).map_err(|error| ClaimError::Bind {
        path: socket_path.to_path_buf(),
        source: error.into(),
    })?;
    bind(listener.as_raw_fd(), &address).map_err(|error| ClaimError::Bind {
        path: socket_path.to_path_buf(),
        source: error.into(),
    })?;
    let backlog = Backlog::new(LISTEN_BACKLOG).map_err(|error| ClaimError::Bind {
        path: socket_path.to_path_buf(),
        source: error.into(),
    })?;
    listen(&listener, backlog).map_err(|error| ClaimError::Bind {
        path: socket_path.to_path_buf(),
        source: error.into(),
    })?;
    let pid = format!("{:>10}\n", unistd::getpid().as_raw());
    write_all(lock, pid.as_bytes())
        .map_err(|error| ClaimError::Bind { path: lock_path.to_path_buf(), source: error })?;
    Ok(XDisplay {
        number,
        listener: Rc::new(listener),
        socket_path: socket_path.to_path_buf(),
        lock_path: lock_path.to_path_buf(),
    })
}

fn write_all(fd: &OwnedFd, bytes: &[u8]) -> Result<(), Error> {
    let mut offset = 0;
    while offset < bytes.len() {
        match unistd::write(fd, &bytes[offset..]) {
            Ok(0) => {
                return Err(Error::new(ErrorKind::WriteZero, "short write on the X lock file"))
            }
            Ok(wrote) => offset += wrote,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn unlink_path(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => log::warn!("could not remove {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixStream},
    };

    use nix::{
        fcntl::{fcntl, FcntlArg, FdFlag},
        unistd,
    };

    use super::*;

    struct Dirs {
        _root: mktemp::Temp,
        sockets: PathBuf,
        locks: PathBuf,
    }

    fn temp_dirs() -> Dirs {
        let root = mktemp::Temp::new_dir().unwrap();
        let sockets = root.to_path_buf().join("sockets");
        let locks = root.to_path_buf().join("locks");
        fs::create_dir(&sockets).unwrap();
        fs::create_dir(&locks).unwrap();
        Dirs { _root: root, sockets, locks }
    }

    fn lock_text(pid: i32) -> String {
        format!("{pid:>10}\n")
    }

    #[test]
    fn the_first_free_number_is_chosen() {
        let dirs = temp_dirs();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 8..10).unwrap();
        assert_eq!(display.number(), 8);
        assert_eq!(
            fs::read_to_string(&display.lock_path).unwrap(),
            lock_text(unistd::getpid().as_raw())
        );
        let mode = fs::metadata(&display.lock_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o444);
        let flags = fcntl(display.listener(), FcntlArg::F_GETFD).unwrap();
        assert!(FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC));

        let next = allocate_in(&dirs.sockets, &dirs.locks, 8..10).unwrap();
        assert_eq!(next.number(), 9);
    }

    #[test]
    fn a_lock_held_by_our_pid_is_skipped() {
        let dirs = temp_dirs();
        fs::write(dirs.locks.join(".X4-lock"), lock_text(unistd::getpid().as_raw())).unwrap();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 4..6).unwrap();
        assert_eq!(display.number(), 5);
        assert!(dirs.locks.join(".X4-lock").exists());
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let dirs = temp_dirs();
        fs::write(dirs.locks.join(".X4-lock"), lock_text(i32::MAX)).unwrap();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 4..6).unwrap();
        assert_eq!(display.number(), 4);
        assert_eq!(
            fs::read_to_string(&display.lock_path).unwrap(),
            lock_text(unistd::getpid().as_raw())
        );
    }

    #[test]
    fn a_garbage_lock_is_skipped() {
        let dirs = temp_dirs();
        fs::write(dirs.locks.join(".X4-lock"), "not-a-pid\n").unwrap();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 4..6).unwrap();
        assert_eq!(display.number(), 5);
        assert_eq!(fs::read_to_string(dirs.locks.join(".X4-lock")).unwrap(), "not-a-pid\n");
    }

    #[test]
    fn a_client_can_connect() {
        let dirs = temp_dirs();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 1..2).unwrap();
        UnixStream::connect(&display.socket_path).unwrap();
    }

    #[test]
    fn drop_unlinks_the_socket_and_the_lock() {
        let dirs = temp_dirs();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 1..2).unwrap();
        let socket_path = display.socket_path.clone();
        let lock_path = display.lock_path.clone();
        assert!(socket_path.exists());
        assert!(lock_path.exists());
        drop(display);
        assert!(!socket_path.exists());
        assert!(!lock_path.exists());
    }

    #[test]
    fn a_missing_socket_directory_is_reported() {
        let dirs = temp_dirs();
        let error = allocate_in(&dirs.sockets.join("missing"), &dirs.locks, 1..2).unwrap_err();
        assert!(matches!(error, XDisplayError::MissingDirectory(_)), "{error}");
    }

    #[test]
    fn a_socket_directory_that_is_a_file_is_reported() {
        let dirs = temp_dirs();
        let file = dirs.sockets.join("not-a-dir");
        fs::write(&file, b"").unwrap();
        let error = allocate_in(&file, &dirs.locks, 1..2).unwrap_err();
        assert!(matches!(error, XDisplayError::NotADirectory(_)), "{error}");
    }

    #[test]
    fn a_read_only_socket_directory_is_reported() {
        let dirs = temp_dirs();
        fs::set_permissions(&dirs.sockets, fs::Permissions::from_mode(0o555)).unwrap();
        let error = allocate_in(&dirs.sockets, &dirs.locks, 1..2).unwrap_err();
        fs::set_permissions(&dirs.sockets, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(error, XDisplayError::NotWritable(_)), "{error}");
    }

    #[test]
    fn a_socket_name_that_cannot_be_removed_is_skipped() {
        let dirs = temp_dirs();
        fs::create_dir(dirs.sockets.join("X3")).unwrap();
        let display = allocate_in(&dirs.sockets, &dirs.locks, 3..5).unwrap();
        assert_eq!(display.number(), 4);
        assert!(!dirs.locks.join(".X3-lock").exists());
        assert!(dirs.sockets.join("X3").is_dir());
    }

    #[test]
    fn a_bind_failure_on_the_only_number_is_returned() {
        let dirs = temp_dirs();
        fs::create_dir(dirs.sockets.join("X3")).unwrap();
        let error = allocate_in(&dirs.sockets, &dirs.locks, 3..4).unwrap_err();
        assert!(matches!(error, XDisplayError::Bind { .. }), "{error}");
        assert!(!dirs.locks.join(".X3-lock").exists());
    }

    #[test]
    fn every_taken_display_reports_the_range_in_use() {
        let dirs = temp_dirs();
        fs::write(dirs.locks.join(".X3-lock"), lock_text(unistd::getpid().as_raw())).unwrap();
        let error = allocate_in(&dirs.sockets, &dirs.locks, 3..4).unwrap_err();
        assert!(matches!(error, XDisplayError::AddressesInUse), "{error}");
    }
}
