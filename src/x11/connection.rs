//! Sans-IO X connection.
//!
//! `XConnection::new` builds the setup request. `on_read` finishes the handshake,
//! then frames replies, errors, and events. Requests sit in a pending queue until
//! a later sequence retires them. `take_outgoing` appends a GetInputFocus when
//! void requests are the last thing written, so those voids can retire.

use std::{
    collections::VecDeque,
    error::Error,
    fmt::{self, Display, Formatter},
};

use crate::runner::{Dispatcher, Redispatcher};

use super::{
    generated::{
        error_name, Event, ExtensionError, ExtensionEvent, ExtensionName, GetInputFocus, Request,
        Setup, SetupAuthenticate, SetupFailed, SetupRequest, Window, XError, XReply,
    },
    wire::{Reader, WireError, Writer},
};

/// `b'l'` or `b'B'`, the byte order this process writes and expects.
const BYTE_ORDER: u8 = if cfg!(target_endian = "little") { b'l' } else { b'B' };

/// Where the handshake is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Handshake,
    Ready,
    Dead,
}

/// Why the connection stopped, or why an id could not be handed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionError {
    /// Status 0. `reason` is the server's reason bytes.
    SetupFailed { reason: Vec<u8> },
    /// Status 2. `reason` is the additional-data bytes.
    SetupAuthenticate { reason: Vec<u8> },
    /// The setup bytes do not form one of the three replies.
    Malformed,
    /// A successful setup listed no screen.
    NoScreen,
    /// Every id in `resource_id_mask` has been handed out.
    IdExhausted,
    /// `generate_id` or `send` was called before the setup succeeded.
    NotReady,
    /// The request is longer than `maximum_request_length` units of 4 bytes.
    RequestTooLong,
    /// The request's extension has no major opcode yet.
    ExtensionMissing(ExtensionName),
    /// The owner closed the connection, or a read or write failed.
    Dead,
}

impl Display for ConnectionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::SetupFailed { reason } => write!(
                formatter,
                "X server rejected the connection: {}",
                String::from_utf8_lossy(reason)
            ),
            Self::SetupAuthenticate { reason } => write!(
                formatter,
                "X server asked for authentication: {}",
                String::from_utf8_lossy(reason)
            ),
            Self::Malformed => formatter.write_str("X connection setup is malformed"),
            Self::NoScreen => formatter.write_str("X server sent no screen"),
            Self::IdExhausted => formatter.write_str("X resource ids are exhausted"),
            Self::NotReady => formatter.write_str("X connection is not ready"),
            Self::RequestTooLong => {
                formatter.write_str("X request is longer than the server accepts")
            }
            Self::ExtensionMissing(name) => {
                write!(formatter, "X extension {name:?} is not available")
            }
            Self::Dead => formatter.write_str("X connection is dead"),
        }
    }
}

/// Why a pending request did not produce its reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XReplyError {
    /// A later sequence arrived before this reply.
    MissingReply,
    /// The connection died while the request was pending.
    Dead,
    /// The server returned an X error for this sequence.
    Protocol(XError),
}

impl Error for ConnectionError {}

impl Display for XReplyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingReply => formatter.write_str("X server did not reply to the request"),
            Self::Dead => formatter.write_str("X connection is dead"),
            Self::Protocol(error) => write!(formatter, "X protocol error {}", error.code),
        }
    }
}

impl Error for XReplyError {}

/// What `on_read` delivered. `dead` is set on the transition into [`State::Dead`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) events: Vec<super::generated::Event>,
    pub(crate) dead: Option<ConnectionError>,
}

/// One extension's server-assigned opcodes. Phase 9 fills these in.
struct ExtensionSlot {
    name: ExtensionName,
    major_opcode: u8,
    first_event: u8,
    first_error: u8,
}

struct Pending {
    sequence: u64,
    name: &'static str,
    kind: PendingKind,
}

enum PendingKind {
    Reply(Box<dyn DeliverReply>),
    Void,
    Sync,
}

trait DeliverReply {
    fn ok(self: Box<Self>, dispatcher: &mut Dispatcher, bytes: &[u8]) -> Result<(), ()>;
    fn fail(self: Box<Self>, dispatcher: &mut Dispatcher, error: XReplyError);
}

struct ReplySlot<T> {
    redispatcher: Redispatcher<Result<T, XReplyError>>,
}

impl<T> DeliverReply for ReplySlot<T>
where
    T: XReply + 'static,
{
    fn ok(self: Box<Self>, dispatcher: &mut Dispatcher, bytes: &[u8]) -> Result<(), ()> {
        let mut reader = Reader::new(bytes);
        match T::parse_reply(&mut reader) {
            Ok(reply) if reader.remaining() == 0 => {
                dispatcher.redispatch_to(self.redispatcher, Ok(reply));
                Ok(())
            }
            _ => {
                dispatcher.redispatch_to(self.redispatcher, Err(XReplyError::Dead));
                Err(())
            }
        }
    }

    fn fail(self: Box<Self>, dispatcher: &mut Dispatcher, error: XReplyError) {
        dispatcher.redispatch_to(self.redispatcher, Err(error));
    }
}

