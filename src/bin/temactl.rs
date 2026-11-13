//! Query a running tema. This file is its own crate: it includes
//! [`proto`] by path and does not build the compositor.

#[allow(dead_code)]
#[path = "../ipc/proto.rs"]
mod proto;

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::Duration,
};

use serde::Serialize;

const USAGE: &str = "\
Usage: temactl <command>

Commands:
  version
  outputs [-j]
  workspaces [-j]
  focused-window [-j]
  window-icon ID [--size N] [--scale S] [-o FILE] [-j]
  subscribe [TOPIC...]
  waybar window [--format FMT] [--tooltip-format FMT] [--empty TEXT]
                [--no-escape] [--once]
  -h, --help

TOPIC is focused_window, workspaces, outputs, or bell. With no topic, subscribe
to all of them.

-j prints the ok value as one line of compact JSON.
waybar window prints one Waybar custom JSON object per change.
";

const EX_REPLY: u8 = 1;
const EX_CONNECT: u8 = 2;
const EX_USAGE: u8 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Outputs { json: bool },
    Workspaces { json: bool },
    FocusedWindow { json: bool },
    WindowIcon(WindowIconCommand),
    Subscribe { topics: Vec<proto::Topic> },
    WaybarWindow(WaybarWindow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WaybarWindow {
    format: String,
    tooltip_format: String,
    empty: String,
    escape: bool,
    once: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowIconCommand {
    id: u64,
    size: u32,
    scale: u32,
    output: Option<String>,
    json: bool,
}

impl Default for WaybarWindow {
    fn default() -> Self {
        Self {
            format: "{title}".to_string(),
            tooltip_format: "{app_id}: {title}".to_string(),
            empty: String::new(),
            escape: true,
            once: false,
        }
    }
}

#[derive(Debug)]
enum Failure {
    Reply(String),
    Connect {
        path: PathBuf,
        error: io::Error,
    },
    Io(String),
    /// The IPC socket closed before `waybar --once` printed a line.
    Closed,
    BrokenPipe,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(args.iter().map(String::as_str)) {
        Err(()) => {
            eprint!("{USAGE}");
            ExitCode::from(EX_USAGE)
        }
        Ok(command) => match execute(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(Failure::BrokenPipe) => ExitCode::SUCCESS,
            Err(failure) => {
                report(&failure);
                ExitCode::from(failure.code())
            }
        },
    }
}

fn parse_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Command, ()> {
    let args: Vec<&str> = args.into_iter().collect();
    match args.as_slice() {
        ["-h" | "--help"] => Ok(Command::Help),
        ["version"] => Ok(Command::Version),
        ["outputs"] => Ok(Command::Outputs { json: false }),
        ["outputs", "-j"] => Ok(Command::Outputs { json: true }),
        ["workspaces"] => Ok(Command::Workspaces { json: false }),
        ["workspaces", "-j"] => Ok(Command::Workspaces { json: true }),
        ["focused-window"] => Ok(Command::FocusedWindow { json: false }),
        ["focused-window", "-j"] => Ok(Command::FocusedWindow { json: true }),
        ["window-icon", id, flags @ ..] => {
            let id = id.parse().map_err(|_| ())?;
            Ok(Command::WindowIcon(parse_window_icon_flags(id, flags)?))
        }
        ["subscribe", topics @ ..] => {
            let mut parsed = Vec::with_capacity(topics.len());
            for topic in topics {
                parsed.push(parse_topic(topic)?);
            }
            Ok(Command::Subscribe { topics: parsed })
        }
        ["waybar", "window", flags @ ..] => Ok(Command::WaybarWindow(parse_waybar_flags(flags)?)),
        _ => Err(()),
    }
}

fn parse_waybar_flags(flags: &[&str]) -> Result<WaybarWindow, ()> {
    let mut opts = WaybarWindow::default();
    let mut flags = flags.iter().copied();
    while let Some(flag) = flags.next() {
        match flag {
            "--format" => opts.format = flags.next().ok_or(())?.to_string(),
            "--tooltip-format" => opts.tooltip_format = flags.next().ok_or(())?.to_string(),
            "--empty" => opts.empty = flags.next().ok_or(())?.to_string(),
            "--no-escape" => opts.escape = false,
            "--once" => opts.once = true,
            _ => return Err(()),
        }
    }
    Ok(opts)
}

fn parse_window_icon_flags(id: u64, flags: &[&str]) -> Result<WindowIconCommand, ()> {
    let mut command = WindowIconCommand { id, size: 32, scale: 1, output: None, json: false };
    let mut flags = flags.iter().copied();
    while let Some(flag) = flags.next() {
        match flag {
            "--size" => command.size = flags.next().ok_or(())?.parse().map_err(|_| ())?,
            "--scale" => command.scale = flags.next().ok_or(())?.parse().map_err(|_| ())?,
            "-o" => command.output = Some(flags.next().ok_or(())?.to_string()),
            "-j" => command.json = true,
            _ => return Err(()),
        }
    }
    Ok(command)
}

fn parse_topic(name: &str) -> Result<proto::Topic, ()> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).map_err(|_| ())
}

