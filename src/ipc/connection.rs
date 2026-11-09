//! One IPC client.
//!
//! Reads are `ReadSome` into a 4 KiB buffer, the same stream read X11 uses.
//! Wayland clients use `Recvmsg` because those sockets carry file descriptors;
//! this one does not. `Write` is the matching stream write: the ring retries a
//! short write before the completion, and [`IpcConnection::note_written`] still
//! drops only the prefix a completion reports.
//!
//! A line that is not UTF-8 closes the connection. Those bytes are not a
//! request, so there is no error reply to send.

use std::{
    collections::{BTreeSet, VecDeque},
    io::ErrorKind,
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
};

use crate::{
    io_uring::{
        model::IoUringAction,
        ring::{ReadWriteCompleted, RingId},
    },
    runner::{Dispatcher, Model, Redispatcher},
    state::State,
};

use super::{
    proto::{self, Reply, Request, Response, Version, PROTOCOL_VERSION},
    server::IpcServerAction,
    snapshot,
};

/// Bytes requested by each `ReadSome`.
const READ_LEN: usize = 4 * 1024;

/// Pending outgoing bytes, including the write that is in flight.
const MAX_QUEUED: usize = 1024 * 1024;

model_instance_id!(pub(crate) IpcConnectionId => IpcConnection);

pub(crate) struct IpcConnection {
    id: IpcConnectionId,
    fd: OwnedFd,
    read_buf: Box<[u8]>,
    lines: proto::LineBuffer,
    out_queue: VecDeque<u8>,
    write_in_flight: bool,
    /// Prefix of `out_queue` submitted to the current `Write`.
    inflight_len: usize,
    read_op_id: Option<RingId>,
    write_op_id: Option<RingId>,
    closing: bool,
    /// `None` until subscribe. `Some` empty set means every topic.
    subscribed: Option<BTreeSet<proto::Topic>>,
}

pub(crate) enum IpcConnectionAction {
    Start,
    Read {
        buffer: Box<[u8]>,
    },
    ReadFailed {
        error: std::io::Error,
    },
    Written {
        written: usize,
    },
    WriteFailed {
        error: std::io::Error,
    },
    /// One encoded event line, shared by every subscriber of that topic.
    Send(Rc<[u8]>),
    Stop,
}

impl IpcConnection {
    pub(crate) fn new(id: IpcConnectionId, fd: OwnedFd) -> Self {
        Self {
            id,
            fd,
            read_buf: vec![0; READ_LEN].into_boxed_slice(),
            lines: proto::LineBuffer::default(),
            out_queue: VecDeque::new(),
            write_in_flight: false,
            inflight_len: 0,
            read_op_id: None,
            write_op_id: None,
            closing: false,
            subscribed: None,
        }
    }

    fn arm_read(&mut self, dispatcher: &mut Dispatcher) {
        if self.closing || self.read_op_id.is_some() {
            return;
        }
        let read_op_id = RingId::fresh();
        self.read_op_id = Some(read_op_id);
        let id = self.id;
        dispatcher.dispatch(IoUringAction::ReadSome {
            id: Some(read_op_id),
            fd: self.fd.as_raw_fd(),
            offset: 0,
            length: self.read_buf.len(),
            dispatch_to: Redispatcher::to_instance(id, |result| match result {
                Ok(ReadWriteCompleted { buffer, .. }) => IpcConnectionAction::Read { buffer },
                Err(error) => IpcConnectionAction::ReadFailed { error },
            }),
        });
    }

    fn pump_write(&mut self, dispatcher: &mut Dispatcher) {
        let Some(buffer) = self.take_write() else {
            return;
        };
        let write_op_id = RingId::fresh();
        self.write_op_id = Some(write_op_id);
        let id = self.id;
        dispatcher.dispatch(IoUringAction::Write {
            id: Some(write_op_id),
            fd: self.fd.as_raw_fd(),
            offset: 0,
            buffer,
            dispatch_to: Redispatcher::to_instance(id, |result| match result {
                Ok(ReadWriteCompleted { buffer, .. }) => {
                    IpcConnectionAction::Written { written: buffer.len() }
                }
                Err(error) => IpcConnectionAction::WriteFailed { error },
            }),
        });
    }