/// The X connection core. The owner writes the setup bytes, then feeds reads here.
pub(crate) struct XConnection {
    state: State,
    incoming: Vec<u8>,
    outgoing: Vec<u8>,
    pending: VecDeque<Pending>,
    extensions: Vec<ExtensionSlot>,
    failure: Option<ConnectionError>,
    root: Window,
    root_visual: u32,
    maximum_request_length: u16,
    id_base: u32,
    id_mask: u32,
    id_step: u32,
    id_counter: u32,
    ids_exhausted: bool,
    /// Next request sequence. The first request is 1.
    next_sequence: u64,
    /// Widened sequence of the last message that carried one.
    last_sequence: u64,
    /// The last committed request was void, so a flush must append GetInputFocus.
    needs_sync: bool,
    #[cfg(test)]
    logged_errors: Vec<String>,
}

impl XConnection {
    /// The connection, plus the setup request to write before any read.
    ///
    /// Protocol 11.0, native byte order, empty authorization.
    pub(crate) fn new() -> (Self, Vec<u8>) {
        let mut bytes = Vec::with_capacity(12);
        SetupRequest {
            byte_order: BYTE_ORDER,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            authorization_protocol_name_len: 0,
            authorization_protocol_data_len: 0,
            authorization_protocol_name: Vec::new(),
            authorization_protocol_data: Vec::new(),
        }
        .serialize(&mut Writer::new(&mut bytes));
        (
            Self {
                state: State::Handshake,
                incoming: Vec::new(),
                outgoing: Vec::new(),
                pending: VecDeque::new(),
                extensions: Vec::new(),
                failure: None,
                root: Window(0),
                root_visual: 0,
                maximum_request_length: 0,
                id_base: 0,
                id_mask: 0,
                id_step: 0,
                id_counter: 0,
                ids_exhausted: false,
                next_sequence: 1,
                last_sequence: 0,
                needs_sync: false,
                #[cfg(test)]
                logged_errors: Vec::new(),
            },
            bytes,
        )
    }

    pub(crate) fn state(&self) -> State {
        self.state
    }

    pub(crate) fn root(&self) -> Option<Window> {
        (self.state == State::Ready).then_some(self.root)
    }

    pub(crate) fn root_visual(&self) -> Option<u32> {
        (self.state == State::Ready).then_some(self.root_visual)
    }

    pub(crate) fn maximum_request_length(&self) -> Option<u16> {
        (self.state == State::Ready).then_some(self.maximum_request_length)
    }

    /// Records an extension's major opcode and event and error bases.
    pub(crate) fn set_extension(
        &mut self,
        name: ExtensionName,
        major_opcode: u8,
        first_event: u8,
        first_error: u8,
    ) {
        if let Some(slot) = self.extensions.iter_mut().find(|slot| slot.name == name) {
            *slot = ExtensionSlot { name, major_opcode, first_event, first_error };
            return;
        }
        self.extensions
            .push(ExtensionSlot { name, major_opcode, first_event, first_error });
    }

    /// Sends a request that expects a reply. The sequence is 1 for the first request.
    pub(crate) fn send<R>(
        &mut self,
        request: &R,
        redispatcher: Redispatcher<Result<R::Reply, XReplyError>>,
    ) -> Result<u64, ConnectionError>
    where
        R: Request,
        R::Reply: XReply + 'static,
    {
        self.write_request(request, false, PendingKind::Reply(Box::new(ReplySlot { redispatcher })))
    }

    /// Sends a request with no reply. Its errors are logged with [`Request::NAME`].
    pub(crate) fn send_void<R>(&mut self, request: &R) -> Result<u64, ConnectionError>
    where
        R: Request<Reply = super::generated::NoReply>,
    {
        self.write_request(request, true, PendingKind::Void)
    }

    /// The sequence the next request will use.
    #[cfg(test)]
    pub(crate) fn test_next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Widens a 16-bit event sequence against the last request this connection sent.
    ///
    /// The high bits come from that request. A wire value above it belongs to the
    /// previous 16-bit block. An event is never newer than the last request sent.
    pub(crate) fn widen_event_sequence(&self, wire: u16) -> u64 {
        let sent = self.next_sequence.saturating_sub(1);
        let mut serial = (sent & !0xffff) | u64::from(wire);
        if serial > sent {
            serial = serial.saturating_sub(0x10000);
        }
        serial
    }

