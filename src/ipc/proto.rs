//! Newline-delimited JSON shared by the compositor and `temactl`.
//!
//! One JSON object per line, UTF-8, `\n` terminated. `serde_json` escapes
//! newlines inside strings, so a line is always one message. Field order is
//! the order declared on each type.

use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Protocol version. Bump it only for a breaking change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Environment variable that overrides [`socket_path`].
pub const SOCKET_ENV: &str = "TEMA_SOCK";

/// Wayland display this socket belongs to. Must stay equal to
/// `clients::WAYLAND_DISPLAY_NAME`; `ipc` tests check that.
pub const DISPLAY_NAME: &str = "tema-0";

/// Longest accepted line, not counting the `\n` or a single trailing `\r`.
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    Version,
    Outputs,
    Workspaces,
    FocusedWindow,
    Subscribe {
        events: Vec<Topic>,
    },
    WindowIcon {
        id: u64,
        #[serde(default = "default_icon_size")]
        size: u32,
        #[serde(default = "one")]
        scale: u32,
    },
}

fn default_icon_size() -> u32 {
    32
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    FocusedWindow,
    Workspaces,
    Outputs,
    Bell,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Response),
    Error(String),
}

/// Externally tagged. The `Subscribed` unit variant is the JSON string
/// `"subscribed"`, so a reply is `{"ok":"subscribed"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Version(Version),
    Outputs(Vec<Output>),
    Workspaces(Vec<Workspace>),
    FocusedWindow(Option<Window>),
    WindowIcon(WindowIcon),
    Subscribed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    FocusedWindow { window: Option<Window> },
    Workspaces { workspaces: Vec<Workspace> },
    Outputs { outputs: Vec<Output> },
    Bell { window: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub compositor: String,
    pub protocol: u32,
}

/// `id` is `WindowId` as `u64` and stays stable while the window lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub workspace: Option<String>,
    pub output: Option<String>,
    pub floating: bool,
    pub fullscreen: bool,
    /// Absent on older clients. An empty protocol string is stored as `None`.
    #[serde(default)]
    pub tag: Option<String>,
    /// Absent on older clients. An empty protocol string is stored as `None`.
    #[serde(default)]
    pub description: Option<String>,
    /// Absent on older clients.
    #[serde(default)]
    pub icon: Option<WindowIconInfo>,
    /// Modal xdg dialog (`xdg_dialog_v1.set_modal` with a live parent). Absent on older clients.
    #[serde(default)]
    pub modal: bool,
    /// Absent on older clients. `None` unless the window's client is sandboxed.
    #[serde(default)]
    pub sandbox: Option<WindowSandbox>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSandbox {
    pub engine: Option<String>,
    pub app_id: Option<String>,
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowIconInfo {
    pub name: Option<String>,
    pub sizes: Vec<IconSize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconSize {
    pub size: u32,
    pub scale: u32,
}

/// One icon image. `data` is standard base64 of little-endian premultiplied BGRA.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowIcon {
    pub id: u64,
    pub name: Option<String>,
    pub size: Option<u32>,
    pub scale: Option<u32>,
    pub format: String,
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    /// 1-based position in the workspace's group.
    pub index: u32,
    pub coordinates: Vec<u32>,
    pub outputs: Vec<String>,
    pub active: bool,
    pub focused: bool,
    pub windows: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub name: String,
    pub make: String,
    pub model: String,
    pub description: String,
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Millihertz, the same unit as the DRM mode (`239960` is 239.960 Hz).
    pub refresh_mhz: i32,
    pub scale: f64,
    pub active_workspace: Option<String>,
}

const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let value = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        push_base64(&mut out, value, 4);
    }
    match chunks.remainder() {
        [b0] => push_base64(&mut out, u32::from(*b0) << 16, 2),
        [b0, b1] => push_base64(&mut out, (u32::from(*b0) << 16) | (u32::from(*b1) << 8), 3),
        _ => {}
    }
    out
}

fn push_base64(out: &mut String, value: u32, chars: usize) {
    for shift in [18, 12, 6, 0].into_iter().take(chars) {
        out.push(BASE64[((value >> shift) & 0x3f) as usize] as char);
    }
    for _ in chars..4 {
        out.push('=');
    }
}

