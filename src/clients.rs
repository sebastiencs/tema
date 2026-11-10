use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixListener,
    },
    path::PathBuf,
    rc::Rc,
};

use libc::getsockopt;

use crate::{
    dbus::Dbus,
    drm::{kick_visual_animation_pump, request_active_output, request_output},
    input::ime::{ImEvent, KeyboardGrabClientEvent, TextInputClientEvent},
    io_uring::{
        model::IoUringAction,
        ring::{AcceptCompleted, RecvmsgCompleted, RingId},
    },
    object::{AbortRequest, IdToObject, Object, ObjectWithId, Parser, ParserError, PostAction},
    protocol_generated::{
        wl_data_device_manager::DndAction, wl_data_offer, wl_display, wl_pointer, Array, Constants,
        ExtDataControlSourceV1Id, ExtWorkspaceManagerV1Id, HyprlandCtmControlManagerV1Id,
        WaylandId, WlCallbackId, WlDataOfferId, WlDataSourceId, WlOutputId, WlRegistryId,
        WlSurfaceId, WpPresentationFeedbackId, XdgPopupId, XdgToplevelId,
        ZwlrDataControlSourceV1Id, ZwlrGammaControlV1Id, ZwlrLayerSurfaceV1Id,
        ZwpInputMethodKeyboardGrabV2Id, ZwpInputMethodV2Id, ZwpInputPopupSurfaceV2Id,
        ZwpPrimarySelectionSourceV1Id, ZwpTextInputV3Id, ZxdgOutputV1Id,
    },
    region::RectI,
    runner::{Dispatcher, Model, ModelInstanceId, Redispatcher},
    scene::visibility::root_has_protocol_visibility,
    seat::SeatClientEvent,
    state::{DataOfferRole, DrmId, GrabbedObject, ObjectId, OutputId, SelectionSourceState, State},
    sync::AcquireReadyWaiterId,
    udev::Udev,
    wayland_objects::{
        ext_data_control_offer_v1::{
            create_ext_data_control_offer, create_ext_data_control_primary_offer,
            inert_ext_data_control_offer,
        },
        ext_workspace_manager_v1::publish_workspace_state_changed,
        hyprland_ctm_control_manager_v1::HyprlandCtmControlManagerV1,
        wl_data_offer::WlDataOffer,
        wl_display::WlDisplay,
        wl_output::WlOutput,
        wp_presentation_feedback::WpPresentationFeedback,
        zwlr_data_control_offer_v1::{
            create_data_control_offer, create_data_control_primary_offer, inert_data_control_offer,
        },
        zwlr_gamma_control_v1::ZwlrGammaControlV1,
        zwlr_layer_surface_v1::relayout_client_layer_surfaces,
        zwp_primary_selection_offer_v1::{
            create_primary_selection_offer, inert_primary_selection_offer,
        },
        zxdg_output_v1::ZxdgOutputV1,
    },
    wire_scale::WireScale,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, derive_more::Into)]
pub struct ClientId(u64);

#[cfg(test)]
impl ClientId {
    pub(crate) fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Resolved<K> {
    client_id: ClientId,
    wayland_id: K,
    object_id: ObjectId,
}

impl<K> Resolved<K> {
    pub fn new(client_id: ClientId, wayland_id: K, object_id: ObjectId) -> Self {
        Self { client_id, wayland_id, object_id }
    }

    pub fn client_id(self) -> ClientId {
        self.client_id
    }

    pub fn object_id(self) -> ObjectId {
        self.object_id
    }

    pub fn wayland_id(&self) -> K
    where
        K: Copy,
    {
        self.wayland_id
    }
}

impl<K> Resolved<K>
where
    K: IdToObject,
{
    pub fn try_ref<'a>(self, state: &'a State) -> Option<&'a K::Object> {
        state.objects.try_ref_object::<K::Object>(self.object_id)
    }
    pub fn rref<'a>(self, state: &'a State) -> &'a K::Object {
        state
            .objects
            .try_ref_object::<K::Object>(self.object_id)
            .unwrap()
    }

    pub fn try_ref_mut<'a>(self, state: &'a mut State) -> Option<&'a mut K::Object> {
        state
            .objects
            .try_ref_object_mut::<K::Object>(self.object_id)
    }

    pub fn ref_mut<'a>(self, state: &'a mut State) -> &'a mut K::Object {
        state
            .objects
            .try_ref_object_mut::<K::Object>(self.object_id)
            .unwrap()
    }

    pub fn try_grab(self, state: &mut State) -> Option<GrabbedObject<K::Object>> {
        state.objects.try_grab_object::<K::Object>(self.object_id)
    }

    pub fn grab(self, state: &mut State) -> GrabbedObject<K::Object> {
        state
            .objects
            .try_grab_object::<K::Object>(self.object_id)
            .unwrap()
    }

    pub fn exists(&self, state: &State) -> bool {
        state.objects.contains(self.object_id)
    }
}

pub const WAYLAND_DISPLAY_NAME: &str = "tema-0";

pub struct ClientManager {
    current_id: u64,
    fd: UnixListener,
    path: PathBuf,
    set: BTreeSet<ClientId>,
    /// Notified once, when an adopted client is closed.
    closed: BTreeMap<ClientId, Redispatcher<ClientId>>,
    /// The Xwayland client, when one is adopted. `SetWireScale` goes only here.
    xwayland_client: Option<ClientId>,
    accept_op_id: Option<RingId>,
    accepting: bool,
    unlinked: bool,
}

/// How an adopted client differs from one accepted on the Wayland socket.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClientOrigin {
    pub(crate) xwayland: bool,
    /// Replaces the pid from `SO_PEERCRED`. Uid and gid still come from there.
    pub(crate) pid: Option<i32>,
}

#[derive(Debug)]
pub struct SocketBindError {
    path: PathBuf,
    source: io::Error,
}

impl std::fmt::Display for SocketBindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bind Wayland socket {}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for SocketBindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub enum ClientManagerAction {
    Start,
    RearmAccept,
    NewClient {
        new_client: RawFd,
    },
    /// Take ownership of an already-connected socket and run it as a client.
    AdoptClient {
        fd: OwnedFd,
        origin: ClientOrigin,
        adopted: Redispatcher<ClientId>,
        closed: Redispatcher<ClientId>,
    },
    LibinputEvent,
    UdevEvent,
    RegistryGlobalAdded {
        name: u32,
        interface: String,
        version: u32,
    },
    RegistryGlobalRemoved {
        name: u32,
    },
    OutputStateChanged {
        output_id: OutputId,
    },
    OutputRemoved {
        output_id: OutputId,
    },
    GammaControlFailed {
        output_id: OutputId,
    },
    DiscardSurfacePresentationFeedbacksForBackend {
        drm_id: DrmId,
    },
    PublishWorkspaceStateChanged {
        affected_outputs: Vec<OutputId>,
    },
    WorkspaceStateChanged,
    LayerShellLayoutChanged,
    Close {
        id: ClientId,
    },
    /// Recompute the wire scale and update the Xwayland client when it changes.
    #[cfg_attr(not(test), allow(dead_code))]
    XwaylandWireScaleChanged,
    Stop,
}

impl ClientManager {
    pub fn new() -> Result<Box<Self>, SocketBindError> {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let path = PathBuf::from(runtime).join(WAYLAND_DISPLAY_NAME);
        Self::bind(path).map(Box::new)
    }

    pub(crate) fn bind(path: impl Into<PathBuf>) -> Result<Self, SocketBindError> {
        let path = path.into();
        if path.exists() {
            if let Err(error) = std::fs::remove_file(&path) {
                log::warn!("could not remove stale Wayland socket {}: {error}", path.display());
            }
        }
        let fd = UnixListener::bind(&path)
            .map_err(|source| SocketBindError { path: path.clone(), source })?;
        log::info!("Listening on Wayland socket {}", path.display());
        Ok(Self {
            current_id: 0,
            fd,
            path,
            set: BTreeSet::default(),
            closed: BTreeMap::new(),
            xwayland_client: None,
            accept_op_id: None,
            accepting: true,
            unlinked: false,
        })
    }

    fn next_id(&mut self) -> ClientId {
        let id = self.current_id;
        self.current_id += 1;
        ClientId(id)
    }

    fn dispatch_accept(&mut self, dispatcher: &mut Dispatcher) {
        if !self.accepting || self.accept_op_id.is_some() {
            return;
        }
        let accept_op_id = RingId::fresh();
        self.accept_op_id = Some(accept_op_id);
        dispatcher.dispatch(IoUringAction::Accept {
            id: Some(accept_op_id),
            fd: self.fd.as_raw_fd(),
            dispatch_to: Redispatcher::to::<Self>(|result| match result {
                Ok(AcceptCompleted { fd: _, new_client }) => {
                    ClientManagerAction::NewClient { new_client }
                }
                Err(e) => {
                    log::error!("accept failed: {:?}", e);
                    ClientManagerAction::RearmAccept
                }
            }),
        });
    }

    fn close_client(&mut self, dispatcher: &mut Dispatcher, id: ClientId) {
        log::trace!("client {:?} closed", id);
        if self.xwayland_client == Some(id) {
            self.xwayland_client = None;
        }
        if self.set.remove(&id) {
            dispatcher.stop_model(id);
        }
        if let Some(closed) = self.closed.remove(&id) {
            dispatcher.redispatch_to(closed, id);
        }
    }

    fn unlink_socket(&mut self) {
        if self.unlinked {
            return;
        }
        self.unlinked = true;
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                log::warn!("could not unlink Wayland socket {}: {error}", self.path.display());
            }
        }
    }

    fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.accepting = false;
        if let Some(accept_op_id) = self.accept_op_id.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: accept_op_id });
        }
        for id in self.set.iter().copied().collect::<Vec<_>>() {
            self.close_client(dispatcher, id);
        }
        self.unlink_socket();
    }

    /// Recompute S and queue `SetWireScale` when it changed.
    ///
    /// `state.xwayland_wire_scale` moves here, before Xwayland has processed the
    /// output events that follow. For that short window the XWM may convert a
    /// stale X geometry with the new S. The `Reconfigure` dispatched from
    /// `SetWireScale` converges it; nothing else waits.
    fn queue_xwayland_wire_scale(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        let Some(scale) = state.refresh_xwayland_wire_scale() else { return };
        let Some(client_id) = self.xwayland_client else { return };
        dispatcher.dispatch_to(client_id, ClientAction::SetWireScale { scale });
    }
}

impl Drop for ClientManager {
    fn drop(&mut self) {
        self.unlink_socket();
    }
}

impl Model for ClientManager {
    type Action = ClientManagerAction;

    const START: Option<Self::Action> = Some(ClientManagerAction::Start);
    const STOP: Option<Self::Action> = Some(ClientManagerAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: ClientManagerAction,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        use ClientManagerAction::*;

        match action {
            Start => {
                self.dispatch_accept(dispatcher);
                dispatcher.start::<Dbus>();
                dispatcher.start::<Udev>();
            }
            RearmAccept => {
                self.accept_op_id = None;
                self.dispatch_accept(dispatcher);
            }
            NewClient { new_client } => {
                self.accept_op_id = None;
                if !self.accepting {
                    drop(unsafe { OwnedFd::from_raw_fd(new_client) });
                    return Ok(());
                }
                self.dispatch_accept(dispatcher);

                let client_id = self.next_id();
                self.set.insert(client_id);
                let fd = unsafe { OwnedFd::from_raw_fd(new_client) };
                dispatcher.start_model(
                    client_id,
                    Client::new(client_id, fd, ClientOrigin { xwayland: false, pid: None }),
                );

                log::trace!("new_client id={:?} fd={:?}", client_id, new_client);
            }
            AdoptClient { fd, origin, adopted, closed } => {
                if !self.accepting {
                    drop(fd);
                    return Ok(());
                }
                let client_id = self.next_id();
                self.set.insert(client_id);
                self.closed.insert(client_id, closed);
                if origin.xwayland {
                    self.xwayland_client = Some(client_id);
                }
                let client = client_for_adoption(client_id, fd, origin, state);
                dispatcher.start_model(client_id, client);
                dispatcher.redispatch_to(adopted, client_id);
                log::trace!("adopted client id={:?} xwayland={}", client_id, origin.xwayland);
            }
            LibinputEvent => {
                // self.libinput.dispatch();
                // while let Some(event) = self.libinput.next_event() {
                //     log::info!("libinput event={:?}", event);
                // }
            }
            UdevEvent => {
                eprintln!("UDEV EVENT");
                // for e in self.udev.udev_monitor.iter() {
                //     let prop = e.property_value("ID_VENDOR_FROM_DATABASE");
                //     dbg!(prop);
                //     // match e.event_type() {
                //     //     udev::EventType::Add => todo!(),
                //     //     udev::EventType::Change => todo!(),
                //     //     udev::EventType::Remove => todo!(),
                //     //     udev::EventType::Bind => todo!(),
                //     //     udev::EventType::Unbind => todo!(),
                //     //     udev::EventType::Unknown => todo!(),
                //     // }
                //     // eprintln!("EVENT={:?}", e);
                // }
            }
            RegistryGlobalAdded { name, interface, version } => {
                // A new `wl_output` is already in the layout. Recompute S before
                // announcing the global, so `SetWireScale` is queued first.
                if interface == WlOutput::INTERFACE_NAME {
                    self.queue_xwayland_wire_scale(state, dispatcher);
                }
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(
                        client_id,
                        ClientAction::RegistryGlobalAdded {
                            name,
                            interface: interface.clone(),
                            version,
                        },
                    );
                }
            }
            RegistryGlobalRemoved { name } => {
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(client_id, ClientAction::RegistryGlobalRemoved { name });
                }
            }
            OutputStateChanged { output_id } => {
                crate::ipc::mark_dirty(state, dispatcher);
                // The output's new scale is already stored. Queue `SetWireScale`
                // before this broadcast so the Xwayland client applies S first.
                self.queue_xwayland_wire_scale(state, dispatcher);
                crate::wayland_objects::ext_session_lock_v1::on_output_state_changed(
                    state, dispatcher, output_id,
                );
                for client_id in self.set.iter().copied() {
                    dispatcher
                        .dispatch_to(client_id, ClientAction::OutputStateChanged { output_id });
                }
            }
            OutputRemoved { output_id } => {
                crate::ipc::mark_dirty(state, dispatcher);
                // The output has already left the layout. Recompute S before the
                // clients are told it is gone.
                self.queue_xwayland_wire_scale(state, dispatcher);
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(client_id, ClientAction::OutputRemoved { output_id });
                }
            }
            GammaControlFailed { output_id } => {
                for client_id in self.set.iter().copied() {
                    dispatcher
                        .dispatch_to(client_id, ClientAction::GammaControlFailed { output_id });
                }
            }
            DiscardSurfacePresentationFeedbacksForBackend { drm_id } => {
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(
                        client_id,
                        ClientAction::DiscardSurfacePresentationFeedbacksForBackend { drm_id },
                    );
                }
            }
            PublishWorkspaceStateChanged { affected_outputs } => {
                publish_workspace_state_changed(state, dispatcher, affected_outputs);
            }
            WorkspaceStateChanged => {
                // Floating and fullscreen keybindings, drags, and workspace
                // publication all arrive here, so the IPC snapshot sees them.
                crate::ipc::mark_dirty(state, dispatcher);
                crate::session::note_window_changed(state, dispatcher);
                crate::idle_inhibit::refresh(state, dispatcher, std::time::Instant::now());
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(client_id, ClientAction::WorkspaceStateChanged);
                }
            }
            LayerShellLayoutChanged => {
                crate::idle_inhibit::refresh(state, dispatcher, std::time::Instant::now());
                for client_id in self.set.iter().copied() {
                    dispatcher.dispatch_to(client_id, ClientAction::RefreshLayerSurfaces);
                }
            }
            XwaylandWireScaleChanged => {
                self.queue_xwayland_wire_scale(state, dispatcher);
            }
            Close { id } => self.close_client(dispatcher, id),
            Stop => self.stop(dispatcher),
        }
        Ok(())
    }
}

/// Build the client for an adoption. The Xwayland client starts with the
/// running wire scale, before its model begins reading output state, so its
/// first `wl_output` bind already sees S. Every other client keeps `ONE`.
fn client_for_adoption(
    client_id: ClientId,
    fd: OwnedFd,
    origin: ClientOrigin,
    state: &State,
) -> Client {
    let mut client = Client::new(client_id, fd, origin);
    if origin.xwayland {
        client.wire_scale = state.xwayland_wire_scale;
    }
    client
}

#[derive(Debug)]
pub(crate) struct ProcessInfo {
    command: Rc<str>,
    cred: libc::ucred,
}

impl ProcessInfo {
    pub(crate) fn command(&self) -> &str {
        &self.command
    }
}

const MIN_SERVER_ID: u32 = 0xff00_0000;

/// A server-allocated id may only disappear while its own destructor is the
/// request being handled. Anything else leaves the client holding a proxy
/// whose next request is a fatal protocol error.
fn is_server_side_removal_of_server_id(id: WaylandId, requester: Option<WaylandId>) -> bool {
    u32::from(id) >= MIN_SERVER_ID && requester != Some(id)
}

#[derive(Clone, Copy, Debug)]
struct ClientObjectEntry {
    type_id: TypeId,
    object_id: ObjectId,
}

pub struct Client {
    id: ClientId,
    fd: OwnedFd,
    xwayland: bool,
    /// Pid to use instead of `SO_PEERCRED`, for a client we spawned ourselves.
    pid_override: Option<i32>,
    reader_id: Option<ClientReaderId>,
    /// the key `WaylandId` is the id we share with the client
    /// the value `ObjectId` is an index in our `State::objects`
    /// So this `objects` is just used to lookup into `State::objects`, from the client `WaylandId`
    // TODO: Replace `BTreeMap` with a `IndexMap` here ?
    objects: BTreeMap<WaylandId, ClientObjectEntry>,
    info: Option<Rc<ProcessInfo>>,
    pending_fds: VecDeque<OwnedFd>,
    next_server_id: u32,
    /// Highest `xwayland_surface_v1.set_serial` accepted for this client. `0` means none.
    last_xwayland_serial: u64,
    /// Integer scale of the Xwayland wire. `ONE` for every normal client.
    wire_scale: WireScale,
}

impl ModelInstanceId for ClientId {
    type Model = Client;

    fn into_raw(self) -> u64 {
        self.0
    }
}

