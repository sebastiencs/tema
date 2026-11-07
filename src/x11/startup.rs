//! Extension and atom startup.
//!
//! The owner calls [`Startup::start`], feeds each [`StartupEvent`] back through
//! [`Startup::handle`], and stops at [`Status::Ready`] or [`Status::Failed`].
//! `QueryExtension` goes out for Composite, XFIXES, RENDER, and X-Resource.
//! `QueryVersion` follows for each one that is present: Composite 0.4, XFIXES
//! 6.0, RENDER 0.11, and X-Resource 1.2. X-Resource may be absent. Present
//! extensions are registered on the connection so later events and errors use
//! those bases. The intern batch for [`crate::x11::atoms::NAMES`] follows the
//! version replies.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

use crate::runner::{AnyAction, Redispatcher};

use super::{
    atoms::{AtomBatch, AtomBatchEvent, AtomError, Atoms},
    generated::{
        CompositeQueryVersion, CompositeQueryVersionReply, ExtensionName, QueryExtension,
        QueryExtensionReply, RenderQueryVersion, RenderQueryVersionReply, ResQueryVersion,
        ResQueryVersionReply, XFixesQueryVersion, XFixesQueryVersionReply,
    },
    ConnectionError, XConnection, XReplyError,
};

/// One extension after its version reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExtensionInfo {
    pub(crate) major_opcode: u8,
    pub(crate) first_event: u8,
    pub(crate) first_error: u8,
    pub(crate) major_version: u32,
    pub(crate) minor_version: u32,
}

/// The extensions startup found. `res` is absent when X-Resource is not present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extensions {
    pub(crate) composite: ExtensionInfo,
    pub(crate) xfixes: ExtensionInfo,
    pub(crate) render: ExtensionInfo,
    pub(crate) res: Option<ExtensionInfo>,
}

/// Extensions and atoms once both steps have succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bringup {
    pub(crate) extensions: Extensions,
    pub(crate) atoms: Atoms,
}

/// Where startup is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// A query is still outstanding.
    Pending,
    /// Extensions and the intern batch both succeeded.
    Ready(Bringup),
    /// Startup stopped. The connection itself stays up.
    Failed(StartupError),
}

/// Why startup stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupError {
    /// `send` failed.
    Connection(ConnectionError),
    /// Composite, XFIXES, or RENDER answered `present == 0`.
    Missing(ExtensionName),
    /// The server's version is older than the one this client requires.
    TooOld { extension: ExtensionName, major: u32, minor: u32 },
    /// `QueryExtension` or `QueryVersion` returned an X error, or the connection died.
    Reply { extension: ExtensionName, error: XReplyError },
    /// A reply arrived for a step that is not in progress.
    OutOfOrder(ExtensionName),
    /// The intern batch failed.
    Atoms(AtomError),
}

/// A reply the owner hands back to [`Startup::handle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupEvent {
    Extension { name: ExtensionName, result: Result<QueryExtensionReply, XReplyError> },
    CompositeVersion(Result<CompositeQueryVersionReply, XReplyError>),
    XFixesVersion(Result<XFixesQueryVersionReply, XReplyError>),
    RenderVersion(Result<RenderQueryVersionReply, XReplyError>),
    ResVersion(Result<ResQueryVersionReply, XReplyError>),
    Atom(AtomBatchEvent),
}

struct Spec {
    name: ExtensionName,
    wire: &'static [u8],
    major: u32,
    minor: u32,
    required: bool,
}

const SPECS: [Spec; 4] = [
    Spec {
        name: ExtensionName::Composite,
        wire: b"Composite",
        major: 0,
        minor: 4,
        required: true,
    },
    Spec {
        name: ExtensionName::XFixes,
        wire: b"XFIXES",
        major: 6,
        minor: 0,
        required: true,
    },
    Spec {
        name: ExtensionName::Render,
        wire: b"RENDER",
        major: 0,
        minor: 11,
        required: true,
    },
    Spec {
        name: ExtensionName::Res,
        wire: b"X-Resource",
        major: 1,
        minor: 2,
        required: false,
    },
];

struct Answer {
    name: ExtensionName,
    result: Result<QueryExtensionReply, XReplyError>,
}

struct Building {
    composite: ExtensionInfo,
    xfixes: ExtensionInfo,
    render: ExtensionInfo,
    res: Option<ExtensionInfo>,
}

enum Phase {
    Idle,
    Extensions { left: usize, answers: Vec<Answer> },
    Versions { left: usize, building: Building },
    Atoms { batch: AtomBatch, extensions: Extensions },
    Done(Status),
}