    /// Copies the queued bytes into the one in-flight write. Bytes appended
    /// while that write is outstanding stay behind the copied prefix.
    fn take_write(&mut self) -> Option<Box<[u8]>> {
        if self.closing || self.write_in_flight || self.out_queue.is_empty() {
            return None;
        }
        self.write_in_flight = true;
        self.inflight_len = self.out_queue.len();
        Some(self.out_queue.make_contiguous().to_vec().into_boxed_slice())
    }

    /// Drops the first `written` bytes of the in-flight prefix. A short write
    /// leaves the rest at the front of the queue.
    fn note_written(&mut self, written: usize) {
        if !self.write_in_flight {
            return;
        }
        let written = written.min(self.inflight_len);
        self.out_queue.drain(..written);
        self.inflight_len = 0;
        self.write_in_flight = false;
    }

    /// Appends `bytes` when they fit in [`MAX_QUEUED`]. `false` means the
    /// caller closes the connection.
    fn enqueue(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > MAX_QUEUED || self.out_queue.len() > MAX_QUEUED - bytes.len() {
            return false;
        }
        self.out_queue.extend(bytes);
        true
    }

    /// Pushes stream bytes, answers each finished line, and reports whether
    /// the connection should stay open.
    fn ingest(&mut self, state: &State, bytes: &[u8], dispatcher: &mut Dispatcher) -> bool {
        self.lines.push(bytes);
        loop {
            match self.lines.next_line() {
                Ok(None) => return true,
                Ok(Some(line)) => {
                    if !self.answer(state, &line, dispatcher) {
                        return false;
                    }
                }
                Err(error) => {
                    log::warn!("ipc connection {:?} closed: {error}", self.id);
                    return false;
                }
            }
        }
    }

    fn answer(&mut self, state: &State, line: &str, dispatcher: &mut Dispatcher) -> bool {
        if self.subscribed.is_some() {
            return self.enqueue_or_close(&proto::encode_line(&Reply::Error(
                "connection is subscribed".to_string(),
            )));
        }
        if let Ok(Request::Subscribe { events }) = serde_json::from_str::<Request>(line) {
            return self.subscribe(state, &events, dispatcher);
        }
        self.enqueue_or_close(&handle_line(state, line))
    }

    /// Empty `events` subscribes to every topic. The reply is `subscribed`,
    /// then one event per topic in request order. The all-topic order is
    /// focused window, workspaces, outputs.
    fn subscribe(
        &mut self,
        state: &State,
        events: &[proto::Topic],
        dispatcher: &mut Dispatcher,
    ) -> bool {
        let topics: BTreeSet<_> = events.iter().copied().collect();
        let snapshot = snapshot::build(state);
        let mut bytes = proto::encode_line(&Reply::Ok(Response::Subscribed));
        for topic in initial_topics(events) {
            let Some(event) = snapshot::topic(&snapshot, topic) else {
                continue;
            };
            bytes.extend(proto::encode_line(&event));
        }
        if !self.enqueue_or_close(&bytes) {
            return false;
        }
        self.subscribed = Some(topics.clone());
        dispatcher.dispatch(IpcServerAction::Subscribed { id: self.id, topics });
        true
    }

    fn enqueue_or_close(&mut self, bytes: &[u8]) -> bool {
        if self.enqueue(bytes) {
            return true;
        }
        log::warn!(
            "ipc connection {:?} outgoing queue exceeded {MAX_QUEUED} bytes, closing",
            self.id
        );
        false
    }

    /// Bytes waiting to be written. Drops an in-flight write marker so a test
    /// can read the queue without a ring completion.
    #[cfg(test)]
    pub(crate) fn take_queued(&mut self) -> Vec<u8> {
        self.write_in_flight = false;
        self.inflight_len = 0;
        self.out_queue.drain(..).collect()
    }

    fn close(&mut self, dispatcher: &mut Dispatcher) {
        let notify = !self.closing;
        self.halt(dispatcher);
        if notify {
            dispatcher.dispatch(IpcServerAction::ConnectionClosed { id: self.id });
        }
    }