pub enum ClientAction {
    Start,
    /// Update this client's Xwayland wire scale. Phase 9 resends output state.
    SetWireScale {
        scale: WireScale,
    },
    MsgReceived {
        request: RawRequest<'static>,
    },
    FdsReceived {
        fds: Vec<OwnedFd>,
    },
    SeatEvent {
        event: SeatClientEvent,
    },
    TextInputEvent {
        text_input: Resolved<ZwpTextInputV3Id>,
        event: TextInputClientEvent,
    },
    /// One batch for `zwp_input_method_v2`. `Done` is incremented when it is written.
    InputMethodEvent {
        input_method: Resolved<ZwpInputMethodV2Id>,
        events: Vec<ImEvent>,
    },
    KeyboardGrabEvent {
        grab: Resolved<ZwpInputMethodKeyboardGrabV2Id>,
        event: KeyboardGrabClientEvent,
    },
    InputPopupEvent {
        popup: Resolved<ZwpInputPopupSurfaceV2Id>,
        cursor: RectI,
    },
    RequestToplevelClose {
        toplevel: Resolved<crate::protocol_generated::XdgToplevelId>,
    },
    RefreshDataDeviceSelection,
    RefreshDataControlSelection,
    RefreshDataControlPrimarySelection,
    RefreshExtDataControlSelection,
    RefreshExtDataControlPrimarySelection,
    RefreshPrimarySelection,
    DataDeviceDragEnter {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        x: f32,
        y: f32,
        source: Option<Resolved<WlDataSourceId>>,
    },
    DataDeviceDragLeave,
    DataDeviceDragMotion {
        time: u32,
        x: f32,
        y: f32,
    },
    DataDeviceDragDrop,
    DataControlSourceEvent {
        source: Resolved<ZwlrDataControlSourceV1Id>,
        event: DataControlSourceEvent,
    },
    ExtDataControlSourceEvent {
        source: Resolved<ExtDataControlSourceV1Id>,
        event: ExtDataControlSourceEvent,
    },
    PrimarySelectionSourceEvent {
        source: Resolved<ZwpPrimarySelectionSourceV1Id>,
        event: PrimarySelectionSourceEvent,
    },
    DataSourceEvent {
        source: Resolved<WlDataSourceId>,
        event: DataSourceClientEvent,
    },
    RegistryGlobalAdded {
        name: u32,
        interface: String,
        version: u32,
    },
    RegistryGlobalRemoved {
        name: u32,
    },
    OutputStateChanged {
        output_id: OutputId,
    },
    OutputRemoved {
        output_id: OutputId,
    },
    GammaControlFailed {
        output_id: OutputId,
    },
    WorkspaceStateChanged,
    RefreshLayerSurfaces,
    RefreshSurfaceOutputs,
    ReleaseBuffer {
        buffer: Resolved<crate::protocol_generated::WlBufferId>,
    },
    CompleteBufferReleaseCallbacks {
        callbacks: Vec<Resolved<WlCallbackId>>,
    },
    CompleteSurfaceFrameCallbacks {
        surfaces: Vec<Resolved<WlSurfaceId>>,
        time_msec: u32,
    },
    SendSessionLockLocked,
    ReconfigureLockOutput {
        output_id: OutputId,
    },
    PresentationFeedbackPresented {
        feedbacks: Vec<Resolved<WpPresentationFeedbackId>>,
        output_id: Option<OutputId>,
        tv_sec_hi: u32,
        tv_sec_lo: u32,
        tv_nsec: u32,
        refresh: u32,
        seq_hi: u32,
        seq_lo: u32,
        flags: u32,
    },
    PresentationFeedbackDiscarded {
        feedbacks: Vec<Resolved<WpPresentationFeedbackId>>,
    },
    DiscardSurfacePresentationFeedbacksForBackend {
        drm_id: DrmId,
    },
    CancelBlockedSurface {
        surface: Resolved<WlSurfaceId>,
        waiter_id: AcquireReadyWaiterId,
    },
    DismissPopupGrab {
        popups: Vec<Resolved<XdgPopupId>>,
    },
    InteractiveResizeConfigure {
        xdg_toplevel: Resolved<XdgToplevelId>,
        width: i32,
        height: i32,
        resizing: bool,
    },
    Close,
    /// Send capture `stopped`, `failed`, `closed`, or foreign-toplevel announcements queued for
    /// this client.
    DeliverCaptureEvents,
    IdleNotification {
        id: crate::protocol_generated::ExtIdleNotificationV1Id,
        notice: crate::wayland_objects::ext_idle_notification_v1::IdleNotice,
    },
    /// Resend the current dmabuf feedback table to one feedback object.
    SendDmabufFeedback {
        id: crate::protocol_generated::ZwpLinuxDmabufFeedbackV1Id,
    },
    /// `xdg_session_v1.replaced` for a session another client took over.
    SessionReplaced {
        session: Resolved<crate::protocol_generated::XdgSessionV1Id>,
    },
}

#[derive(Debug)]
pub enum DataControlSourceEvent {
    Send { mime_type: String, fd: Rc<OwnedFd> },
    Cancelled,
}

#[derive(Debug)]
pub enum ExtDataControlSourceEvent {
    Send { mime_type: String, fd: Rc<OwnedFd> },
    Cancelled,
}

#[derive(Debug)]
pub enum PrimarySelectionSourceEvent {
    Send { mime_type: String, fd: Rc<OwnedFd> },
    Cancelled,
}

pub enum DataSourceClientEvent {
    Target(Option<String>),
    Send { mime_type: String, fd: Rc<OwnedFd> },
    Cancelled,
    DndDropPerformed,
    DndFinished,
    Action { dnd_action: u32 },
}

impl Client {
    fn new(client_id: ClientId, fd: OwnedFd, origin: ClientOrigin) -> Self {
        Self {
            id: client_id,
            fd,
            xwayland: origin.xwayland,
            pid_override: origin.pid,
            reader_id: None,
            objects: BTreeMap::default(),
            info: None,
            pending_fds: VecDeque::new(),
            next_server_id: MIN_SERVER_ID,
            last_xwayland_serial: 0,
            wire_scale: WireScale::ONE,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests(client_id: ClientId) -> Self {
        let fd = std::fs::File::open("/dev/null").unwrap().into();
        Self::new(client_id, fd, ClientOrigin { xwayland: false, pid: None })
    }

    #[cfg(test)]
    pub(crate) fn new_xwayland_for_tests(client_id: ClientId) -> Self {
        let fd = std::fs::File::open("/dev/null").unwrap().into();
        Self::new(client_id, fd, ClientOrigin { xwayland: true, pid: None })
    }

    pub fn is_xwayland(&self) -> bool {
        self.xwayland
    }

    /// The scale of the Xwayland wire for this client. `ONE` for normal clients.
    pub fn wire_scale(&self) -> WireScale {
        self.wire_scale
    }

    #[cfg(test)]
    pub(crate) fn set_wire_scale_for_tests(&mut self, scale: WireScale) {
        self.wire_scale = scale;
    }

    /// Store a new wire scale on the client and on the dispatcher that sends its
    /// events in this `process` call.
    fn apply_wire_scale(&mut self, dispatcher: &mut ClientDispatcher<'_>, scale: WireScale) {
        self.wire_scale = scale;
        dispatcher.wire_scale = scale;
    }

    /// Apply a wire scale that really changed: store it, resend every bound
    /// output, recompute on-screen surfaces from their raw wire inputs, and ask
    /// the X window manager to reconfigure and refresh DPI and cursor hints.
    fn apply_changed_wire_scale(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher<'_>,
        scale: WireScale,
    ) {
        if self.wire_scale == scale {
            return;
        }
        self.apply_wire_scale(dispatcher, scale);
        let output_ids = state
            .outputs
            .iter()
            .map(|(output_id, _)| output_id)
            .collect::<Vec<_>>();
        for output_id in output_ids {
            self.handle_output_state_changed(state, dispatcher, output_id);
        }
        self.refresh_surface_outputs(state, dispatcher);
        self.recompute_wire_scaled_surfaces(state, dispatcher);
        dispatcher.dispatch(crate::xwayland::XwaylandAction::Reconfigure);
        dispatcher.dispatch(crate::xwayland::XwaylandAction::RefreshHints);
    }

    /// Recompute each of this client's on-screen surfaces for the scale just
    /// stored, then mark root geometry dirty and ask the covered outputs to draw.
    fn recompute_wire_scaled_surfaces(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher<'_>,
    ) {
        let scale = dispatcher.wire_scale();
        let surfaces = self
            .iter_resolved_of::<WlSurfaceId>(state)
            .collect::<Vec<_>>();
        for surface_id in surfaces {
            let changed = surface_id
                .try_grab(state)
                .is_some_and(|mut surface| surface.recompute_for_wire_scale(state, scale));
            if !changed {
                continue;
            }
            let root = state
                .root_surface_for_surface(surface_id)
                .unwrap_or(surface_id);
            state.mark_root_geometry_dirty(root);
            if let Some(bounds) = state.surface_scene_bounds(surface_id) {
                crate::drm::request_outputs_for_rect(state, bounds, dispatcher);
            }
        }
    }

    pub fn client_id(&self) -> ClientId {
        self.id
    }

    pub(crate) fn last_xwayland_serial(&self) -> u64 {
        self.last_xwayland_serial
    }

    pub(crate) fn set_last_xwayland_serial(&mut self, serial: u64) {
        self.last_xwayland_serial = serial;
    }

    /// Send `wl_display.error` for `object_id` with a protocol error `code`, then disconnect the client.
    pub fn protocol_violation_disconnect(
        &mut self,
        dispatcher: &mut ClientDispatcher,
        object_id: WaylandId,
        code: u32,
        message: impl AsRef<str>,
    ) {
        dispatcher.send_wl_display_error(object_id, code, message.as_ref());
        dispatcher.disconnect_client(self.id);
    }

    /// Send `wl_display.error` without disconnecting (rare; prefer [`Self::protocol_violation_disconnect`]).
    pub fn send_protocol_error(
        &self,
        dispatcher: &mut ClientDispatcher,
        object_id: WaylandId,
        code: u32,
        message: impl AsRef<str>,
    ) {
        dispatcher.send_wl_display_error(object_id, code, message.as_ref());
    }

    fn read_process_info(&mut self) {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as u32;
        let result = unsafe {
            getsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut _,
                &mut len as *mut _,
            )
        };
        if result == -1 {
            let e = std::io::Error::last_os_error();
            log::error!("Unable to `getsockopt` of new client: {:?}", e);
            return;
        }
        if let Some(pid) = self.pid_override {
            cred.pid = pid;
        }
        let command = match std::fs::read(format!("/proc/{}/cmdline", cred.pid)) {
            Ok(cmdline) if !cmdline.is_empty() => {
                let argv = cmdline
                    .split(|byte| *byte == 0)
                    .filter(|arg| !arg.is_empty())
                    .map(String::from_utf8_lossy)
                    .map(|arg| arg.into_owned())
                    .collect::<Vec<_>>();
                if argv.is_empty() {
                    None
                } else {
                    Some(argv.join(" "))
                }
            }
            Ok(_) | Err(_) => match std::fs::read(format!("/proc/{}/comm", cred.pid)) {
                Ok(comm) => Some(String::from_utf8_lossy(&comm).trim().to_string()),
                Err(e) => {
                    log::error!("Unable to read process name of new client: {:?}", e);
                    None
                }
            },
        };
        let Some(command) = command else {
            return;
        };
        let command = Rc::from(command);

        self.info = Some(Rc::new(ProcessInfo { command, cred }))
    }

    pub fn ref_object_mut<'a, K>(&self, id: K, state: &'a mut State) -> &'a mut K::Object
    where
        K: IdToObject,
    {
        let id: WaylandId = id.into();
        let obj_id = self.objects[&id].object_id;
        state.objects[obj_id].downcast_mut().unwrap()
    }

