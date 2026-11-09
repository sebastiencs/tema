//! IPC listen socket.
//!
//! The accept loop follows [`crate::clients::ClientManager`]: one `Accept`,
//! re-armed after each completion, cancelled on stop, and the path unlinked
//! on stop and on drop. A bind failure is not fatal; the compositor logs it
//! and keeps running. Hyprland's command socket does the same. Sway aborts.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{fs::PermissionsExt, net::UnixListener},
    },
    path::PathBuf,
    rc::Rc,
};

use crate::{
    io_uring::{
        model::IoUringAction,
        ring::{AcceptCompleted, RingId},
    },
    runner::{Dispatcher, Model, Redispatcher},
    state::State,
};

use super::{
    connection::{IpcConnection, IpcConnectionAction, IpcConnectionId},
    proto,
    snapshot::{self, IpcSnapshot},
};

/// Live connections. The next accepted fd is closed once this many are open.
const MAX_CONNECTIONS: usize = 64;

pub(crate) struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
    accept_op_id: Option<RingId>,
    accepting: bool,
    unlinked: bool,
    connections: BTreeSet<IpcConnectionId>,
    /// Empty set means every topic. Absent means the connection is not subscribed.
    subscribers: BTreeMap<IpcConnectionId, BTreeSet<proto::Topic>>,
    last: IpcSnapshot,
    next_id: u64,
    /// Set while a run of accepts is being refused for [`MAX_CONNECTIONS`].
    limit_warned: bool,
}

#[derive(Debug)]
pub(crate) struct IpcBindError {
    path: PathBuf,
    source: io::Error,
}

impl std::fmt::Display for IpcBindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bind IPC socket {}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for IpcBindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub(crate) enum IpcServerAction {
    Start,
    NewConnection { fd: RawFd },
    RearmAccept,
    ConnectionClosed { id: IpcConnectionId },
    Subscribed { id: IpcConnectionId, topics: BTreeSet<proto::Topic> },
    Refresh,
    Stop,
}

impl IpcServer {
    pub(crate) fn new() -> Result<Box<Self>, IpcBindError> {
        Self::bind(proto::default_socket_path()).map(Box::new)
    }

    pub(crate) fn bind(path: impl Into<PathBuf>) -> Result<Self, IpcBindError> {
        let path = path.into();
        if path.exists() {
            if let Err(error) = std::fs::remove_file(&path) {
                log::warn!("could not remove stale IPC socket {}: {error}", path.display());
            }
        }
        let listener = UnixListener::bind(&path)
            .map_err(|source| IpcBindError { path: path.clone(), source })?;
        if let Err(source) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&path);
            return Err(IpcBindError { path, source });
        }
        log::info!("Listening on IPC socket {}", path.display());
        Ok(Self {
            listener,
            path,
            accept_op_id: None,
            accepting: true,
            unlinked: false,
            connections: BTreeSet::new(),
            subscribers: BTreeMap::new(),
            last: IpcSnapshot::default(),
            next_id: 1,
            limit_warned: false,
        })
    }

    fn dispatch_accept(&mut self, dispatcher: &mut Dispatcher) {
        if !self.accepting || self.accept_op_id.is_some() {
            return;
        }
        let accept_op_id = RingId::fresh();
        self.accept_op_id = Some(accept_op_id);
        dispatcher.dispatch(IoUringAction::Accept {
            id: Some(accept_op_id),
            fd: self.listener.as_raw_fd(),
            dispatch_to: Redispatcher::to::<Self>(|result| match result {
                Ok(AcceptCompleted { new_client, fd: _ }) => {
                    IpcServerAction::NewConnection { fd: new_client }
                }
                Err(error) => {
                    log::error!("ipc accept failed: {error}");
                    IpcServerAction::RearmAccept
                }
            }),
        });
    }

    /// `true` when another connection must be refused. Warns once until the
    /// count drops below [`MAX_CONNECTIONS`].
    fn connection_limit_reached(&mut self) -> bool {
        if self.connections.len() < MAX_CONNECTIONS {
            self.limit_warned = false;
            return false;
        }
        if !self.limit_warned {
            log::warn!(
                "ipc connection limit of {MAX_CONNECTIONS} reached, closing new connections"
            );
            self.limit_warned = true;
        }
        true
    }

    fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.accepting = false;
        if let Some(accept_op_id) = self.accept_op_id.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: accept_op_id });
        }
        for id in self.connections.iter().copied().collect::<Vec<_>>() {
            self.connections.remove(&id);
            self.subscribers.remove(&id);
            dispatcher.stop_model(id);
        }
        self.unlink_socket();
    }

    fn unlink_socket(&mut self) {
        if self.unlinked {
            return;
        }
        self.unlinked = true;
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                log::warn!("could not unlink IPC socket {}: {error}", self.path.display());
            }
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.unlink_socket();
    }
}