    /// Bytes waiting to be written. Appends GetInputFocus when void requests trail.
    pub(crate) fn take_outgoing(&mut self) -> Vec<u8> {
        if self.state == State::Ready && self.needs_sync {
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            self.pending.push_back(Pending {
                sequence,
                name: GetInputFocus::NAME,
                kind: PendingKind::Sync,
            });
            GetInputFocus
                .serialize(&mut Writer::new(&mut self.outgoing), 0)
                .expect("GetInputFocus is 4 bytes");
            self.needs_sync = false;
        }
        std::mem::take(&mut self.outgoing)
    }

    /// Appends bytes. A finished handshake is followed by replies, errors, and events.
    ///
    /// `dead` is reported only on the transition into [`State::Dead`].
    pub(crate) fn on_read(&mut self, bytes: &[u8], dispatcher: &mut Dispatcher) -> Read {
        if self.state == State::Dead {
            return Read { events: Vec::new(), dead: None };
        }
        self.incoming.extend_from_slice(bytes);
        if self.state == State::Handshake {
            if let Err(error) = self.try_handshake() {
                return self.die(dispatcher, error);
            }
        }
        if self.state != State::Ready {
            return Read { events: Vec::new(), dead: None };
        }
        let mut events = Vec::new();
        match self.pump(dispatcher, &mut events) {
            Ok(()) => Read { events, dead: None },
            Err(error) => {
                let mut read = self.die(dispatcher, error);
                read.events = events;
                read
            }
        }
    }

    /// Marks the connection dead, fails every pending reply once, and reports it once.
    pub(crate) fn disconnect(&mut self, dispatcher: &mut Dispatcher) -> Read {
        if self.state == State::Dead {
            return Read { events: Vec::new(), dead: None };
        }
        self.die(dispatcher, ConnectionError::Dead)
    }

    #[cfg(test)]
    pub(crate) fn logged_errors(&self) -> &[String] {
        &self.logged_errors
    }

    /// `resource_id_base | (counter & resource_id_mask)`.
    ///
    /// `counter` starts at 0 and advances by `1 << mask.trailing_zeros()`. The id
    /// with every mask bit set is handed out. The next call then fails.
    pub(crate) fn generate_id(&mut self) -> Result<u32, ConnectionError> {
        if self.state != State::Ready {
            return Err(self.failure.clone().unwrap_or(ConnectionError::NotReady));
        }
        if self.ids_exhausted {
            return Err(ConnectionError::IdExhausted);
        }
        let id = self.id_base | (self.id_counter & self.id_mask);
        match self.id_counter.checked_add(self.id_step) {
            Some(next) if self.id_step != 0 && next & !self.id_mask == 0 => {
                self.id_counter = next;
            }
            _ => self.ids_exhausted = true,
        }
        Ok(id)
    }

    fn try_handshake(&mut self) -> Result<(), ConnectionError> {
        if self.incoming.len() < 8 {
            return Ok(());
        }
        let extra = usize::from(u16::from_ne_bytes(self.incoming[6..8].try_into().unwrap())) * 4;
        let total = 8 + extra;
        if self.incoming.len() < total {
            return Ok(());
        }
        let rest = self.incoming.split_off(total);
        let frame = std::mem::replace(&mut self.incoming, rest);
        let mut reader = Reader::new(&frame);
        match frame[0] {
            0 => {
                let failed =
                    SetupFailed::parse(&mut reader).map_err(|_| ConnectionError::Malformed)?;
                Err(ConnectionError::SetupFailed { reason: failed.reason })
            }
            1 => {
                let setup = Setup::parse(&mut reader).map_err(map_wire)?;
                if reader.remaining() != 0 {
                    return Err(ConnectionError::Malformed);
                }
                self.accept(setup)
            }
            2 => {
                let auth = SetupAuthenticate::parse(&mut reader)
                    .map_err(|_| ConnectionError::Malformed)?;
                Err(ConnectionError::SetupAuthenticate { reason: auth.reason })
            }
            _ => Err(ConnectionError::Malformed),
        }
    }

    fn accept(&mut self, setup: Setup) -> Result<(), ConnectionError> {
        let screen = setup.roots.first().ok_or(ConnectionError::NoScreen)?;
        self.root = screen.root;
        self.root_visual = screen.root_visual;
        self.maximum_request_length = setup.maximum_request_length;
        self.id_base = setup.resource_id_base;
        self.id_mask = setup.resource_id_mask;
        self.id_step = match setup.resource_id_mask.trailing_zeros() {
            32 => 0,
            shift => 1 << shift,
        };
        self.id_counter = 0;
        self.ids_exhausted = false;
        self.state = State::Ready;
        Ok(())
    }