    pub fn ref_object<'a, K>(&self, id: K, state: &'a State) -> &'a K::Object
    where
        K: IdToObject,
    {
        let id: WaylandId = id.into();
        let obj_id = self.objects[&id].object_id;
        state.objects[obj_id].downcast_ref().unwrap()
    }

    pub fn with_ungrabbed<O: ObjectWithId, R>(
        &self,
        object: GrabbedObject<O>,
        state: &mut State,
        fun: impl FnOnce(&mut State) -> R,
    ) -> (GrabbedObject<O>, R) {
        let object_id = self.resolve_this(&object, state);
        drop(object);
        let result = fun(state);
        let object = object_id.grab(state);
        (object, result)
    }

    pub fn with_ungrabbed_mut<O: ObjectWithId, R>(
        &mut self,
        object: GrabbedObject<O>,
        state: &mut State,
        fun: impl FnOnce(&mut State, &mut Client) -> R,
    ) -> (GrabbedObject<O>, R) {
        let object_id = self.resolve_this(&object, state);
        drop(object);
        let result = fun(state, self);
        let object = object_id.grab(state);
        (object, result)
    }

    pub fn resolve_this<O: ObjectWithId>(
        &self,
        this: &GrabbedObject<O>,
        state: &State,
    ) -> Resolved<O::Id> {
        self.resolve::<O::Id>(O::Id::from(this.id()), state)
            .unwrap()
    }

    /// Resolve a client-visible Wayland id to a live compositor object of the
    /// expected interface type.
    pub fn resolve<K>(&self, id: K, state: &State) -> Option<Resolved<K>>
    where
        K: IdToObject,
    {
        let wayland_id: WaylandId = id.into();
        let entry = self.objects.get(&wayland_id)?;
        (entry.type_id == TypeId::of::<K::Object>() && state.objects.contains(entry.object_id))
            .then_some(Resolved {
                client_id: self.id,
                wayland_id: id,
                object_id: entry.object_id,
            })
    }

    pub fn resolve_or_disconnect<K>(
        &mut self,
        id: K,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<Resolved<K>, AbortRequest>
    where
        K: IdToObject,
        K::Object: Constants,
    {
        let Some(resolved) = self.resolve(id, state) else {
            let requester_object_id = dispatcher
                .requester_object_id
                .expect("missing requester object id");
            self.protocol_violation_disconnect(
                dispatcher,
                requester_object_id,
                wl_display::Error::InvalidObject as u32,
                format!("requires a live {}", K::Object::INTERFACE_NAME),
            );
            return Err(AbortRequest::InvalidObject);
        };
        Ok(resolved)
    }

    /// Resolve a client-visible Wayland id to our internal `ObjectId` if the id
    /// is still live in both the per-client object map and `State::objects`.
    ///
    /// This is useful for request validation paths where we must distinguish
    /// "the client sent an unknown/deleted object id" from "the id exists but is
    /// the wrong interface", without panicking on a typed downcast.
    pub fn try_object_id<K>(&self, id: K, state: &State) -> Option<ObjectId>
    where
        K: Into<WaylandId>,
    {
        let id: WaylandId = id.into();
        let object_id = self.objects.get(&id)?.object_id;
        state.objects.contains(object_id).then_some(object_id)
    }

    /// Deprecated: Use `iter_resolved_of()` instead.
    fn iter_object_ids_of<'a, T: Object>(
        &'a self,
        state: &'a State,
    ) -> impl Iterator<Item = ObjectId> + 'a {
        let type_id = TypeId::of::<T>();
        self.objects.values().filter_map(move |entry| {
            (entry.type_id == type_id && state.objects.contains(entry.object_id))
                .then_some(entry.object_id)
        })
    }

    fn iter_resolved_of<'a, K>(&'a self, state: &'a State) -> impl Iterator<Item = Resolved<K>> + 'a
    where
        K: IdToObject,
    {
        let type_id = TypeId::of::<K::Object>();
        self.objects.iter().filter_map(move |(wayland_id, entry)| {
            (entry.type_id == type_id && state.objects.contains(entry.object_id))
                .then_some(Resolved::new(self.id, K::from(*wayland_id), entry.object_id))
        })
    }

    /// Return whether a client-visible Wayland id currently refers to a live
    /// object of the expected interface type `K::Object`.
    ///
    /// This is intentionally stricter than `contains_object()`: it checks both
    /// liveness and the concrete object type, which lets protocol handlers reject
    /// malformed ids with a protocol error instead of panicking in `get_object()`.
    pub fn object_is<K>(&self, id: K, state: &State) -> bool
    where
        K: IdToObject,
    {
        self.resolve(id, state).is_some()
    }

    pub fn grab_object<K>(&self, id: K, state: &mut State) -> GrabbedObject<K::Object>
    where
        K: IdToObject,
    {
        let id: WaylandId = id.into();
        let obj_id = self.objects[&id].object_id;
        state.objects.grab_object(obj_id)
    }

    pub fn object_id<K>(&self, id: K) -> ObjectId
    where
        K: Into<WaylandId>,
    {
        let id: WaylandId = id.into();
        self.objects[&id].object_id
    }

    pub fn object_id_for_wayland<K: Into<WaylandId>>(&self, id: K) -> Option<ObjectId> {
        self.objects.get(&id.into()).map(|entry| entry.object_id)
    }

    pub fn allocate_server_object_id<I>(&mut self) -> I
    where
        I: From<u32>,
    {
        loop {
            let next = self.next_server_id;
            self.next_server_id = self
                .next_server_id
                .checked_add(1)
                .expect("per-client server object id space exhausted");

            let wayland_id = WaylandId::from(next);
            if self.objects.contains_key(&wayland_id) {
                continue;
            }

            return I::from(next);
        }
    }

    pub fn insert_object<'a, O>(
        &mut self,
        state: &'a mut State,
        object: O,
    ) -> (Resolved<O::Id>, &'a mut O)
    where
        O: ObjectWithId,
    {
        let (wayland_id, object_id) = state.insert_object(object);
        self.objects
            .insert(wayland_id, ClientObjectEntry { type_id: TypeId::of::<O>(), object_id });
        let object: &mut O = state.objects[object_id].downcast_mut::<O>().unwrap();
        let id = O::Id::from(wayland_id);
        (Resolved::new(self.client_id(), id, object_id), object)
    }

    pub fn contains_object<K>(&self, id: K, state: &State) -> bool
    where
        K: IdToObject,
    {
        let id: WaylandId = id.into();
        let Some(obj_id) = self.objects.get(&id).map(|entry| entry.object_id) else {
            return false;
        };
        state.objects.contains(obj_id)
    }

    pub fn delete_id(
        &mut self,
        wl_display: &WlDisplay,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        id: WaylandId,
    ) {
        if is_server_side_removal_of_server_id(id, dispatcher.requester_object_id()) {
            log::warn!(
                "removing server-allocated object {id:?} of client {:?} outside its own request; \
                 the client still holds it and its next request on it will be a fatal error",
                self.id,
            );
        }
        let object_id = self.objects.remove(&id).unwrap().object_id;
        state.remove_object(object_id);
        // `delete_id` exists so the client can reuse *client-allocated* ids. Server-created
        // objects (ids from MIN_SERVER_ID up) are never announced with it: libwayland's
        // client drops its map entry when the proxy is destroyed, so a later delete_id
        // references an unknown id, which some clients (Firefox) treat as fatal.
        if u32::from(id) < MIN_SERVER_ID {
            wl_display.send_event(wl_display::Event::DeleteId { id: id.into() }, dispatcher);
        }
    }

    pub(crate) fn dismiss_popup_ids(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        popups: Vec<Resolved<XdgPopupId>>,
        detached_parent_layer_surface_id: Option<ZwlrLayerSurfaceV1Id>,
    ) {
        for popup in popups.into_iter().rev() {
            if !popup.exists(state) {
                continue;
            }

            crate::seat::remove_popup_grab(state, dispatcher, popup, None);

            let (xdg_surface_id, parent_surface_id, parent_layer_surface) = {
                let popup = popup.rref(state);
                (popup.xdg_surface(), popup.parent_surface(), popup.parent_layer_surface())
            };

            if let Some(parent_surface_id) = parent_surface_id {
                if parent_surface_id.exists(state) {
                    let mut parent_surface = parent_surface_id.grab(state);
                    parent_surface.remove_popup_child(popup);
                }
            }
            if let Some(parent_layer_surface) = parent_layer_surface {
                if Some(parent_layer_surface.wayland_id()) != detached_parent_layer_surface_id
                    && parent_layer_surface.exists(state)
                {
                    let mut parent_surface = parent_layer_surface.grab(state);
                    parent_surface.remove_popup_child(popup);
                }
            }

            if xdg_surface_id.exists(state) {
                let wl_surface_id = {
                    let xdg_surface = xdg_surface_id.rref(state);
                    xdg_surface.wl_surface().wayland_id()
                };
                if self.contains_object(wl_surface_id, state) {
                    let mut wl_surface = self.grab_object(wl_surface_id, state);
                    wl_surface.force_unmap_for_role_dismissal(self, state, dispatcher);
                }
            }

            {
                let mut popup = popup.grab(state);
                popup.dismiss(dispatcher);
            }
        }
    }

    /// Scale 1 keeps the `f32` bits. Any other scale multiplies exactly.
    fn wire_coord(scale: WireScale, value: f32) -> f32 {
        if scale.is_one() {
            value
        } else {
            scale.to_wire_f64(f64::from(value)) as f32
        }
    }

    fn wire_point(scale: WireScale, x: f32, y: f32) -> (f32, f32) {
        (Self::wire_coord(scale, x), Self::wire_coord(scale, y))
    }

    /// Continuous axis distance is multiplied. Wheel steps (`axis_discrete` and
    /// `axis_value120`) stay in hardware units.
    fn wire_scroll_frame(
        scale: WireScale,
        mut frame: crate::seat::PointerScrollFrame,
    ) -> crate::seat::PointerScrollFrame {
        if scale.is_one() {
            return frame;
        }
        frame.vertical = frame
            .vertical
            .map(|change| Self::wire_axis_change(scale, change));
        frame.horizontal = frame
            .horizontal
            .map(|change| Self::wire_axis_change(scale, change));
        frame
    }

    fn wire_axis_change(
        scale: WireScale,
        change: crate::seat::PointerAxisChange,
    ) -> crate::seat::PointerAxisChange {
        match change {
            crate::seat::PointerAxisChange::Scroll {
                value,
                discrete,
                value120,
                relative_direction,
            } => crate::seat::PointerAxisChange::Scroll {
                value: Self::wire_coord(scale, value),
                discrete,
                value120,
                relative_direction,
            },
            stop @ crate::seat::PointerAxisChange::Stop => stop,
        }
    }

    fn handle_seat_event(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        event: SeatClientEvent,
    ) {
        if crate::logger::ENABLE_NOISY_LOG {
            log::debug!("client {:?} seat event {:?}", self.id, event);
        }
        // Surface-local pointer values are logical. A wire-scaled client
        // receives them multiplied by S. wl_touch is not advertised. Tablet,
        // gestures and wp_pointer_warp do not exist, so there is nothing else
        // to scale on the way out.
        let scale = dispatcher.wire_scale();
        match event {
            SeatClientEvent::PointerEnter { serial, surface, x, y } => {
                let (x, y) = Self::wire_point(scale, x, y);
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    pointer
                        .ref_mut(state)
                        .send_enter(serial, surface, x, y, dispatcher);
                }
            }
            SeatClientEvent::PointerLeave { serial, surface } => {
                if surface.exists(state) {
                    for pointer in state.seat.pointer_resources(self.id) {
                        if !pointer.exists(state) {
                            continue;
                        }
                        state.seat.record_client_serial(self.id, serial);
                        pointer
                            .ref_mut(state)
                            .send_leave(serial, surface, dispatcher);
                    }
                }
            }
            SeatClientEvent::PointerMotion { time, x, y } => {
                let (x, y) = Self::wire_point(scale, x, y);
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    pointer.ref_mut(state).send_motion(time, x, y, dispatcher);
                }
            }
            SeatClientEvent::PointerWarp { time, x, y } => {
                let (x, y) = Self::wire_point(scale, x, y);
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    let pointer = pointer.ref_mut(state);
                    if pointer.version() >= wl_pointer::WARP_SINCE {
                        pointer.send_warp(x, y, dispatcher);
                    } else {
                        pointer.send_motion(time, x, y, dispatcher);
                    }
                }
            }
            SeatClientEvent::PointerButton { serial, time, button, state: button_state } => {
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    pointer.ref_mut(state).send_button(
                        serial,
                        time,
                        button,
                        button_state,
                        dispatcher,
                    );
                }
            }
            SeatClientEvent::PointerLockLocked { lock } => {
                if let Some(lock) = lock.try_ref(state) {
                    lock.send_locked(dispatcher);
                }
            }
            SeatClientEvent::PointerLockUnlocked { lock } => {
                if let Some(lock) = lock.try_ref(state) {
                    lock.send_unlocked(dispatcher);
                }
            }
            SeatClientEvent::PointerConfineConfined { confine } => {
                if let Some(confine) = confine.try_ref(state) {
                    confine.send_confined(dispatcher);
                }
            }
            SeatClientEvent::PointerConfineUnconfined { confine } => {
                if let Some(confine) = confine.try_ref(state) {
                    confine.send_unconfined(dispatcher);
                }
            }
            SeatClientEvent::PointerFrame => {
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    pointer.ref_mut(state).send_frame(dispatcher);
                }
            }
            SeatClientEvent::RelativePointerMotion {
                utime_hi,
                utime_lo,
                dx,
                dy,
                dx_unaccel,
                dy_unaccel,
            } => {
                let (dx, dy) = Self::wire_point(scale, dx, dy);
                let (dx_unaccel, dy_unaccel) = Self::wire_point(scale, dx_unaccel, dy_unaccel);
                for relative in state.seat.relative_pointers_for_client(self.id) {
                    if !relative.exists(state) {
                        continue;
                    }
                    relative.ref_mut(state).send_relative_motion(
                        utime_hi, utime_lo, dx, dy, dx_unaccel, dy_unaccel, dispatcher,
                    );
                }
            }
            SeatClientEvent::PointerScroll(frame) => {
                let frame = Self::wire_scroll_frame(scale, frame);
                for pointer in state.seat.pointer_resources(self.id) {
                    if !pointer.exists(state) {
                        continue;
                    }
                    pointer.ref_mut(state).send_scroll(&frame, dispatcher);
                }
            }
            SeatClientEvent::KeyboardEnter { serial, surface, keys } => {
                for keyboard in state.seat.keyboard_resources(self.id) {
                    if !keyboard.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    keyboard
                        .ref_mut(state)
                        .send_enter(serial, surface, keys.clone(), dispatcher);
                }
            }
            SeatClientEvent::KeyboardLeave { serial, surface } => {
                if surface.exists(state) {
                    for keyboard in state.seat.keyboard_resources(self.id) {
                        if !keyboard.exists(state) {
                            continue;
                        }
                        state.seat.record_client_serial(self.id, serial);
                        keyboard
                            .ref_mut(state)
                            .send_leave(serial, surface, dispatcher);
                    }
                }
            }
            SeatClientEvent::KeyboardKey { serial, time, key, state: key_state } => {
                for keyboard in state.seat.keyboard_resources(self.id) {
                    if !keyboard.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    keyboard
                        .ref_mut(state)
                        .send_key(serial, time, key, key_state, dispatcher);
                }
            }
            SeatClientEvent::KeyboardKeymap {
                serial,
                surface,
                keymap,
                keys,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                let held = !keys.0.is_empty() && surface.exists(state);
                for keyboard in state.seat.keyboard_resources(self.id) {
                    if !keyboard.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    let keyboard = keyboard.ref_mut(state);
                    if held {
                        keyboard.send_leave(serial, surface, dispatcher);
                    }
                    keyboard.send_keymap_blob(&keymap, dispatcher);
                    if held {
                        keyboard.send_enter(serial, surface, keys.clone(), dispatcher);
                    }
                    keyboard.send_modifiers(
                        serial,
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        group,
                        dispatcher,
                    );
                }
            }
            SeatClientEvent::KeyboardModifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                for keyboard in state.seat.keyboard_resources(self.id) {
                    if !keyboard.exists(state) {
                        continue;
                    }
                    state.seat.record_client_serial(self.id, serial);
                    keyboard.ref_mut(state).send_modifiers(
                        serial,
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        group,
                        dispatcher,
                    );
                }
            }
        }
    }

    /// Detach a drag offer from the drag and its source without removing the
    /// Wayland object. Offers have server-allocated ids, so the client is never
    /// told about a removal (no `delete_id`); requests it already sent, or sends
    /// before it handles `leave`, must still find the object. The object is
    /// removed when the client sends `wl_data_offer.destroy`.
    pub(crate) fn detach_drag_offer(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        offer: Resolved<WlDataOfferId>,
    ) {
        if offer.client_id() != self.id {
            return;
        }
        if !offer.exists(state) {
            return;
        }
        let Some(mut offer_obj) = offer.try_grab(state) else {
            return;
        };
        if offer_obj.is_detached() {
            return;
        }
        if let Some(source) = offer_obj.source_object_id() {
            state.unregister_data_offer_for_source(source, offer);
        }
        offer_obj.before_destroy(self.id, offer, state, dispatcher);
        offer_obj.mark_detached();
    }

    fn create_data_offer_for_device(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        device_id: crate::protocol_generated::WlDataDeviceId,
        source: Resolved<WlDataSourceId>,
        role: DataOfferRole,
    ) -> Option<Resolved<WlDataOfferId>> {
        if !self.contains_object(device_id, state) {
            return None;
        }
        let (device_version, previous_chosen_action) = {
            let device = self.ref_object(device_id, state);
            (
                device.version(),
                state
                    .seat
                    .active_drag()
                    .map(|drag| drag.negotiation.chosen_action)
                    .unwrap_or(0),
            )
        };
        let mime_types = SelectionSourceState::WlData(source).mime_types(state)?;
        let source_actions = source.try_ref(state)?.effective_dnd_actions();
        let chosen_action = if role == DataOfferRole::Drag
            && device_version < wl_data_offer::SOURCE_ACTIONS_SINCE
        {
            if previous_chosen_action != 0 {
                previous_chosen_action
            } else if source_actions & DndAction::Copy as u32 != 0 {
                DndAction::Copy as u32
            } else {
                0
            }
        } else {
            previous_chosen_action
        };

        let offer_id = self.allocate_server_object_id::<WlDataOfferId>();
        let offer = WlDataOffer::new(offer_id, device_version, role, Some(source));
        let (resolved_offer, _) = self.insert_object(state, offer);
        state.register_data_offer_for_source(source.object_id(), resolved_offer);
        {
            let device = self.ref_object(device_id, state);
            device.send_data_offer(resolved_offer, dispatcher);
        }
        for mime_type in mime_types {
            let offer = self.ref_object(offer_id, state);
            offer.send_offer(mime_type, dispatcher);
        }
        if role == DataOfferRole::Drag && device_version >= wl_data_offer::SOURCE_ACTIONS_SINCE {
            let offer = self.ref_object_mut(offer_id, state);
            offer.send_source_actions(source_actions, dispatcher);
            if chosen_action != 0 {
                offer.send_action(chosen_action, dispatcher);
            }
        }
        Some(resolved_offer)
    }

    fn refresh_data_device_selection(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let has_keyboard_focus = state
            .seat
            .keyboard_focus()
            .is_some_and(|focus| focus.client_id() == self.id);
        let selection_source = has_keyboard_focus
            .then(|| state.seat.selection_source())
            .flatten();

        for data_device in state.seat.data_device_resources(self.id) {
            if !data_device.exists(state) {
                continue;
            }

            let previous_offer = {
                let data_device = data_device.ref_mut(state);
                data_device.take_selection_offer()
            };
            if let Some(previous_offer) = previous_offer {
                WlDataOffer::make_selection_offer_inert(previous_offer, state);
            }

            let next_offer = selection_source.and_then(|selection| {
                self.create_selection_data_offer(
                    state,
                    dispatcher,
                    data_device.wayland_id(),
                    selection,
                )
            });

            let data_device = data_device.ref_mut(state);
            data_device.set_selection_offer(next_offer);
            data_device.send_selection(next_offer, dispatcher);
        }
    }

    fn create_selection_data_offer(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        device_id: crate::protocol_generated::WlDataDeviceId,
        selection: SelectionSourceState,
    ) -> Option<Resolved<WlDataOfferId>> {
        if !self.contains_object(device_id, state) {
            return None;
        }
        let device_version = self.ref_object(device_id, state).version();
        let mime_types = selection.mime_types(state)?;
        let offer_id = self.allocate_server_object_id::<WlDataOfferId>();
        let offer = WlDataOffer::from_selection(offer_id, device_version, selection);
        let (resolved_offer, _) = self.insert_object(state, offer);
        state.register_data_offer_for_source(selection.object_id(), resolved_offer);
        {
            let device = self.ref_object(device_id, state);
            device.send_data_offer(resolved_offer, dispatcher);
        }
        for mime_type in mime_types {
            self.ref_object(offer_id, state)
                .send_offer(mime_type, dispatcher);
        }
        Some(resolved_offer)
    }

    fn refresh_data_control_selection(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let selection = state.seat.selection_source();
        for device in state.seat.data_control_devices(self.id) {
            if !device.exists(state) || device.rref(state).is_inert() {
                continue;
            }
            let previous_offer = device.ref_mut(state).take_selection_offer();
            if let Some(previous_offer) = previous_offer {
                inert_data_control_offer(previous_offer, state);
            }
            let next_offer = selection.and_then(|selection| {
                create_data_control_offer(self, state, dispatcher, device, selection)
            });
            let device = device.ref_mut(state);
            device.set_selection_offer(next_offer);
            device.send_selection(next_offer, dispatcher);
        }
    }

    fn refresh_data_control_primary_selection(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let selection = state.seat.primary_selection_source();
        for device in state.seat.data_control_devices(self.id) {
            if !device.exists(state) || device.rref(state).is_inert() {
                continue;
            }
            if device.rref(state).version()
                < crate::protocol_generated::zwlr_data_control_device_v1::PRIMARY_SELECTION_SINCE
            {
                continue;
            }
            let previous_offer = device.ref_mut(state).take_primary_offer();
            if let Some(previous_offer) = previous_offer {
                inert_data_control_offer(previous_offer, state);
            }
            let next_offer = selection.and_then(|selection| {
                create_data_control_primary_offer(self, state, dispatcher, device, selection)
            });
            let device = device.ref_mut(state);
            device.set_primary_offer(next_offer);
            device.send_primary_selection(next_offer, dispatcher);
        }
    }

    fn refresh_ext_data_control_selection(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let selection = state.seat.selection_source();
        for device in state.seat.ext_data_control_devices(self.id) {
            if !device.exists(state) || device.rref(state).is_inert() {
                continue;
            }
            let previous_offer = device.ref_mut(state).take_selection_offer();
            if let Some(previous_offer) = previous_offer {
                inert_ext_data_control_offer(previous_offer, state);
            }
            let next_offer = selection.and_then(|selection| {
                create_ext_data_control_offer(self, state, dispatcher, device, selection)
            });
            let device = device.ref_mut(state);
            device.set_selection_offer(next_offer);
            device.send_selection(next_offer, dispatcher);
        }
    }

    fn refresh_ext_data_control_primary_selection(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let selection = state.seat.primary_selection_source();
        for device in state.seat.ext_data_control_devices(self.id) {
            if !device.exists(state) || device.rref(state).is_inert() {
                continue;
            }
            let previous_offer = device.ref_mut(state).take_primary_offer();
            if let Some(previous_offer) = previous_offer {
                inert_ext_data_control_offer(previous_offer, state);
            }
            let next_offer = selection.and_then(|selection| {
                create_ext_data_control_primary_offer(self, state, dispatcher, device, selection)
            });
            let device = device.ref_mut(state);
            device.set_primary_offer(next_offer);
            device.send_primary_selection(next_offer, dispatcher);
        }
    }

    fn refresh_primary_selection(&mut self, state: &mut State, dispatcher: &mut ClientDispatcher) {
        let has_keyboard_focus = state
            .seat
            .keyboard_focus()
            .is_some_and(|focus| focus.client_id() == self.id);
        let selection = has_keyboard_focus
            .then(|| state.seat.primary_selection_source())
            .flatten();

        for device in state.seat.primary_selection_devices(self.id) {
            if !device.exists(state) {
                continue;
            }
            let previous_offer = device.ref_mut(state).take_selection_offer();
            if let Some(previous_offer) = previous_offer {
                inert_primary_selection_offer(previous_offer, state);
            }
            let next_offer = selection.and_then(|selection| {
                create_primary_selection_offer(self, state, dispatcher, device, selection)
            });
            let device = device.ref_mut(state);
            device.set_selection_offer(next_offer);
            device.send_selection(next_offer, dispatcher);
        }
    }

    fn handle_data_device_drag_enter(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        x: f32,
        y: f32,
        source: Option<Resolved<WlDataSourceId>>,
    ) {
        let has_legacy_device = state
            .seat
            .data_device_resources(self.id)
            .into_iter()
            .filter(|data_device| data_device.exists(state))
            .any(|data_device| {
                data_device.rref(state).version() < wl_data_offer::SOURCE_ACTIONS_SINCE
            });
        if has_legacy_device
            && state
                .seat
                .active_drag()
                .is_some_and(|drag| drag.negotiation.chosen_action == 0)
        {
            let source_actions = source
                .and_then(|source| {
                    source
                        .try_ref(state)
                        .map(|source| source.effective_dnd_actions())
                })
                .unwrap_or(0);
            if source_actions & DndAction::Copy as u32 != 0 {
                let _ = state
                    .seat
                    .set_active_drag_chosen_action(self.id, DndAction::Copy as u32);
            }
        }
        for data_device in state.seat.data_device_resources(self.id) {
            if !data_device.exists(state) {
                continue;
            }
            let device_version = data_device.rref(state).version();

            let previous_offer = {
                let data_device = data_device.ref_mut(state);
                data_device.take_drag_offer()
            };
            if let Some(previous_offer) = previous_offer {
                self.detach_drag_offer(state, dispatcher, previous_offer);
            }

            let offer = source.and_then(|source| {
                self.create_data_offer_for_device(
                    state,
                    dispatcher,
                    data_device.wayland_id(),
                    source,
                    DataOfferRole::Drag,
                )
            });
            if let Some(offer) = offer {
                let _ = state.seat.register_active_drag_offer(
                    self.id,
                    offer,
                    device_version >= wl_data_offer::ACTION_SINCE,
                );
            }

            let data_device = data_device.ref_mut(state);
            data_device.set_drag_offer(offer);
            data_device.send_enter(serial, surface, x, y, offer, dispatcher);
        }
    }

    fn handle_data_device_drag_leave(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        for data_device in state.seat.data_device_resources(self.id) {
            if !data_device.exists(state) {
                continue;
            }
            let previous_offer = {
                let data_device = data_device.ref_mut(state);
                data_device.send_leave(dispatcher);
                data_device.take_drag_offer()
            };
            if let Some(previous_offer) = previous_offer {
                self.detach_drag_offer(state, dispatcher, previous_offer);
            }
        }
    }

    fn handle_data_device_drag_motion(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        time: u32,
        x: f32,
        y: f32,
    ) {
        for data_device in state.seat.data_device_resources(self.id) {
            if !data_device.exists(state) {
                continue;
            }
            data_device.rref(state).send_motion(time, x, y, dispatcher);
        }
    }

    fn handle_data_device_drag_drop(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        for data_device in state.seat.data_device_resources(self.id) {
            if !data_device.exists(state) {
                continue;
            }
            data_device.rref(state).send_drop(dispatcher);
        }

        if let Some(drag) = state.seat.active_drag() {
            if drag
                .target
                .is_some_and(|target| target.client_id() == self.id)
            {
                if let Some(source) = drag.source.resolved() {
                    if drag.negotiation.chosen_action == 0 {
                        return;
                    }
                    dispatcher.dispatch_to(
                        source.client_id(),
                        ClientAction::DataSourceEvent {
                            source,
                            event: DataSourceClientEvent::Action {
                                dnd_action: drag.negotiation.chosen_action,
                            },
                        },
                    );
                }
            }
        }
    }

    fn handle_data_control_source_event(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        source: Resolved<ZwlrDataControlSourceV1Id>,
        event: DataControlSourceEvent,
    ) {
        if !source.exists(state) {
            if let DataControlSourceEvent::Send { fd, .. } = event {
                drop(fd);
            }
            return;
        }
        let source = source.ref_mut(state);
        match event {
            DataControlSourceEvent::Send { mime_type, fd } => {
                source.send_send(mime_type, fd, dispatcher);
            }
            DataControlSourceEvent::Cancelled => source.send_cancelled(dispatcher),
        }
    }

    fn handle_ext_data_control_source_event(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        source: Resolved<ExtDataControlSourceV1Id>,
        event: ExtDataControlSourceEvent,
    ) {
        if !source.exists(state) {
            if let ExtDataControlSourceEvent::Send { fd, .. } = event {
                drop(fd);
            }
            return;
        }
        let source = source.ref_mut(state);
        match event {
            ExtDataControlSourceEvent::Send { mime_type, fd } => {
                source.send_send(mime_type, fd, dispatcher);
            }
            ExtDataControlSourceEvent::Cancelled => source.send_cancelled(dispatcher),
        }
    }

    fn handle_primary_selection_source_event(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        source: Resolved<ZwpPrimarySelectionSourceV1Id>,
        event: PrimarySelectionSourceEvent,
    ) {
        if !source.exists(state) {
            if let PrimarySelectionSourceEvent::Send { fd, .. } = event {
                drop(fd);
            }
            return;
        }
        let source = source.ref_mut(state);
        match event {
            PrimarySelectionSourceEvent::Send { mime_type, fd } => {
                source.send_send(mime_type, fd, dispatcher);
            }
            PrimarySelectionSourceEvent::Cancelled => source.send_cancelled(dispatcher),
        }
    }

    fn handle_data_source_event(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        source: Resolved<WlDataSourceId>,
        event: DataSourceClientEvent,
    ) {
        if !source.exists(state) {
            if let DataSourceClientEvent::Send { fd, .. } = event {
                drop(fd);
            }
            return;
        };
        let source = source.ref_mut(state);
        match event {
            DataSourceClientEvent::Target(mime_type) => {
                source.send_target(mime_type, dispatcher);
            }
            DataSourceClientEvent::Send { mime_type, fd } => {
                source.send_send(mime_type, fd, dispatcher);
            }
            DataSourceClientEvent::Cancelled => {
                source.send_cancelled(dispatcher);
            }
            DataSourceClientEvent::DndDropPerformed => {
                source.send_dnd_drop_performed(dispatcher);
            }
            DataSourceClientEvent::DndFinished => {
                source.send_dnd_finished(dispatcher);
            }
            DataSourceClientEvent::Action { dnd_action } => {
                source.send_action(dnd_action, dispatcher);
            }
        }
    }

    fn complete_surface_frame_callbacks(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        surfaces: Vec<Resolved<WlSurfaceId>>,
        time_msec: u32,
    ) {
        let display = WlDisplay::default();
        for surface in surfaces {
            if !surface.exists(state) {
                continue;
            }
            let callbacks = {
                let Some(mut surface) = surface.try_grab(state) else {
                    continue;
                };
                surface.take_frame_callbacks()
            };
            for callback_id in callbacks {
                if !callback_id.exists(state) {
                    continue;
                }
                {
                    let callback = callback_id.rref(state);
                    callback.done(time_msec, dispatcher);
                }
                self.delete_id(&display, state, dispatcher, callback_id.wayland_id().into());
            }
        }
    }

    fn output_resources_for_feedback(&self, state: &State, output_id: OutputId) -> Vec<WaylandId> {
        let mut resources = Vec::new();
        for wl_output in self.iter_resolved_of::<WlOutputId>(state) {
            let wl_output = wl_output.rref(state);
            if wl_output.matches_output(output_id) {
                resources.push(wl_output.wl_output_id().into());
            }
        }
        resources
    }

    fn present_feedback(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        feedbacks: Vec<Resolved<WpPresentationFeedbackId>>,
        output_id: Option<OutputId>,
        tv_sec_hi: u32,
        tv_sec_lo: u32,
        tv_nsec: u32,
        refresh: u32,
        seq_hi: u32,
        seq_lo: u32,
        flags: u32,
    ) {
        let display = WlDisplay::default();
        let sync_outputs =
            output_id.map(|output_id| self.output_resources_for_feedback(state, output_id));
        for feedback in feedbacks {
            if !feedback.exists(state) {
                continue;
            }
            {
                let feedback: &mut WpPresentationFeedback = feedback.ref_mut(state);
                if let Some(sync_outputs) = sync_outputs.as_ref() {
                    for output in sync_outputs {
                        feedback.send_sync_output((*output).into(), dispatcher);
                    }
                }
                feedback.send_presented(
                    tv_sec_hi, tv_sec_lo, tv_nsec, refresh, seq_hi, seq_lo, flags, dispatcher,
                );
            }
            self.delete_id(&display, state, dispatcher, feedback.wayland_id().into());
        }
    }

    fn discard_presentation_feedback(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        feedbacks: Vec<Resolved<WpPresentationFeedbackId>>,
    ) {
        let display = WlDisplay::default();
        for feedback in feedbacks {
            if !feedback.exists(state) {
                continue;
            }
            {
                let feedback = feedback.rref(state);
                feedback.send_discarded(dispatcher);
            }
            self.delete_id(&display, state, dispatcher, feedback.wayland_id().into());
        }
    }

    fn discard_surface_presentation_feedbacks_for_backend(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        drm_id: DrmId,
    ) {
        let backend = crate::state::BackendOutputRef::new(drm_id);
        let surface_ids = self
            .iter_resolved_of::<WlSurfaceId>(state)
            .collect::<Vec<_>>();
        for surface_id in surface_ids {
            if !surface_id.exists(state) {
                continue;
            }
            let mut surface = surface_id.grab(state);
            surface.discard_presentation_feedbacks_for_backend(self, state, dispatcher, backend);
        }
    }

    fn log_request<R: Parser>(&self, object_id: u32, result: Result<&R::Request, &ParserError>)
    where
        R::Request: std::fmt::Debug,
    {
        let interface_name = R::INTERFACE_NAME;
        let (proc, pid) = match self.info.as_ref() {
            Some(info) => (&*info.command, info.cred.pid),
            None => ("UNKNOWN", 0),
        };
        let id = self.id;
        let s = format!("in {:?}-{}-{}", proc, pid, id.0);
        match result {
            Ok(req) => {
                log::trace!(target: &s, "[{}] {}::{:?}", object_id, interface_name, req)
            }
            Err(err) => {
                log::error!(target: &s, "[{}] {}: {:?}", object_id, interface_name, err)
            }
        }
    }

    fn log_event<E: EventWriter + Constants>(
        writer: &E,
        event: &E::Event,
        dispatcher: &mut ClientDispatcher,
    ) {
        if log::Level::Trace > log::max_level() {
            return;
        }
        let (proc, pid) = match dispatcher.command.as_ref() {
            Some(info) => (&*info.command, info.cred.pid),
            None => ("UNKNOWN", 0),
        };
        let id = dispatcher.client_id;
        let s = format!("out {:?}-{}-{}", proc, pid, id.0);
        let skip = (!crate::logger::ENABLE_NOISY_LOG)
            .then(|| ["wl_pointer"].contains(&E::INTERFACE_NAME))
            .unwrap_or(false);
        if skip {
            return;
        }
        log::trace!(target: &s, "[{}] {}::{:?}", u32::from(writer.object_id()), E::INTERFACE_NAME, event);
    }

    /// Helper that just log the request
    pub fn parse_and_log<R: Parser>(&self, req: RawRequest) -> Result<R::Request, ParserError>
    where
        R::Request: std::fmt::Debug,
    {
        let object_id = req.object_id;
        let result = R::parse_request(req);
        if log::Level::Trace <= log::max_level() {
            self.log_request::<R>(object_id, result.as_ref());
        }
        result
    }

    /// Decode a request. On failure, post `wl_display.invalid_method` and disconnect this client.
    pub fn parse<R: Parser>(
        &mut self,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<R::Request, AbortRequest>
    where
        R::Request: std::fmt::Debug,
    {
        let object_id = req.object_id();
        match self.parse_and_log::<R>(req) {
            Ok(request) => Ok(request),
            Err(error) => {
                log::warn!(
                    "malformed {} request on object {object_id:?}: {error:?}",
                    R::INTERFACE_NAME
                );
                self.protocol_violation_disconnect(
                    dispatcher,
                    object_id,
                    wl_display::Error::InvalidMethod as u32,
                    format!("malformed request: {error:?}"),
                );
                Err(AbortRequest::Malformed)
            }
        }
    }

    fn handle_registry_global_added(
        &self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        name: u32,
        interface: String,
        version: u32,
    ) {
        let global_name = crate::globals::GlobalName::from_raw(name);
        if state.globals.is_xwayland_only(global_name) && !self.is_xwayland() {
            return;
        }
        let registries = self
            .iter_resolved_of::<WlRegistryId>(state)
            .collect::<Vec<_>>();
        for registry in registries {
            registry
                .rref(state)
                .send_global(dispatcher, name, interface.clone(), version);
            state
                .globals
                .note_offer(crate::globals::GlobalName::from_raw(name), registry);
        }
    }

    fn handle_registry_global_removed(
        &self,
        state: &State,
        dispatcher: &mut ClientDispatcher,
        name: u32,
    ) {
        let global_name = crate::globals::GlobalName::from_raw(name);
        if state.globals.is_xwayland_only(global_name) && !self.is_xwayland() {
            return;
        }
        for registry in self.iter_resolved_of::<WlRegistryId>(state) {
            if !state.globals.was_offered(global_name, registry) {
                continue;
            }
            let registry = registry.rref(state);
            registry.send_global_remove(dispatcher, name);
        }
    }

    fn handle_output_state_changed(
        &mut self,
        state: &State,
        dispatcher: &mut ClientDispatcher,
        output_id: OutputId,
    ) {
        let xdg_output_ids = self
            .iter_resolved_of::<ZxdgOutputV1Id>(state)
            .collect::<Vec<_>>();
        let wl_output_ids = self
            .iter_resolved_of::<WlOutputId>(state)
            .collect::<Vec<_>>();

        for xdg_output in xdg_output_ids {
            let xdg_output = xdg_output.rref(state);
            if xdg_output.matches_output(output_id) {
                xdg_output.send_state(state, dispatcher);
            }
        }

        for wl_output in wl_output_ids {
            let wl_output = wl_output.rref(state);
            if wl_output.matches_output(output_id) {
                wl_output.send_state(state, dispatcher);
            }
        }
    }

    pub(crate) fn output_object_ids_for_output(
        &self,
        state: &State,
        output_id: OutputId,
    ) -> Vec<Resolved<WlOutputId>> {
        let mut output_object_ids = Vec::new();
        let object_ids = self
            .iter_resolved_of::<WlOutputId>(state)
            .collect::<Vec<_>>();
        for wl_output in object_ids {
            if wl_output.rref(state).matches_output(output_id) {
                output_object_ids.push(wl_output);
            }
        }
        output_object_ids
    }

    fn send_surface_output_membership_event(
        &self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        surface: Resolved<WlSurfaceId>,
        output: Resolved<WlOutputId>,
        entered: bool,
    ) {
        log::debug!(
            "surface output event surface={:?} output={:?} entered={} wl_output_resources={:?}",
            surface.object_id(),
            output.wayland_id(),
            entered,
            [output.wayland_id()]
        );
        let Some(surface) = surface.try_ref(state) else {
            return;
        };
        if entered {
            surface.send_output_enter(output, dispatcher);
        } else {
            surface.send_output_leave(output, dispatcher);
        }
    }

    pub(crate) fn refresh_workspace_protocols(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let manager_object_ids = self
            .iter_resolved_of::<ExtWorkspaceManagerV1Id>(state)
            .collect::<Vec<_>>();
        for manager in manager_object_ids {
            let Some(mut manager) = manager.try_grab(state) else {
                continue;
            };
            manager.sync(self, state, dispatcher, true);
        }
    }

    pub(crate) fn replay_surface_output_for_output(
        &self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        output_id: OutputId,
    ) {
        let object_ids = self
            .iter_resolved_of::<WlSurfaceId>(state)
            .collect::<Vec<_>>();
        let mut surfaces = Vec::new();
        for surface in object_ids {
            let surface_ref = surface.rref(state);
            if surface_ref.contains_output(output_id) {
                surfaces.push(surface);
            }
        }
        let output_objects = self.output_object_ids_for_output(state, output_id);
        for surface in surfaces {
            for output in &output_objects {
                self.send_surface_output_membership_event(
                    state, dispatcher, surface, *output, true,
                );
            }
        }
    }

    pub(crate) fn refresh_surface_outputs(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let root_surface_ids = state.root_surfaces();
        let active_cursor_surface = active_cursor_surface_refresh_params(state, self.id);
        let active_dnd_icon_surface = active_dnd_icon_surface_refresh_params(state, self.id);
        log::debug!("refresh_surface_outputs client={:?} roots={:?}", self.id, root_surface_ids);
        for root_surface in root_surface_ids {
            self.refresh_root_surface_outputs(
                state,
                dispatcher,
                root_surface,
                active_cursor_surface,
                active_dnd_icon_surface,
            );
        }
    }

    fn refresh_root_surface_outputs(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        root_surface: Resolved<WlSurfaceId>,
        active_cursor_surface: Option<(Resolved<WlSurfaceId>, i32, i32)>,
        active_dnd_icon_surface: Option<(Resolved<WlSurfaceId>, i32, i32)>,
    ) {
        let Some(surface) = root_surface.try_ref(state) else {
            return;
        };
        log::debug!(
            "refresh_surface_outputs root candidate object={:?} client={:?} is_root={}",
            surface.object_id(),
            surface.client_id(),
            surface.is_root()
        );
        if surface.client_id() != self.id || !surface.is_root() {
            return;
        }
        if state.is_popup_root_surface(root_surface) {
            return;
        }
        if surface.has_cursor_role() {
            if let Some((active_surface, base_x, base_y)) = active_cursor_surface {
                if active_surface == root_surface {
                    refresh_surface_tree_outputs(
                        self,
                        state,
                        dispatcher,
                        root_surface,
                        base_x,
                        base_y,
                    );
                } else {
                    clear_surface_tree_outputs(self, state, dispatcher, root_surface);
                }
            } else {
                clear_surface_tree_outputs(self, state, dispatcher, root_surface);
            }
            return;
        }
        if surface.has_input_popup_role() {
            if let Some((base_x, base_y)) =
                crate::input::ime::popup_output_base(state, root_surface)
            {
                refresh_surface_tree_outputs(self, state, dispatcher, root_surface, base_x, base_y);
            } else {
                clear_surface_tree_outputs(self, state, dispatcher, root_surface);
            }
            return;
        }
        if surface.has_dnd_icon_role() {
            if let Some((active_surface, base_x, base_y)) = active_dnd_icon_surface {
                if active_surface == root_surface {
                    refresh_surface_tree_outputs(
                        self,
                        state,
                        dispatcher,
                        root_surface,
                        base_x,
                        base_y,
                    );
                } else {
                    clear_surface_tree_outputs(self, state, dispatcher, root_surface);
                }
            } else {
                clear_surface_tree_outputs(self, state, dispatcher, root_surface);
            }
            return;
        }
        if !root_has_protocol_visibility(state, root_surface) {
            clear_surface_tree_outputs(self, state, dispatcher, root_surface);
            return;
        }
        refresh_surface_tree_outputs(self, state, dispatcher, root_surface, 0, 0);
    }

    fn reconfigure_mapped_toplevels(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let opened = state.relayout_dirty_workspaces();
        if !opened.is_empty() {
            kick_visual_animation_pump(dispatcher);
        }
        let tiled_resize_surfaces = crate::seat::tiled_resize_active_root_surfaces(state);
        let toplevels = self
            .iter_resolved_of::<XdgToplevelId>(state)
            .collect::<Vec<_>>();
        for toplevel in toplevels {
            let toplevel_ref = toplevel.rref(state);
            let xdg_surface = toplevel_ref.xdg_surface();
            let has_decoration_object = toplevel_ref.has_decoration_object();
            let xdg_surface_ref = xdg_surface.rref(state);
            let wl_surface = xdg_surface_ref.wl_surface();
            let Some(root_surface) = state.root_surface_for_surface(wl_surface) else {
                continue;
            };
            if !root_has_protocol_visibility(state, root_surface) {
                continue;
            }
            let window = toplevel_ref.window();
            let tiled_geometry = state.tiled_window_geometry(window);
            let resizing = tiled_resize_surfaces.contains(&root_surface);
            let surface_configure_bounds = state.configure_bounds_for_surface(wl_surface, None);
            let scheduled_floating_restore =
                if tiled_geometry.is_none() && !state.window_is_fullscreen(window) {
                    state.take_window_floating_restore(window)
                } else {
                    None
                };
            let desired_decoration_mode =
                Some(state.xdg_decoration_mode_for_surface(has_decoration_object));

            {
                let mut xdg_surface = xdg_surface.grab(state);
                let mut toplevel = toplevel.grab(state);
                if let Some(mode) = desired_decoration_mode {
                    toplevel.schedule_decoration_mode(mode);
                }
                let tiled_configure_geometry = tiled_geometry
                    .map(|geometry| crate::root_geometry::project_configure_geometry(geometry));
                let configure_bounds = tiled_configure_geometry
                    .map(|geometry| geometry.size)
                    .or(surface_configure_bounds);
                if let Some((geometry, configure_geometry)) =
                    tiled_geometry.zip(tiled_configure_geometry)
                {
                    toplevel.send_tiled_layout_configure(
                        self,
                        state,
                        &mut xdg_surface,
                        geometry,
                        configure_geometry,
                        resizing,
                        dispatcher,
                    );
                } else if let Some(remembered_state) = scheduled_floating_restore {
                    toplevel.send_interactive_resize_configure(
                        self,
                        state,
                        configure_bounds,
                        &mut xdg_surface,
                        remembered_state.configure_size.0,
                        remembered_state.configure_size.1,
                        false,
                        dispatcher,
                    );
                } else {
                    toplevel.schedule_configure(
                        self,
                        state,
                        configure_bounds,
                        &mut xdg_surface,
                        dispatcher,
                    );
                }
            }
            if let Some(toplevel) = toplevel.try_ref(state) {
                let window = toplevel.window();
                let (maximized, fullscreen) = toplevel.published_shell_state();
                crate::capture::note_window_state(state, dispatcher, window, maximized, fullscreen);
            }
        }
        dispatcher.dispatch(crate::xwayland::XwaylandAction::Reconfigure);
    }

    #[cfg(test)]
    pub(crate) fn test_reconfigure_mapped_toplevels(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.reconfigure_mapped_toplevels(state, dispatcher);
    }

    fn handle_output_removed(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        output_id: OutputId,
    ) {
        self.invalidate_gamma_control_for_output(state, dispatcher, output_id);
        self.forget_ctm_state_for_output(state, output_id);
        self.refresh_surface_outputs(state, dispatcher);
        let wl_output_ids = self
            .iter_object_ids_of::<WlOutput>(state)
            .collect::<Vec<_>>();
        let xdg_output_ids = self
            .iter_object_ids_of::<ZxdgOutputV1>(state)
            .collect::<Vec<_>>();

        for object_id in wl_output_ids {
            let Some(object) = state.objects.try_get_mut(object_id) else {
                continue;
            };
            let Some(wl_output) = object.downcast_mut::<WlOutput>() else {
                continue;
            };
            if wl_output.matches_output(output_id) {
                wl_output.mark_removed();
            }
        }
        for object_id in xdg_output_ids {
            let Some(object) = state.objects.try_get_mut(object_id) else {
                continue;
            };
            let Some(xdg_output) = object.downcast_mut::<ZxdgOutputV1>() else {
                continue;
            };
            if xdg_output.matches_output(output_id) {
                xdg_output.mark_removed();
            }
        }
        self.reconfigure_mapped_toplevels(state, dispatcher);
        relayout_client_layer_surfaces(self, state, dispatcher);
    }

    fn invalidate_gamma_control_for_output(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        output_id: OutputId,
    ) -> bool {
        let gamma_control_ids = self
            .objects
            .iter()
            .filter_map(|(wayland_id, entry)| {
                (entry.type_id == TypeId::of::<ZwlrGammaControlV1>())
                    .then_some(ZwlrGammaControlV1Id::from(u32::from(*wayland_id)))
            })
            .collect::<Vec<_>>();
        let mut invalidated = false;
        for gamma_control_id in gamma_control_ids {
            if !self.contains_object(gamma_control_id, state) {
                continue;
            }
            let gamma_control_object_id = self.object_id(gamma_control_id);
            let matches_output = state
                .objects
                .try_ref_object::<ZwlrGammaControlV1>(gamma_control_object_id)
                .is_some_and(|gamma_control| gamma_control.controls_output(output_id));
            if !matches_output {
                continue;
            }
            let mut gamma_control = self.grab_object(gamma_control_id, state);
            gamma_control.invalidate(state, dispatcher);
            invalidated = true;
        }
        invalidated
    }

    fn forget_ctm_state_for_output(&mut self, state: &mut State, output_id: OutputId) {
        let ctm_manager_ids = self
            .objects
            .iter()
            .filter_map(|(wayland_id, entry)| {
                (entry.type_id == TypeId::of::<HyprlandCtmControlManagerV1>())
                    .then_some(HyprlandCtmControlManagerV1Id::from(u32::from(*wayland_id)))
            })
            .collect::<Vec<_>>();
        for ctm_manager_id in ctm_manager_ids {
            if !self.contains_object(ctm_manager_id, state) {
                continue;
            }
            let mut ctm_manager = self.grab_object(ctm_manager_id, state);
            ctm_manager.forget_output(output_id);
        }
    }
}