/// Drives `QueryExtension` and `QueryVersion`. The owner owns the I/O.
pub(crate) struct Startup {
    phase: Phase,
    redispatch: Option<Redispatcher<StartupEvent>>,
}

impl Startup {
    pub(crate) fn new() -> Self {
        Self { phase: Phase::Idle, redispatch: None }
    }

    pub(crate) fn status(&self) -> Status {
        self.current()
    }

    /// Sends the four `QueryExtension` requests.
    ///
    /// A second call leaves the machine where it is.
    pub(crate) fn start(
        &mut self,
        connection: &mut XConnection,
        redispatch: Redispatcher<StartupEvent>,
    ) -> Status {
        if !matches!(self.phase, Phase::Idle) {
            return self.current();
        }
        self.redispatch = Some(redispatch);
        for spec in &SPECS {
            let name = spec.name;
            let redispatch = self.redispatch.clone().expect("redispatch is stored above");
            let slot = Redispatcher::to_any(move |result| {
                deliver(&redispatch, StartupEvent::Extension { name, result })
            });
            if let Err(error) = connection.send(&QueryExtension { name: spec.wire }, slot) {
                return self.fail(StartupError::Connection(error));
            }
        }
        self.phase = Phase::Extensions { left: SPECS.len(), answers: Vec::new() };
        Status::Pending
    }

    /// Accepts one reply and sends the version queries once every extension has answered.
    pub(crate) fn handle(&mut self, event: StartupEvent, connection: &mut XConnection) -> Status {
        if matches!(self.phase, Phase::Done(_)) {
            return self.current();
        }
        match event {
            StartupEvent::Extension { name, result } => {
                self.note_extension(name, result, connection)
            }
            StartupEvent::CompositeVersion(result) => {
                self.note_version(connection, ExtensionName::Composite, versions(result))
            }
            StartupEvent::XFixesVersion(result) => {
                self.note_version(connection, ExtensionName::XFixes, versions(result))
            }
            StartupEvent::RenderVersion(result) => {
                self.note_version(connection, ExtensionName::Render, versions(result))
            }
            StartupEvent::ResVersion(result) => self.note_version(
                connection,
                ExtensionName::Res,
                result.map(|reply| (u32::from(reply.server_major), u32::from(reply.server_minor))),
            ),
            StartupEvent::Atom(event) => self.note_atom(event),
        }
    }

    fn note_extension(
        &mut self,
        name: ExtensionName,
        result: Result<QueryExtensionReply, XReplyError>,
        connection: &mut XConnection,
    ) -> Status {
        let step = match &mut self.phase {
            Phase::Extensions { left, answers } => {
                if answers.iter().any(|answer| answer.name == name) {
                    Step::Pending
                } else {
                    answers.push(Answer { name, result });
                    *left -= 1;
                    if *left == 0 {
                        Step::Complete
                    } else {
                        Step::Pending
                    }
                }
            }
            _ => Step::Bad(StartupError::OutOfOrder(name)),
        };
        match step {
            Step::Pending => Status::Pending,
            Step::Complete => self.finish_extensions(connection),
            Step::Bad(error) => self.fail(error),
        }
    }