impl Model for IpcServer {
    type Action = IpcServerAction;

    const START: Option<Self::Action> = Some(IpcServerAction::Start);
    const STOP: Option<Self::Action> = Some(IpcServerAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: IpcServerAction,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        match action {
            IpcServerAction::Start => self.dispatch_accept(dispatcher),
            IpcServerAction::RearmAccept => {
                self.accept_op_id = None;
                self.dispatch_accept(dispatcher);
            }
            IpcServerAction::NewConnection { fd } => {
                self.accept_op_id = None;
                if !self.accepting {
                    drop(adopt_fd(fd));
                    return Ok(());
                }
                self.dispatch_accept(dispatcher);
                if self.connection_limit_reached() {
                    drop(adopt_fd(fd));
                    return Ok(());
                }
                let id = IpcConnectionId::from(self.next_id);
                self.next_id += 1;
                self.connections.insert(id);
                dispatcher.start_model(id, IpcConnection::new(id, adopt_fd(fd)));
            }
            IpcServerAction::ConnectionClosed { id } => {
                self.subscribers.remove(&id);
                if self.connections.remove(&id) {
                    dispatcher.stop_model(id);
                }
                if self.connections.len() < MAX_CONNECTIONS {
                    self.limit_warned = false;
                }
            }
            IpcServerAction::Subscribed { id, topics } => {
                if self.connections.contains(&id) {
                    self.subscribers.insert(id, topics);
                    // The connection just sent this snapshot. Later diffs start here.
                    self.last = snapshot::build(state);
                }
            }
            IpcServerAction::Refresh => self.refresh(state, dispatcher),
            IpcServerAction::Stop => {
                state.ipc_enabled = false;
                self.stop(dispatcher);
            }
        }
        Ok(())
    }
}

impl IpcServer {
    fn refresh(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        state.ipc_dirty = false;
        let next = snapshot::build(state);
        if self.subscribers.is_empty() {
            self.last = next;
            return;
        }
        let topics = changed_topics(&self.last, &next);
        self.last = next;
        for topic in topics {
            let bytes: Rc<[u8]> = Rc::from(proto::encode_line(&snapshot::topic(&self.last, topic)));
            for (id, subscribed) in &self.subscribers {
                if subscribed.is_empty() || subscribed.contains(&topic) {
                    dispatcher.dispatch_to(*id, IpcConnectionAction::Send(Rc::clone(&bytes)));
                }
            }
        }
    }
}

/// Focused window, then workspaces, then outputs. Same order as an empty subscribe.
fn changed_topics(previous: &IpcSnapshot, next: &IpcSnapshot) -> Vec<proto::Topic> {
    let mut topics = Vec::new();
    if previous.focused_window != next.focused_window {
        topics.push(proto::Topic::FocusedWindow);
    }
    if previous.workspaces != next.workspaces {
        topics.push(proto::Topic::Workspaces);
    }
    if previous.outputs != next.outputs {
        topics.push(proto::Topic::Outputs);
    }
    topics
}