fn surface_intersecting_outputs(
    state: &State,
    render_state: &crate::wayland_objects::wl_surface::SurfaceRenderState,
) -> Vec<OutputId> {
    let Some(surface_bounds) =
        RectI::new(render_state.x, render_state.y, render_state.width, render_state.height)
    else {
        return Vec::new();
    };
    let mut outputs = state.outputs_intersecting_rect(surface_bounds);
    if let Some(primary_output_id) = state.primary_output_for_rect(surface_bounds, None) {
        outputs.sort_by_key(|output_id| (*output_id != primary_output_id, output_id.as_u64()));
    }
    outputs
}

fn refresh_surface_tree_outputs(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    surface: Resolved<WlSurfaceId>,
    base_x: i32,
    base_y: i32,
) {
    let (stack, child_base, delta) = {
        let Some(mut surface_obj) = surface.try_grab(state) else {
            return;
        };
        let render_state = surface_obj.render_state_at(base_x, base_y);
        let surface_origin = render_state
            .as_ref()
            .map(|render_state| (render_state.x, render_state.y))
            .unwrap_or_else(|| {
                let (x, y) = surface_obj.root_position();
                (base_x + x, base_y + y)
            });
        let current_position = surface_obj.current_subsurface_position();
        let next_outputs = render_state
            .as_ref()
            .map(|render_state| surface_intersecting_outputs(state, render_state))
            .unwrap_or_default();
        let child_base =
            (surface_origin.0 + current_position.0, surface_origin.1 + current_position.1);
        log::debug!(
            "surface output refresh surface={:?} render={:?} child_base={:?} next_outputs={:?}",
            surface.object_id(),
            render_state.as_ref().map(|r| (r.x, r.y, r.width, r.height)),
            child_base,
            next_outputs
        );
        let delta = surface_obj.update_output_membership(next_outputs);
        log::debug!(
            "surface output delta surface={:?} entered={:?} left={:?} active_changed={}",
            surface.object_id(),
            delta.entered,
            delta.left,
            delta.active_output_changed
        );
        (surface_obj.current_stack().to_vec(), child_base, delta)
    };
    if let Some(window) = state.window_for_root(surface) {
        crate::capture::sync_toplevel_outputs(state, dispatcher, window);
    }

    for output_id in delta.left {
        for output in client.output_object_ids_for_output(state, output_id) {
            client.send_surface_output_membership_event(state, dispatcher, surface, output, false);
        }
    }
    for output_id in delta.entered {
        for output in client.output_object_ids_for_output(state, output_id) {
            client.send_surface_output_membership_event(state, dispatcher, surface, output, true);
        }
    }

    let preferred_output = surface
        .try_ref(state)
        .and_then(|surface_ref| surface_ref.preferred_output_id_for_object(state, surface))
        .and_then(|output_id| {
            state
                .outputs
                .try_get(output_id)
                .map(|output| (output.scale, output.legacy_scale().max(1), output.transform as u32))
        });
    if let Some((fractional_scale, scale, transform)) = preferred_output {
        let Some(mut surface_obj) = surface.try_grab(state) else {
            return;
        };
        surface_obj.send_preferred_fractional_scale_for_output(fractional_scale, state, dispatcher);
        surface_obj.send_preferred_buffer_state_for_output(scale, transform, dispatcher);
    }
    crate::wayland_objects::zwp_linux_dmabuf_feedback_v1::refresh_surface_feedback(
        state, dispatcher, surface,
    );

    for entry in stack {
        if let crate::wayland_objects::wl_surface::StackEntry::Child(child_surface) = entry {
            refresh_surface_tree_outputs(
                client,
                state,
                dispatcher,
                child_surface,
                child_base.0,
                child_base.1,
            );
        }
    }
    for popup_surface in state.popup_child_surfaces(surface) {
        let Some((popup_x, popup_y)) = state.surface_scene_origin(popup_surface) else {
            continue;
        };
        refresh_surface_tree_outputs(client, state, dispatcher, popup_surface, popup_x, popup_y);
    }
}