/// Standard base64 with `=` padding. Rejects a missing or misplaced pad.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, ()> {
    if text.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let bytes = text.as_bytes();
    for chunk in bytes.chunks_exact(4) {
        let (a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        if a == b'=' || b == b'=' || (c == b'=' && d != b'=') {
            return Err(());
        }
        let pad = match (c, d) {
            (b'=', b'=') => 2,
            (_, b'=') => 1,
            _ => 0,
        };
        let value = (base64_value(a)? << 18)
            | (base64_value(b)? << 12)
            | (base64_value(if pad == 2 { b'A' } else { c })? << 6)
            | base64_value(if pad >= 1 { b'A' } else { d })?;
        out.push((value >> 16) as u8);
        if pad < 2 {
            out.push((value >> 8) as u8);
        }
        if pad < 1 {
            out.push(value as u8);
        }
    }
    Ok(out)
}

fn base64_value(byte: u8) -> Result<u32, ()> {
    match byte {
        b'A'..=b'Z' => Ok(u32::from(byte - b'A')),
        b'a'..=b'z' => Ok(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Ok(u32::from(byte - b'0') + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(()),
    }
}

pub fn encode_line<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("ipc value serializes");
    bytes.push(b'\n');
    bytes
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    TooLong,
    InvalidUtf8,
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => write!(f, "ipc line longer than {MAX_LINE} bytes"),
            Self::InvalidUtf8 => write!(f, "ipc line is not utf-8"),
        }
    }
}

impl std::error::Error for LineError {}

/// Bytes pushed from a stream socket, split into lines.
///
/// A `\r` immediately before `\n` is dropped. Unlike the Xwayland stderr
/// splitter, an overlong line and invalid UTF-8 are errors: the caller closes
/// the connection instead of guessing at a boundary.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    pub fn next_line(&mut self) -> Result<Option<String>, LineError> {
        let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') else {
            if pending_is_too_long(&self.pending) {
                return Err(LineError::TooLong);
            }
            return Ok(None);
        };

        let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.len() > MAX_LINE {
            return Err(LineError::TooLong);
        }
        String::from_utf8(line)
            .map(Some)
            .map_err(|_| LineError::InvalidUtf8)
    }
}

/// A finished line may be [`MAX_LINE`] bytes plus one `\r` that CRLF will
/// strip. Anything past that cannot become a legal line.
fn pending_is_too_long(pending: &[u8]) -> bool {
    if pending.len() > MAX_LINE + 1 {
        return true;
    }
    pending.len() > MAX_LINE && pending.last() != Some(&b'\r')
}

/// `$TEMA_SOCK` when it is set and non-empty, otherwise [`default_socket_path`].
pub fn socket_path() -> PathBuf {
    match env::var(SOCKET_ENV) {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => default_socket_path(),
    }
}

/// Socket the compositor binds. Ignores `TEMA_SOCK`: that variable is how a
/// client finds a compositor, not how the compositor chooses its own path.
pub fn default_socket_path() -> PathBuf {
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(format!("{DISPLAY_NAME}.ipc.sock"))
}