fn execute(command: Command) -> Result<(), Failure> {
    match command {
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            let version = expect_version(request(&proto::Request::Version)?)?;
            print!(
                "{}",
                format_version(env!("CARGO_PKG_VERSION"), proto::PROTOCOL_VERSION, &version)
            );
            Ok(())
        }
        Command::Outputs { json } => {
            show(request(&proto::Request::Outputs)?, json, |response| match response {
                proto::Response::Outputs(outputs) => Some(format_outputs(outputs)),
                _ => None,
            })
        }
        Command::Workspaces { json } => {
            show(request(&proto::Request::Workspaces)?, json, |response| match response {
                proto::Response::Workspaces(workspaces) => Some(format_workspaces(workspaces)),
                _ => None,
            })
        }
        Command::FocusedWindow { json } => {
            show(request(&proto::Request::FocusedWindow)?, json, |response| match response {
                proto::Response::FocusedWindow(window) => {
                    Some(format_focused_window(window.as_ref()))
                }
                _ => None,
            })
        }
        Command::WindowIcon(command) => show_window_icon(command),
        Command::Subscribe { topics } => subscribe_at(&proto::socket_path(), &topics, emit_stdout),
        Command::WaybarWindow(opts) => run_waybar_window(&opts),
    }
}

fn show(
    reply: proto::Reply,
    json: bool,
    text: impl FnOnce(&proto::Response) -> Option<String>,
) -> Result<(), Failure> {
    let response = ok_response(reply)?;
    if json {
        print!("{}", json_line(&response));
        return Ok(());
    }
    let Some(text) = text(&response) else {
        return Err(Failure::Reply("unexpected reply".to_string()));
    };
    print!("{text}");
    Ok(())
}

fn show_window_icon(command: WindowIconCommand) -> Result<(), Failure> {
    let reply = request(&proto::Request::WindowIcon {
        id: command.id,
        size: command.size,
        scale: command.scale,
    })?;
    let proto::Response::WindowIcon(icon) = ok_response(reply)? else {
        return Err(Failure::Reply("unexpected reply".to_string()));
    };
    if let Some(path) = &command.output {
        let png = icon_png(&icon).map_err(|message| Failure::Reply(message))?;
        std::fs::write(path, png).map_err(io_failure)?;
    }
    if command.json {
        print!("{}", json_line(&proto::Response::WindowIcon(icon)));
    } else if command.output.is_none() {
        print!("{}", format_window_icon(&icon));
    }
    Ok(())
}

fn expect_version(reply: proto::Reply) -> Result<proto::Version, Failure> {
    match ok_response(reply)? {
        proto::Response::Version(version) => Ok(version),
        _ => Err(Failure::Reply("unexpected reply".to_string())),
    }
}

fn ok_response(reply: proto::Reply) -> Result<proto::Response, Failure> {
    match reply {
        proto::Reply::Ok(response) => Ok(response),
        proto::Reply::Error(message) => Err(Failure::Reply(message)),
    }
}

fn format_version(client: &str, client_protocol: u32, version: &proto::Version) -> String {
    let protocol = if version.protocol == client_protocol {
        format!("protocol {}", version.protocol)
    } else {
        format!("protocol {} (this client speaks {client_protocol})", version.protocol)
    };
    format!("client {client}\ncompositor {}\n{protocol}\n", version.compositor)
}

fn format_outputs(outputs: &[proto::Output]) -> String {
    if outputs.is_empty() {
        return "No outputs\n".to_string();
    }
    let mut text = String::new();
    for (index, output) in outputs.iter().enumerate() {
        push_block(&mut text, index, &format_output(output));
    }
    text
}

fn format_output(output: &proto::Output) -> String {
    let mut text = format!(
        "Output {}{}{}\n",
        output.name,
        quoted_identity(&output.make, &output.model),
        if output.enabled { "" } else { " (disabled)" }
    );
    if !output.description.is_empty() {
        text.push_str(&format!("  Description: {}\n", output.description));
    }
    let hertz = f64::from(output.refresh_mhz) / 1000.0;
    text.push_str(&format!(
        "  Position: {},{}\n  Mode: {}x{} @ {hertz:.3} Hz\n  Scale: {}\n  Workspace: {}\n",
        output.x,
        output.y,
        output.width,
        output.height,
        output.scale,
        output.active_workspace.as_deref().unwrap_or("none")
    ));
    text
}

fn format_workspaces(workspaces: &[proto::Workspace]) -> String {
    if workspaces.is_empty() {
        return "No workspaces\n".to_string();
    }
    let mut text = String::new();
    for (index, workspace) in workspaces.iter().enumerate() {
        push_block(&mut text, index, &format_workspace(workspace));
    }
    text
}

fn format_workspace(workspace: &proto::Workspace) -> String {
    format!(
        "Workspace {}{}\n  Id: {}\n  Index: {}\n  Coordinates: {}\n  Outputs: {}\n  Windows: {}\n",
        workspace.name,
        workspace_suffix(workspace),
        workspace.id,
        workspace.index,
        joined_or_none(&workspace.coordinates),
        joined_or_none(&workspace.outputs),
        workspace.windows
    )
}

fn format_focused_window(window: Option<&proto::Window>) -> String {
    let Some(window) = window else {
        return "No focused window\n".to_string();
    };
    let mut text = format!(
        "Window {}: {}\n  App id: {}\n  Workspace: {}\n  Output: {}\n  Floating: {}\n  Fullscreen: {}\n",
        window.id,
        window.title,
        window.app_id,
        window.workspace.as_deref().unwrap_or("none"),
        window.output.as_deref().unwrap_or("none"),
        yes_no(window.floating),
        yes_no(window.fullscreen)
    );
    if window.modal {
        text.push_str("  Modal: yes\n");
    }
    if let Some(tag) = &window.tag {
        text.push_str(&format!("  Tag: {tag}\n"));
    }
    if let Some(description) = &window.description {
        text.push_str(&format!("  Description: {description}\n"));
    }
    if let Some(sandbox) = &window.sandbox {
        let field = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_string());
        text.push_str(&format!(
            "  Sandbox: {} app={} instance={}\n",
            field(&sandbox.engine),
            field(&sandbox.app_id),
            field(&sandbox.instance_id)
        ));
    }
    text
}