fn clear_surface_tree_outputs(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    surface: Resolved<WlSurfaceId>,
) {
    let (stack, delta) = {
        let Some(mut surface_obj) = surface.try_grab(state) else {
            return;
        };
        let delta = surface_obj.update_output_membership(Vec::new());
        (surface_obj.current_stack().to_vec(), delta)
    };
    if let Some(window) = state.window_for_root(surface) {
        crate::capture::sync_toplevel_outputs(state, dispatcher, window);
    }

    for output_id in delta.left {
        for output in client.output_object_ids_for_output(state, output_id) {
            client.send_surface_output_membership_event(state, dispatcher, surface, output, false);
        }
    }

    for entry in stack {
        if let crate::wayland_objects::wl_surface::StackEntry::Child(child_surface) = entry {
            clear_surface_tree_outputs(client, state, dispatcher, child_surface);
        }
    }
    for popup_surface in state.popup_child_surfaces(surface) {
        clear_surface_tree_outputs(client, state, dispatcher, popup_surface);
    }
}

fn active_cursor_surface_refresh_params(
    state: &State,
    client_id: ClientId,
) -> Option<(Resolved<WlSurfaceId>, i32, i32)> {
    let (pointer_x, pointer_y) = state.seat.pointer_position();
    let crate::cursor::CursorSourceRef::Surface { cursor } = state.seat.visible_cursor_source()?
    else {
        return None;
    };
    if cursor.surface.client_id() != client_id {
        return None;
    }
    let surface = cursor.surface.try_ref(state)?;
    let (root_x, root_y) = surface.root_position();
    let base_x = pointer_x.round() as i32 - cursor.hotspot_x - root_x;
    let base_y = pointer_y.round() as i32 - cursor.hotspot_y - root_y;
    Some((cursor.surface, base_x, base_y))
}

fn active_dnd_icon_surface_refresh_params(
    state: &State,
    client_id: ClientId,
) -> Option<(Resolved<WlSurfaceId>, i32, i32)> {
    let drag = state.seat.active_drag()?;
    if drag.source.initiator_client_id() != client_id {
        return None;
    }
    let icon_surface = drag.icon_surface?;
    let surface = icon_surface.try_ref(state)?;
    let (pointer_x, pointer_y) = state.seat.pointer_position();
    let (root_x, root_y) = surface.root_position();
    Some((icon_surface, pointer_x.round() as i32 - root_x, pointer_y.round() as i32 - root_y))
}

/// A specialized `Dispatcher`
///
/// Difference is that this one contains the client fd, so we can send events
#[derive(derive_more::Deref, derive_more::DerefMut)]
pub struct ClientDispatcher<'a> {
    #[deref]
    #[deref_mut]
    dispatcher: &'a mut Dispatcher,
    client_fd: RawFd,
    client_id: ClientId,
    requester_object_id: Option<WaylandId>,
    command: Option<Rc<ProcessInfo>>,
    /// The client's wire scale, copied here so every send/request handler can read it.
    wire_scale: WireScale,
    /// Deferred work to run after the current `Object::on` handler returns and its grabbed object
    /// is released (see `MsgReceived` in `Client::process`).
    post_actions: VecDeque<Box<dyn PostAction>>,
}

impl<'a> ClientDispatcher<'a> {
    fn make(client: &Client, dispatcher: &'a mut Dispatcher) -> Self {
        let command =
            if log::Level::Trace <= log::max_level() { client.info.clone() } else { None };
        Self {
            dispatcher,
            client_fd: client.fd.as_raw_fd(),
            client_id: client.id,
            requester_object_id: None,
            command,
            wire_scale: client.wire_scale(),
            post_actions: VecDeque::new(),
        }
    }

    /// The wire scale of the client this dispatcher sends events to.
    pub fn wire_scale(&self) -> WireScale {
        self.wire_scale
    }

    pub(crate) fn requester_object_id(&self) -> Option<WaylandId> {
        self.requester_object_id
    }

    #[cfg(test)]
    pub fn set_requester_object_id_for_tests(&mut self, id: WaylandId) {
        self.requester_object_id = Some(id);
    }

    /// Enqueue work to run after the current `Object::on` handler returns and its grabbed object
    /// is released.
    pub fn dispatch_post_action<P: PostAction>(&mut self, action: P) {
        self.post_actions.push_back(Box::new(action));
    }

    /// Run all queued post-actions until the queue is empty (later actions may enqueue more).
    #[cfg(test)]
    pub fn drain_post_actions(&mut self, client: &mut Client, state: &mut State) {
        while let Some(action) = self.post_actions.pop_front() {
            action.execute(client, state, self);
        }
    }

    /// `wl_display` singleton id is always `1` in this compositor.
    pub fn send_wl_display_error(
        &mut self,
        object_id: WaylandId,
        code: u32,
        message: impl AsRef<str>,
    ) {
        // The raw event skips `EventWriter::send_event`, so the error would otherwise
        // never show up in the client trace.
        let (proc, pid) = match self.command.as_ref() {
            Some(info) => (&*info.command, info.cred.pid),
            None => ("UNKNOWN", 0),
        };
        let target = format!("out {:?}-{}-{}", proc, pid, self.client_id.0);
        log::warn!(
            target: &target,
            "[1] wl_display::Error {{ object_id: WaylandId({}), code: {}, message: {:?} }}",
            u32::from(object_id),
            code,
            message.as_ref()
        );
        let mut raw = RawEvent::new(WaylandId::from(1u32), 0, 3);
        raw.id(object_id);
        raw.uint(code);
        raw.string(message.as_ref().to_string());
        self.send_event(raw);
    }

    pub fn disconnect_client(&mut self, client_id: ClientId) {
        self.dispatch(ClientManagerAction::Close { id: client_id });
    }

    pub fn send_event(&mut self, mut event: RawEvent) {
        let fds = event.take_fds();
        let action = IoUringAction::Sendmsg {
            fd: self.client_fd,
            buffer: event.into_bytes(),
            fds,
            dispatch_to: Redispatcher::None,
        };
        self.dispatch(action);
    }
}

#[cfg(test)]
impl Client {
    pub(crate) fn test_dispatcher<'a>(
        &self,
        dispatcher: &'a mut Dispatcher,
    ) -> ClientDispatcher<'a> {
        ClientDispatcher::make(self, dispatcher)
    }
}

impl Model for Client {
    type Action = ClientAction;
    const START: Option<Self::Action> = Some(Self::Action::Start);
    const STOP: Option<Self::Action> = Some(Self::Action::Close);

    fn process(
        &mut self,
        state: &mut State,
        action: ClientAction,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        use ClientAction::*;

        let mut dispatcher = ClientDispatcher::make(self, dispatcher);

        match action {
            SetWireScale { scale } => {
                self.apply_changed_wire_scale(state, &mut dispatcher, scale);
            }
            Start => {
                self.read_process_info();
                state.set_client_process_info(self.id, self.info.clone());
                log::info!(
                    "new client_id={:?} command={:?} xwayland={}",
                    self.id,
                    self.info.as_ref().map(|p| &p.command),
                    self.xwayland
                );

                let display: WlDisplay = WlDisplay::default();
                let _ = self.insert_object(state, display);

                let (client_reader_id, client_reader) =
                    ClientReader::new(self.fd.as_raw_fd(), self.id);
                self.reader_id = Some(client_reader_id);
                dispatcher.start_model(client_reader_id, client_reader);
            }
            FdsReceived { fds } => {
                self.pending_fds.extend(fds);
            }
            SeatEvent { event } => {
                self.handle_seat_event(state, &mut dispatcher, event);
            }
            TextInputEvent { text_input, event } => {
                crate::input::ime::deliver_text_input_event(
                    state,
                    &mut dispatcher,
                    text_input,
                    event,
                );
            }
            InputMethodEvent { input_method, events } => {
                crate::input::ime::deliver_input_method_event(
                    state,
                    &mut dispatcher,
                    input_method,
                    events,
                );
            }
            KeyboardGrabEvent { grab, event } => {
                crate::wayland_objects::zwp_input_method_keyboard_grab_v2::deliver_keyboard_grab_event(
                    state,
                    &mut dispatcher,
                    grab,
                    event,
                );
            }
            InputPopupEvent { popup, cursor } => {
                crate::wayland_objects::zwp_input_popup_surface_v2::deliver_input_popup_event(
                    state,
                    &mut dispatcher,
                    popup,
                    cursor,
                );
            }
            RequestToplevelClose { toplevel } => {
                if let Some(toplevel) = toplevel.try_ref(state) {
                    toplevel.send_close(&mut dispatcher);
                }
            }
            RefreshDataDeviceSelection => {
                self.refresh_data_device_selection(state, &mut dispatcher);
            }
            RefreshDataControlSelection => {
                self.refresh_data_control_selection(state, &mut dispatcher);
            }
            RefreshDataControlPrimarySelection => {
                self.refresh_data_control_primary_selection(state, &mut dispatcher);
            }
            RefreshExtDataControlSelection => {
                self.refresh_ext_data_control_selection(state, &mut dispatcher);
            }
            RefreshExtDataControlPrimarySelection => {
                self.refresh_ext_data_control_primary_selection(state, &mut dispatcher);
            }
            DataControlSourceEvent { source, event } => {
                self.handle_data_control_source_event(state, &mut dispatcher, source, event);
            }
            ExtDataControlSourceEvent { source, event } => {
                self.handle_ext_data_control_source_event(state, &mut dispatcher, source, event);
            }
            RefreshPrimarySelection => {
                self.refresh_primary_selection(state, &mut dispatcher);
            }
            PrimarySelectionSourceEvent { source, event } => {
                self.handle_primary_selection_source_event(state, &mut dispatcher, source, event);
            }
            DataDeviceDragEnter { serial, surface, x, y, source } => {
                self.handle_data_device_drag_enter(
                    state,
                    &mut dispatcher,
                    serial,
                    surface,
                    x,
                    y,
                    source,
                );
            }
            DataDeviceDragLeave => {
                self.handle_data_device_drag_leave(state, &mut dispatcher);
            }
            DataDeviceDragMotion { time, x, y } => {
                self.handle_data_device_drag_motion(state, &mut dispatcher, time, x, y);
            }
            DataDeviceDragDrop => {
                self.handle_data_device_drag_drop(state, &mut dispatcher);
            }
            DataSourceEvent { source, event } => {
                self.handle_data_source_event(state, &mut dispatcher, source, event);
            }
            RegistryGlobalAdded { name, interface, version } => {
                self.handle_registry_global_added(state, &mut dispatcher, name, interface, version);
            }
            RegistryGlobalRemoved { name } => {
                self.handle_registry_global_removed(state, &mut dispatcher, name);
            }
            OutputStateChanged { output_id } => {
                self.handle_output_state_changed(state, &mut dispatcher, output_id);
                self.refresh_surface_outputs(state, &mut dispatcher);
                self.refresh_workspace_protocols(state, &mut dispatcher);
                self.reconfigure_mapped_toplevels(state, &mut dispatcher);
                relayout_client_layer_surfaces(self, state, &mut dispatcher);
            }
            OutputRemoved { output_id } => {
                self.handle_output_removed(state, &mut dispatcher, output_id);
                self.refresh_workspace_protocols(state, &mut dispatcher);
            }
            GammaControlFailed { output_id } => {
                if self.invalidate_gamma_control_for_output(state, &mut dispatcher, output_id)
                    && state.outputs.try_get(output_id).is_some()
                {
                    request_output(state, output_id, &mut dispatcher);
                }
            }
            RefreshLayerSurfaces => {
                relayout_client_layer_surfaces(self, state, &mut dispatcher);
                self.reconfigure_mapped_toplevels(state, &mut dispatcher);
            }
            WorkspaceStateChanged => {
                self.reconfigure_mapped_toplevels(state, &mut dispatcher);
                self.refresh_surface_outputs(state, &mut dispatcher);
                self.refresh_workspace_protocols(state, &mut dispatcher);
            }
            RefreshSurfaceOutputs => {
                self.refresh_surface_outputs(state, &mut dispatcher);
            }
            ReleaseBuffer { buffer } => {
                let Some(live) = buffer.try_ref_mut(state) else {
                    return Ok(());
                };
                live.send_release(&mut dispatcher);
            }
            CompleteBufferReleaseCallbacks { callbacks } => {
                use crate::wayland_objects::wl_display::WlDisplay;
                let display = WlDisplay::default();
                for callback in callbacks {
                    if !self.contains_object(callback.wayland_id(), state) {
                        continue;
                    }
                    callback.rref(state).done(0, &mut dispatcher);
                    self.delete_id(&display, state, &mut dispatcher, callback.wayland_id().into());
                }
            }
            CompleteSurfaceFrameCallbacks { surfaces, time_msec } => {
                self.complete_surface_frame_callbacks(state, &mut dispatcher, surfaces, time_msec);
            }
            SendSessionLockLocked => {
                if let Some(lock) = state.session_lock.lock() {
                    if lock.client_id() == self.id {
                        if let Some(lock) = lock.try_ref_mut(state) {
                            lock.send_locked(&mut dispatcher);
                        }
                    }
                }
            }
            ReconfigureLockOutput { output_id } => {
                let Some(surface_id) = state.session_lock.surface_for_output(output_id) else {
                    return Ok(());
                };
                if surface_id.client_id() != self.id {
                    return Ok(());
                }
                let Some((width, height)) = state
                    .outputs
                    .try_get(output_id)
                    .map(|output| output.logical_size())
                else {
                    return Ok(());
                };
                let width = u32::try_from(width).unwrap_or(0);
                let height = u32::try_from(height).unwrap_or(0);
                let serial = state.seat.next_serial();
                let Some(surface) = surface_id.try_ref_mut(state) else {
                    return Ok(());
                };
                if surface.is_inert() || surface.configured_size() == Some((width, height)) {
                    return Ok(());
                }
                surface.configure(serial, width, height, &mut dispatcher);
            }
            PresentationFeedbackPresented {
                feedbacks,
                output_id,
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                self.present_feedback(
                    state,
                    &mut dispatcher,
                    feedbacks,
                    output_id,
                    tv_sec_hi,
                    tv_sec_lo,
                    tv_nsec,
                    refresh,
                    seq_hi,
                    seq_lo,
                    flags,
                );
            }
            PresentationFeedbackDiscarded { feedbacks } => {
                self.discard_presentation_feedback(state, &mut dispatcher, feedbacks);
            }
            DiscardSurfacePresentationFeedbacksForBackend { drm_id } => {
                self.discard_surface_presentation_feedbacks_for_backend(
                    state,
                    &mut dispatcher,
                    drm_id,
                );
            }
            CancelBlockedSurface { surface, waiter_id } => {
                let Some(mut surface) = surface.try_grab(state) else {
                    return Ok(());
                };
                surface.cancel_blocked_if_waiter_matches(waiter_id, self, state, &mut dispatcher);
            }
            DismissPopupGrab { popups } => {
                self.dismiss_popup_ids(state, &mut dispatcher, popups, None);
            }
            InteractiveResizeConfigure { xdg_toplevel, width, height, resizing } => {
                let Some(xdg_surface) = xdg_toplevel
                    .try_ref(state)
                    .map(|toplevel| toplevel.xdg_surface())
                else {
                    return Ok(());
                };
                if !xdg_surface.exists(state) {
                    return Ok(());
                }
                let configure_bounds = {
                    xdg_surface.try_ref(state).and_then(|xdg_surface| {
                        let wl_surface = xdg_surface.wl_surface();
                        state.configure_bounds_for_surface(wl_surface, None)
                    })
                };
                {
                    let mut xdg_surface = xdg_surface.grab(state);
                    let mut toplevel = xdg_toplevel.grab(state);
                    toplevel.send_interactive_resize_configure(
                        self,
                        state,
                        configure_bounds,
                        &mut xdg_surface,
                        width,
                        height,
                        resizing,
                        &mut dispatcher,
                    );
                }
                if let Some(toplevel) = xdg_toplevel.try_ref(state) {
                    let window = toplevel.window();
                    let (maximized, fullscreen) = toplevel.published_shell_state();
                    crate::capture::note_window_state(
                        state,
                        &mut dispatcher,
                        window,
                        maximized,
                        fullscreen,
                    );
                }
            }
            MsgReceived { mut request } => {
                let requester_object_id = request.object_id();
                let Some(obj_id) = self
                    .objects
                    .get(&request.object_id())
                    .map(|entry| entry.object_id)
                else {
                    log::warn!(
                        "unknown Wayland object id {:?} from client {:?} (request opcode {})",
                        request.object_id(),
                        self.id,
                        request.request_id
                    );
                    self.protocol_violation_disconnect(
                        &mut dispatcher,
                        request.object_id(),
                        wl_display::Error::InvalidObject as u32,
                        "invalid object id",
                    );
                    return Ok(());
                };

                assert!(dispatcher.requester_object_id.is_none());
                let mut pending_fds = std::mem::take(&mut self.pending_fds);
                request.fds = Some(&mut pending_fds);
                dispatcher.requester_object_id = Some(requester_object_id);

                let _ = {
                    let obj = state.objects.grab(obj_id);
                    obj.on(self, state, request, &mut dispatcher)
                };
                while let Some(action) = dispatcher.post_actions.pop_front() {
                    action.execute(self, state, &mut dispatcher);
                }

                dispatcher.requester_object_id = None;
                self.pending_fds = pending_fds;
            }
            IdleNotification { id, notice } => {
                if let Some(notification) = self.resolve(id, state) {
                    notification
                        .rref(state)
                        .send_notice(notice, &mut dispatcher);
                }
            }
            SessionReplaced { session } => {
                crate::wayland_objects::xdg_session_v1::send_replaced(
                    session,
                    state,
                    &mut dispatcher,
                );
            }
            SendDmabufFeedback { id } => {
                let Some(resolved) = self.resolve(id, state) else {
                    return Ok(());
                };
                if !state
                    .dmabuf_feedbacks
                    .iter()
                    .any(|live| live.object == resolved)
                {
                    return Ok(());
                }
                if let Some(mut feedback) = resolved.try_grab(state) {
                    crate::wayland_objects::zwp_linux_dmabuf_feedback_v1::send_current(
                        &mut feedback,
                        state,
                        &mut dispatcher,
                    );
                }
            }
            DeliverCaptureEvents => {
                crate::capture::deliver(self, state, &mut dispatcher);
            }
            Close => {
                log::info!("client_id={:?} closed", self.id);
                state.capture.drop_client(self.id);
                if let Some(id) = self.reader_id.take() {
                    dispatcher.stop_model(id);
                };
                state.clear_client_process_info(self.id);
                state.activation_tokens.remove_client(self.id);
                let client_objects: Vec<(WaylandId, ObjectId)> = self
                    .objects
                    .iter()
                    .map(|(wid, entry)| (*wid, entry.object_id))
                    .collect();
                let had_client_objects = !client_objects.is_empty();
                for (_, object_id) in &client_objects {
                    if let Some(mut object) = state.objects.try_grab(*object_id) {
                        object.on_client_disconnect(self, state, &mut dispatcher);
                    }
                }
                for (wid, object_id) in client_objects {
                    self.objects.remove(&wid);
                    let _ = state.remove_object(object_id);
                }
                let seat_removal = state.seat.remove_client(self.id);
                if seat_removal.selection_cleared {
                    state
                        .seat
                        .publish_clipboard_selection(&mut dispatcher, Some(self.id));
                }
                if seat_removal.primary_selection_cleared {
                    state
                        .seat
                        .publish_primary_selection(&mut dispatcher, Some(self.id));
                }
                if seat_removal.interactive_grab_cleared {
                    crate::seat::recover_pointer_after_interactive_grab_loss(
                        state,
                        &mut dispatcher,
                        0,
                    );
                }
                if state
                    .seat
                    .active_drag()
                    .is_some_and(|drag| drag.phase == crate::state::DragPhase::Cancelled)
                {
                    crate::seat::cancel_active_drag(state, &mut dispatcher, None);
                }
                if seat_removal.keyboard_focus_cleared {
                    crate::seat::focus_after_client_disconnect(state, &mut dispatcher);
                }
                crate::idle_inhibit::refresh(state, &mut dispatcher, std::time::Instant::now());
                self.pending_fds.clear();
                if seat_removal.cursor_changed
                    || seat_removal.interactive_grab_cleared
                    || had_client_objects
                {
                    request_active_output(state, &mut dispatcher);
                }
                if had_client_objects {
                    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
                }
            }
        };

        Ok(())
    }
}