    fn halt(&mut self, dispatcher: &mut Dispatcher) {
        self.closing = true;
        self.out_queue.clear();
        self.write_in_flight = false;
        self.inflight_len = 0;
        if let Some(read_op_id) = self.read_op_id.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: read_op_id });
        }
        if let Some(write_op_id) = self.write_op_id.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: write_op_id });
        }
    }
}

/// One request line, encoded as the reply line. A parse failure stays on the
/// connection; [`proto::LineError::TooLong`] and [`proto::LineError::InvalidUtf8`]
/// never reach this function.
pub(crate) fn handle_line(state: &State, line: &str) -> Vec<u8> {
    let reply = match serde_json::from_str::<Request>(line) {
        Ok(Request::Version) => Reply::Ok(Response::Version(Version {
            compositor: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL_VERSION,
        })),
        Ok(Request::Outputs) => Reply::Ok(Response::Outputs(snapshot::build(state).outputs)),
        Ok(Request::Workspaces) => {
            Reply::Ok(Response::Workspaces(snapshot::build(state).workspaces))
        }
        Ok(Request::FocusedWindow) => {
            Reply::Ok(Response::FocusedWindow(snapshot::build(state).focused_window))
        }
        Ok(Request::Subscribe { .. }) => Reply::Ok(Response::Subscribed),
        Err(_) => Reply::Error(request_error(line)),
    };
    proto::encode_line(&reply)
}

/// Topics named in the request, in that order, without duplicates.
/// An empty request is every topic.
fn initial_topics(events: &[proto::Topic]) -> Vec<proto::Topic> {
    if events.is_empty() {
        return vec![proto::Topic::FocusedWindow, proto::Topic::Workspaces, proto::Topic::Outputs];
    }
    let mut seen = BTreeSet::new();
    events
        .iter()
        .copied()
        .filter(|topic| seen.insert(*topic))
        .collect()
}

fn request_error(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return "malformed request".to_string();
    };
    let Some(name) = value.get("request").and_then(|request| request.as_str()) else {
        return "malformed request".to_string();
    };
    match name {
        "version" | "outputs" | "workspaces" | "focused_window" | "subscribe" => {
            "malformed request".to_string()
        }
        other => format!("unknown request `{other}`"),
    }
}

impl Model for IpcConnection {
    type Action = IpcConnectionAction;

