//! Saved xdg-session-management state. Nothing reads it yet besides startup and shutdown.

use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub(crate) mod model;

const MAX_SESSION_AGE_SECS: u64 = 90 * 24 * 60 * 60;
const MAX_SESSIONS: usize = 256;
const MAX_TOPLEVELS: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct StoreFile {
    pub version: u32,
    pub sessions: BTreeMap<String, SessionRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SessionRecord {
    pub created: u64,
    pub last_used: u64,
    pub toplevels: BTreeMap<String, ToplevelRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ToplevelRecord {
    pub last_used: u64,
    pub output: Option<String>,
    pub workspace: Option<String>,
    pub floating: Option<FloatingRecord>,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FloatingRecord {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub frame_x: i32,
    pub frame_y: i32,
    pub frame_width: i32,
    pub frame_height: i32,
}

pub(crate) struct SessionStore {
    path: PathBuf,
    file: StoreFile,
    dirty: bool,
}

impl SessionStore {
    /// `$XDG_STATE_HOME/tema/sessions.json`, else `$HOME/.local/state/tema/sessions.json`.
    pub(crate) fn default_path() -> Option<PathBuf> {
        if let Ok(xdg_state_home) = env::var("XDG_STATE_HOME") {
            if !xdg_state_home.is_empty() {
                return Some(
                    PathBuf::from(xdg_state_home)
                        .join("tema")
                        .join("sessions.json"),
                );
            }
        }
        if let Ok(home) = env::var("HOME") {
            if !home.is_empty() {
                return Some(
                    PathBuf::from(home)
                        .join(".local")
                        .join("state")
                        .join("tema")
                        .join("sessions.json"),
                );
            }
        }
        None
    }

    /// A missing file is an empty store. Anything else unreadable is quarantined.
    pub(crate) fn load(path: PathBuf) -> Self {
        let empty = Self {
            path: path.clone(),
            file: StoreFile { version: 1, sessions: BTreeMap::new() },
            dirty: false,
        };
        if !path.exists() {
            return empty;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!("session store unreadable: {error}");
                quarantine(&path);
                return empty;
            }
        };
        match serde_json::from_slice::<StoreFile>(&bytes) {
            Ok(file) if file.version == 1 => Self { path, file, dirty: false },
            Ok(file) => {
                log::warn!("session store version {} is not supported", file.version);
                quarantine(&path);
                empty
            }
            Err(error) => {
                log::warn!("session store is invalid: {error}");
                quarantine(&path);
                empty
            }
        }
    }

    pub(crate) fn new_session_id() -> String {
        let bytes: [u8; 32] = rand::random();
        let mut id = String::with_capacity(64);
        for byte in bytes {
            use std::fmt::Write;
            let _ = write!(id, "{byte:02x}");
        }
        id
    }

    pub(crate) fn has_session(&self, id: &str) -> bool {
        self.file.sessions.contains_key(id)
    }

    pub(crate) fn touch_session(&mut self, id: &str, now: u64) {
        self.dirty = true;
        let session = self
            .file
            .sessions
            .entry(id.to_string())
            .or_insert(SessionRecord { created: now, last_used: now, toplevels: BTreeMap::new() });
        session.last_used = now;
    }

    pub(crate) fn remove_session(&mut self, id: &str) {
        self.dirty = true;
        self.file.sessions.remove(id);
    }

    pub(crate) fn toplevel(&self, id: &str, name: &str) -> Option<&ToplevelRecord> {
        self.file.sessions.get(id)?.toplevels.get(name)
    }

    pub(crate) fn put_toplevel(&mut self, id: &str, name: &str, record: ToplevelRecord) {
        self.dirty = true;
        let Some(session) = self.file.sessions.get_mut(id) else {
            return;
        };
        session.toplevels.insert(name.to_string(), record);
    }

    pub(crate) fn remove_toplevel(&mut self, id: &str, name: &str) {
        self.dirty = true;
        let Some(session) = self.file.sessions.get_mut(id) else {
            return;
        };
        session.toplevels.remove(name);
    }

    pub(crate) fn rename_toplevel(&mut self, id: &str, from: &str, to: &str) {
        self.dirty = true;
        let Some(session) = self.file.sessions.get_mut(id) else {
            return;
        };
        let Some(record) = session.toplevels.remove(from) else {
            return;
        };
        session.toplevels.insert(to.to_string(), record);
    }

    pub(crate) fn gc(&mut self, now: u64) {
        self.dirty = true;
        let cutoff = now.saturating_sub(MAX_SESSION_AGE_SECS);
        self.file
            .sessions
            .retain(|_, session| session.last_used >= cutoff);
        retain_most_recent(&mut self.file.sessions, MAX_SESSIONS, |session| session.last_used);
        for session in self.file.sessions.values_mut() {
            retain_most_recent(&mut session.toplevels, MAX_TOPLEVELS, |record| record.last_used);
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn save(&mut self, now: u64) -> io::Result<()> {
        self.gc(now);
        let dir = self
            .path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "session store path has no directory")
            })?;
        ensure_private_dir(dir)?;
        let tmp = dir.join("sessions.json.tmp");
        let bytes = serde_json::to_vec(&self.file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            use std::io::Write;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

fn retain_most_recent<T>(
    map: &mut BTreeMap<String, T>,
    keep: usize,
    last_used: impl Fn(&T) -> u64,
) {
    if map.len() <= keep {
        return;
    }
    let mut ranked: Vec<(u64, String)> = map
        .iter()
        .map(|(key, value)| (last_used(value), key.clone()))
        .collect();
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    for (_, key) in ranked.into_iter().skip(keep) {
        map.remove(&key);
    }
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn quarantine(path: &Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let dest = PathBuf::from(corrupt);
    if dest.exists() {
        if let Err(error) = fs::remove_file(&dest) {
            log::warn!("failed to replace quarantined session store: {error}");
        }
    }
    if let Err(error) = fs::rename(path, dest) {
        log::warn!("failed to quarantine session store: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let output = std::process::Command::new("mktemp")
                .arg("-d")
                .output()
                .unwrap();
            assert!(output.status.success(), "mktemp failed");
            Self(PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()))
        }

        fn path(&self) -> PathBuf {
            self.0.join("tema").join("sessions.json")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn floating() -> FloatingRecord {
        FloatingRecord {
            x: 40,
            y: 30,
            width: 900,
            height: 700,
            frame_x: 39,
            frame_y: 29,
            frame_width: 902,
            frame_height: 702,
        }
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn save_then_load_round_trips_and_creates_a_private_file() {
        let dir = TempDir::new();
        let path = dir.path();
        let mut store = SessionStore::load(path.clone());
        store.touch_session("session", 1_700_000_000);
        store.put_toplevel(
            "session",
            "main",
            ToplevelRecord {
                last_used: 1_700_000_100,
                output: Some("DP-1".to_string()),
                workspace: Some("2".to_string()),
                floating: Some(floating()),
                fullscreen: false,
            },
        );
        store.save(1_700_000_100).unwrap();
        assert!(!store.is_dirty());
        assert!(!path.with_file_name("sessions.json.tmp").exists());
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        let loaded = SessionStore::load(path);
        assert_eq!(loaded.file.version, 1);
        assert_eq!(loaded.toplevel("session", "main").unwrap().floating, Some(floating()));
        assert_eq!(loaded.file.sessions["session"].created, 1_700_000_000);
        assert!(!loaded.is_dirty());
    }

    #[test]
    fn missing_fields_default_and_a_bad_file_is_quarantined() {
        let dir = TempDir::new();
        let path = dir.0.join("sessions.json");
        fs::write(&path, br#"{"version":1,"sessions":{"ab":{"toplevels":{"main":{}}}}}"#).unwrap();
        let loaded = SessionStore::load(path.clone());
        let record = loaded.toplevel("ab", "main").unwrap();
        assert_eq!(record.output, None);
        assert_eq!(record.floating, None);
        assert!(!record.fullscreen);

        let missing = dir.0.join("absent.json");
        let empty = SessionStore::load(missing.clone());
        assert!(!empty.has_session("ab"));
        assert!(!missing.exists());
        assert!(!PathBuf::from(format!("{}.corrupt", missing.display())).exists());

        let invalid = dir.0.join("invalid.json");
        fs::write(&invalid, b"not json").unwrap();
        let empty = SessionStore::load(invalid.clone());
        assert!(!empty.has_session("ab"));
        assert!(!invalid.exists());
        assert!(PathBuf::from(format!("{}.corrupt", invalid.display())).exists());

        let future = dir.0.join("future.json");
        fs::write(&future, br#"{"version":2}"#).unwrap();
        let empty = SessionStore::load(future.clone());
        assert!(empty.file.sessions.is_empty());
        assert!(!future.exists());
        assert!(PathBuf::from(format!("{}.corrupt", future.display())).exists());
    }

    #[test]
    fn gc_drops_old_sessions_then_keeps_the_most_recent() {
        let path = TempDir::new().path();
        let mut store = SessionStore::load(path);
        let now = 20_000_000u64;
        let cutoff = now - MAX_SESSION_AGE_SECS;
        store.touch_session("ancient", cutoff - 1);
        store.touch_session("boundary", cutoff);
        store.touch_session("recent", now);
        store.gc(now);
        assert!(!store.has_session("ancient"));
        assert!(store.has_session("boundary"));
        assert!(store.has_session("recent"));

        let mut capped = SessionStore::load(TempDir::new().path());
        for index in 0..257 {
            capped.touch_session(&format!("s{index:03}"), index);
        }
        capped.gc(10_000);
        assert!(!capped.has_session("s000"));
        assert!(capped.has_session("s001"));
        assert!(capped.has_session("s256"));
        assert_eq!(capped.file.sessions.len(), MAX_SESSIONS);

        let mut tied = SessionStore::load(TempDir::new().path());
        for index in 0..255 {
            tied.touch_session(&format!("new{index:03}"), 50);
        }
        tied.touch_session("b", 1);
        tied.touch_session("a", 1);
        tied.gc(10_000);
        assert!(tied.has_session("a"));
        assert!(!tied.has_session("b"));
        assert_eq!(tied.file.sessions.len(), MAX_SESSIONS);

        store.touch_session("recent", now);
        for index in 0..129 {
            store.put_toplevel(
                "recent",
                &format!("t{index:03}"),
                ToplevelRecord { last_used: index, ..ToplevelRecord::default() },
            );
        }
        store.gc(now);
        assert!(store.toplevel("recent", "t000").is_none());
        assert!(store.toplevel("recent", "t001").is_some());
        assert!(store.toplevel("recent", "t128").is_some());
        assert_eq!(store.file.sessions["recent"].toplevels.len(), MAX_TOPLEVELS);
    }

    #[test]
    fn rename_moves_a_toplevel_and_overwrites_the_destination() {
        let mut store = SessionStore::load(TempDir::new().path());
        store.touch_session("s", 1);
        store.put_toplevel(
            "s",
            "from",
            ToplevelRecord { last_used: 5, ..ToplevelRecord::default() },
        );
        store.put_toplevel("s", "to", ToplevelRecord { last_used: 9, ..ToplevelRecord::default() });
        store.rename_toplevel("s", "from", "to");
        assert!(store.toplevel("s", "from").is_none());
        assert_eq!(store.toplevel("s", "to").unwrap().last_used, 5);

        store.rename_toplevel("s", "missing", "to");
        assert_eq!(store.toplevel("s", "to").unwrap().last_used, 5);
        store.remove_session("absent");
        assert!(store.is_dirty());
    }

    #[test]
    fn new_session_id_is_64_lowercase_hex_chars() {
        let first = SessionStore::new_session_id();
        let second = SessionStore::new_session_id();
        assert_eq!(first.len(), 64);
        assert!(first
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
        assert_ne!(first, second);
    }
}