/// Store and parse clients messages
struct Buffer {
    data: Vec<u8>,
    pos: usize,
}

impl Buffer {
    fn new() -> Self {
        Self { data: vec![0; 32 * 1024], pos: 0 }
    }

    fn append(&mut self, buffer: &[u8]) {
        let pos = self.pos;
        let buffer_len = buffer.len();
        self.data[pos..pos + buffer_len].copy_from_slice(buffer);
        self.pos += buffer_len;
    }

    fn get_header(&self) -> Option<[u32; 2]> {
        if self.pos < 8 {
            return None;
        }

        let object_id = u32::from_ne_bytes(self.data[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(self.data[4..8].try_into().unwrap());

        // word is 2 u16: [len; request]
        Some([object_id, word])
    }

    fn get_whole_message(&self) -> Option<&[u8]> {
        let [_obj_id, word] = self.get_header()?;
        let len = (word >> 16) as usize;
        // let req = (word & 0xFFFF) as usize;
        // dbg!(obj_id, len, req);
        self.data.get(..len)
    }

    fn len(&self) -> usize {
        self.pos
    }

    fn consume_msg(&mut self) {
        let [_obj_id, word] = self.get_header().unwrap();
        let len = (word >> 16) as usize;

        if self.pos == len {
            self.pos = 0;
        } else {
            assert!(self.pos > len);
            self.data.copy_within(len..self.pos, 0);
            self.pos -= len;
        }
    }
}

model_instance_id!(ClientReaderId => ClientReader);

/// Responsible for reading client's fd, parsing the message, and
/// notify `Client`.
struct ClientReader {
    fd: RawFd,
    id: ClientReaderId,
    client_id: ClientId,
    read_buffer: Buffer,
    neof: usize,
    recvmsg_op_id: Option<RingId>,
}

enum ClientReaderAction {
    Start,
    ReadMsg { buffer: Box<[u8]>, fds: Vec<OwnedFd> },
    ErrorOnSocket { error: std::io::Error },
    Stop,
}

impl ClientReader {
    fn new(fd: RawFd, client_id: ClientId) -> (ClientReaderId, Self) {
        let id = ClientReader::next_id();
        let read_buffer = Buffer::new();
        let neof = 0;
        let this = Self { fd, id, client_id, read_buffer, neof, recvmsg_op_id: None };
        (id, this)
    }

    fn next_id() -> ClientReaderId {
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        pub static CLIENT_READER_ID: AtomicU64 = AtomicU64::new(0);
        let id = CLIENT_READER_ID.fetch_add(1, Relaxed);
        ClientReaderId(id)
    }

    fn is_eof(&mut self, buffer: &[u8]) -> bool {
        if !buffer.is_empty() {
            self.neof = 0;
        } else {
            self.neof += 1;
            // Number of EOF (len == 0) received
            // Consider EOF reached after 2
            // https://github.com/facebook/rocksdb/pull/6441#issuecomment-589843435
            if self.neof == 2 {
                return true;
            }
        }
        false
    }

    fn dispatch_recvmsg(&mut self, dispatcher: &mut Dispatcher) {
        let recvmsg_op_id = RingId::fresh();
        self.recvmsg_op_id = Some(recvmsg_op_id);
        dispatcher.dispatch(IoUringAction::Recvmsg {
            id: Some(recvmsg_op_id),
            fd: self.fd,
            dispatch_to: Redispatcher::to_instance(self.id, |result| match result {
                Ok(RecvmsgCompleted { fd: _, buffer, fds }) => {
                    ClientReaderAction::ReadMsg { buffer, fds }
                }
                Err(error) => ClientReaderAction::ErrorOnSocket { error },
            }),
        });
    }

    fn dispatch_close(&self, dispatcher: &mut Dispatcher) {
        dispatcher.dispatch(ClientManagerAction::Close { id: self.client_id });
    }
}

impl Model for ClientReader {
    type Action = ClientReaderAction;
    const START: Option<Self::Action> = Some(Self::Action::Start);
    const STOP: Option<Self::Action> = Some(Self::Action::Stop);

    fn process(
        &mut self,
        _state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        use ClientReaderAction::*;

        match action {
            Start => {
                self.dispatch_recvmsg(dispatcher);
            }
            ReadMsg { buffer, fds } => {
                self.recvmsg_op_id = None;
                if self.is_eof(&buffer) {
                    self.dispatch_close(dispatcher);
                    return Ok(());
                }
                if !fds.is_empty() {
                    dispatcher.dispatch_to(self.client_id, ClientAction::FdsReceived { fds });
                }
                self.read_buffer.append(&buffer);
                loop {
                    let msg: &[u8] = match self.read_buffer.get_whole_message() {
                        Some(msg) => msg,
                        None => {
                            self.dispatch_recvmsg(dispatcher);
                            return Ok(());
                        }
                    };
                    let request: RawRequest = match msg.try_into() {
                        Ok(request) => request,
                        Err(()) => {
                            log::warn!(
                                "malformed Wayland message from client {:?}, disconnecting",
                                self.client_id
                            );
                            self.dispatch_close(dispatcher);
                            return Ok(());
                        }
                    };
                    dispatcher.dispatch_to(self.client_id, ClientAction::MsgReceived { request });
                    self.read_buffer.consume_msg();
                }
            }
            ErrorOnSocket { error } => {
                self.recvmsg_op_id = None;
                log::error!("An error occured on the socket of {:?}: {:?}", self.client_id, error);
                // Dispatch an action stop our Model
                self.dispatch_close(dispatcher);
            }
            Stop => {
                if let Some(recvmsg_op_id) = self.recvmsg_op_id.take() {
                    dispatcher.dispatch(IoUringAction::Cancel { target: recvmsg_op_id });
                }
            }
        };

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct RawEvent {
    pub object_id: u32,
    pub request_id: u16,
    pub arguments: Arguments,
    fds: Vec<Rc<OwnedFd>>,
}

pub trait EventWriter: Constants + Sized {
    type Event: 'static + std::fmt::Debug;
    /// Helper method, for loggings
    fn object_id(&self) -> WaylandId;
    fn to_raw_event(&self, event: Self::Event) -> RawEvent;
    fn send_event(&self, event: Self::Event, dispatcher: &mut ClientDispatcher) {
        Client::log_event::<Self>(self, &event, dispatcher);
        dispatcher.send_event(self.to_raw_event(event));
    }
    /// Send `wl_display.error` with this object's id as the source of the error.
    fn send_error(&self, dispatcher: &mut ClientDispatcher, code: u32, message: impl AsRef<str>) {
        dispatcher.send_wl_display_error(self.object_id(), code, message.as_ref());
    }
}

impl RawEvent {
    pub fn new(id: WaylandId, req_id: u16, nargs: usize) -> Self {
        Self {
            object_id: id.into(),
            request_id: req_id,
            arguments: Arguments {
                // + 2 for object_id + (length | req_id) on serialization
                args: VecDeque::with_capacity(nargs + 2),
            },
            fds: Vec::default(),
        }
    }

    fn push(&mut self, value: u32) {
        self.arguments.args.push_back(value);
    }

    pub fn uint(&mut self, uint: u32) {
        self.push(uint);
    }

    pub fn int(&mut self, int: i32) {
        self.push(int as u32);
    }

    pub fn id<I>(&mut self, id: I)
    where
        I: Into<u32>,
    {
        self.push(id.into());
    }

    pub fn new_id_with_interface<I>(&mut self, id: I) -> Result<I, ParserError>
    where
        I: Into<WaylandId> + Copy,
    {
        let w: WaylandId = id.into();
        self.id(w);
        Ok(id)
    }

    /// A null Wayland string. The length word is 0 and no bytes follow.
    pub fn null_string(&mut self) {
        self.push(0);
    }

    pub fn string<S: AsRef<str>>(&mut self, s: S) {
        let s = s.as_ref();
        let len: u32 = s.len().try_into().unwrap();
        self.push(len + 1); // +1 for terminating null byte

        let null_byte = std::iter::once(0);
        let mut iter = s
            .as_bytes()
            .iter()
            .copied()
            .chain(null_byte)
            .array_chunks::<4>();
        for bytes in &mut iter {
            self.push(u32::from_ne_bytes(bytes));
        }
        let remainder = iter.into_remainder();
        let remainder = remainder.as_slice();
        if !remainder.is_empty() {
            let mut bytes = [0u8; 4];
            bytes[..remainder.len()].copy_from_slice(remainder);
            self.push(u32::from_ne_bytes(bytes));
        }
    }

    /// Wayland `fixed` is a signed 24.8 value in the wire format.
    pub fn fixed(&mut self, fixed: f32) {
        let v = (fixed * 256.0).round() as i32;
        self.int(v);
    }

    pub fn array(&mut self, array: Array) {
        let bytes = array.as_bytes();
        let len: u32 = bytes.len().try_into().unwrap();
        self.push(len);
        let mut chunks = bytes.chunks_exact(4);
        for chunk in chunks.by_ref() {
            self.push(u32::from_ne_bytes(chunk.try_into().unwrap()));
        }
        let remainder = chunks.remainder();
        if !remainder.is_empty() {
            let mut padded = [0u8; 4];
            padded[..remainder.len()].copy_from_slice(remainder);
            self.push(u32::from_ne_bytes(padded));
        }
    }

    pub fn fd(&mut self, fd: Rc<OwnedFd>) {
        self.fds.push(fd)
    }

    fn take_fds(&mut self) -> Vec<Rc<OwnedFd>> {
        std::mem::take(&mut self.fds)
    }

    pub fn into_bytes(self) -> Box<[u8]> {
        // let cloned = self.clone();

        let mut words = self.arguments.args;
        let length = words.len() + 2;

        let length: u16 = (length * 4).try_into().unwrap();
        let word = (length as u32) << 16 | self.request_id as u32;

        words.push_front(word);
        words.push_front(self.object_id);

        let bytes: Box<[u8]> = words.into_iter().flat_map(u32::to_ne_bytes).collect();

        // let req: RawRequest = (&*bytes).try_into().unwrap();
        // eprintln!("req parsed =     {:?}", req);
        // assert_eq!(req.arguments, cloned.arguments);
        // assert_eq!(req.object_id, cloned.object_id);
        // assert_eq!(req.request_id, cloned.request_id);

        bytes
    }
}

#[derive(Debug, derive_more::Deref, derive_more::DerefMut)]
pub struct RawRequest<'a> {
    pub object_id: u32,
    pub request_id: u16,
    #[deref]
    #[deref_mut]
    pub arguments: Arguments,
    pub fds: Option<&'a mut VecDeque<OwnedFd>>,
}

impl RawRequest<'_> {
    pub fn object_id(&self) -> WaylandId {
        WaylandId::from(self.object_id)
    }

    pub fn fd(&mut self) -> Result<Rc<OwnedFd>, ParserError> {
        self.fds
            .as_mut()
            .unwrap()
            .pop_front()
            .map(Rc::new)
            .ok_or(ParserError::MissingFileDescriptor)
    }
}

impl TryFrom<&[u8]> for RawRequest<'static> {
    type Error = ();

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() < 8 {
            return Err(());
        }

        let object_id = u32::from_ne_bytes(value[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(value[4..8].try_into().unwrap());

        let len = (word >> 16) as usize;
        let request_id = (word & 0xFFFF) as u16;

        if len % 4 != 0 {
            return Err(()); // Invalid
        }

        let args = value[8..]
            .iter()
            .copied()
            .array_chunks::<4>()
            .map(u32::from_ne_bytes)
            .collect();

        Ok(Self {
            object_id,
            request_id,
            arguments: Arguments { args },
            fds: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arguments {
    args: VecDeque<u32>,
}

#[cfg(test)]
impl Arguments {
    pub(crate) fn from_words(args: VecDeque<u32>) -> Self {
        Self { args }
    }
}

impl Arguments {
    fn pop(&mut self) -> Result<u32, ParserError> {
        self.args.pop_front().ok_or(ParserError::Malformed)
    }

    pub fn uint(&mut self) -> Result<u32, ParserError> {
        self.pop()
    }

    pub fn int(&mut self) -> Result<i32, ParserError> {
        self.uint().map(|uint| uint as i32)
    }

    pub fn id<I>(&mut self) -> Result<I, ParserError>
    where
        I: From<u32>,
    {
        self.pop().map(Into::into)
    }

    /// Parses Wayland `new_id` marshalling: interface name, client-requested version, object id.
    pub fn new_id_with_interface<I>(&mut self) -> Result<(I, u32), ParserError>
    where
        I: From<u32>,
    {
        let _interface_name = self.string()?;
        let requested_version = self.uint()?;
        let id = self.id()?;
        Ok((id, requested_version))
    }

    pub fn string(&mut self) -> Result<String, ParserError> {
        let len = self.uint()? as usize;
        if len == 0 {
            return Ok(String::new()); // null string; handlers treat "" as null
        }
        let nwords = len.next_multiple_of(4) / 4;
        let mut bytes = Vec::with_capacity(nwords * 4);
        for _ in 0..nwords {
            bytes.extend(u32::to_ne_bytes(self.pop()?));
        }
        if bytes[len - 1] != 0 || bytes[..len - 1].contains(&0) {
            return Err(ParserError::InvalidString);
        }
        bytes.truncate(len - 1);
        String::from_utf8(bytes).map_err(|_| ParserError::InvalidString)
    }

    pub fn fixed(&mut self) -> Result<f32, ParserError> {
        let v = self.int()?;
        Ok(v as f32 / 256.0)
    }

    /// Wayland array: a byte length, then the contents padded to a 32-bit boundary.
    pub fn array(&mut self) -> Result<Array, ParserError> {
        let len = self.uint()? as usize;
        let nwords = len.next_multiple_of(4) / 4;
        let mut bytes = Vec::with_capacity(nwords * 4);
        for _ in 0..nwords {
            bytes.extend_from_slice(&self.pop()?.to_ne_bytes());
        }
        bytes.truncate(len);
        Ok(Array(bytes))
    }

    pub fn fd(&mut self) -> Result<RawFd, ParserError> {
        // Implemented by `RawRequest`
        Err(ParserError::Malformed)
    }
}

#[cfg(test)]
mod clients_wire_tests {
    use std::{
        collections::VecDeque,
        os::fd::{FromRawFd, IntoRawFd, OwnedFd},
        rc::Rc,
    };

    use {
        super::{Arguments, ParserError, RawEvent, RawRequest},
        crate::protocol_generated::{Array, WaylandId},
    };

    #[test]
    fn arguments_array_reads_padded_words() {
        let mut args = VecDeque::new();
        args.push_back(8);
        args.push_back(0x1111);
        args.push_back(0x2222);
        let mut args = Arguments::from_words(args);
        assert_eq!(args.array().unwrap().to_u32s(), vec![0x1111, 0x2222]);

        let mut partial = VecDeque::new();
        partial.push_back(2);
        partial.push_back(u32::from_ne_bytes([1, 2, 0, 0]));
        let mut partial = Arguments::from_words(partial);
        assert_eq!(partial.array().unwrap().as_bytes(), &[1, 2]);
        assert!(partial.uint().is_err());
    }

    fn array_message(payload: &[u8]) -> Box<[u8]> {
        let mut event = RawEvent::new(WaylandId::from(1), 0, 1);
        event.array(Array(payload.to_vec()));
        event.into_bytes()
    }

    #[test]
    fn array_byte_lengths_round_trip_with_zero_padding() {
        for len in [0usize, 2, 4, 6, 8, 9] {
            let payload: Vec<u8> = (0..len)
                .map(|index| (index as u8).wrapping_add(1))
                .collect();
            let bytes = array_message(&payload);
            let declared = u32::from_ne_bytes(bytes[8..12].try_into().unwrap()) as usize;
            assert_eq!(declared, payload.len(), "declared length for {len} bytes");
            let padded = payload.len().next_multiple_of(4);
            let body = &bytes[12..12 + padded];
            assert_eq!(&body[..payload.len()], payload.as_slice());
            assert!(body[payload.len()..].iter().all(|byte| *byte == 0));

            let mut request = RawRequest::try_from(bytes.as_ref()).unwrap();
            assert_eq!(request.array().unwrap().as_bytes(), payload.as_slice());
            assert!(request.uint().is_err());
        }
    }

    #[test]
    fn array_u32_words_keep_their_wire_words() {
        let words = [0x1111u32, 0x2222];
        let mut event = RawEvent::new(WaylandId::from(1), 0, 1);
        event.array(Array::from_u32s(&words));
        let bytes = event.into_bytes();
        let body: Vec<u32> = bytes[8..]
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
            .collect();
        assert_eq!(body, vec![8, 0x1111, 0x2222]);
        assert_eq!(Array::from_u32s(&words).to_u32s(), words);
    }

    #[test]
    fn array_u16_indices_pack_without_padding_in_the_length() {
        let indices = [1u16, 2, 3];
        let array = Array::from_u16s(&indices);
        assert_eq!(array.as_bytes().len(), 6);
        let bytes = array_message(array.as_bytes());
        let declared = u32::from_ne_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(declared, 6);
        let mut packed = Vec::new();
        for index in indices {
            packed.extend_from_slice(&index.to_ne_bytes());
        }
        assert_eq!(&bytes[12..18], packed.as_slice());
        assert_eq!(&bytes[18..20], &[0, 0]);
    }

    #[test]
    #[should_panic(expected = "not a multiple of 4")]
    fn array_to_u32s_rejects_a_partial_word() {
        let _ = Array(vec![1, 2]).to_u32s();
    }

    #[test]
    fn arguments_string_rejects_invalid_utf8() {
        let mut args = VecDeque::new();
        args.push_back(2);
        args.push_back(u32::from_ne_bytes([0xFFu8, 0xFF, 0, 0]));
        let mut a = Arguments { args };
        assert_eq!(a.string(), Err(ParserError::InvalidString));
    }

    #[test]
    fn arguments_null_string_is_empty() {
        let mut args = Arguments { args: VecDeque::from([0]) };
        assert_eq!(args.string().unwrap(), "");
        assert!(args.uint().is_err());
    }

    #[test]
    fn arguments_string_rejects_a_missing_terminator() {
        let mut args = Arguments {
            args: VecDeque::from([2, u32::from_ne_bytes([b'a', b'b', 0, 0])]),
        };
        assert_eq!(args.string(), Err(ParserError::InvalidString));
    }

    #[test]
    fn arguments_string_rejects_an_embedded_nul() {
        let mut args = Arguments {
            args: VecDeque::from([4, u32::from_ne_bytes([b'a', 0, b'b', 0])]),
        };
        assert_eq!(args.string(), Err(ParserError::InvalidString));
    }

    #[test]
    fn arguments_string_round_trips_with_the_writer() {
        let mut event = RawEvent::new(WaylandId::from(1), 0, 1);
        event.string("hello");
        let mut args = Arguments { args: event.arguments.args };
        assert_eq!(args.string().unwrap(), "hello");
        assert!(args.uint().is_err());
    }

    #[test]
    fn arguments_fixed_roundtrip() {
        let v = (1.5f32 * 256.0).round() as i32 as u32;
        let mut a = Arguments { args: VecDeque::from([v]) };
        assert!((a.fixed().unwrap() - 1.5f32).abs() < 0.002);
    }

    #[test]
    fn raw_request_try_from_rejects_short_buffer() {
        assert!(RawRequest::try_from([1u8, 0, 0, 0].as_slice()).is_err());
    }

    #[test]
    fn malformed_opcodes_fail_for_core_shell_buffer_and_extension() {
        use crate::{
            object::Parser,
            wayland_objects::{
                wl_buffer::WlBuffer, wl_registry::WlRegistry, wp_viewporter::WpViewporter,
                xdg_surface::XdgSurface,
            },
        };

        let request = |opcode| RawRequest {
            object_id: 1,
            request_id: opcode,
            arguments: Arguments { args: VecDeque::new() },
            fds: None,
        };
        assert!(WlRegistry::parse_request(request(50)).is_err());
        assert!(XdgSurface::parse_request(request(50)).is_err());
        assert!(WlBuffer::parse_request(request(50)).is_err());
        assert!(WpViewporter::parse_request(request(50)).is_err());
    }

    #[test]
    fn a_malformed_request_disconnects_only_that_client() {
        use crate::{runner::Dispatcher, wayland_objects::wl_display::WlDisplay};

        use super::{Client, ClientId, ClientManagerAction};

        let mut client = Client::new_for_tests(ClientId::from_raw(7));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let request = RawRequest {
            object_id: 5,
            request_id: 50,
            arguments: Arguments { args: VecDeque::new() },
            fds: None,
        };
        assert!(matches!(
            client.parse::<WlDisplay>(request, &mut client_dispatcher),
            Err(crate::object::AbortRequest::Malformed)
        ));
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(7))
        }));
    }

    #[test]
    fn raw_request_fd_consumes_owned_fd_queue() {
        let raw = std::fs::File::open("/dev/null").unwrap().into_raw_fd();
        let mut fds = VecDeque::from([unsafe { OwnedFd::from_raw_fd(raw) }]);
        let mut request = RawRequest {
            object_id: 1,
            request_id: 2,
            arguments: Arguments { args: VecDeque::new() },
            fds: Some(&mut fds),
        };

        let returned = request.fd().unwrap();
        assert_eq!(Rc::strong_count(&returned), 1);
        drop(returned);

        assert!(fds.is_empty());
    }
}

#[cfg(test)]
mod surface_output_refresh_tests {
    use crate::{
        clients::{Client, ClientId, Resolved},
        io_uring::model::IoUringAction,
        runner::Dispatcher,
        state::{BackendOutputRef, DrmId, FractionalScale, ObjectId},
        test_support::{state_for_tests, test_output_drm1},
        wayland_objects::{wl_surface::WlSurface, wp_fractional_scale_v1::WpFractionalScaleV1},
    };

    #[test]
    fn refresh_surface_outputs_resends_fractional_scale_when_output_scale_changes() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(9));
        let output_id = state.insert_output(test_output_drm1(0, 0, 200, 120));
        let surface_id = crate::protocol_generated::WlSurfaceId::from(10u32);
        let fractional_id = crate::protocol_generated::WpFractionalScaleV1Id::from(11u32);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);