fn adopt_fd(fd: RawFd) -> OwnedFd {
    // SAFETY: the accept completion transferred ownership of this descriptor.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        os::unix::fs::{FileTypeExt, PermissionsExt},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering::Relaxed},
        time::{SystemTime, UNIX_EPOCH},
    };

    use {
        super::{IpcConnectionId, IpcServer, IpcServerAction, IpcSnapshot, MAX_CONNECTIONS},
        crate::{
            capture,
            clients::{Client, ClientManager, ClientManagerAction, Resolved},
            input::SurfaceFocus,
            io_uring::model::IoUringAction,
            ipc::{
                connection::{IpcConnection, IpcConnectionAction},
                proto, snapshot,
            },
            object::Object,
            protocol_generated::{WlSurfaceId, XdgSurfaceId, XdgToplevelId, XdgWmBaseId},
            runner::{Dispatcher, Model},
            state::{BackendOutputRef, DrmId, ObjectId, State},
            test_support::{
                get_toplevel_request, state_for_tests, test_client_and_dispatcher, test_output,
            },
            wayland_objects::{
                wl_surface::{Role, WlSurface},
                xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
            },
            window::WindowId,
        },
    };

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "tema-ipc-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Relaxed),
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn bind_is_mode_0600_replaces_a_stale_file_and_unlink_removes_it() {
        let dir = TempDir::new();
        let path = dir.path.join("sock");
        std::fs::write(&path, b"stale").unwrap();

        let server = IpcServer::bind(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        drop(server);
        assert!(!path.exists());

        let mut server = IpcServer::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        server
            .process(&mut state, IpcServerAction::Start, &mut dispatcher)
            .unwrap();
        server
            .process(&mut state, IpcServerAction::Stop, &mut dispatcher)
            .unwrap();
        assert!(!path.exists());
        let mut cancel_count = 0;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if matches!(action, IoUringAction::Cancel { .. }) {
                cancel_count += 1;
            }
        });
        assert_eq!(cancel_count, 1);
    }

    #[test]
    fn connection_limit_warns_once_until_the_count_drops() {
        let dir = TempDir::new();
        let mut server = IpcServer::bind(dir.path.join("sock")).unwrap();
        for id in 0..MAX_CONNECTIONS {
            server.connections.insert(IpcConnectionId::from(id as u64));
        }
        assert!(server.connection_limit_reached());
        assert!(server.limit_warned);
        assert!(server.connection_limit_reached());
        assert!(server.limit_warned);

        server.connections.pop_first();
        assert!(!server.connection_limit_reached());
        assert!(!server.limit_warned);

        server.connections.insert(IpcConnectionId::from(1_000));
        assert!(server.connection_limit_reached());
        assert!(server.limit_warned);
    }

    #[test]
    fn refresh_after_a_title_change_sends_only_focused_window() {
        let dir = TempDir::new();
        let mut server = IpcServer::bind(dir.path.join("sock")).unwrap();
        let mut fixture = open_toplevel();
        capture::map_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        fixture.state.seat.keyboard_focus = Some(SurfaceFocus::new(fixture.root));
        let id = IpcConnectionId::from(1);
        server.connections.insert(id);
        server.subscribers.insert(id, BTreeSet::new());
        server.last = snapshot::build(&fixture.state);
        let before = server.last.clone();

        fixture.state.ipc_enabled = true;
        capture::retitle_window(
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.window,
            "renamed".to_string(),
        );
        let queued = fixture
            .dispatcher
            .test_take_queued_actions::<IpcServerAction>();
        assert!(matches!(queued.as_slice(), [IpcServerAction::Refresh]));
        for action in queued {
            server
                .process(&mut fixture.state, action, &mut fixture.dispatcher)
                .unwrap();
        }
        let after = snapshot::build(&fixture.state);
        assert_eq!(super::changed_topics(&before, &after), vec![proto::Topic::FocusedWindow]);
        assert_eq!(
            sent_lines(&mut fixture.dispatcher, id),
            vec![encoded_event(&after, proto::Topic::FocusedWindow)]
        );
        assert!(after.focused_window.unwrap().title.contains("renamed"));

        server
            .process(&mut fixture.state, IpcServerAction::Refresh, &mut fixture.dispatcher)
            .unwrap();
        assert!(sent_lines(&mut fixture.dispatcher, id).is_empty());
        assert!(!fixture.state.ipc_dirty);
    }

    #[test]
    fn hooks_emit_the_diff_for_map_focus_title_workspace_and_close() {
        let dir = TempDir::new();
        let mut server = IpcServer::bind(dir.path.join("ipc")).unwrap();
        let mut manager = ClientManager::bind(dir.path.join("wl")).unwrap();
        let mut fixture = open_toplevel();
        // Creation queues workspace publication. Drain it before IPC is enabled
        // so the subscription baseline is the state the client is about to see.
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        fixture.state.ipc_enabled = true;
        let id = IpcConnectionId::from(1);
        server.connections.insert(id);
        let mut connection =
            IpcConnection::new(id, std::fs::File::open("/dev/null").unwrap().into());
        let line = b"{\"request\":\"subscribe\",\"events\":[]}\n";
        let mut buffer = vec![0u8; line.len()].into_boxed_slice();
        buffer.copy_from_slice(line);
        connection
            .process(
                &mut fixture.state,
                IpcConnectionAction::Read { buffer },
                &mut fixture.dispatcher,
            )
            .unwrap();
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        assert_eq!(connection.take_queued(), initial_bytes(&fixture.state));
        assert!(sent_lines(&mut fixture.dispatcher, id).is_empty());

        let before = snapshot::build(&fixture.state);
        capture::map_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        expect_diff(&mut fixture.dispatcher, id, &before, &snapshot::build(&fixture.state));

        let before = snapshot::build(&fixture.state);
        crate::input::set_keyboard_focus(
            &mut fixture.state,
            &mut fixture.dispatcher,
            Some(SurfaceFocus::new(fixture.root)),
        );
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        expect_diff(&mut fixture.dispatcher, id, &before, &snapshot::build(&fixture.state));

        let before = snapshot::build(&fixture.state);
        capture::retitle_window(
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.window,
            "renamed".to_string(),
        );
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        let after = snapshot::build(&fixture.state);
        assert_eq!(super::changed_topics(&before, &after), vec![proto::Topic::FocusedWindow]);
        expect_diff(&mut fixture.dispatcher, id, &before, &after);

        let group_id = fixture
            .state
            .output_workspace_groups
            .get(&fixture.output_id)
            .copied()
            .unwrap();
        let second = fixture
            .state
            .workspace_groups
            .try_get(group_id)
            .unwrap()
            .workspaces[1];
        let before = snapshot::build(&fixture.state);
        assert!(fixture.state.activate_workspace(second));
        fixture
            .dispatcher
            .dispatch(ClientManagerAction::WorkspaceStateChanged);
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        expect_diff(&mut fixture.dispatcher, id, &before, &snapshot::build(&fixture.state));

        let before = snapshot::build(&fixture.state);
        capture::unmap_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        drain(&mut fixture.state, &mut fixture.dispatcher, &mut server, &mut manager);
        expect_diff(&mut fixture.dispatcher, id, &before, &snapshot::build(&fixture.state));
    }

    fn drain(
        state: &mut State,
        dispatcher: &mut Dispatcher,
        server: &mut IpcServer,
        manager: &mut ClientManager,
    ) {
        loop {
            let manager_actions = dispatcher.test_take_queued_actions::<ClientManagerAction>();
            let server_actions = dispatcher.test_take_queued_actions::<IpcServerAction>();
            if manager_actions.is_empty() && server_actions.is_empty() {
                break;
            }
            for action in manager_actions {
                manager.process(state, action, dispatcher).unwrap();
            }
            for action in server_actions {
                server.process(state, action, dispatcher).unwrap();
            }
        }
    }

    fn expect_diff(
        dispatcher: &mut Dispatcher,
        id: IpcConnectionId,
        before: &IpcSnapshot,
        after: &IpcSnapshot,
    ) {
        let expected = super::changed_topics(before, after)
            .into_iter()
            .map(|topic| encoded_event(after, topic))
            .collect::<Vec<_>>();
        assert_eq!(sent_lines(dispatcher, id), expected);
    }

    fn encoded_event(snapshot: &IpcSnapshot, topic: proto::Topic) -> String {
        String::from_utf8(proto::encode_line(&snapshot::topic(snapshot, topic))).unwrap()
    }

    fn initial_bytes(state: &State) -> Vec<u8> {
        let snapshot = snapshot::build(state);
        let mut bytes = proto::encode_line(&proto::Reply::Ok(proto::Response::Subscribed));
        for topic in [proto::Topic::FocusedWindow, proto::Topic::Workspaces, proto::Topic::Outputs]
        {
            bytes.extend(encoded_event(&snapshot, topic).into_bytes());
        }
        bytes
    }

    fn sent_lines(dispatcher: &mut Dispatcher, id: IpcConnectionId) -> Vec<String> {
        dispatcher
            .test_take_actions_for::<IpcConnectionAction, IpcConnectionId>(id)
            .into_iter()
            .filter_map(|action| match action {
                IpcConnectionAction::Send(bytes) => {
                    Some(String::from_utf8(bytes.to_vec()).unwrap())
                }
                _ => None,
            })
            .collect()
    }

    struct Fixture {
        state: State,
        dispatcher: Dispatcher,
        window: WindowId,
        root: Resolved<WlSurfaceId>,
        output_id: crate::state::OutputId,
        _client: Client,
    }

    fn open_toplevel() -> Fixture {
        let mut state = state_for_tests();
        let output_id = insert_named_output(&mut state, "DP-1");
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (window, root) = {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let (root, _) = client.insert_object(
                &mut state,
                WlSurface::new(WlSurfaceId::from(20), client.client_id(), 6),
            );
            {
                let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
                assert!(surface
                    .set_role(Role::XdgSurface, &mut client_dispatcher)
                    .is_ok());
            }
            state.register_root_surface(root);
            let xdg_surface_id = XdgSurfaceId::from(21);
            let (xdg_surface, _) = client.insert_object(
                &mut state,
                XdgSurface::new(
                    xdg_surface_id,
                    Resolved::new(
                        client.client_id(),
                        XdgWmBaseId::from(1),
                        ObjectId::from(1_000_000usize),
                    ),
                    root,
                    7,
                ),
            );
            state
                .objects
                .grab_object::<WlSurface>(root.object_id())
                .set_xdg_surface_impl(xdg_surface);
            state
                .objects
                .grab_object::<XdgSurface>(xdg_surface.object_id())
                .on(
                    &mut client,
                    &mut state,
                    get_toplevel_request(xdg_surface_id, XdgToplevelId::from(22)),
                    &mut client_dispatcher,
                )
                .unwrap();
            let window = state.window_for_root(root).unwrap();
            let role = state.window_role_object(window).unwrap();
            state
                .objects
                .try_ref_object_mut::<XdgToplevel>(role)
                .unwrap()
                .test_set_identity("nvim — ~/src", "Alacritty");
            (window, root)
        };
        Fixture { state, dispatcher, window, root, output_id, _client: client }
    }

    fn insert_named_output(state: &mut State, name: &str) -> crate::state::OutputId {
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let mut output = test_output(backend, 1, 10, 20, 2560, 1440, name);
        output.make = "Dell".to_string();
        output.model = "U2723QE".to_string();
        output.description = "Dell Inc. U2723QE".to_string();
        state.insert_output(output)
    }
}