    const START: Option<Self::Action> = Some(IpcConnectionAction::Start);
    const STOP: Option<Self::Action> = Some(IpcConnectionAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        match action {
            IpcConnectionAction::Start => self.arm_read(dispatcher),
            IpcConnectionAction::Read { buffer } => {
                self.read_op_id = None;
                if self.closing {
                    return Ok(());
                }
                if buffer.is_empty() {
                    log::debug!("ipc connection {:?} closed", self.id);
                    self.close(dispatcher);
                    return Ok(());
                }
                if !self.ingest(state, &buffer, dispatcher) {
                    self.close(dispatcher);
                    return Ok(());
                }
                self.arm_read(dispatcher);
                self.pump_write(dispatcher);
            }
            IpcConnectionAction::ReadFailed { error } => {
                self.read_op_id = None;
                if self.closing {
                    return Ok(());
                }
                if error.kind() == ErrorKind::UnexpectedEof {
                    log::debug!("ipc connection {:?} closed", self.id);
                } else {
                    log::warn!("ipc connection {:?} read failed: {error}", self.id);
                }
                self.close(dispatcher);
            }
            IpcConnectionAction::Written { written } => {
                self.write_op_id = None;
                if self.closing {
                    return Ok(());
                }
                self.note_written(written);
                self.pump_write(dispatcher);
            }
            IpcConnectionAction::Send(bytes) => {
                if self.closing {
                    return Ok(());
                }
                if !self.enqueue_or_close(&bytes) {
                    self.close(dispatcher);
                    return Ok(());
                }
                self.pump_write(dispatcher);
            }
            IpcConnectionAction::WriteFailed { error } => {
                self.write_op_id = None;
                if self.closing {
                    return Ok(());
                }
                log::warn!("ipc connection {:?} write failed: {error}", self.id);
                self.close(dispatcher);
            }
            IpcConnectionAction::Stop => self.halt(dispatcher),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;

    use super::{
        handle_line,
        proto::{self, Reply, Response},
        snapshot, IpcConnection, IpcConnectionAction, IpcConnectionId, MAX_QUEUED,
    };

    use crate::{
        io_uring::model::IoUringAction,
        ipc::IpcServerAction,
        runner::{Dispatcher, Model},
        test_support::{state_for_tests, test_output_drm1},
    };

    fn open_connection() -> IpcConnection {
        let fd: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        IpcConnection::new(IpcConnectionId::from(1), fd)
    }

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap()
    }

    #[test]
    fn queries_match_the_snapshot_and_bad_lines_stay_open() {
        let state = state_for_tests();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"version"}"#)),
            format!("{{\"ok\":{{\"version\":{{\"compositor\":\"{version}\",\"protocol\":1}}}}}}\n"),
        );
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"outputs"}"#)),
            "{\"ok\":{\"outputs\":[]}}\n",
        );
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"workspaces"}"#)),
            "{\"ok\":{\"workspaces\":[]}}\n",
        );
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"focused_window"}"#)),
            "{\"ok\":{\"focused_window\":null}}\n",
        );
        assert_eq!(
            handle_line(&state, r#"{"request":"version","extra":true}"#),
            handle_line(&state, r#"{"request":"version"}"#),
        );
        assert_eq!(text(&handle_line(&state, "nope")), "{\"error\":\"malformed request\"}\n");
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"subscribe"}"#)),
            "{\"error\":\"malformed request\"}\n",
        );
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"foo"}"#)),
            "{\"error\":\"unknown request `foo`\"}\n",
        );
        assert_eq!(
            text(&handle_line(&state, r#"{"request":"subscribe","events":["outputs"]}"#)),
            "{\"ok\":\"subscribed\"}\n",
        );

        let mut state = state_for_tests();
        state.insert_output(test_output_drm1(0, 0, 800, 600));
        let snapshot = snapshot::build(&state);
        assert_eq!(
            handle_line(&state, r#"{"request":"outputs"}"#),
            proto::encode_line(&Reply::Ok(Response::Outputs(snapshot.outputs))),
        );
        assert_eq!(
            handle_line(&state, r#"{"request":"workspaces"}"#),
            proto::encode_line(&Reply::Ok(Response::Workspaces(snapshot.workspaces))),
        );
        assert_eq!(
            handle_line(&state, r#"{"request":"focused_window"}"#),
            proto::encode_line(&Reply::Ok(Response::FocusedWindow(snapshot.focused_window))),
        );
    }

    #[test]
    fn short_write_keeps_the_unwritten_prefix_and_bytes_queued_behind_it() {
        let mut connection = open_connection();
        let mut dispatcher = Dispatcher::default();
        let state = state_for_tests();
        assert!(connection.ingest(&state, br#"{"request":"version"}"#, &mut dispatcher));
        assert!(connection.take_write().is_none());
        assert!(connection.ingest(&state, b"\n{\"request\":\"outputs\"}\n", &mut dispatcher));

        let mut expected = handle_line(&state, r#"{"request":"version"}"#);
        expected.extend(handle_line(&state, r#"{"request":"outputs"}"#));
        let first = connection.take_write().unwrap();
        assert_eq!(&*first, expected);
        assert!(connection.take_write().is_none());

        assert!(connection.enqueue(b"later"));
        connection.note_written(4);
        let mut rest = expected[4..].to_vec();
        rest.extend_from_slice(b"later");
        assert_eq!(connection.out_queue.make_contiguous(), rest.as_slice());
        let second = connection.take_write().unwrap();
        assert_eq!(&*second, rest.as_slice());
        connection.note_written(second.len());
        assert!(connection.out_queue.is_empty());
        assert!(connection.take_write().is_none());
    }

    #[test]
    fn over_limit_refuses_more_bytes_and_closes_from_a_read() {
        let mut connection = open_connection();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        assert!(connection.enqueue(&vec![b'a'; MAX_QUEUED]));
        assert!(!connection.enqueue(b"b"));
        assert_eq!(connection.out_queue.len(), MAX_QUEUED);

        connection
            .process(&mut state, IpcConnectionAction::Start, &mut dispatcher)
            .unwrap();
        connection
            .process(
                &mut state,
                IpcConnectionAction::Read { buffer: br#"{"request":"version"}"#.to_vec().into() },
                &mut dispatcher,
            )
            .unwrap();
        // The version line has no newline yet, so the full queue is still fine.
        assert!(!connection.closing);

        connection
            .process(
                &mut state,
                IpcConnectionAction::Read { buffer: b"\n".to_vec().into() },
                &mut dispatcher,
            )
            .unwrap();
        assert!(connection.closing);
        assert!(connection.out_queue.is_empty());

        let mut cancels = 0;
        let mut closed = 0;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if matches!(action, IoUringAction::Cancel { .. }) {
                cancels += 1;
            }
        });
        dispatcher.test_for_each_queued_action::<IpcServerAction>(|action| {
            if matches!(action, IpcServerAction::ConnectionClosed { .. }) {
                closed += 1;
            }
        });
        assert_eq!(cancels, 1);
        assert_eq!(closed, 1);
    }

    #[test]
    fn too_long_and_invalid_utf8_close_without_a_reply() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let mut connection = open_connection();
        connection
            .process(&mut state, IpcConnectionAction::Start, &mut dispatcher)
            .unwrap();
        let mut line = vec![b'x'; proto::MAX_LINE + 1];
        line.push(b'\n');
        connection
            .process(&mut state, IpcConnectionAction::Read { buffer: line.into() }, &mut dispatcher)
            .unwrap();
        assert!(connection.closing);
        assert!(connection.out_queue.is_empty());

        let mut connection = open_connection();
        let mut dispatcher = Dispatcher::default();
        connection
            .process(&mut state, IpcConnectionAction::Start, &mut dispatcher)
            .unwrap();
        connection
            .process(
                &mut state,
                IpcConnectionAction::Read { buffer: b"\xff\n".to_vec().into() },
                &mut dispatcher,
            )
            .unwrap();
        assert!(connection.closing);
        assert!(connection.out_queue.is_empty());
    }

    #[test]
    fn subscribe_sends_initial_events_and_rejects_later_requests() {
        let mut connection = open_connection();
        let mut dispatcher = Dispatcher::default();
        let state = state_for_tests();
        assert!(connection.ingest(
            &state,
            b"{\"request\":\"subscribe\",\"events\":[]}\n",
            &mut dispatcher,
        ));
        let queued = connection.take_queued();
        assert_eq!(
            text(&queued),
            "{\"ok\":\"subscribed\"}\n\
             {\"event\":\"focused_window\",\"window\":null}\n\
             {\"event\":\"workspaces\",\"workspaces\":[]}\n\
             {\"event\":\"outputs\",\"outputs\":[]}\n",
        );
        let subscribed = dispatcher.test_take_queued_actions::<IpcServerAction>();
        assert!(matches!(
            subscribed.as_slice(),
            [IpcServerAction::Subscribed { topics, .. }] if topics.is_empty()
        ));

        assert!(connection.ingest(&state, b"{\"request\":\"version\"}\n", &mut dispatcher,));
        assert_eq!(text(&connection.take_queued()), "{\"error\":\"connection is subscribed\"}\n",);

        let mut connection = open_connection();
        let mut dispatcher = Dispatcher::default();
        assert!(connection.ingest(
            &state,
            b"{\"request\":\"subscribe\",\"events\":[\"outputs\",\"outputs\"]}\n",
            &mut dispatcher,
        ));
        assert_eq!(
            text(&connection.take_queued()),
            "{\"ok\":\"subscribed\"}\n{\"event\":\"outputs\",\"outputs\":[]}\n",
        );

        let mut connection = open_connection();
        let mut dispatcher = Dispatcher::default();
        assert!(connection.ingest(
            &state,
            b"{\"request\":\"subscribe\",\"events\":[\"bell\"]}\n",
            &mut dispatcher,
        ));
        assert_eq!(text(&connection.take_queued()), "{\"ok\":\"subscribed\"}\n");
    }
}