        let fractional_object_id = state
            .insert_object(WpFractionalScaleV1::new(
                fractional_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ))
            .1;
        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface
                .set_fractional_scale_object(Resolved::new(
                    client.client_id(),
                    fractional_id,
                    fractional_object_id,
                ))
                .unwrap();
            surface.test_map(ObjectId::from(999usize), 0, 0, 200, 120);
        }

        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.refresh_surface_outputs(&mut state, &mut client_dispatcher);
        }
        assert!(dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));

        state.outputs.try_get_mut(output_id).unwrap().scale = FractionalScale::ONE_POINT_FIVE;
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.refresh_surface_outputs(&mut state, &mut client_dispatcher);
        }
        assert!(dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));

        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.refresh_surface_outputs(&mut state, &mut client_dispatcher);
        }
        assert!(!dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));
    }

    #[test]
    fn protocol_visibility_skips_disabled_outputs_without_layout_bounds() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let enabled = state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 100, 100,
        ));
        let disabled = state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 2, 100, 0, 100, 100,
        ));
        state.outputs.try_get_mut(disabled).unwrap().enabled = false;

        let surface_id = crate::protocol_generated::WlSurfaceId::from(12u32);
        let (surface_resolved, _) = Client::new_for_tests(ClientId::from_raw(9))
            .insert_object(&mut state, WlSurface::new(surface_id, ClientId::from_raw(9), 6));
        state.register_root_surface(surface_resolved);
        state
            .objects
            .get_mut(surface_resolved.object_id())
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(998usize), 10, 10, 20, 20);

        assert_eq!(state.output_bounds(enabled).is_some(), true);
        assert!(crate::scene::visibility::root_has_protocol_visibility(&state, surface_resolved));
    }
}

#[cfg(test)]
mod socket_shutdown_tests {
    use std::{cell::RefCell, io::Read, os::unix::net::UnixStream, rc::Rc};

    use {
        super::*,
        crate::{
            runner::{Runner, ShutdownReason},
            test_support::state_for_tests,
        },
    };

    fn temp_socket() -> (mktemp::Temp, PathBuf) {
        let dir = mktemp::Temp::new_dir().unwrap();
        let path = dir.to_path_buf().join("wayland-test");
        (dir, path)
    }

    fn socket_pair() -> (RawFd, UnixStream) {
        use std::os::fd::IntoRawFd as _;
        let (server, client) = UnixStream::pair().unwrap();
        let server: RawFd = server.into_raw_fd();
        (server, client)
    }

    #[test]
    fn stop_closes_every_client_and_cancels_accept() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        manager
            .process(&mut state, ClientManagerAction::Start, &mut dispatcher)
            .unwrap();
        let (first, _first_peer) = socket_pair();
        let (second, _second_peer) = socket_pair();
        manager
            .process(
                &mut state,
                ClientManagerAction::NewClient { new_client: first },
                &mut dispatcher,
            )
            .unwrap();
        manager
            .process(
                &mut state,
                ClientManagerAction::NewClient { new_client: second },
                &mut dispatcher,
            )
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::Stop, &mut dispatcher)
            .unwrap();

        assert!(dispatcher.test_stop_queued(ClientId(0)));
        assert!(dispatcher.test_stop_queued(ClientId(1)));
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &IoUringAction| {
                matches!(action, IoUringAction::Cancel { .. })
            }),
            1
        );
        assert!(!path.exists());
    }

    #[test]
    fn repeated_stop_cancels_accept_once() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        manager
            .process(&mut state, ClientManagerAction::Start, &mut dispatcher)
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::Stop, &mut dispatcher)
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::Stop, &mut dispatcher)
            .unwrap();
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &IoUringAction| {
                matches!(action, IoUringAction::Cancel { .. })
            }),
            1
        );
    }

    #[test]
    fn shutdown_does_not_accept_a_new_client() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        manager
            .process(&mut state, ClientManagerAction::Start, &mut dispatcher)
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::Stop, &mut dispatcher)
            .unwrap();
        let accepts = dispatcher.test_count_queued_action(|action: &IoUringAction| {
            matches!(action, IoUringAction::Accept { .. })
        });

        let (server, mut peer) = socket_pair();
        peer.set_nonblocking(true).unwrap();
        manager
            .process(
                &mut state,
                ClientManagerAction::NewClient { new_client: server },
                &mut dispatcher,
            )
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::RearmAccept, &mut dispatcher)
            .unwrap();

        let mut buffer = [0];
        assert_eq!(peer.read(&mut buffer).unwrap(), 0);
        assert!(!dispatcher.test_stop_queued(ClientId(0)));
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &IoUringAction| {
                matches!(action, IoUringAction::Accept { .. })
            }),
            accepts
        );
    }

    #[test]
    fn stale_socket_is_replaced_and_drop_unlinks_it() {
        let (_dir, path) = temp_socket();
        std::fs::write(&path, b"stale").unwrap();
        let manager = ClientManager::bind(&path).unwrap();
        UnixStream::connect(&path).unwrap();
        drop(manager);
        assert!(!path.exists());

        std::fs::write(&path, b"stale").unwrap();
        let manager = ClientManager::bind(&path).unwrap();
        UnixStream::connect(&path).unwrap();
        drop(manager);
        assert!(!path.exists());
    }

    #[test]
    fn shutdown_closes_clients_and_unlinks_before_returning() {
        let (_dir, path) = temp_socket();
        let manager = ClientManager::bind(&path).unwrap();
        let mut runner = Runner::with_state(state_for_tests());
        runner.add_model(Box::new(manager));
        let (server, _peer) = socket_pair();
        let mut dispatcher = Dispatcher::default();
        dispatcher.dispatch(ClientManagerAction::NewClient { new_client: server });
        dispatcher.request_shutdown(ShutdownReason::Quit);
        let outcome = runner.run(dispatcher);

        assert_eq!(outcome.code, 0);
        assert!(!path.exists());
        assert!(!runner.test_has_model::<ClientManager>());
        assert!(!runner.test_has_instance(ClientId(0)));
    }

    #[test]
    fn quit_unlinks_the_socket_and_the_next_session_can_bind_it() {
        let (_dir, path) = temp_socket();
        let manager = ClientManager::bind(&path).unwrap();
        let state = state_for_tests();
        let watcher = crate::config::ConfigWatcher::try_new(&state.config).unwrap();
        let mut runner = Runner::with_state(state);
        runner.add_model(Box::new(manager));
        runner.add_model(Box::new(watcher));
        runner.add_model(Box::new(crate::drm::VisualAnimationPump::new()));
        let mut dispatcher = Dispatcher::default();
        dispatcher.request_shutdown(ShutdownReason::Quit);
        dispatcher
            .request_shutdown(ShutdownReason::Fatal { source: "late", message: "ignored".into() });
        let outcome = runner.run(dispatcher);

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.reason, ShutdownReason::Quit);
        assert_eq!(outcome.diagnostics, vec!["late: ignored".to_string()]);
        assert!(!path.exists());
        assert!(!runner.test_has_model::<ClientManager>());
        assert!(!runner.test_has_model::<crate::config::ConfigWatcher>());
        assert!(!runner.test_has_model::<crate::drm::VisualAnimationPump>());

        let manager = ClientManager::bind(&path).unwrap();
        UnixStream::connect(&path).unwrap();
        drop(manager);
        assert!(!path.exists());
    }

    #[test]
    fn a_fatal_model_still_unlinks_the_socket() {
        let (_dir, path) = temp_socket();
        let manager = ClientManager::bind(&path).unwrap();
        let mut runner = Runner::with_state(state_for_tests());
        runner.add_model(Box::new(manager));
        runner.add_model(Box::new(Boom));
        let mut dispatcher = Dispatcher::default();
        dispatcher.dispatch(BoomAction::Fail);
        let outcome = runner.run(dispatcher);

        assert_eq!(outcome.code, 1);
        assert!(!path.exists());
        assert!(!runner.test_has_model::<ClientManager>());
    }

    #[test]
    fn an_unsupported_request_does_not_fail_the_session() {
        use std::collections::VecDeque;

        let mut client = Client::new_for_tests(ClientId::from_raw(7));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let request = RawRequest {
            object_id: 1,
            request_id: 50,
            arguments: Arguments { args: VecDeque::new() },
            fds: None,
        };
        assert!(matches!(
            client.parse::<WlDisplay>(request, &mut client_dispatcher),
            Err(AbortRequest::Malformed)
        ));
        assert!(!dispatcher.shutdown_requested());
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(7))
        }));
        dispatcher.request_shutdown(ShutdownReason::Quit);
        let outcome = Runner::with_state(state_for_tests()).run(dispatcher);
        assert_eq!(outcome.code, 0);
    }

    #[test]
    fn bind_failure_is_an_error() {
        let (_dir, path) = temp_socket();
        let missing = path.join("nested").join("sock");
        match ClientManager::bind(&missing) {
            Err(error) => assert!(error.to_string().contains("bind Wayland socket")),
            Ok(_manager) => panic!("bind succeeded"),
        }
    }

    fn record_ids() -> (Rc<RefCell<Vec<ClientId>>>, Redispatcher<ClientId>) {
        let ids = Rc::new(RefCell::new(Vec::new()));
        let recorded = ids.clone();
        let notify = Redispatcher::to_any(move |id| {
            recorded.borrow_mut().push(id);
            crate::runner::AnyAction::empty()
        });
        (ids, notify)
    }

    #[test]
    fn adopting_a_socket_starts_a_client_and_returns_its_id() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (adopted, adopted_notify) = record_ids();
        let (_closed, closed_notify) = record_ids();
        let (socket, _peer) = UnixStream::pair().unwrap();

        manager
            .process(
                &mut state,
                ClientManagerAction::AdoptClient {
                    fd: socket.into(),
                    origin: ClientOrigin { xwayland: true, pid: None },
                    adopted: adopted_notify,
                    closed: closed_notify,
                },
                &mut dispatcher,
            )
            .unwrap();

        let id = adopted.borrow()[0];
        assert_eq!(id, ClientId(0));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            id,
            |action| matches!(action, ClientAction::Start)
        ));
    }

    #[test]
    fn closing_an_adopted_client_fires_once() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (_adopted, adopted_notify) = record_ids();
        let (closed, closed_notify) = record_ids();
        let (socket, _peer) = UnixStream::pair().unwrap();
        manager
            .process(
                &mut state,
                ClientManagerAction::AdoptClient {
                    fd: socket.into(),
                    origin: ClientOrigin { xwayland: true, pid: None },
                    adopted: adopted_notify,
                    closed: closed_notify,
                },
                &mut dispatcher,
            )
            .unwrap();
        let id = ClientId(0);

        manager
            .process(&mut state, ClientManagerAction::Close { id }, &mut dispatcher)
            .unwrap();
        manager
            .process(&mut state, ClientManagerAction::Close { id }, &mut dispatcher)
            .unwrap();

        assert_eq!(closed.borrow().as_slice(), &[id]);
        assert!(dispatcher.test_stop_queued(id));
    }

    #[test]
    fn adopt_after_stop_drops_the_socket() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        manager
            .process(&mut state, ClientManagerAction::Stop, &mut dispatcher)
            .unwrap();
        let (adopted, adopted_notify) = record_ids();
        let (closed, closed_notify) = record_ids();
        let (socket, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();

        manager
            .process(
                &mut state,
                ClientManagerAction::AdoptClient {
                    fd: socket.into(),
                    origin: ClientOrigin { xwayland: false, pid: None },
                    adopted: adopted_notify,
                    closed: closed_notify,
                },
                &mut dispatcher,
            )
            .unwrap();

        let mut buffer = [0u8; 1];
        assert_eq!(peer.read(&mut buffer).unwrap(), 0);
        assert!(adopted.borrow().is_empty());
        assert!(closed.borrow().is_empty());
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId(0),
            |action| matches!(action, ClientAction::Start)
        ));
    }

    #[test]
    fn a_pid_override_reads_that_process_command_line() {
        let pid = std::process::id() as i32;
        let (socket, _peer) = UnixStream::pair().unwrap();
        let mut client = Client::new(
            ClientId::from_raw(3),
            socket.into(),
            ClientOrigin { xwayland: true, pid: Some(pid) },
        );
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        client
            .process(&mut state, ClientAction::Start, &mut dispatcher)
            .unwrap();

        let info = client.info.as_ref().expect("process info");
        assert!(client.is_xwayland());
        assert_eq!(info.cred.pid, pid);
        assert_eq!(info.cred.uid, unsafe { libc::getuid() });
        assert_eq!(info.cred.gid, unsafe { libc::getgid() });
        assert_eq!(info.command(), cmdline_of(pid));
        assert!(!Client::new_for_tests(ClientId::from_raw(1)).is_xwayland());
    }

    fn cmdline_of(pid: i32) -> String {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        bytes
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod wire_scale_tests {
    use std::{
        os::{fd::IntoRawFd, unix::net::UnixStream},
        path::PathBuf,
    };

    use {super::*, crate::test_support::state_for_tests};

    fn temp_socket() -> (mktemp::Temp, PathBuf) {
        let dir = mktemp::Temp::new_dir().unwrap();
        let path = dir.to_path_buf().join("wayland-test");
        (dir, path)
    }

    fn no_notify() -> Redispatcher<ClientId> {
        Redispatcher::None
    }

    /// Adopt the Xwayland client (id 0) and accept a normal client (id 1).
    fn adopt_xwayland_and_normal(
        manager: &mut ClientManager,
        state: &mut State,
        dispatcher: &mut Dispatcher,
    ) {
        let (socket, _peer) = UnixStream::pair().unwrap();
        manager
            .process(
                state,
                ClientManagerAction::AdoptClient {
                    fd: socket.into(),
                    origin: ClientOrigin { xwayland: true, pid: None },
                    adopted: no_notify(),
                    closed: no_notify(),
                },
                dispatcher,
            )
            .unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        manager
            .process(
                state,
                ClientManagerAction::NewClient { new_client: socket.into_raw_fd() },
                dispatcher,
            )
            .unwrap();
    }

    #[test]
    fn normal_clients_always_have_wire_scale_one() {
        assert_eq!(Client::new_for_tests(ClientId::from_raw(1)).wire_scale(), WireScale::ONE);
        assert_eq!(
            Client::new_xwayland_for_tests(ClientId::from_raw(2)).wire_scale(),
            WireScale::ONE,
        );
        let mut state = state_for_tests();
        state.xwayland_wire_scale = WireScale::new(2);
        let (socket, _peer) = UnixStream::pair().unwrap();
        let normal = client_for_adoption(
            ClientId::from_raw(3),
            socket.into(),
            ClientOrigin { xwayland: false, pid: None },
            &state,
        );
        assert_eq!(normal.wire_scale(), WireScale::ONE);
    }

    #[test]
    fn adopted_xwayland_client_gets_the_states_wire_scale() {
        let mut state = state_for_tests();
        // S = 1 leaves the wire untouched.
        let (socket_one, _peer_one) = UnixStream::pair().unwrap();
        let at_one = client_for_adoption(
            ClientId::from_raw(0),
            socket_one.into(),
            ClientOrigin { xwayland: true, pid: None },
            &state,
        );
        assert_eq!(at_one.wire_scale(), WireScale::ONE);

        state.xwayland_wire_scale = WireScale::new(2);
        let (socket_two, _peer_two) = UnixStream::pair().unwrap();
        let at_two = client_for_adoption(
            ClientId::from_raw(1),
            socket_two.into(),
            ClientOrigin { xwayland: true, pid: None },
            &state,
        );
        assert_eq!(at_two.wire_scale(), WireScale::new(2));
    }

    #[test]
    fn a_client_dispatcher_carries_the_clients_wire_scale() {
        let mut client = Client::new_for_tests(ClientId::from_raw(3));
        let mut dispatcher = Dispatcher::default();
        assert_eq!(client.test_dispatcher(&mut dispatcher).wire_scale(), WireScale::ONE);
        client.set_wire_scale_for_tests(WireScale::new(2));
        assert_eq!(client.test_dispatcher(&mut dispatcher).wire_scale(), WireScale::new(2));
    }

    #[test]
    fn applying_a_wire_scale_updates_the_client_and_the_dispatcher_together() {
        let mut client = Client::new_for_tests(ClientId::from_raw(3));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        assert_eq!(client_dispatcher.wire_scale(), WireScale::ONE);
        client.apply_wire_scale(&mut client_dispatcher, WireScale::new(2));
        assert_eq!(client.wire_scale(), WireScale::new(2));
        assert_eq!(client_dispatcher.wire_scale(), WireScale::new(2));
    }

    #[test]
    fn set_wire_scale_action_stores_the_value_on_the_client() {
        let mut client = Client::new_for_tests(ClientId::from_raw(3));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        client
            .process(
                &mut state,
                ClientAction::SetWireScale { scale: WireScale::new(2) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(client.wire_scale(), WireScale::new(2));
        assert_eq!(client.test_dispatcher(&mut dispatcher).wire_scale(), WireScale::new(2));
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::Reconfigure)
            }),
            1,
        );
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::RefreshHints)
            }),
            1,
        );
        client
            .process(
                &mut state,
                ClientAction::SetWireScale { scale: WireScale::new(2) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::Reconfigure)
            }),
            1,
        );
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::RefreshHints)
            }),
            1,
        );
    }

    #[test]
    fn adopt_tracks_and_close_clears_only_the_xwayland_client() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        adopt_xwayland_and_normal(&mut manager, &mut state, &mut dispatcher);
        assert_eq!(manager.xwayland_client, Some(ClientId(0)));

        // Closing a normal client leaves the Xwayland tracking intact.
        manager
            .process(&mut state, ClientManagerAction::Close { id: ClientId(1) }, &mut dispatcher)
            .unwrap();
        assert_eq!(manager.xwayland_client, Some(ClientId(0)));

        manager
            .process(&mut state, ClientManagerAction::Close { id: ClientId(0) }, &mut dispatcher)
            .unwrap();
        assert_eq!(manager.xwayland_client, None);
    }

    #[test]
    fn xwayland_wire_scale_changed_queues_set_wire_scale_only_for_the_xwayland_client() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        adopt_xwayland_and_normal(&mut manager, &mut state, &mut dispatcher);

        // The running scale was 2; `default` mode recomputes it to ONE, so a
        // change is detected. Set the running value directly, as the phase rules
        // require, rather than building HiDPI outputs.
        state.xwayland_wire_scale = WireScale::new(2);

        manager
            .process(&mut state, ClientManagerAction::XwaylandWireScaleChanged, &mut dispatcher)
            .unwrap();

        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId(0),
            |action| matches!(action, ClientAction::SetWireScale { scale } if scale.get() == 1),
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId(1),
            |action| matches!(action, ClientAction::SetWireScale { .. }),
        ));
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(action, ClientAction::SetWireScale { .. })
            }),
            1,
        );
        assert_eq!(state.xwayland_wire_scale, WireScale::ONE);
    }

    #[test]
    fn xwayland_wire_scale_changed_is_quiet_when_the_scale_is_unchanged() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        adopt_xwayland_and_normal(&mut manager, &mut state, &mut dispatcher);

        manager
            .process(&mut state, ClientManagerAction::XwaylandWireScaleChanged, &mut dispatcher)
            .unwrap();

        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(action, ClientAction::SetWireScale { .. })
            }),
            0,
        );
        assert_eq!(state.xwayland_wire_scale, WireScale::ONE);
    }

    fn xwayland_client_marks(dispatcher: &Dispatcher) -> Vec<u32> {
        let mut marks = Vec::new();
        dispatcher.test_matching_actions_for::<ClientAction, ClientId>(ClientId(0), |action| {
            marks.push(match action {
                ClientAction::SetWireScale { scale } => scale.get(),
                ClientAction::OutputStateChanged { .. } => 0,
                ClientAction::RegistryGlobalAdded { .. } => 100,
                _ => 99,
            });
        });
        marks
    }

    #[test]
    fn an_output_scale_change_queues_set_wire_scale_before_output_state() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        state.config.xwayland.scaling_mode = crate::config::XwaylandScalingMode::Downscaled;
        let mut dispatcher = Dispatcher::default();
        adopt_xwayland_and_normal(&mut manager, &mut state, &mut dispatcher);
        let output_id = state.insert_output(crate::test_support::test_output_drm0_wh(800, 600));
        state.outputs.try_get_mut(output_id).unwrap().scale = crate::state::FractionalScale::TWO;
        let _ = dispatcher.test_take_queued_actions::<ClientAction>();

        manager
            .process(
                &mut state,
                ClientManagerAction::OutputStateChanged { output_id },
                &mut dispatcher,
            )
            .unwrap();

        let marks = xwayland_client_marks(&dispatcher);
        let scale_at = marks
            .iter()
            .position(|mark| *mark == 2)
            .expect("set wire scale");
        let output_at = marks
            .iter()
            .position(|mark| *mark == 0)
            .expect("output state");
        assert!(scale_at < output_at);
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId(1),
            |action| matches!(action, ClientAction::OutputStateChanged { .. }),
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId(1),
            |action| matches!(action, ClientAction::SetWireScale { .. }),
        ));

        let queued = dispatcher.test_count_queued_action::<ClientAction>(|action| {
            matches!(action, ClientAction::SetWireScale { .. })
        });
        manager
            .process(
                &mut state,
                ClientManagerAction::OutputStateChanged { output_id },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(action, ClientAction::SetWireScale { .. })
            }),
            queued,
        );
    }

    #[test]
    fn adding_a_wl_output_queues_set_wire_scale_before_the_global() {
        let (_dir, path) = temp_socket();
        let mut manager = ClientManager::bind(&path).unwrap();
        let mut state = state_for_tests();
        state.config.xwayland.scaling_mode = crate::config::XwaylandScalingMode::Downscaled;
        let mut dispatcher = Dispatcher::default();
        adopt_xwayland_and_normal(&mut manager, &mut state, &mut dispatcher);
        let output_id = state.insert_output(crate::test_support::test_output_drm0_wh(800, 600));
        state.outputs.try_get_mut(output_id).unwrap().scale = crate::state::FractionalScale::TWO;
        let _ = dispatcher.test_take_queued_actions::<ClientAction>();

        manager
            .process(
                &mut state,
                ClientManagerAction::RegistryGlobalAdded {
                    name: 7,
                    interface: "wl_output".to_string(),
                    version: 4,
                },
                &mut dispatcher,
            )
            .unwrap();

        let marks = xwayland_client_marks(&dispatcher);
        let scale_at = marks
            .iter()
            .position(|mark| *mark == 2)
            .expect("set wire scale");
        let global_at = marks.iter().position(|mark| *mark == 100).expect("global");
        assert!(scale_at < global_at);
    }
}