    fn finish_extensions(&mut self, connection: &mut XConnection) -> Status {
        let answers = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Extensions { answers, .. } => answers,
            other => {
                self.phase = other;
                return self.current();
            }
        };
        let mut present = Vec::new();
        for spec in &SPECS {
            let Some(answer) = answers.iter().find(|answer| answer.name == spec.name) else {
                return self.fail(StartupError::OutOfOrder(spec.name));
            };
            match answer.result {
                Err(ref error) => {
                    return self
                        .fail(StartupError::Reply { extension: spec.name, error: error.clone() });
                }
                Ok(reply) if reply.present == 0 => {
                    if spec.required {
                        return self.fail(StartupError::Missing(spec.name));
                    }
                }
                Ok(reply) => present.push((spec, reply)),
            }
        }
        let mut building = Building {
            composite: blank(),
            xfixes: blank(),
            render: blank(),
            res: None,
        };
        for (spec, reply) in &present {
            connection.set_extension(
                spec.name,
                reply.major_opcode,
                reply.first_event,
                reply.first_error,
            );
            let info = ExtensionInfo {
                major_opcode: reply.major_opcode,
                first_event: reply.first_event,
                first_error: reply.first_error,
                major_version: 0,
                minor_version: 0,
            };
            match spec.name {
                ExtensionName::Composite => building.composite = info,
                ExtensionName::XFixes => building.xfixes = info,
                ExtensionName::Render => building.render = info,
                ExtensionName::Res => building.res = Some(info),
            }
        }
        let left = present.len();
        for (spec, _) in &present {
            if let Err(error) = self.send_version(connection, spec.name) {
                return self.fail(StartupError::Connection(error));
            }
        }
        self.phase = Phase::Versions { left, building };
        Status::Pending
    }

    fn send_version(
        &self,
        connection: &mut XConnection,
        name: ExtensionName,
    ) -> Result<(), ConnectionError> {
        let redispatch = self.redispatch.clone().expect("startup was started");
        let spec = spec(name);
        match name {
            ExtensionName::Composite => {
                let request = CompositeQueryVersion {
                    client_major_version: spec.major,
                    client_minor_version: spec.minor,
                };
                connection.send(&request, map_reply(redispatch, StartupEvent::CompositeVersion))?;
            }
            ExtensionName::XFixes => {
                let request = XFixesQueryVersion {
                    client_major_version: spec.major,
                    client_minor_version: spec.minor,
                };
                connection.send(&request, map_reply(redispatch, StartupEvent::XFixesVersion))?;
            }
            ExtensionName::Render => {
                let request = RenderQueryVersion {
                    client_major_version: spec.major,
                    client_minor_version: spec.minor,
                };
                connection.send(&request, map_reply(redispatch, StartupEvent::RenderVersion))?;
            }
            ExtensionName::Res => {
                let request = ResQueryVersion {
                    client_major: u8::try_from(spec.major).expect("res major fits in a byte"),
                    client_minor: u8::try_from(spec.minor).expect("res minor fits in a byte"),
                };
                connection.send(&request, map_reply(redispatch, StartupEvent::ResVersion))?;
            }
        }
        Ok(())
    }

    fn note_version(
        &mut self,
        connection: &mut XConnection,
        extension: ExtensionName,
        result: Result<(u32, u32), XReplyError>,
    ) -> Status {
        if !matches!(self.phase, Phase::Versions { .. }) {
            return self.fail(StartupError::OutOfOrder(extension));
        }
        let (major, minor) = match result {
            Ok(version) => version,
            Err(error) => return self.fail(StartupError::Reply { extension, error }),
        };
        let required = spec(extension);
        if major < required.major || (major == required.major && minor < required.minor) {
            return self.fail(StartupError::TooOld { extension, major, minor });
        }
        let noted = match &mut self.phase {
            Phase::Versions { left, building } => {
                let slot = match extension {
                    ExtensionName::Composite => Some(&mut building.composite),
                    ExtensionName::XFixes => Some(&mut building.xfixes),
                    ExtensionName::Render => Some(&mut building.render),
                    ExtensionName::Res => building.res.as_mut(),
                };
                match slot {
                    Some(slot) => {
                        slot.major_version = major;
                        slot.minor_version = minor;
                        *left -= 1;
                        if *left == 0 {
                            Noted::Ready
                        } else {
                            Noted::Pending
                        }
                    }
                    None => Noted::Absent,
                }
            }
            _ => Noted::Absent,
        };
        match noted {
            Noted::Pending => Status::Pending,
            Noted::Ready => self.finish_versions(connection),
            Noted::Absent => self.fail(StartupError::OutOfOrder(extension)),
        }
    }

    fn finish_versions(&mut self, connection: &mut XConnection) -> Status {
        let building = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Versions { building, .. } => building,
            other => {
                self.phase = other;
                return self.current();
            }
        };
        let extensions = Extensions {
            composite: building.composite,
            xfixes: building.xfixes,
            render: building.render,
            res: building.res,
        };
        let owner = self.redispatch.clone().expect("startup was started");
        let forward = Redispatcher::to_any(move |event: AtomBatchEvent| {
            owner
                .clone()
                .make(StartupEvent::Atom(event))
                .unwrap_or_else(AnyAction::empty)
        });
        let batch = match AtomBatch::send(connection, forward) {
            Ok(batch) => batch,
            Err(error) => return self.fail(StartupError::Connection(error)),
        };
        self.phase = Phase::Atoms { batch, extensions };
        Status::Pending
    }

    fn note_atom(&mut self, event: AtomBatchEvent) -> Status {
        let step = match &mut self.phase {
            Phase::Atoms { batch, .. } => match batch.handle(event) {
                Ok(None) => AtomStep::Pending,
                Ok(Some(atoms)) => AtomStep::Ready(atoms),
                Err(error) => AtomStep::Failed(error),
            },
            _ => AtomStep::Failed(AtomError::OutOfOrder),
        };
        match step {
            AtomStep::Pending => Status::Pending,
            AtomStep::Ready(atoms) => {
                let extensions = match std::mem::replace(&mut self.phase, Phase::Idle) {
                    Phase::Atoms { extensions, .. } => extensions,
                    other => {
                        self.phase = other;
                        return self.current();
                    }
                };
                let status = Status::Ready(Bringup { extensions, atoms });
                self.phase = Phase::Done(status.clone());
                status
            }
            AtomStep::Failed(error) => self.fail(StartupError::Atoms(error)),
        }
    }

    fn fail(&mut self, error: StartupError) -> Status {
        let status = Status::Failed(error);
        self.phase = Phase::Done(status.clone());
        status
    }

    fn current(&self) -> Status {
        match &self.phase {
            Phase::Done(status) => status.clone(),
            Phase::Idle
            | Phase::Extensions { .. }
            | Phase::Versions { .. }
            | Phase::Atoms { .. } => Status::Pending,
        }
    }
}