fn format_window_icon(icon: &proto::WindowIcon) -> String {
    let name = icon.name.as_deref().unwrap_or("none");
    if icon.data.is_none() {
        format!("name: {name}\nno pixels\n")
    } else {
        format!(
            "name: {name}\nsize: {}\nscale: {}\n",
            icon.size.unwrap_or(0),
            icon.scale.unwrap_or(0)
        )
    }
}

fn icon_png(icon: &proto::WindowIcon) -> Result<Vec<u8>, String> {
    let Some(size) = icon.size else {
        return Err("window has no icon pixels".to_string());
    };
    let Some(data) = icon.data.as_deref() else {
        return Err("window has no icon pixels".to_string());
    };
    let pixels = proto::base64_decode(data).map_err(|()| "malformed icon data".to_string())?;
    let expected = (size as usize)
        .saturating_mul(size as usize)
        .saturating_mul(4);
    if size == 0 || pixels.len() != expected {
        return Err("malformed icon data".to_string());
    }
    Ok(png_rgba(size, size, &unpremultiply_bgra(&pixels)))
}

fn unpremultiply_bgra(pixels: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixels.len());
    for px in pixels.chunks_exact(4) {
        let (blue, green, red, alpha) = (px[0], px[1], px[2], px[3]);
        let channel = |component: u8| {
            if alpha == 0 {
                0
            } else {
                ((u32::from(component) * 255 / u32::from(alpha)).min(255)) as u8
            }
        };
        rgba.extend([channel(red), channel(green), channel(blue), alpha]);
    }
    rgba
}

fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row_bytes = (width as usize).saturating_mul(4);
    let mut raw = Vec::with_capacity((row_bytes + 1).saturating_mul(height as usize));
    for y in 0..height as usize {
        raw.push(0);
        let start = y.saturating_mul(row_bytes);
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png.extend(png_chunk(b"IHDR", &ihdr));
    png.extend(png_chunk(b"IDAT", &zlib_store(&raw)));
    png.extend(png_chunk(b"IEND", &[]));
    png
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(12 + data.len());
    chunk.extend(u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    chunk.extend(kind);
    chunk.extend(data);
    let mut crc_bytes = kind.to_vec();
    crc_bytes.extend(data);
    chunk.extend(crc32(&crc_bytes).to_be_bytes());
    chunk
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut zlib = Vec::with_capacity(data.len() + 16);
    zlib.extend([0x78, 0x01]);
    zlib.extend(deflate_stored(data));
    zlib.extend(adler32(data).to_be_bytes());
    zlib
}

fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    let mut offset = 0;
    loop {
        let end = (offset + 65_535).min(data.len());
        let chunk = &data[offset..end];
        let last = end == data.len();
        out.push(if last { 1 } else { 0 });
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend(chunk);
        if last {
            break;
        }
        offset = end;
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn json_line(response: &proto::Response) -> String {
    format!("{}\n", serde_json::to_string(response).expect("ipc response serializes"))
}

fn push_block(text: &mut String, index: usize, block: &str) {
    if index > 0 {
        text.push('\n');
    }
    text.push_str(block);
}

fn quoted_identity(make: &str, model: &str) -> String {
    let label = [make, model]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        String::new()
    } else {
        format!(" '{label}'")
    }
}

fn workspace_suffix(workspace: &proto::Workspace) -> &'static str {
    match (workspace.active, workspace.focused) {
        (true, true) => " (active, focused)",
        (true, false) => " (active)",
        (false, true) => " (focused)",
        (false, false) => "",
    }
}