#[cfg(test)]
mod pointer_wire_scale_tests {
    use crate::{
        clients::{Client, ClientAction, Resolved},
        io_uring::model::IoUringAction,
        protocol_generated::{WlPointerId, WlSurfaceId, ZwpRelativePointerV1Id},
        runner::{Dispatcher, Model},
        seat::{PointerAxisChange, PointerScrollFrame, PointerScrollSource, SeatClientEvent},
        state::ObjectId,
        test_support::test_client_and_dispatcher,
        wayland_objects::{wl_pointer::WlPointer, zwp_relative_pointer_v1::ZwpRelativePointerV1},
        wire_scale::WireScale,
    };

    fn fixed_bits(value: f32) -> u32 {
        ((value * 256.0).round() as i32) as u32
    }

    fn args(dispatcher: &Dispatcher, object: u32, opcode: u32) -> Vec<Vec<u32>> {
        let mut found = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Sendmsg { buffer, .. } = action {
                let words: Vec<u32> = buffer
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
                    .collect();
                if words.len() >= 2 && words[0] == object && (words[1] & 0xffff) == opcode {
                    found.push(words[2..].to_vec());
                }
            }
        });
        found
    }

    fn deliver(scale: WireScale) -> Dispatcher {
        let mut state = crate::test_support::state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        client.set_wire_scale_for_tests(scale);
        let (warp, _) = client.insert_object(&mut state, WlPointer::new(WlPointerId::from(11), 11));
        let (discrete, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(7), 7));
        let (value120, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(8), 8));
        state.seat.register_pointer(warp);
        state.seat.register_pointer(discrete);
        state.seat.register_pointer(value120);
        let (relative, _) = client.insert_object(
            &mut state,
            ZwpRelativePointerV1::new(ZwpRelativePointerV1Id::from(12), warp),
        );
        state.seat.register_relative_pointer(warp, relative);
        let surface =
            Resolved::new(client.client_id(), WlSurfaceId::from(8), ObjectId::from(1usize));

        let send =
            |client: &mut Client, state: &mut crate::State, dispatcher: &mut Dispatcher, event| {
                client
                    .process(state, ClientAction::SeatEvent { event }, dispatcher)
                    .unwrap();
            };
        send(
            &mut client,
            &mut state,
            &mut dispatcher,
            SeatClientEvent::PointerEnter { serial: 4, surface, x: 10.5, y: 20.0 },
        );
        send(
            &mut client,
            &mut state,
            &mut dispatcher,
            SeatClientEvent::PointerMotion { time: 9, x: 10.5, y: 20.0 },
        );
        send(
            &mut client,
            &mut state,
            &mut dispatcher,
            SeatClientEvent::PointerWarp { time: 9, x: 10.5, y: 20.0 },
        );
        send(
            &mut client,
            &mut state,
            &mut dispatcher,
            SeatClientEvent::PointerScroll(PointerScrollFrame {
                time: 40,
                source: PointerScrollSource::Wheel,
                vertical: Some(PointerAxisChange::Scroll {
                    value: 1.5,
                    discrete: Some(3),
                    value120: Some(360),
                    relative_direction: None,
                }),
                horizontal: None,
            }),
        );
        send(
            &mut client,
            &mut state,
            &mut dispatcher,
            SeatClientEvent::RelativePointerMotion {
                utime_hi: 1,
                utime_lo: 2,
                dx: 1.5,
                dy: -2.25,
                dx_unaccel: 0.5,
                dy_unaccel: -0.25,
            },
        );
        dispatcher
    }

    fn expect_motion(scale: WireScale, x: f32, y: f32, axis: f32, relative: [f32; 4]) {
        let dispatcher = deliver(scale);
        let xy = [fixed_bits(x), fixed_bits(y)];
        assert_eq!(args(&dispatcher, 11, 0), vec![vec![4, 8, xy[0], xy[1]]], "enter");
        assert_eq!(args(&dispatcher, 11, 2), vec![vec![9, xy[0], xy[1]]], "motion");
        assert_eq!(args(&dispatcher, 11, 11), vec![vec![xy[0], xy[1]]], "warp");
        assert_eq!(args(&dispatcher, 7, 8), vec![vec![0, 3]], "axis_discrete stays in wheel steps");
        assert_eq!(args(&dispatcher, 7, 4), vec![vec![40, 0, fixed_bits(axis)]], "continuous axis");
        assert_eq!(args(&dispatcher, 8, 9), vec![vec![0, 360]], "axis_value120");
        assert_eq!(args(&dispatcher, 8, 4), vec![vec![40, 0, fixed_bits(axis)]]);
        assert_eq!(
            args(&dispatcher, 12, 0),
            vec![vec![
                1,
                2,
                fixed_bits(relative[0]),
                fixed_bits(relative[1]),
                fixed_bits(relative[2]),
                fixed_bits(relative[3]),
            ]]
        );
    }

    #[test]
    fn wire_scale_two_multiplies_pointer_motion_and_keeps_wheel_steps() {
        expect_motion(WireScale::new(2), 21.0, 40.0, 3.0, [3.0, -4.5, 1.0, -0.5]);
    }

    #[test]
    fn wire_scale_one_leaves_pointer_motion_unchanged() {
        expect_motion(WireScale::ONE, 10.5, 20.0, 1.5, [1.5, -2.25, 0.5, -0.25]);
    }
}

#[cfg(test)]
struct Boom;

#[cfg(test)]
enum BoomAction {
    Fail,
    Stop,
}

#[cfg(test)]
impl Model for Boom {
    type Action = BoomAction;
    const STOP: Option<Self::Action> = Some(BoomAction::Stop);

    fn process(
        &mut self,
        _state: &mut State,
        action: Self::Action,
        _dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        match action {
            BoomAction::Fail => {
                Err(crate::runner::ModelFailure::Shutdown(crate::runner::ShutdownReason::Fatal {
                    source: "test",
                    message: "injected".into(),
                }))
            }
            BoomAction::Stop => Ok(()),
        }
    }
}

#[cfg(test)]
mod drag_offer_lifetime_tests {
    use {
        super::{Client, ClientId, ClientManagerAction, Resolved},
        crate::{
            object::Object,
            protocol_generated::{
                wl_data_device_manager::DndAction, WaylandId, WlDataDeviceId, WlDataOfferId,
                WlDataSourceId, WlSeatId,
            },
            runner::Dispatcher,
            state::{
                DataOfferRole, DragNegotiationState, DragPhase, DragSource, DragState,
                DragTargetState, ObjectId,
            },
            test_support::{state_for_tests, test_resolved_wl_surface},
            wayland_objects::{
                wl_data_device::WlDataDevice,
                wl_data_offer::{
                    tests::{accept_request, destroy_request, finish_request, set_actions_request},
                    WlDataOffer,
                },
            },
        },
    };

    const SERVER_OFFER_ID: u32 = 0xff00_0000;

    struct DragFixture {
        state: crate::state::State,
        client: Client,
        offer: Resolved<WlDataOfferId>,
    }

    fn drag_fixture(
        phase: DragPhase,
        accepted_mime: Option<&str>,
        chosen_action: u32,
    ) -> DragFixture {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(2));
        let source_client_id = ClientId::from_raw(1);
        let source_id = WlDataSourceId::from(10);
        let offer_id = WlDataOfferId::from(SERVER_OFFER_ID);
        let data_device_id = WlDataDeviceId::from(2);
        let (data_device, _) = client
            .insert_object(&mut state, WlDataDevice::new(data_device_id, WlSeatId::from(2), 3));
        state.seat.register_data_device(data_device);
        let (offer, _) = client.insert_object(
            &mut state,
            WlDataOffer::new(
                offer_id,
                3,
                DataOfferRole::Drag,
                Some(Resolved::new(source_client_id, source_id, ObjectId::from(0usize))),
            ),
        );
        data_device.ref_mut(&mut state).set_drag_offer(Some(offer));
        state.seat.set_active_drag(DragState {
            source: DragSource::DataSource {
                source: Resolved::new(source_client_id, source_id, ObjectId::from(0usize)),
            },
            origin_surface: test_resolved_wl_surface(source_client_id, 30, ObjectId::from(30usize)),
            icon_surface: None,
            target: Some(DragTargetState {
                surface: test_resolved_wl_surface(client.client_id(), 31, ObjectId::from(0usize)),
            }),
            negotiation: DragNegotiationState {
                offer_ids: vec![offer],
                accepted_mime: accepted_mime.map(str::to_string),
                destination_actions: chosen_action,
                preferred_action: chosen_action,
                compositor_action: 0,
                chosen_action,
                finished: false,
                has_finish_capable_offer: true,
            },
            phase,
        });
        DragFixture { state, client, offer }
    }

    fn client_closed(dispatcher: &Dispatcher, client_id: ClientId) -> bool {
        dispatcher.test_has_queued_action::<ClientManagerAction>(
            |action| matches!(action, ClientManagerAction::Close { id } if *id == client_id),
        )
    }

    fn leave(fixture: &mut DragFixture, dispatcher: &mut Dispatcher) {
        let mut client_dispatcher = fixture.client.test_dispatcher(dispatcher);
        fixture
            .client
            .handle_data_device_drag_leave(&mut fixture.state, &mut client_dispatcher);
    }

    #[test]
    fn drag_leave_keeps_offer_alive_for_in_flight_requests() {
        let mut fixture = drag_fixture(DragPhase::Dragging, None, 0);
        let offer_id = fixture.offer.wayland_id();
        let client_id = fixture.client.client_id();
        let mut dispatcher = Dispatcher::default();
        leave(&mut fixture, &mut dispatcher);

        assert!(fixture.client.contains_object(offer_id, &fixture.state));
        assert!(fixture
            .client
            .ref_object(offer_id, &fixture.state)
            .is_detached());

        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            let offer_object_id = fixture.offer.object_id();
            let offer = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(offer_object_id);
            offer
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    set_actions_request(offer_id, DndAction::Copy as u32, DndAction::Copy as u32),
                    &mut client_dispatcher,
                )
                .unwrap();
            let offer = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(offer_object_id);
            offer
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    accept_request(offer_id, 1, "text/plain"),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!client_closed(&dispatcher, client_id));
    }

    #[test]
    fn drag_leave_then_client_destroy_removes_offer() {
        let mut fixture = drag_fixture(DragPhase::Dragging, None, 0);
        let offer_id = fixture.offer.wayland_id();
        let client_id = fixture.client.client_id();
        let mut dispatcher = Dispatcher::default();
        leave(&mut fixture, &mut dispatcher);
        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(offer_id.into());
            let offer = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(fixture.offer.object_id());
            offer
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    destroy_request(offer_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!fixture.client.contains_object(offer_id, &fixture.state));
        assert!(!client_closed(&dispatcher, client_id));
    }

    #[test]
    fn drag_leave_after_finish_keeps_the_offer_until_destroy() {
        let mut fixture =
            drag_fixture(DragPhase::DropSent, Some("text/plain"), DndAction::Copy as u32);
        let offer_id = fixture.offer.wayland_id();
        let client_id = fixture.client.client_id();
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            let offer = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(fixture.offer.object_id());
            offer
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    finish_request(offer_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(fixture.state.seat.active_drag().is_none());
        assert!(!client_closed(&dispatcher, client_id));

        leave(&mut fixture, &mut dispatcher);
        assert!(fixture.client.contains_object(offer_id, &fixture.state));
        assert!(fixture
            .client
            .ref_object(offer_id, &fixture.state)
            .is_detached());
        assert!(!client_closed(&dispatcher, client_id));

        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(offer_id.into());
            let offer = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(fixture.offer.object_id());
            offer
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    destroy_request(offer_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!fixture.client.contains_object(offer_id, &fixture.state));
        assert!(!client_closed(&dispatcher, client_id));
    }

    #[test]
    fn drag_enter_detaches_previous_offer_instead_of_deleting_it() {
        let mut fixture = drag_fixture(DragPhase::Dragging, None, 0);
        let previous_id = fixture.offer.wayland_id();
        let client_id = fixture.client.client_id();
        let source_id = WlDataSourceId::from(11);
        let (source, _) = fixture.client.insert_object(
            &mut fixture.state,
            crate::wayland_objects::wl_data_source::WlDataSource::new(
                source_id,
                fixture.client.client_id(),
                3,
            ),
        );
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            fixture.client.handle_data_device_drag_enter(
                &mut fixture.state,
                &mut client_dispatcher,
                1,
                test_resolved_wl_surface(client_id, 31, ObjectId::from(0usize)),
                1.0,
                2.0,
                Some(source),
            );
        }

        assert!(fixture.client.contains_object(previous_id, &fixture.state));
        assert!(fixture
            .client
            .ref_object(previous_id, &fixture.state)
            .is_detached());
        let data_device_id = WlDataDeviceId::from(2);
        let new_offer = fixture
            .client
            .ref_object(data_device_id, &fixture.state)
            .drag_offer()
            .expect("enter creates the next drag offer");
        assert_ne!(new_offer.wayland_id(), previous_id);
        assert!(fixture
            .client
            .contains_object(new_offer.wayland_id(), &fixture.state));

        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut dispatcher);
            let previous = fixture
                .state
                .objects
                .grab_object::<WlDataOffer>(fixture.offer.object_id());
            previous
                .on(
                    &mut fixture.client,
                    &mut fixture.state,
                    set_actions_request(
                        previous_id,
                        DndAction::Copy as u32,
                        DndAction::Copy as u32,
                    ),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!client_closed(&dispatcher, client_id));
    }

    #[test]
    fn delete_id_of_server_id_during_its_own_request_does_not_warn() {
        let id = WaylandId::from(0xff00_0000);
        assert!(!super::is_server_side_removal_of_server_id(id, Some(id)));
        assert!(!super::is_server_side_removal_of_server_id(WaylandId::from(20), None));
    }

    #[test]
    fn delete_id_of_server_id_outside_its_request_warns() {
        let id = WaylandId::from(0xff00_0000);
        assert!(super::is_server_side_removal_of_server_id(id, None));
        assert!(super::is_server_side_removal_of_server_id(id, Some(WaylandId::from(1))));
    }
}