enum Step {
    Pending,
    Complete,
    Bad(StartupError),
}

enum Noted {
    Pending,
    Ready,
    Absent,
}

enum AtomStep {
    Pending,
    Ready(super::atoms::Atoms),
    Failed(AtomError),
}

fn spec(name: ExtensionName) -> &'static Spec {
    SPECS
        .iter()
        .find(|spec| spec.name == name)
        .expect("every extension is listed")
}

fn blank() -> ExtensionInfo {
    ExtensionInfo {
        major_opcode: 0,
        first_event: 0,
        first_error: 0,
        major_version: 0,
        minor_version: 0,
    }
}

fn versions<T>(result: Result<T, XReplyError>) -> Result<(u32, u32), XReplyError>
where
    T: VersionReply,
{
    result.map(|reply| (reply.major(), reply.minor()))
}

trait VersionReply {
    fn major(&self) -> u32;
    fn minor(&self) -> u32;
}

impl VersionReply for CompositeQueryVersionReply {
    fn major(&self) -> u32 {
        self.major_version
    }
    fn minor(&self) -> u32 {
        self.minor_version
    }
}

impl VersionReply for XFixesQueryVersionReply {
    fn major(&self) -> u32 {
        self.major_version
    }
    fn minor(&self) -> u32 {
        self.minor_version
    }
}

impl VersionReply for RenderQueryVersionReply {
    fn major(&self) -> u32 {
        self.major_version
    }
    fn minor(&self) -> u32 {
        self.minor_version
    }
}

fn deliver(redispatch: &Redispatcher<StartupEvent>, event: StartupEvent) -> AnyAction {
    redispatch
        .clone()
        .make(event)
        .unwrap_or_else(AnyAction::empty)
}

fn map_reply<T>(
    redispatch: Redispatcher<StartupEvent>,
    wrap: fn(Result<T, XReplyError>) -> StartupEvent,
) -> Redispatcher<Result<T, XReplyError>>
where
    T: 'static,
{
    Redispatcher::to_any(move |result| deliver(&redispatch, wrap(result)))
}

fn wire_name(name: ExtensionName) -> &'static str {
    match name {
        ExtensionName::Composite => "Composite",
        ExtensionName::XFixes => "XFIXES",
        ExtensionName::Render => "RENDER",
        ExtensionName::Res => "X-Resource",
    }
}