fn joined_or_none<T: std::fmt::Display>(values: &[T]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[derive(Serialize)]
struct WaybarStatus {
    text: String,
    tooltip: String,
    alt: String,
    class: Vec<String>,
}

enum StreamEnd {
    Done,
    Closed,
}

fn run_waybar_window(opts: &WaybarWindow) -> Result<(), Failure> {
    run_waybar_window_at(&proto::socket_path(), opts, emit_stdout)
}

/// Follows `focused_window`. A dropped socket prints one empty line and tries
/// again after a second, so a bar started before tema keeps working.
/// `--once` prints the current line and returns; a failed connect or a close
/// before that line is [`Failure::Connect`] or [`Failure::Closed`].
fn run_waybar_window_at(
    path: &Path,
    opts: &WaybarWindow,
    mut emit: impl FnMut(&str) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut previous = None;
    loop {
        match stream_waybar(path, opts, &mut previous, &mut emit) {
            Ok(StreamEnd::Done) => return Ok(()),
            Ok(StreamEnd::Closed) => {
                if opts.once {
                    return Err(Failure::Closed);
                }
            }
            Err(Failure::BrokenPipe) => return Err(Failure::BrokenPipe),
            Err(Failure::Connect { path, error }) => {
                if opts.once {
                    return Err(Failure::Connect { path, error });
                }
            }
            Err(Failure::Reply(message)) => {
                if opts.once {
                    return Err(Failure::Reply(message));
                }
            }
            Err(Failure::Io(_) | Failure::Closed) => {
                if opts.once {
                    return Err(Failure::Closed);
                }
            }
        }
        let empty = render_waybar(opts, None);
        emit_if_changed(&empty, &mut previous, &mut emit)?;
        thread::sleep(Duration::from_secs(1));
    }
}

fn stream_waybar(
    path: &Path,
    opts: &WaybarWindow,
    previous: &mut Option<String>,
    emit: &mut impl FnMut(&str) -> Result<(), Failure>,
) -> Result<StreamEnd, Failure> {
    let mut stream = connect(path)?;
    stream
        .write_all(&proto::encode_line(&proto::Request::Subscribe {
            events: vec![proto::Topic::FocusedWindow],
        }))
        .map_err(|error| Failure::Io(error.to_string()))?;
    let mut lines = BufReader::new(stream).lines();
    let ack = match lines.next() {
        Some(Ok(line)) => line,
        Some(Err(error)) => return Err(Failure::Io(error.to_string())),
        None => return Ok(StreamEnd::Closed),
    };
    match serde_json::from_str::<proto::Reply>(&ack) {
        Ok(proto::Reply::Ok(proto::Response::Subscribed)) => {}
        Ok(proto::Reply::Error(message)) => return Err(Failure::Reply(message)),
        Ok(proto::Reply::Ok(_)) => return Err(Failure::Reply("unexpected reply".to_string())),
        Err(_) => return Err(Failure::Reply("malformed reply".to_string())),
    }
    for line in lines {
        let line = line.map_err(|error| Failure::Io(error.to_string()))?;
        let Some(window) = focused_window_event(&line)? else {
            continue;
        };
        let rendered = render_waybar(opts, window.as_ref());
        emit_if_changed(&rendered, previous, emit)?;
        if opts.once {
            return Ok(StreamEnd::Done);
        }
    }
    Ok(StreamEnd::Closed)
}

fn focused_window_event(line: &str) -> Result<Option<Option<proto::Window>>, Failure> {
    match serde_json::from_str::<proto::Event>(line) {
        Ok(proto::Event::FocusedWindow { window }) => Ok(Some(window)),
        Ok(_) => Ok(None),
        Err(_) => {
            accept_event_line(line)?;
            Ok(None)
        }
    }
}

fn render_waybar(opts: &WaybarWindow, window: Option<&proto::Window>) -> String {
    let status = match window {
        None => WaybarStatus {
            text: maybe_escape(&opts.empty, opts.escape),
            tooltip: String::new(),
            alt: String::new(),
            class: vec!["empty".to_string()],
        },
        Some(window) => WaybarStatus {
            text: substitute(&opts.format, window, opts.escape),
            tooltip: substitute(&opts.tooltip_format, window, opts.escape),
            alt: window.app_id.clone(),
            class: window_classes(window),
        },
    };
    serde_json::to_string(&status).expect("waybar status serializes")
}

fn substitute(template: &str, window: &proto::Window, escape: bool) -> String {
    let title = maybe_escape(&window.title, escape);
    let app_id = maybe_escape(&window.app_id, escape);
    let workspace = maybe_escape(window.workspace.as_deref().unwrap_or(""), escape);
    let output = maybe_escape(window.output.as_deref().unwrap_or(""), escape);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + '{'.len_utf8()..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        match &after[..end] {
            "title" => out.push_str(&title),
            "app_id" => out.push_str(&app_id),
            "workspace" => out.push_str(&workspace),
            "output" => out.push_str(&output),
            name => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[end + '}'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

fn maybe_escape(text: &str, escape: bool) -> String {
    if escape {
        pango_escape(text)
    } else {
        text.to_string()
    }
}

/// Waybar feeds `text` to Pango as markup. Escape the characters that start markup.
fn pango_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
    out
}

/// `empty` when nothing is focused. Otherwise `fullscreen`, then `floating`,
/// then the app id folded into a CSS class.
fn window_classes(window: &proto::Window) -> Vec<String> {
    let mut class = Vec::new();
    if window.fullscreen {
        class.push("fullscreen".to_string());
    }
    if window.floating {
        class.push("floating".to_string());
    }
    let app = css_class(&window.app_id);
    if !app.is_empty() {
        class.push(app);
    }
    class
}

fn css_class(app_id: &str) -> String {
    app_id
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| match character {
            'a'..='z' | '0'..='9' | '_' | '-' => character,
            _ => '-',
        })
        .collect()
}

fn emit_if_changed(
    line: &str,
    previous: &mut Option<String>,
    emit: &mut impl FnMut(&str) -> Result<(), Failure>,
) -> Result<(), Failure> {
    if previous.as_deref() == Some(line) {
        return Ok(());
    }
    emit(line)?;
    *previous = Some(line.to_string());
    Ok(())
}

fn request(request: &proto::Request) -> Result<proto::Reply, Failure> {
    request_at(&proto::socket_path(), request)
}

fn request_at(path: &Path, request: &proto::Request) -> Result<proto::Reply, Failure> {
    let mut stream = connect(path)?;
    write_request(&mut stream, request)?;
    let line = next_line(&mut BufReader::new(stream).lines())?;
    serde_json::from_str(&line).map_err(|_| Failure::Reply("malformed reply".to_string()))
}

fn subscribe_at(
    path: &Path,
    topics: &[proto::Topic],
    mut emit: impl FnMut(&str) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut stream = connect(path)?;
    write_request(&mut stream, &proto::Request::Subscribe { events: topics.to_vec() })?;
    let mut lines = BufReader::new(stream).lines();
    let ack = next_line(&mut lines)?;
    match serde_json::from_str::<proto::Reply>(&ack) {
        Ok(proto::Reply::Ok(proto::Response::Subscribed)) => {}
        Ok(proto::Reply::Error(message)) => return Err(Failure::Reply(message)),
        Ok(proto::Reply::Ok(_)) => return Err(Failure::Reply("unexpected reply".to_string())),
        Err(_) => return Err(Failure::Reply("malformed reply".to_string())),
    }
    for line in lines {
        let line = line.map_err(io_failure)?;
        accept_event_line(&line)?;
        emit(&line)?;
    }
    Ok(())
}

fn connect(path: &Path) -> Result<UnixStream, Failure> {
    UnixStream::connect(path).map_err(|error| Failure::Connect { path: path.to_path_buf(), error })
}

fn write_request(stream: &mut UnixStream, request: &proto::Request) -> Result<(), Failure> {
    stream
        .write_all(&proto::encode_line(request))
        .map_err(io_failure)
}

fn next_line<I>(lines: &mut I) -> Result<String, Failure>
where
    I: Iterator<Item = io::Result<String>>,
{
    match lines.next() {
        Some(Ok(line)) => Ok(line),
        Some(Err(error)) => Err(io_failure(error)),
        None => Err(Failure::Io("connection closed".to_string())),
    }
}

/// `Ok` means the line is an event and should be printed unchanged.
/// An unknown event name is still an event: clients ignore events they do not know.
fn accept_event_line(line: &str) -> Result<(), Failure> {
    if let Ok(reply) = serde_json::from_str::<proto::Reply>(line) {
        return match reply {
            proto::Reply::Error(message) => Err(Failure::Reply(message)),
            proto::Reply::Ok(_) => Err(Failure::Reply("unexpected reply".to_string())),
        };
    }
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|_| Failure::Reply("malformed reply".to_string()))?;
    if value
        .get("event")
        .and_then(|event| event.as_str())
        .is_some()
    {
        Ok(())
    } else {
        Err(Failure::Reply("malformed reply".to_string()))
    }
}

fn emit_stdout(line: &str) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{line}").and_then(|()| stdout.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(Failure::BrokenPipe),
        Err(error) => Err(io_failure(error)),
    }
}