    fn write_request<R: Request>(
        &mut self,
        request: &R,
        void: bool,
        kind: PendingKind,
    ) -> Result<u64, ConnectionError> {
        if self.state != State::Ready {
            return Err(self.failure.clone().unwrap_or(ConnectionError::NotReady));
        }
        let major = self.major_opcode::<R>()?;
        let mut bytes = Vec::new();
        request
            .serialize(&mut Writer::new(&mut bytes), major)
            .map_err(|_| ConnectionError::RequestTooLong)?;
        let max_bytes = usize::from(self.maximum_request_length) * 4;
        if bytes.len() > max_bytes {
            return Err(ConnectionError::RequestTooLong);
        }
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.pending
            .push_back(Pending { sequence, name: R::NAME, kind });
        self.outgoing.extend_from_slice(&bytes);
        self.needs_sync = void;
        Ok(sequence)
    }

    fn major_opcode<R: Request>(&self) -> Result<u8, ConnectionError> {
        let Some(name) = R::EXTENSION else {
            return Ok(0);
        };
        self.extensions
            .iter()
            .find(|slot| slot.name == name)
            .map(|slot| slot.major_opcode)
            .ok_or(ConnectionError::ExtensionMissing(name))
    }

    fn pump(
        &mut self,
        dispatcher: &mut Dispatcher,
        events: &mut Vec<Event>,
    ) -> Result<(), ConnectionError> {
        while self.incoming.len() >= 32 {
            let total = frame_len(&self.incoming);
            if self.incoming.len() < total {
                break;
            }
            let rest = self.incoming.split_off(total);
            let frame = std::mem::replace(&mut self.incoming, rest);
            self.handle_frame(&frame, dispatcher, events)?;
        }
        Ok(())
    }

    fn handle_frame(
        &mut self,
        frame: &[u8],
        dispatcher: &mut Dispatcher,
        events: &mut Vec<Event>,
    ) -> Result<(), ConnectionError> {
        let code = frame[0] & 0x7f;
        let serial = (code != KEYMAP_NOTIFY).then(|| self.advance_sequence(frame, dispatcher));
        match code {
            0 => self.handle_error(frame, serial.unwrap(), dispatcher),
            1 => self.handle_reply(frame, serial.unwrap(), dispatcher),
            _ => {
                let bases = self.event_bases();
                let event = Event::parse(frame, &bases).map_err(|_| ConnectionError::Malformed)?;
                events.push(event);
                Ok(())
            }
        }
    }

    fn handle_error(
        &mut self,
        frame: &[u8],
        serial: u64,
        dispatcher: &mut Dispatcher,
    ) -> Result<(), ConnectionError> {
        let mut reader = Reader::new(frame);
        let error = XError::parse(&mut reader).map_err(|_| ConnectionError::Malformed)?;
        if reader.remaining() != 0 {
            return Err(ConnectionError::Malformed);
        }
        match self.take_exact(serial) {
            Some(Pending { kind: PendingKind::Reply(slot), .. }) => {
                slot.fail(dispatcher, XReplyError::Protocol(error));
            }
            Some(pending) => self.note_unawaited(pending.name, &error),
            None => self.note_unawaited("unmatched", &error),
        }
        Ok(())
    }

    fn handle_reply(
        &mut self,
        frame: &[u8],
        serial: u64,
        dispatcher: &mut Dispatcher,
    ) -> Result<(), ConnectionError> {
        match self.take_exact(serial) {
            Some(Pending { kind: PendingKind::Reply(slot), .. }) => {
                if slot.ok(dispatcher, frame).is_err() {
                    return Err(ConnectionError::Malformed);
                }
            }
            Some(Pending { kind: PendingKind::Sync, .. }) => {}
            Some(Pending { name, kind: PendingKind::Void, .. }) => {
                log::error!("X request {name} received a reply");
            }
            None => {}
        }
        Ok(())
    }

    fn advance_sequence(&mut self, frame: &[u8], dispatcher: &mut Dispatcher) -> u64 {
        let wire = u16::from_ne_bytes([frame[2], frame[3]]);
        let mut serial = (self.last_sequence & !0xffff) | u64::from(wire);
        if serial < self.last_sequence {
            serial += 0x10000;
        }
        self.last_sequence = serial;
        while self
            .pending
            .front()
            .is_some_and(|pending| pending.sequence < serial)
        {
            let pending = self.pending.pop_front().unwrap();
            if let PendingKind::Reply(slot) = pending.kind {
                slot.fail(dispatcher, XReplyError::MissingReply);
            }
        }
        serial
    }

    fn take_exact(&mut self, serial: u64) -> Option<Pending> {
        if self
            .pending
            .front()
            .is_some_and(|pending| pending.sequence == serial)
        {
            self.pending.pop_front()
        } else {
            None
        }
    }

    fn event_bases(&self) -> Vec<ExtensionEvent> {
        self.extensions
            .iter()
            .filter(|slot| slot.first_event != 0)
            .map(|slot| ExtensionEvent { extension: slot.name, first_event: slot.first_event })
            .collect()
    }