impl Display for StartupError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(error) => write!(formatter, "{error}"),
            Self::Missing(name) => write!(formatter, "X extension {} is missing", wire_name(*name)),
            Self::TooOld { extension, major, minor } => {
                let required = spec(*extension);
                write!(
                    formatter,
                    "X extension {} version {major}.{minor} is older than {}.{}",
                    wire_name(*extension),
                    required.major,
                    required.minor
                )
            }
            Self::Reply { extension, error } => {
                write!(formatter, "X extension {} query failed: {error}", wire_name(*extension))
            }
            Self::OutOfOrder(name) => {
                write!(
                    formatter,
                    "X extension {} reply does not match this startup step",
                    wire_name(*name)
                )
            }
            Self::Atoms(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for StartupError {}

#[cfg(test)]
mod tests {
    use {
        super::{
            ExtensionInfo, ExtensionName, Extensions, Startup, StartupError, StartupEvent, Status,
        },
        crate::{
            runner::{AnyAction, Dispatcher, Redispatcher},
            x11::{
                generated::{
                    Atom, BackingStore, Colormap, DestroyWindow, Event, EventMask, ImageOrder,
                    QueryExtensionReply, Screen, SelectionEvent, Setup, Window, XError,
                    XFixesSelectionNotify,
                },
                wire::Writer,
                ConnectionError, State, XConnection, XReplyError,
            },
        },
    };

    struct ServerExt {
        present: u8,
        major_opcode: u8,
        first_event: u8,
        first_error: u8,
        major: u32,
        minor: u32,
    }

    fn standard_server() -> [ServerExt; 4] {
        [
            ServerExt {
                present: 1,
                major_opcode: 142,
                first_event: 80,
                first_error: 140,
                major: 0,
                minor: 4,
            },
            ServerExt {
                present: 1,
                major_opcode: 143,
                first_event: 85,
                first_error: 160,
                major: 6,
                minor: 1,
            },
            ServerExt {
                present: 1,
                major_opcode: 144,
                first_event: 90,
                first_error: 170,
                major: 0,
                minor: 11,
            },
            ServerExt {
                present: 1,
                major_opcode: 145,
                first_event: 0,
                first_error: 0,
                major: 1,
                minor: 2,
            },
        ]
    }

    fn screen() -> Screen {
        Screen {
            root: Window(0x100),
            default_colormap: Colormap(0),
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: EventMask::empty(),
            width_in_pixels: 1,
            height_in_pixels: 1,
            width_in_millimeters: 1,
            height_in_millimeters: 1,
            min_installed_maps: 1,
            max_installed_maps: 1,
            root_visual: 0x21,
            backing_stores: BackingStore::NotUseful,
            save_unders: 0,
            root_depth: 24,
            allowed_depths_len: 0,
            allowed_depths: Vec::new(),
        }
    }

    fn ready_connection() -> (XConnection, Dispatcher) {
        let mut setup = Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: 0x20,
            resource_id_mask: 0b1100,
            motion_buffer_size: 0,
            vendor_len: 0,
            maximum_request_length: 256,
            roots_len: 1,
            pixmap_formats_len: 0,
            image_byte_order: ImageOrder::LSBFirst,
            bitmap_format_bit_order: ImageOrder::LSBFirst,
            bitmap_format_scanline_unit: 32,
            bitmap_format_scanline_pad: 32,
            min_keycode: 8,
            max_keycode: 255,
            vendor: Vec::new(),
            pixmap_formats: Vec::new(),
            roots: vec![screen()],
        };
        let mut bytes = Vec::new();
        setup.serialize(&mut Writer::new(&mut bytes));
        let extra = bytes.len() - 8;
        setup.length = u16::try_from(extra / 4).unwrap();
        bytes.clear();
        setup.serialize(&mut Writer::new(&mut bytes));
        let (mut connection, _) = XConnection::new();
        let mut dispatcher = Dispatcher::default();
        let read = connection.on_read(&bytes, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(connection.state(), State::Ready);
        (connection, dispatcher)
    }

    fn started() -> (XConnection, Dispatcher, Startup, Vec<u8>) {
        let (mut connection, dispatcher) = ready_connection();
        let mut startup = Startup::new();
        let status = startup.start(&mut connection, Redispatcher::to_any(AnyAction::from));
        assert_eq!(status, Status::Pending);
        let queries = connection.take_outgoing();
        (connection, dispatcher, startup, queries)
    }

    fn extension_reply(sequence: u16, server: &ServerExt) -> Vec<u8> {
        let mut bytes = Vec::new();
        QueryExtensionReply {
            sequence,
            present: server.present,
            major_opcode: server.major_opcode,
            first_event: server.first_event,
            first_error: server.first_error,
        }
        .serialize(&mut Writer::new(&mut bytes))
        .unwrap();
        bytes
    }

    fn version_reply(index: usize, sequence: u16, major: u32, minor: u32) -> Vec<u8> {
        use crate::x11::generated::{
            CompositeQueryVersionReply, RenderQueryVersionReply, ResQueryVersionReply,
            XFixesQueryVersionReply,
        };
        let mut bytes = Vec::new();
        match index {
            0 => {
                CompositeQueryVersionReply { sequence, major_version: major, minor_version: minor }
                    .serialize(&mut Writer::new(&mut bytes))
                    .unwrap()
            }
            1 => XFixesQueryVersionReply { sequence, major_version: major, minor_version: minor }
                .serialize(&mut Writer::new(&mut bytes))
                .unwrap(),
            2 => RenderQueryVersionReply { sequence, major_version: major, minor_version: minor }
                .serialize(&mut Writer::new(&mut bytes))
                .unwrap(),
            3 => ResQueryVersionReply {
                sequence,
                server_major: u16::try_from(major).unwrap(),
                server_minor: u16::try_from(minor).unwrap(),
            }
            .serialize(&mut Writer::new(&mut bytes))
            .unwrap(),
            _ => unreachable!(),
        }
        bytes
    }

    fn error_frame(sequence: u16, code: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        XError {
            code,
            sequence,
            bad_value: 0,
            minor_opcode: 0,
            major_opcode: 0,
        }
        .serialize(&mut Writer::new(&mut bytes))
        .unwrap();
        bytes
    }

    fn requests(bytes: &[u8]) -> Vec<&[u8]> {
        let mut found = Vec::new();
        let mut offset = 0;
        while offset + 4 <= bytes.len() {
            let units = u16::from_ne_bytes(bytes[offset + 2..offset + 4].try_into().unwrap());
            let len = usize::from(units) * 4;
            found.push(&bytes[offset..offset + len]);
            offset += len;
        }
        assert_eq!(offset, bytes.len());
        found
    }

    fn query_name(request: &[u8]) -> &[u8] {
        let len = u16::from_ne_bytes(request[4..6].try_into().unwrap()) as usize;
        &request[8..8 + len]
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn events_since(dispatcher: &Dispatcher, seen: &mut usize) -> Vec<StartupEvent> {
        let mut all = Vec::new();
        dispatcher.test_for_each_queued_action(|event: &StartupEvent| all.push(event.clone()));
        let fresh = all[*seen..].to_vec();
        *seen = all.len();
        fresh
    }

    fn pump(
        startup: &mut Startup,
        connection: &mut XConnection,
        dispatcher: &mut Dispatcher,
        wire: &[u8],
        seen: &mut usize,
    ) -> Status {
        let read = connection.on_read(wire, dispatcher);
        assert!(read.dead.is_none());
        let mut status = startup.status();
        for event in events_since(dispatcher, seen) {
            status = startup.handle(event, connection);
        }
        status
    }

    fn intern_wire(start: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (index, _) in crate::x11::atoms::NAMES.iter().enumerate() {
            let mut reply = Vec::new();
            crate::x11::generated::InternAtomReply {
                sequence: start + u16::try_from(index).unwrap(),
                atom: Atom(1_000 + index as u32),
            }
            .serialize(&mut Writer::new(&mut reply))
            .unwrap();
            bytes.extend(reply);
        }
        bytes
    }

    fn extension_wire(server: &[ServerExt]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (index, server) in server.iter().enumerate() {
            bytes.extend(extension_reply(u16::try_from(index + 1).unwrap(), server));
        }
        bytes
    }

    #[test]
    fn queries_use_the_wire_names_and_a_second_start_is_idle() {
        let (mut connection, _, mut startup, queries) = started();
        let names: Vec<&[u8]> = requests(&queries)
            .iter()
            .map(|request| query_name(request))
            .collect();
        assert_eq!(names, [b"Composite".as_slice(), b"XFIXES", b"RENDER", b"X-Resource"]);
        startup.start(&mut connection, Redispatcher::to_any(AnyAction::from));
        assert!(connection.take_outgoing().is_empty());
    }

    #[test]
    fn a_fake_server_reports_ready_and_plugs_the_bases() {
        let server = standard_server();
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut seen = 0;
        let status = pump(
            &mut startup,
            &mut connection,
            &mut dispatcher,
            &extension_wire(&server),
            &mut seen,
        );
        assert_eq!(status, Status::Pending);
        let version_bytes = connection.take_outgoing();
        let version_requests = requests(&version_bytes);
        assert_eq!(version_requests.len(), 4);
        assert_eq!(version_requests[0][0], 142);
        assert_eq!(u32_at(version_requests[0], 4), 0);
        assert_eq!(u32_at(version_requests[0], 8), 4);
        assert_eq!(version_requests[1][0], 143);
        assert_eq!(u32_at(version_requests[1], 4), 6);
        assert_eq!(u32_at(version_requests[1], 8), 0);
        assert_eq!(version_requests[2][0], 144);
        assert_eq!(u32_at(version_requests[2], 4), 0);
        assert_eq!(u32_at(version_requests[2], 8), 11);
        assert_eq!(version_requests[3][0], 145);
        assert_eq!(version_requests[3][4], 1);
        assert_eq!(version_requests[3][5], 2);

        let mut versions = Vec::new();
        for (index, server) in server.iter().enumerate() {
            versions.extend(version_reply(
                index,
                u16::try_from(index + 5).unwrap(),
                server.major,
                server.minor,
            ));
        }
        let status = pump(&mut startup, &mut connection, &mut dispatcher, &versions, &mut seen);
        assert_eq!(status, Status::Pending);
        let atom_bytes = connection.take_outgoing();
        let atom_requests = requests(&atom_bytes);
        assert_eq!(atom_requests.len(), crate::x11::atoms::NAMES.len());
        assert_eq!(atom_requests[0][1], 0);
        assert_eq!(query_name(atom_requests[0]), b"CLIPBOARD");

        let mut event = Vec::new();
        XFixesSelectionNotify {
            send_event: false,
            code: 85,
            sequence: 8,
            subtype: SelectionEvent::SetSelectionOwner,
            window: Window(1),
            owner: Window(0),
            selection: Atom(1),
            timestamp: 1,
            selection_timestamp: 1,
        }
        .serialize(&mut Writer::new(&mut event))
        .unwrap();
        let read = connection.on_read(&event, &mut dispatcher);
        assert!(matches!(read.events.as_slice(), [Event::XFixesSelectionNotify(_)]));

        let status =
            pump(&mut startup, &mut connection, &mut dispatcher, &intern_wire(9), &mut seen);
        let info = |server: &ServerExt| ExtensionInfo {
            major_opcode: server.major_opcode,
            first_event: server.first_event,
            first_error: server.first_error,
            major_version: server.major,
            minor_version: server.minor,
        };
        match status {
            Status::Ready(bringup) => {
                assert_eq!(
                    bringup.extensions,
                    Extensions {
                        composite: info(&server[0]),
                        xfixes: info(&server[1]),
                        render: info(&server[2]),
                        res: Some(info(&server[3])),
                    }
                );
                assert_eq!(bringup.atoms.CLIPBOARD, Atom(1_000));
                assert_eq!(bringup.atoms.PRIMARY, Atom::PRIMARY);
                assert_eq!(bringup.atoms.WM_NAME, Atom::WM_NAME);
            }
            other => panic!("expected ready, got {other:?}"),
        }

        let destroy = u16::try_from(9 + crate::x11::atoms::NAMES.len()).unwrap();
        connection
            .send_void(&DestroyWindow { window: Window(1) })
            .unwrap();
        connection.take_outgoing();
        let read = connection.on_read(&error_frame(destroy, 160), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(connection.logged_errors(), ["DestroyWindow BadRegion"]);
    }

    #[test]
    fn version_queries_wait_until_every_extension_has_answered() {
        let server = standard_server();
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut seen = 0;
        let mut wire = Vec::new();
        for (index, server) in server.iter().enumerate() {
            wire.extend(extension_reply(u16::try_from(index + 1).unwrap(), server));
            let status = pump(&mut startup, &mut connection, &mut dispatcher, &wire, &mut seen);
            wire.clear();
            if index < 3 {
                assert_eq!(status, Status::Pending);
                assert!(connection.take_outgoing().is_empty());
            } else {
                assert_eq!(status, Status::Pending);
                assert_eq!(requests(&connection.take_outgoing()).len(), 4);
            }
        }
    }

    #[test]
    fn extension_replies_are_matched_by_name() {
        let (mut connection, _, mut startup, _) = started();
        let order = [
            ExtensionName::Res,
            ExtensionName::Render,
            ExtensionName::XFixes,
            ExtensionName::Composite,
        ];
        let mut status = Status::Pending;
        for (index, name) in order.into_iter().enumerate() {
            status = startup.handle(
                StartupEvent::Extension {
                    name,
                    result: Ok(QueryExtensionReply {
                        sequence: 0,
                        present: 1,
                        major_opcode: 150 + u8::try_from(index).unwrap(),
                        first_event: 90,
                        first_error: 180,
                    }),
                },
                &mut connection,
            );
            if index < 3 {
                assert_eq!(status, Status::Pending);
                assert!(connection.take_outgoing().is_empty());
            }
        }
        assert_eq!(status, Status::Pending);
        let version_bytes = connection.take_outgoing();
        let version_requests = requests(&version_bytes);
        assert_eq!(version_requests.len(), 4);
        assert_eq!(version_requests[0][0], 153);
        assert_eq!(version_requests[3][0], 150);
    }

    #[test]
    fn a_missing_x_resource_is_ready_without_a_version_query() {
        let mut server = standard_server();
        server[3].present = 0;
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut seen = 0;
        pump(&mut startup, &mut connection, &mut dispatcher, &extension_wire(&server), &mut seen);
        let version_bytes = connection.take_outgoing();
        let version_requests = requests(&version_bytes);
        assert_eq!(version_requests.len(), 3);
        assert_eq!(
            version_requests
                .iter()
                .map(|request| request[0])
                .collect::<Vec<_>>(),
            vec![142, 143, 144]
        );
        let mut versions = Vec::new();
        for (index, server) in server.iter().take(3).enumerate() {
            versions.extend(version_reply(
                index,
                u16::try_from(index + 5).unwrap(),
                server.major,
                server.minor,
            ));
        }
        let status = pump(&mut startup, &mut connection, &mut dispatcher, &versions, &mut seen);
        assert_eq!(status, Status::Pending);
        assert_eq!(requests(&connection.take_outgoing()).len(), crate::x11::atoms::NAMES.len());
        let status =
            pump(&mut startup, &mut connection, &mut dispatcher, &intern_wire(8), &mut seen);
        match status {
            Status::Ready(bringup) => assert!(bringup.extensions.res.is_none()),
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_required_extension_fails() {
        let mut server = standard_server();
        server[0].present = 0;
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut seen = 0;
        let status = pump(
            &mut startup,
            &mut connection,
            &mut dispatcher,
            &extension_wire(&server),
            &mut seen,
        );
        assert_eq!(status, Status::Failed(StartupError::Missing(ExtensionName::Composite)));
        assert!(startup.status().failure_text().contains("Composite"));
        assert!(connection.take_outgoing().is_empty());
    }

    #[test]
    fn an_old_xfixes_fails_and_stays_failed() {
        let mut server = standard_server();
        server[1].major = 5;
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut seen = 0;
        pump(&mut startup, &mut connection, &mut dispatcher, &extension_wire(&server), &mut seen);
        connection.take_outgoing();
        let mut versions = Vec::new();
        for (index, server) in server.iter().enumerate() {
            versions.extend(version_reply(
                index,
                u16::try_from(index + 5).unwrap(),
                server.major,
                server.minor,
            ));
        }
        let status = pump(&mut startup, &mut connection, &mut dispatcher, &versions, &mut seen);
        assert_eq!(
            status,
            Status::Failed(StartupError::TooOld {
                extension: ExtensionName::XFixes,
                major: 5,
                minor: 1,
            })
        );
        let text = startup.status().failure_text();
        assert!(text.contains("5.1"));
        assert!(text.contains("6.0"));
    }

    #[test]
    fn a_query_error_fails_before_version_queries() {
        let server = standard_server();
        let (mut connection, mut dispatcher, mut startup, _) = started();
        let mut wire = extension_reply(1, &server[0]);
        wire.extend(error_frame(2, 2));
        wire.extend(extension_reply(3, &server[2]));
        wire.extend(extension_reply(4, &server[3]));
        let mut seen = 0;
        let status = pump(&mut startup, &mut connection, &mut dispatcher, &wire, &mut seen);
        assert_eq!(
            status,
            Status::Failed(StartupError::Reply {
                extension: ExtensionName::XFixes,
                error: XReplyError::Protocol(XError {
                    code: 2,
                    sequence: 2,
                    bad_value: 0,
                    minor_opcode: 0,
                    major_opcode: 0,
                }),
            })
        );
        assert!(connection.take_outgoing().is_empty());
    }

    #[test]
    fn a_dead_connection_fails_the_batch() {
        let (mut connection, mut dispatcher, mut startup, _) = started();
        assert_eq!(connection.disconnect(&mut dispatcher).dead, Some(ConnectionError::Dead));
        let mut seen = 0;
        let status = pump(&mut startup, &mut connection, &mut dispatcher, &[], &mut seen);
        assert_eq!(
            status,
            Status::Failed(StartupError::Reply {
                extension: ExtensionName::Composite,
                error: XReplyError::Dead,
            })
        );
    }

    trait FailureText {
        fn failure_text(&self) -> String;
    }

    impl FailureText for Status {
        fn failure_text(&self) -> String {
            match self {
                Status::Failed(error) => error.to_string(),
                Status::Pending | Status::Ready(_) => String::new(),
            }
        }
    }
}