fn io_failure(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Failure::BrokenPipe
    } else {
        Failure::Io(error.to_string())
    }
}

fn report(failure: &Failure) {
    match failure {
        Failure::Reply(message) => eprintln!("temactl: {message}"),
        Failure::Connect { path, error } => eprint!("{}", connect_line(path, error)),
        Failure::Io(message) => eprintln!("temactl: {message}"),
        Failure::Closed => eprintln!("temactl: connection closed"),
        Failure::BrokenPipe => {}
    }
}

fn connect_line(path: &Path, error: &io::Error) -> String {
    format!("temactl: cannot connect to {}: {error} (is tema running?)\n", path.display())
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Self::Connect { .. } | Self::Closed => EX_CONNECT,
            Self::BrokenPipe => 0,
            Self::Reply(_) | Self::Io(_) => EX_REPLY,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering::Relaxed},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn protocol_name(topic: proto::Topic) -> &'static str {
        match topic {
            proto::Topic::FocusedWindow => "focused_window",
            proto::Topic::Workspaces => "workspaces",
            proto::Topic::Outputs => "outputs",
            proto::Topic::Bell => "bell",
        }
    }

    fn window() -> proto::Window {
        proto::Window {
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

    #[test]
    fn focused_window_prints_the_sandbox_line() {
        let mut sandboxed = window();
        sandboxed.sandbox = Some(proto::WindowSandbox {
            engine: Some("org.flatpak".to_string()),
            app_id: Some("org.example.App".to_string()),
            instance_id: None,
        });
        assert!(format_focused_window(Some(&sandboxed))
            .contains("  Sandbox: org.flatpak app=org.example.App instance=-\n"));
        assert!(!format_focused_window(Some(&window())).contains("Sandbox:"));
    }

    fn workspace(active: bool, focused: bool) -> proto::Workspace {
        proto::Workspace {
            id: "ws-1-2".to_string(),
            name: "2".to_string(),
            index: 2,
            coordinates: vec![1],
            outputs: vec!["DP-1".to_string()],
            active,
            focused,
            windows: 3,
        }
    }

    fn output(enabled: bool) -> proto::Output {
        proto::Output {
            name: "DP-1".to_string(),
            make: "Dell".to_string(),
            model: "U2723QE".to_string(),
            description: "Dell Inc. U2723QE".to_string(),
            enabled,
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
    fn arguments_select_commands_and_reject_bad_usage() {
        assert_eq!(parse_args(["-h"]).unwrap(), Command::Help);
        assert_eq!(parse_args(["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_args(["version"]).unwrap(), Command::Version);
        assert_eq!(parse_args(["outputs"]).unwrap(), Command::Outputs { json: false });
        assert_eq!(parse_args(["outputs", "-j"]).unwrap(), Command::Outputs { json: true });
        assert_eq!(parse_args(["workspaces", "-j"]).unwrap(), Command::Workspaces { json: true });
        assert_eq!(
            parse_args(["focused-window", "-j"]).unwrap(),
            Command::FocusedWindow { json: true }
        );
        assert_eq!(parse_args(["subscribe"]).unwrap(), Command::Subscribe { topics: vec![] });
        assert_eq!(
            parse_args(["subscribe", "outputs", "outputs"]).unwrap(),
            Command::Subscribe { topics: vec![proto::Topic::Outputs, proto::Topic::Outputs] }
        );

        assert!(parse_args(std::iter::empty()).is_err());
        assert!(parse_args(["-j", "outputs"]).is_err());
        assert!(parse_args(["outputs", "-j", "-j"]).is_err());
        assert!(parse_args(["version", "-j"]).is_err());
        assert!(parse_args(["subscribe", "focused-window"]).is_err());
        assert!(parse_args(["help"]).is_err());
        assert!(parse_args(["--help", "outputs"]).is_err());
        assert!(parse_args(["waybar"]).is_err());
        assert!(parse_args(["waybar", "window", "--format"]).is_err());
        assert!(parse_args(["waybar", "--once", "window"]).is_err());
        assert!(parse_args(["window-icon"]).is_err());
        assert!(parse_args(["window-icon", "nope"]).is_err());
        assert!(parse_args(["window-icon", "12", "--size"]).is_err());
        assert!(parse_args(["window-icon", "12", "--size", "big"]).is_err());
    }

    #[test]
    fn window_icon_flags_default_to_32_at_scale_one() {
        assert_eq!(
            parse_args(["window-icon", "12"]).unwrap(),
            Command::WindowIcon(WindowIconCommand {
                id: 12,
                size: 32,
                scale: 1,
                output: None,
                json: false,
            })
        );
        assert_eq!(
            parse_args(["window-icon", "9", "-j", "--scale", "2", "-o", "i.png", "--size", "64"])
                .unwrap(),
            Command::WindowIcon(WindowIconCommand {
                id: 9,
                size: 64,
                scale: 2,
                output: Some("i.png".to_string()),
                json: true,
            })
        );
    }

    #[test]
    fn waybar_flags_keep_the_defaults_and_override_them() {
        assert_eq!(
            parse_args(["waybar", "window"]).unwrap(),
            Command::WaybarWindow(WaybarWindow::default())
        );
        assert_eq!(
            parse_args([
                "waybar",
                "window",
                "--once",
                "--no-escape",
                "--empty",
                "desktop",
                "--format",
                "{app_id}",
                "--tooltip-format",
                "{title}",
            ])
            .unwrap(),
            Command::WaybarWindow(WaybarWindow {
                format: "{app_id}".to_string(),
                tooltip_format: "{title}".to_string(),
                empty: "desktop".to_string(),
                escape: false,
                once: true,
            })
        );
    }

    #[test]
    fn waybar_substitutes_escapes_and_builds_classes() {
        let opts = WaybarWindow::default();
        let focused = window();
        assert_eq!(
            substitute("{title} [{workspace}/{output}] {app_id} {nope}", &focused, false),
            "nvim — ~/src [ws-1-2/DP-1] Alacritty {nope}"
        );
        let mut loose = focused.clone();
        loose.workspace = None;
        loose.output = None;
        assert_eq!(substitute("{workspace}|{output}|{title", &loose, false), "||{title");

        loose.title = "a&b<c>d'e\"f".to_string();
        assert_eq!(pango_escape(&loose.title), "a&amp;b&lt;c&gt;d&apos;e&quot;f");
        assert_eq!(substitute("{title}", &loose, true), "a&amp;b&lt;c&gt;d&apos;e&quot;f");
        assert_eq!(substitute("{title}", &loose, false), "a&b<c>d'e\"f");

        assert_eq!(window_classes(&focused), vec!["alacritty".to_string()]);
        let mut both = focused.clone();
        both.floating = true;
        both.fullscreen = true;
        both.app_id = "Org.Foo Bar".to_string();
        assert_eq!(
            window_classes(&both),
            vec!["fullscreen".to_string(), "floating".to_string(), "org-foo-bar".to_string()]
        );
        both.app_id.clear();
        assert_eq!(window_classes(&both), vec!["fullscreen".to_string(), "floating".to_string()]);
        assert_eq!(css_class("A_B.C"), "a_b-c");

        assert_eq!(
            render_waybar(&opts, Some(&focused)),
            "{\"text\":\"nvim — ~/src\",\"tooltip\":\"Alacritty: nvim — ~/src\",\"alt\":\"Alacritty\",\"class\":[\"alacritty\"]}"
        );
        let mut hidden = opts.clone();
        hidden.empty = "a<b".to_string();
        assert_eq!(
            render_waybar(&hidden, None),
            "{\"text\":\"a&lt;b\",\"tooltip\":\"\",\"alt\":\"\",\"class\":[\"empty\"]}"
        );
        hidden.escape = false;
        assert_eq!(
            render_waybar(&hidden, None),
            "{\"text\":\"a<b\",\"tooltip\":\"\",\"alt\":\"\",\"class\":[\"empty\"]}"
        );
    }

    #[test]
    fn waybar_prints_only_when_the_line_changes() {
        let opts = WaybarWindow::default();
        let first = render_waybar(&opts, Some(&window()));
        let second = render_waybar(&opts, None);
        let mut previous = None;
        let mut printed = Vec::new();
        emit_if_changed(&first, &mut previous, &mut |line| {
            printed.push(line.to_string());
            Ok(())
        })
        .unwrap();
        emit_if_changed(&first, &mut previous, &mut |line| {
            printed.push(line.to_string());
            Ok(())
        })
        .unwrap();
        emit_if_changed(&second, &mut previous, &mut |line| {
            printed.push(line.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(printed, vec![first, second]);
    }

    #[test]
    fn waybar_once_prints_the_focused_window_and_stops() {
        let event = proto::Event::FocusedWindow { window: Some(window()) };
        let path = serve("waybar", move |mut stream| {
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "{\"request\":\"subscribe\",\"events\":[\"focused_window\"]}\n");
            let mut bytes = proto::encode_line(&proto::Reply::Ok(proto::Response::Subscribed));
            bytes.extend(proto::encode_line(&event));
            stream.write_all(&bytes).unwrap();
        });
        let opts = WaybarWindow { once: true, ..WaybarWindow::default() };
        let mut lines = Vec::new();
        run_waybar_window_at(&path, &opts, |line| {
            lines.push(line.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(lines, vec![render_waybar(&opts, Some(&window()))]);

        let missing = PathBuf::from("/tmp/temactl-waybar-missing.sock");
        let failure = run_waybar_window_at(&missing, &opts, |_| Ok(())).unwrap_err();
        assert!(matches!(failure, Failure::Connect { .. }));
        assert_eq!(failure.code(), EX_CONNECT);
    }

    #[test]
    fn subscribe_accepts_every_topic_name() {
        for topic in [
            proto::Topic::FocusedWindow,
            proto::Topic::Workspaces,
            proto::Topic::Outputs,
            proto::Topic::Bell,
        ] {
            let name = protocol_name(topic);
            assert_eq!(serde_json::to_value(topic).unwrap(), name);
            assert!(USAGE.contains(name), "{name}");
            assert_eq!(
                parse_args(["subscribe", name]).unwrap(),
                Command::Subscribe { topics: vec![topic] }
            );
        }
    }

    #[test]
    fn text_formats_the_protocol_objects() {
        let version = proto::Version { compositor: "0.1.0".to_string(), protocol: 1 };
        assert_eq!(
            format_version("0.1.0", 1, &version),
            "client 0.1.0\ncompositor 0.1.0\nprotocol 1\n"
        );
        let newer = proto::Version { compositor: "0.2.0".to_string(), protocol: 2 };
        assert_eq!(
            format_version("0.1.0", 1, &newer),
            "client 0.1.0\ncompositor 0.2.0\nprotocol 2 (this client speaks 1)\n"
        );

        assert_eq!(
            format_outputs(&[output(true)]),
            "\
Output DP-1 'Dell U2723QE'
  Description: Dell Inc. U2723QE
  Position: 0,0
  Mode: 2560x1440 @ 239.960 Hz
  Scale: 1.25
  Workspace: ws-1-2
"
        );
        let mut disabled = output(false);
        disabled.make.clear();
        disabled.model.clear();
        disabled.description.clear();
        disabled.active_workspace = None;
        assert_eq!(
            format_output(&disabled),
            "\
Output DP-1 (disabled)
  Position: 0,0
  Mode: 2560x1440 @ 239.960 Hz
  Scale: 1.25
  Workspace: none
"
        );
        assert_eq!(format_outputs(&[]), "No outputs\n");

        let mut second = workspace(false, false);
        second.name = "3".to_string();
        second.coordinates.clear();
        second.outputs.clear();
        assert_eq!(
            format_workspaces(&[workspace(true, true), second]),
            "\
Workspace 2 (active, focused)
  Id: ws-1-2
  Index: 2
  Coordinates: 1
  Outputs: DP-1
  Windows: 3

Workspace 3
  Id: ws-1-2
  Index: 2
  Coordinates: none
  Outputs: none
  Windows: 3
"
        );
        assert_eq!(format_workspaces(&[]), "No workspaces\n");

        assert_eq!(
            format_focused_window(Some(&window())),
            "\
Window 12: nvim — ~/src
  App id: Alacritty
  Workspace: ws-1-2
  Output: DP-1
  Floating: no
  Fullscreen: no
"
        );
        let mut tagged = window();
        tagged.tag = Some("main".to_string());
        tagged.description = Some("the main window".to_string());
        assert_eq!(
            format_focused_window(Some(&tagged)),
            "\
Window 12: nvim — ~/src
  App id: Alacritty
  Workspace: ws-1-2
  Output: DP-1
  Floating: no
  Fullscreen: no
  Tag: main
  Description: the main window
"
        );
        let mut bare = window();
        bare.workspace = None;
        bare.output = None;
        bare.floating = true;
        bare.fullscreen = true;
        assert!(format_focused_window(Some(&bare))
            .contains("  Workspace: none\n  Output: none\n  Floating: yes\n  Fullscreen: yes\n"));

        let mut modal = window();
        modal.modal = true;
        assert!(format_focused_window(Some(&modal)).ends_with("  Fullscreen: no\n  Modal: yes\n"));
        assert!(!format_focused_window(Some(&window())).contains("Modal"));

        let named = proto::WindowIcon {
            id: 12,
            name: Some("firefox".to_string()),
            size: Some(32),
            scale: Some(1),
            format: "argb8888_premultiplied".to_string(),
            data: Some("AQIDBA==".to_string()),
        };
        assert_eq!(format_window_icon(&named), "name: firefox\nsize: 32\nscale: 1\n");
        let mut bare_icon = named;
        bare_icon.name = None;
        bare_icon.size = None;
        bare_icon.scale = None;
        bare_icon.data = None;
        assert_eq!(format_window_icon(&bare_icon), "name: none\nno pixels\n");
        assert_eq!(icon_png(&bare_icon).unwrap_err(), "window has no icon pixels");
        assert_eq!(format_focused_window(None), "No focused window\n");
    }

    #[test]
    fn png_chunks_match_recomputed_crc_and_adler() {
        let one = png_rgba(1, 1, &[10, 20, 30, 255]);
        assert_png(&one, 1, 1, &[10, 20, 30, 255]);
        let mut six = Vec::new();
        for pixel in 0..6u8 {
            six.extend([pixel, pixel.wrapping_add(1), pixel.wrapping_add(2), 255]);
        }
        assert_png(&png_rgba(3, 2, &six), 3, 2, &six);
        let wide = vec![7u8; 16_384 * 4];
        assert_png(&png_rgba(16_384, 1, &wide), 16_384, 1, &wide);

        let icon = proto::WindowIcon {
            id: 1,
            name: None,
            size: Some(1),
            scale: Some(1),
            format: "argb8888_premultiplied".to_string(),
            data: Some(proto::base64_encode(&[128, 0, 0, 128])),
        };
        assert_eq!(icon_png(&icon).unwrap(), png_rgba(1, 1, &[0, 0, 255, 128]));
    }

    struct PngChunk {
        kind: [u8; 4],
        data: Vec<u8>,
        crc: u32,
    }

    fn assert_png(png: &[u8], width: u32, height: u32, rgba: &[u8]) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut rest = &png[8..];
        let ihdr = read_chunk(&mut rest);
        assert_eq!(ihdr.kind, *b"IHDR");
        assert_eq!(u32::from_be_bytes(ihdr.data[0..4].try_into().unwrap()), width);
        assert_eq!(u32::from_be_bytes(ihdr.data[4..8].try_into().unwrap()), height);
        assert_eq!(&ihdr.data[8..], &[8, 6, 0, 0, 0]);
        assert_eq!(ihdr.crc, chunk_crc(&ihdr));
        let idat = read_chunk(&mut rest);
        assert_eq!(idat.kind, *b"IDAT");
        assert_eq!(idat.crc, chunk_crc(&idat));
        assert_eq!(&idat.data[..2], &[0x78, 0x01]);
        let stored = inflate_stored(&idat.data[2..idat.data.len() - 4]);
        let adler = u32::from_be_bytes(idat.data[idat.data.len() - 4..].try_into().unwrap());
        assert_eq!(adler, adler_of(&stored));
        let mut expected = Vec::new();
        let row = (width as usize).saturating_mul(4);
        for y in 0..height as usize {
            expected.push(0);
            expected.extend_from_slice(&rgba[y * row..(y + 1) * row]);
        }
        assert_eq!(stored, expected);
        let iend = read_chunk(&mut rest);
        assert_eq!(iend.kind, *b"IEND");
        assert!(iend.data.is_empty());
        assert_eq!(iend.crc, chunk_crc(&iend));
        assert!(rest.is_empty());
    }

    fn read_chunk(rest: &mut &[u8]) -> PngChunk {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let kind = rest[4..8].try_into().unwrap();
        let data = rest[8..8 + len].to_vec();
        let crc = u32::from_be_bytes(rest[8 + len..12 + len].try_into().unwrap());
        *rest = &rest[12 + len..];
        PngChunk { kind, data, crc }
    }

    fn chunk_crc(chunk: &PngChunk) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in chunk.kind.into_iter().chain(chunk.data.iter().copied()) {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn adler_of(data: &[u8]) -> u32 {
        let mut a = 1u32;
        let mut b = 0u32;
        for byte in data {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    fn inflate_stored(mut data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let header = data[0];
            data = &data[1..];
            let len = u16::from_le_bytes(data[..2].try_into().unwrap()) as usize;
            let nlen = u16::from_le_bytes(data[2..4].try_into().unwrap());
            assert_eq!(nlen, !(len as u16));
            data = &data[4..];
            out.extend_from_slice(&data[..len]);
            data = &data[len..];
            if header & 1 == 1 {
                assert!(data.is_empty());
                return out;
            }
        }
    }

    #[test]
    fn json_is_the_ok_value_on_one_line() {
        let outputs = proto::Response::Outputs(vec![output(true)]);
        let line = json_line(&outputs);
        assert!(line.ends_with('\n'));
        assert_eq!(line.chars().filter(|byte| *byte == '\n').count(), 1);
        assert_eq!(line.trim_end(), serde_json::to_string(&outputs).unwrap());
        let wrapped = serde_json::to_value(proto::Reply::Ok(outputs)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed, wrapped["ok"]);
        assert_eq!(
            json_line(&proto::Response::FocusedWindow(None)).trim_end(),
            r#"{"focused_window":null}"#
        );
    }

    #[test]
    fn event_lines_pass_and_replies_do_not() {
        assert!(
            accept_event_line(r#"{"event":"focused_window","window":null,"future":true}"#).is_ok()
        );
        assert!(accept_event_line(r#"{"event":"keyboard_layout"}"#).is_ok());
        assert!(matches!(
            accept_event_line(r#"{"error":"connection is subscribed"}"#),
            Err(Failure::Reply(message)) if message == "connection is subscribed"
        ));
        assert!(matches!(
            accept_event_line(r#"{"ok":"subscribed"}"#),
            Err(Failure::Reply(message)) if message == "unexpected reply"
        ));
        assert!(matches!(
            accept_event_line("not json"),
            Err(Failure::Reply(message)) if message == "malformed reply"
        ));
    }

    #[test]
    fn a_missing_socket_reports_the_connect_line() {
        let path = PathBuf::from("/tmp/temactl-missing.sock");
        let failure = request_at(&path, &proto::Request::Version).unwrap_err();
        assert_eq!(failure.code(), EX_CONNECT);
        let Failure::Connect { path: got, error } = failure else {
            panic!("expected a connect failure");
        };
        assert_eq!(got, path);
        let line = connect_line(&got, &error);
        assert_eq!(
            line,
            format!("temactl: cannot connect to {}: {error} (is tema running?)\n", got.display())
        );
    }

    #[test]
    fn a_query_and_subscribe_round_trip_on_a_socket() {
        let version = serve("version", |mut stream| {
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "{\"request\":\"version\"}\n");
            stream
                .write_all(b"{\"ok\":{\"version\":{\"compositor\":\"0.1.0\",\"protocol\":1}}}\n")
                .unwrap();
        });
        let reply = request_at(&version, &proto::Request::Version).unwrap();
        assert_eq!(
            reply,
            proto::Reply::Ok(proto::Response::Version(proto::Version {
                compositor: "0.1.0".to_string(),
                protocol: 1,
            }))
        );

        let subscribed = serve("subscribe", |mut stream| {
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "{\"request\":\"subscribe\",\"events\":[\"outputs\"]}\n");
            stream
                .write_all(b"{\"ok\":\"subscribed\"}\n{\"event\":\"outputs\",\"outputs\":[],\"extra\":1}\n")
                .unwrap();
        });
        let mut events = Vec::new();
        subscribe_at(&subscribed, &[proto::Topic::Outputs], |line| {
            events.push(line.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(events, vec![r#"{"event":"outputs","outputs":[],"extra":1}"#.to_string()]);
    }

    fn serve(name: &str, handler: impl FnOnce(UnixStream) + Send + 'static) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "temactl-{name}-{}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let path_for_cleanup = path.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handler(stream);
            let _ = fs::remove_file(path_for_cleanup);
        });
        path
    }
}