    fn error_bases(&self) -> Vec<ExtensionError> {
        self.extensions
            .iter()
            .filter(|slot| slot.first_error != 0)
            .map(|slot| ExtensionError { extension: slot.name, first_error: slot.first_error })
            .collect()
    }

    fn note_unawaited(&mut self, name: &str, error: &XError) {
        let bases = self.error_bases();
        let label = error_name(error.code, &bases).unwrap_or("unknown");
        log::error!("X request {name} failed with {label}");
        #[cfg(test)]
        self.logged_errors.push(format!("{name} {label}"));
    }

    fn die(&mut self, dispatcher: &mut Dispatcher, error: ConnectionError) -> Read {
        self.failure = Some(error.clone());
        self.state = State::Dead;
        self.incoming.clear();
        self.outgoing.clear();
        self.needs_sync = false;
        while let Some(pending) = self.pending.pop_front() {
            if let PendingKind::Reply(slot) = pending.kind {
                slot.fail(dispatcher, XReplyError::Dead);
            }
        }
        Read { events: Vec::new(), dead: Some(error) }
    }
}

const KEYMAP_NOTIFY: u8 = 11;

fn frame_len(buf: &[u8]) -> usize {
    let code = buf[0] & 0x7f;
    if code == 1 || code == 35 {
        let units = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
        32 + units as usize * 4
    } else {
        32
    }
}