/// Serializes tests that mutate `TEMA_SOCK` or `XDG_RUNTIME_DIR`.
#[cfg(test)]
pub(crate) fn lock_socket_env() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_json<T>(value: &T, json: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_string(value).unwrap(), json);
        assert_eq!(&serde_json::from_str::<T>(json).unwrap(), value);
        let mut line = json.as_bytes().to_vec();
        line.push(b'\n');
        assert_eq!(encode_line(value), line);
    }

    fn window() -> Window {
        Window {
            id: 12,
            title: "nvim — ~/src".to_string(),
            app_id: "Alacritty".to_string(),
            workspace: Some("ws-1-2".to_string()),
            output: Some("DP-1".to_string()),
            floating: false,
            fullscreen: false,
            tag: None,
            description: None,
            icon: None,
            modal: false,
            sandbox: None,
        }
    }

    fn workspace() -> Workspace {
        Workspace {
            id: "ws-1-2".to_string(),
            name: "2".to_string(),
            index: 2,
            coordinates: vec![1],
            outputs: vec!["DP-1".to_string()],
            active: true,
            focused: true,
            windows: 3,
        }
    }

    fn output() -> Output {
        Output {
            name: "DP-1".to_string(),
            make: "Dell".to_string(),
            model: "U2723QE".to_string(),
            description: "Dell Inc. U2723QE".to_string(),
            enabled: true,
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            refresh_mhz: 239_960,
            scale: 1.25,
            active_workspace: Some("ws-1-2".to_string()),
        }
    }

    #[test]
    fn requests_match_the_reference_json() {
        assert_json(&Request::Version, r#"{"request":"version"}"#);
        assert_json(&Request::Outputs, r#"{"request":"outputs"}"#);
        assert_json(&Request::Workspaces, r#"{"request":"workspaces"}"#);
        assert_json(&Request::FocusedWindow, r#"{"request":"focused_window"}"#);
        assert_json(
            &Request::Subscribe {
                events: vec![Topic::FocusedWindow, Topic::Workspaces, Topic::Outputs],
            },
            r#"{"request":"subscribe","events":["focused_window","workspaces","outputs"]}"#,
        );
        assert_json(
            &Request::Subscribe { events: vec![] },
            r#"{"request":"subscribe","events":[]}"#,
        );
        assert_json(
            &Request::WindowIcon { id: 12, size: 32, scale: 1 },
            r#"{"request":"window_icon","id":12,"size":32,"scale":1}"#,
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"request":"window_icon","id":12}"#).unwrap(),
            Request::WindowIcon { id: 12, size: 32, scale: 1 },
        );
    }

    #[test]
    fn replies_and_events_round_trip_in_field_order() {
        let version = Version { compositor: "0.1.0".to_string(), protocol: PROTOCOL_VERSION };
        assert_json(
            &Reply::Ok(Response::Version(version)),
            r#"{"ok":{"version":{"compositor":"0.1.0","protocol":1}}}"#,
        );
        assert_json(
            &Reply::Ok(Response::Outputs(vec![output()])),
            r#"{"ok":{"outputs":[{"name":"DP-1","make":"Dell","model":"U2723QE","description":"Dell Inc. U2723QE","enabled":true,"x":0,"y":0,"width":2560,"height":1440,"refresh_mhz":239960,"scale":1.25,"active_workspace":"ws-1-2"}]}}"#,
        );
        assert_json(
            &Reply::Ok(Response::Workspaces(vec![workspace()])),
            r#"{"ok":{"workspaces":[{"id":"ws-1-2","name":"2","index":2,"coordinates":[1],"outputs":["DP-1"],"active":true,"focused":true,"windows":3}]}}"#,
        );
        assert_json(
            &Reply::Ok(Response::FocusedWindow(Some(window()))),
            r#"{"ok":{"focused_window":{"id":12,"title":"nvim — ~/src","app_id":"Alacritty","workspace":"ws-1-2","output":"DP-1","floating":false,"fullscreen":false,"tag":null,"description":null,"icon":null,"modal":false,"sandbox":null}}}"#,
        );
        assert_json(&Reply::Ok(Response::FocusedWindow(None)), r#"{"ok":{"focused_window":null}}"#);
        assert_json(&Reply::Ok(Response::Subscribed), r#"{"ok":"subscribed"}"#);
        assert_json(
            &Reply::Ok(Response::WindowIcon(WindowIcon {
                id: 12,
                name: Some("firefox".to_string()),
                size: Some(32),
                scale: Some(1),
                format: "argb8888_premultiplied".to_string(),
                data: Some(base64_encode(&[1, 2, 3, 4])),
            })),
            r#"{"ok":{"window_icon":{"id":12,"name":"firefox","size":32,"scale":1,"format":"argb8888_premultiplied","data":"AQIDBA=="}}}"#,
        );
        assert_json(
            &Reply::Error("unknown request `foo`".to_string()),
            r#"{"error":"unknown request `foo`"}"#,
        );

        assert_json(
            &Event::FocusedWindow { window: Some(window()) },
            r#"{"event":"focused_window","window":{"id":12,"title":"nvim — ~/src","app_id":"Alacritty","workspace":"ws-1-2","output":"DP-1","floating":false,"fullscreen":false,"tag":null,"description":null,"icon":null,"modal":false,"sandbox":null}}"#,
        );
        assert_json(
            &Event::FocusedWindow { window: None },
            r#"{"event":"focused_window","window":null}"#,
        );
        assert_json(
            &Event::Workspaces { workspaces: vec![workspace()] },
            r#"{"event":"workspaces","workspaces":[{"id":"ws-1-2","name":"2","index":2,"coordinates":[1],"outputs":["DP-1"],"active":true,"focused":true,"windows":3}]}"#,
        );
        assert_json(
            &Event::Outputs { outputs: vec![output()] },
            r#"{"event":"outputs","outputs":[{"name":"DP-1","make":"Dell","model":"U2723QE","description":"Dell Inc. U2723QE","enabled":true,"x":0,"y":0,"width":2560,"height":1440,"refresh_mhz":239960,"scale":1.25,"active_workspace":"ws-1-2"}]}"#,
        );
        assert_json(
            &Event::Workspaces { workspaces: vec![] },
            r#"{"event":"workspaces","workspaces":[]}"#,
        );
        assert_json(&Event::Outputs { outputs: vec![] }, r#"{"event":"outputs","outputs":[]}"#);
        assert_json(&Event::Bell { window: Some(12) }, r#"{"event":"bell","window":12}"#);
        assert_json(&Event::Bell { window: None }, r#"{"event":"bell","window":null}"#);
        assert_eq!(serde_json::from_str::<Topic>(r#""bell""#).unwrap(), Topic::Bell);

        let mut nulls = window();
        nulls.workspace = None;
        nulls.output = None;
        assert_json(
            &nulls,
            r#"{"id":12,"title":"nvim — ~/src","app_id":"Alacritty","workspace":null,"output":null,"floating":false,"fullscreen":false,"tag":null,"description":null,"icon":null,"modal":false,"sandbox":null}"#,
        );
        let older = r#"{"id":12,"title":"nvim — ~/src","app_id":"Alacritty","workspace":null,"output":null,"floating":false,"fullscreen":false}"#;
        assert_eq!(serde_json::from_str::<Window>(older).unwrap().sandbox, None);
        let mut disabled = output();
        disabled.active_workspace = None;
        assert_json(
            &Reply::Ok(Response::Outputs(vec![disabled])),
            r#"{"ok":{"outputs":[{"name":"DP-1","make":"Dell","model":"U2723QE","description":"Dell Inc. U2723QE","enabled":true,"x":0,"y":0,"width":2560,"height":1440,"refresh_mhz":239960,"scale":1.25,"active_workspace":null}]}}"#,
        );
    }

    #[test]
    fn base64_round_trips_short_inputs_and_a_known_vector() {
        for length in 0..=5 {
            let bytes: Vec<u8> = (0..length).map(|index| index * 17).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)), Ok(bytes));
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9vYmFy"), Ok(b"foobar".to_vec()));
        assert!(base64_decode("Zm9vYmF").is_err());
        assert!(base64_decode("Zm9vYm=A").is_err());
    }

    #[test]
    fn a_title_newline_stays_inside_one_line() {
        let mut titled = window();
        titled.title = "a\nb".to_string();
        let line = encode_line(&titled);
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(line.ends_with(b"\n"));
    }

    #[test]
    fn unknown_fields_are_ignored_and_an_unknown_request_is_an_error() {
        let request: Request =
            serde_json::from_str(r#"{"request":"version","future":true}"#).unwrap();
        assert_eq!(request, Request::Version);

        let event: Event = serde_json::from_str(
            r#"{"event":"focused_window","window":null,"keyboard_layout":"us"}"#,
        )
        .unwrap();
        assert_eq!(event, Event::FocusedWindow { window: None });

        let window: Window = serde_json::from_str(
            r#"{"id":1,"title":"","app_id":"","workspace":null,"output":null,"floating":false,"fullscreen":false,"urgent":true}"#,
        )
        .unwrap();
        assert_eq!(window.id, 1);
        assert_eq!(window.tag, None);
        assert_eq!(window.description, None);
        assert_eq!(window.icon, None);

        let error = serde_json::from_str::<Request>(r#"{"request":"foo"}"#).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("foo"), "{message}");
        assert!(serde_json::from_str::<Event>(r#"{"event":"keyboard_layout"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"request":"subscribe"}"#).is_err());
    }

    #[test]
    fn lines_split_across_pushes_and_several_arrive_together() {
        let mut buffer = LineBuffer::default();
        buffer.push(b"hel");
        assert_eq!(buffer.next_line().unwrap(), None);
        buffer.push(b"lo\nwor");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("hello"));
        assert_eq!(buffer.next_line().unwrap(), None);
        buffer.push(b"ld\n");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("world"));
        assert_eq!(buffer.next_line().unwrap(), None);

        buffer.push(b"a\nb\nc\n");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("a"));
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("b"));
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("c"));
        assert_eq!(buffer.next_line().unwrap(), None);
    }

    #[test]
    fn crlf_drops_one_trailing_cr() {
        let mut buffer = LineBuffer::default();
        buffer.push(b"a\r\nb\r\n");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("a"));
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("b"));

        buffer.push(b"a\r");
        assert_eq!(buffer.next_line().unwrap(), None);
        buffer.push(b"\n");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("a"));

        buffer.push(b"a\r\r\n");
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("a\r"));
    }

    #[test]
    fn a_line_at_the_limit_is_kept_and_past_it_is_too_long() {
        let mut exact = vec![b'x'; MAX_LINE];
        exact.push(b'\n');
        let mut buffer = LineBuffer::default();
        buffer.push(&exact);
        assert_eq!(buffer.next_line().unwrap().unwrap().len(), MAX_LINE);

        let mut exact_crlf = vec![b'y'; MAX_LINE];
        exact_crlf.push(b'\r');
        let mut buffer = LineBuffer::default();
        buffer.push(&exact_crlf);
        assert_eq!(buffer.next_line().unwrap(), None);
        buffer.push(b"\n");
        assert_eq!(buffer.next_line().unwrap().unwrap().len(), MAX_LINE);

        let mut over = vec![b'z'; MAX_LINE + 1];
        over.push(b'\n');
        let mut buffer = LineBuffer::default();
        buffer.push(&over);
        assert_eq!(buffer.next_line().unwrap_err(), LineError::TooLong);

        let mut buffer = LineBuffer::default();
        buffer.push(&vec![b'z'; MAX_LINE + 1]);
        assert_eq!(buffer.next_line().unwrap_err(), LineError::TooLong);
    }

    #[test]
    fn invalid_utf8_is_an_error_and_the_next_line_remains() {
        let mut buffer = LineBuffer::default();
        buffer.push(b"\xff\nok\n");
        assert_eq!(buffer.next_line().unwrap_err(), LineError::InvalidUtf8);
        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("ok"));
    }

    struct EnvRestore {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = env::var_os(key);
            unsafe {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn socket_path_prefers_tema_sock_and_otherwise_uses_the_runtime_dir() {
        let _lock = super::lock_socket_env();

        let _tema = EnvRestore::set(SOCKET_ENV, Some("/tmp/from-tema.sock"));
        let _runtime = EnvRestore::set("XDG_RUNTIME_DIR", Some("/run/user/7"));
        assert_eq!(socket_path(), PathBuf::from("/tmp/from-tema.sock"));
        drop(_tema);

        let _tema = EnvRestore::set(SOCKET_ENV, Some(""));
        assert_eq!(socket_path(), PathBuf::from("/run/user/7/tema-0.ipc.sock"));
        drop(_runtime);

        let _runtime = EnvRestore::set("XDG_RUNTIME_DIR", None);
        assert_eq!(socket_path(), PathBuf::from("/tmp/tema-0.ipc.sock"));
        assert_eq!(default_socket_path(), PathBuf::from("/tmp/tema-0.ipc.sock"));
    }
}