fn map_wire(error: WireError) -> ConnectionError {
    match error {
        WireError::Short | WireError::RequestTooLong => ConnectionError::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{ConnectionError, Read, State, XConnection, XReplyError},
        crate::{
            runner::{AnyAction, Dispatcher, Redispatcher},
            x11::{
                generated::{
                    Atom, BackingStore, Colormap, CompositeRedirectSubwindows, DestroyWindow,
                    EventMask, ExtensionName, GetInputFocus, InternAtom, InternAtomReply, Redirect,
                    Request, Screen, Setup, SetupAuthenticate, SetupFailed, Window, XError,
                },
                wire::Writer,
            },
        },
    };

    fn screen(root: u32, visual: u32) -> Screen {
        Screen {
            root: Window(root),
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
            root_visual: visual,
            backing_stores: BackingStore::NotUseful,
            save_unders: 0,
            root_depth: 24,
            allowed_depths_len: 0,
            allowed_depths: Vec::new(),
        }
    }

    fn setup(base: u32, mask: u32, maximum_request_length: u16, roots: Vec<Screen>) -> Setup {
        Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: base,
            resource_id_mask: mask,
            motion_buffer_size: 0,
            vendor_len: 0,
            maximum_request_length,
            roots_len: roots.len() as u8,
            pixmap_formats_len: 0,
            image_byte_order: crate::x11::generated::ImageOrder::LSBFirst,
            bitmap_format_bit_order: crate::x11::generated::ImageOrder::LSBFirst,
            bitmap_format_scanline_unit: 32,
            bitmap_format_scanline_pad: 32,
            min_keycode: 8,
            max_keycode: 255,
            vendor: Vec::new(),
            pixmap_formats: Vec::new(),
            roots,
        }
    }

    /// Serializes `setup` and patches `length` to the additional data that was written.
    fn setup_bytes(setup: Setup) -> Vec<u8> {
        let mut draft = setup;
        draft.length = 0;
        let mut bytes = Vec::new();
        draft.serialize(&mut Writer::new(&mut bytes));
        let extra = bytes.len() - 8;
        assert_eq!(extra % 4, 0);
        draft.length = u16::try_from(extra / 4).unwrap();
        bytes.clear();
        draft.serialize(&mut Writer::new(&mut bytes));
        bytes
    }

    fn feed_split(connection: &mut XConnection, bytes: &[u8]) -> Read {
        let mut dispatcher = Dispatcher::default();
        let mut last = Read { events: Vec::new(), dead: None };
        for (index, byte) in bytes.iter().enumerate() {
            last = connection.on_read(&[*byte], &mut dispatcher);
            if index + 1 != bytes.len() {
                assert_eq!(connection.state(), State::Handshake);
                assert!(last.events.is_empty());
                assert!(last.dead.is_none());
            }
        }
        last
    }

    #[test]
    fn new_sends_protocol_11_without_authorization() {
        let (mut connection, bytes) = XConnection::new();
        assert_eq!(connection.state(), State::Handshake);
        assert!(connection.root().is_none());
        let mut expected = vec![if cfg!(target_endian = "little") { b'l' } else { b'B' }, 0];
        expected.extend(11u16.to_ne_bytes());
        expected.extend(0u16.to_ne_bytes());
        expected.extend(0u16.to_ne_bytes());
        expected.extend(0u16.to_ne_bytes());
        expected.extend_from_slice(&[0, 0]);
        assert_eq!(bytes, expected);
        assert_eq!(connection.generate_id(), Err(ConnectionError::NotReady));
    }

    #[test]
    fn setup_round_trip_keeps_the_first_screen() {
        let bytes =
            setup_bytes(setup(0x20, 0b1100, 256, vec![screen(0x100, 0x21), screen(0x200, 0x22)]));
        let (mut connection, _) = XConnection::new();
        let read = feed_split(&mut connection, &bytes);
        assert!(read.events.is_empty());
        assert!(read.dead.is_none());
        assert_eq!(connection.state(), State::Ready);
        assert_eq!(connection.root(), Some(Window(0x100)));
        assert_eq!(connection.root_visual(), Some(0x21));
        assert_eq!(connection.maximum_request_length(), Some(256));

        let mut trailed = bytes.clone();
        trailed.push(0xab);
        let (mut connection, _) = XConnection::new();
        let read = connection.on_read(&trailed, &mut Dispatcher::default());
        assert!(read.dead.is_none());
        assert_eq!(connection.state(), State::Ready);
        assert_eq!(
            connection
                .on_read(&[1, 2], &mut Dispatcher::default())
                .events
                .len(),
            0
        );
    }

    #[test]
    fn setup_failure_reports_the_reason_once() {
        let mut bytes = Vec::new();
        SetupFailed {
            status: 0,
            reason_len: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 1,
            reason: b"x".to_vec(),
        }
        .serialize(&mut Writer::new(&mut bytes));
        bytes.extend_from_slice(&[0, 0, 0]);

        let (mut connection, _) = XConnection::new();
        let read = feed_split(&mut connection, &bytes);
        assert_eq!(read.dead, Some(ConnectionError::SetupFailed { reason: b"x".to_vec() }));
        assert!(read.dead.unwrap().to_string().contains('x'));
        assert_eq!(connection.state(), State::Dead);
        assert_eq!(connection.on_read(&[0], &mut Dispatcher::default()).dead, None);
        assert_eq!(connection.state(), State::Dead);
        assert_eq!(
            connection.generate_id(),
            Err(ConnectionError::SetupFailed { reason: b"x".to_vec() })
        );
    }

    #[test]
    fn setup_authenticate_is_dead() {
        let mut bytes = Vec::new();
        SetupAuthenticate { status: 2, length: 1, reason: b"auth".to_vec() }
            .serialize(&mut Writer::new(&mut bytes));
        let (mut connection, _) = XConnection::new();
        let read = connection.on_read(&bytes, &mut Dispatcher::default());
        assert_eq!(
            read.dead,
            Some(ConnectionError::SetupAuthenticate { reason: b"auth".to_vec() })
        );
        assert_eq!(connection.state(), State::Dead);
    }

    #[test]
    fn a_short_or_unknown_setup_is_malformed() {
        let (mut connection, _) = XConnection::new();
        let mut bytes = vec![1, 0];
        bytes.extend(11u16.to_ne_bytes());
        bytes.extend(0u16.to_ne_bytes());
        bytes.extend(0u16.to_ne_bytes());
        assert_eq!(
            connection.on_read(&bytes, &mut Dispatcher::default()).dead,
            Some(ConnectionError::Malformed)
        );

        let (mut connection, _) = XConnection::new();
        let mut bytes = vec![3, 0, 0, 0, 0, 0];
        bytes.extend(0u16.to_ne_bytes());
        assert_eq!(
            connection.on_read(&bytes, &mut Dispatcher::default()).dead,
            Some(ConnectionError::Malformed)
        );
    }

    #[test]
    fn extra_setup_bytes_and_no_screen_are_rejected() {
        let mut bytes = setup_bytes(setup(0x20, 1, 16, vec![screen(1, 2)]));
        let length = u16::from_ne_bytes(bytes[6..8].try_into().unwrap());
        bytes[6..8].copy_from_slice(&(length + 1).to_ne_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let (mut connection, _) = XConnection::new();
        assert_eq!(
            connection.on_read(&bytes, &mut Dispatcher::default()).dead,
            Some(ConnectionError::Malformed)
        );

        let bytes = setup_bytes(setup(0x20, 1, 16, Vec::new()));
        let (mut connection, _) = XConnection::new();
        assert_eq!(
            connection.on_read(&bytes, &mut Dispatcher::default()).dead,
            Some(ConnectionError::NoScreen)
        );
    }

    #[test]
    fn generate_id_walks_the_mask_and_then_stops() {
        let bytes = setup_bytes(setup(0x20, 0b1100, 64, vec![screen(1, 2)]));
        let (mut connection, _) = XConnection::new();
        connection.on_read(&bytes, &mut Dispatcher::default());
        assert_eq!(connection.generate_id(), Ok(0x20));
        assert_eq!(connection.generate_id(), Ok(0x24));
        assert_eq!(connection.generate_id(), Ok(0x28));
        assert_eq!(connection.generate_id(), Ok(0x2c));
        assert_eq!(connection.generate_id(), Err(ConnectionError::IdExhausted));
        assert_eq!(connection.generate_id(), Err(ConnectionError::IdExhausted));
        assert_eq!(connection.state(), State::Ready);
    }

    fn ready(maximum_request_length: u16) -> (XConnection, Dispatcher) {
        let bytes = setup_bytes(setup(0x20, 0x1f, maximum_request_length, vec![screen(1, 2)]));
        let (mut connection, _) = XConnection::new();
        let mut dispatcher = Dispatcher::default();
        let read = connection.on_read(&bytes, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(connection.state(), State::Ready);
        (connection, dispatcher)
    }

    fn replies<T: Clone + 'static>(dispatcher: &Dispatcher) -> Vec<T> {
        let mut found = Vec::new();
        dispatcher.test_for_each_queued_action(|reply: &T| found.push(reply.clone()));
        found
    }

    fn intern(name: &[u8]) -> InternAtom<'_> {
        InternAtom { only_if_exists: 0, name }
    }

    fn atom_reply(sequence: u16, atom: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        InternAtomReply { sequence, atom: Atom(atom) }
            .serialize(&mut Writer::new(&mut bytes))
            .unwrap();
        bytes
    }

    fn error_frame(code: u8, sequence: u16) -> Vec<u8> {
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

    #[test]
    fn replies_in_one_read_are_delivered_in_order() {
        let (mut connection, mut dispatcher) = ready(65535);
        let slot = Redispatcher::to_any(AnyAction::from);
        assert_eq!(connection.send(&intern(b"A"), slot).unwrap(), 1);
        let slot = Redispatcher::to_any(AnyAction::from);
        assert_eq!(connection.send(&intern(b"B"), slot).unwrap(), 2);

        let mut burst = atom_reply(1, 10);
        let head = burst.len() / 2;
        let read = connection.on_read(&burst[..head], &mut dispatcher);
        assert!(read.events.is_empty());
        assert!(replies::<Result<InternAtomReply, XReplyError>>(&dispatcher).is_empty());

        burst.extend(atom_reply(2, 20));
        let read = connection.on_read(&burst[head..], &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![
                Ok(InternAtomReply { sequence: 1, atom: Atom(10) }),
                Ok(InternAtomReply { sequence: 2, atom: Atom(20) }),
            ]
        );
    }

    #[test]
    fn a_skipped_reply_fails_with_missing_reply() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        connection
            .send(&intern(b"B"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let read = connection.on_read(&atom_reply(2, 5), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![
                Err(XReplyError::MissingReply),
                Ok(InternAtomReply { sequence: 2, atom: Atom(5) }),
            ]
        );
    }

    #[test]
    fn an_error_is_delivered_to_the_waiting_request() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let read = connection.on_read(&error_frame(3, 1), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![Err(XReplyError::Protocol(XError {
                code: 3,
                sequence: 1,
                bad_value: 0,
                minor_opcode: 0,
                major_opcode: 0,
            }))]
        );
    }

    #[test]
    fn a_void_error_is_logged_with_the_request_name() {
        let (mut connection, mut dispatcher) = ready(65535);
        assert_eq!(
            connection
                .send_void(&DestroyWindow { window: Window(1) })
                .unwrap(),
            1
        );
        let read = connection.on_read(&error_frame(3, 1), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(connection.state(), State::Ready);
        assert_eq!(connection.logged_errors(), &["DestroyWindow Window".to_string()]);
        assert!(replies::<Result<InternAtomReply, XReplyError>>(&dispatcher).is_empty());
    }

    #[test]
    fn take_outgoing_appends_get_input_focus_after_void_requests() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send_void(&DestroyWindow { window: Window(1) })
            .unwrap();
        let bytes = connection.take_outgoing();
        let mut expected = vec![DestroyWindow::OPCODE, 0];
        expected.extend(2u16.to_ne_bytes());
        expected.extend(1u32.to_ne_bytes());
        expected.push(GetInputFocus::OPCODE);
        expected.push(0);
        expected.extend(1u16.to_ne_bytes());
        assert_eq!(bytes, expected);
        assert!(connection.take_outgoing().is_empty());

        let mut reply = Vec::new();
        crate::x11::generated::GetInputFocusReply {
            sequence: 2,
            revert_to: crate::x11::generated::InputFocus::None,
            focus: Window(0),
        }
        .serialize(&mut Writer::new(&mut reply))
        .unwrap();
        let read = connection.on_read(&reply, &mut dispatcher);
        assert!(read.dead.is_none());
        assert!(connection.logged_errors().is_empty());

        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let mut alone = Vec::new();
        intern(b"A")
            .serialize(&mut Writer::new(&mut alone), 0)
            .unwrap();
        assert_eq!(connection.take_outgoing(), alone);
    }

    #[test]
    fn event_sequence_widens_below_and_at_the_last_request() {
        let (mut connection, _) = ready(65535);
        for _ in 0..100 {
            connection
                .send_void(&DestroyWindow { window: Window(1) })
                .unwrap();
        }
        assert_eq!(connection.widen_event_sequence(50), 50);
        assert_eq!(connection.widen_event_sequence(100), 100);
    }

    #[test]
    fn event_sequence_widens_across_the_16_bit_boundary() {
        let (mut connection, _) = ready(65535);
        for _ in 0..65540 {
            connection
                .send_void(&DestroyWindow { window: Window(1) })
                .unwrap();
        }
        assert_eq!(connection.widen_event_sequence(0), 65536);
        assert_eq!(connection.widen_event_sequence(4), 65540);
        assert_eq!(connection.widen_event_sequence(100), 100);
    }

    #[test]
    fn sequence_numbers_widen_past_16_bits() {
        let (mut connection, mut dispatcher) = ready(65535);
        for _ in 0..65535 {
            connection
                .send_void(&DestroyWindow { window: Window(1) })
                .unwrap();
        }
        let sequence = connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        assert_eq!(sequence, 65536);

        let mut event = vec![50, 0];
        event.extend(65535u16.to_ne_bytes());
        event.resize(32, 0);
        let read = connection.on_read(&event, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(read.events.len(), 1);
        assert!(replies::<Result<InternAtomReply, XReplyError>>(&dispatcher).is_empty());

        let read = connection.on_read(&atom_reply(0, 7), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![Ok(InternAtomReply { sequence: 0, atom: Atom(7) })]
        );
    }

    #[test]
    fn keymap_notify_does_not_advance_the_sequence() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let mut keymap = vec![11, 0];
        keymap.extend(65535u16.to_ne_bytes());
        keymap.resize(32, 0);
        connection.on_read(&keymap, &mut dispatcher);
        let read = connection.on_read(&atom_reply(1, 4), &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![Ok(InternAtomReply { sequence: 1, atom: Atom(4) })]
        );
    }

    #[test]
    fn disconnect_fails_pending_replies_once() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let read = connection.disconnect(&mut dispatcher);
        assert_eq!(read.dead, Some(ConnectionError::Dead));
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![Err::<InternAtomReply, _>(XReplyError::Dead)]
        );
        assert!(connection.disconnect(&mut dispatcher).dead.is_none());
        assert_eq!(replies::<Result<InternAtomReply, XReplyError>>(&dispatcher).len(), 1);
        assert_eq!(
            connection.send_void(&DestroyWindow { window: Window(1) }),
            Err(ConnectionError::Dead)
        );
    }

    #[test]
    fn a_request_past_the_maximum_length_is_not_sent() {
        let (mut connection, _) = ready(1);
        assert_eq!(
            connection.send_void(&DestroyWindow { window: Window(1) }),
            Err(ConnectionError::RequestTooLong)
        );
        assert!(connection.take_outgoing().is_empty());
        assert_eq!(
            connection
                .send(&GetInputFocus, Redispatcher::to_any(AnyAction::from))
                .unwrap(),
            1
        );
    }

    #[test]
    fn an_extension_request_needs_its_major_opcode() {
        let (mut connection, _) = ready(65535);
        let request = CompositeRedirectSubwindows { window: Window(1), update: Redirect::Manual };
        assert_eq!(
            connection.send_void(&request),
            Err(ConnectionError::ExtensionMissing(ExtensionName::Composite))
        );
        connection.set_extension(ExtensionName::Composite, 142, 0, 0);
        assert_eq!(connection.send_void(&request).unwrap(), 1);
        let bytes = connection.take_outgoing();
        assert_eq!(bytes[0], 142);
        assert_eq!(bytes[1], CompositeRedirectSubwindows::OPCODE);
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn a_reply_longer_than_its_fields_kills_the_connection() {
        let (mut connection, mut dispatcher) = ready(65535);
        connection
            .send(&intern(b"A"), Redispatcher::to_any(AnyAction::from))
            .unwrap();
        let mut bytes = atom_reply(1, 1);
        let length = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
        bytes[4..8].copy_from_slice(&(length + 1).to_ne_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let read = connection.on_read(&bytes, &mut dispatcher);
        assert_eq!(read.dead, Some(ConnectionError::Malformed));
        assert_eq!(
            replies::<Result<InternAtomReply, XReplyError>>(&dispatcher),
            vec![Err::<InternAtomReply, _>(XReplyError::Dead)]
        );
        assert_eq!(connection.state(), State::Dead);
    }
}
