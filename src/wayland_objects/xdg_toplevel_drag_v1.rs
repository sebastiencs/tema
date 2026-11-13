//! `xdg_toplevel_drag_v1`. Attach one toplevel to a drag-and-drop source.

use crate::{
    clients::{Client, ClientDispatcher, ClientId, RawRequest, Resolved},
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_toplevel_drag_v1, WaylandId, WlDataSourceId, XdgToplevelDragV1Id, XdgToplevelId,
    },
    state::{GrabbedObject, ObjectId, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragObjPhase {
    NotStarted,
    Active,
    Ended,
}

#[derive(Debug)]
pub struct XdgToplevelDragV1 {
    id: XdgToplevelDragV1Id,
    client_id: ClientId,
    object_id: ObjectId,
    source: Option<Resolved<WlDataSourceId>>,
    pending: Option<(Resolved<XdgToplevelId>, (i32, i32))>,
    phase: DragObjPhase,
}

impl XdgToplevelDragV1 {
    pub(crate) fn new(
        id: XdgToplevelDragV1Id,
        client_id: ClientId,
        source: Resolved<WlDataSourceId>,
    ) -> Self {
        Self {
            id,
            client_id,
            object_id: ObjectId::invalid(),
            source: Some(source),
            pending: None,
            phase: DragObjPhase::NotStarted,
        }
    }

    pub(crate) fn bind(&mut self, object_id: ObjectId) {
        self.object_id = object_id;
    }

    pub(crate) fn resolved(&self) -> Resolved<XdgToplevelDragV1Id> {
        Resolved::new(self.client_id, self.id, self.object_id)
    }

    pub(crate) fn phase(&self) -> DragObjPhase {
        self.phase
    }

    pub(crate) fn source(&self) -> Option<Resolved<WlDataSourceId>> {
        self.source
    }

    pub(crate) fn clear_source(&mut self) {
        self.source = None;
    }

    pub(crate) fn pending(&self) -> Option<(Resolved<XdgToplevelId>, (i32, i32))> {
        self.pending
    }

    pub(crate) fn take_pending(&mut self) -> Option<(Resolved<XdgToplevelId>, (i32, i32))> {
        self.pending.take()
    }

    /// `start_drag` is running. The returned attachment is applied to the seat.
    pub(crate) fn activate(&mut self) -> Option<(Resolved<XdgToplevelId>, (i32, i32))> {
        self.phase = DragObjPhase::Active;
        self.pending.take()
    }

    pub(crate) fn mark_ended(&mut self) {
        self.phase = DragObjPhase::Ended;
        self.pending = None;
    }

    /// Drop a pre-start attachment of `toplevel`. The toplevel's own link is cleared by the caller.
    pub(crate) fn take_pending_if(&mut self, toplevel: XdgToplevelId) -> bool {
        let matches = self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| pending.wayland_id() == toplevel);
        if matches {
            self.pending = None;
        }
        matches
    }

    fn current_attachment(&self, state: &State) -> Option<Resolved<XdgToplevelId>> {
        match self.phase {
            DragObjPhase::NotStarted => self.pending.map(|(toplevel, _)| toplevel),
            DragObjPhase::Active => state.seat.toplevel_drag.as_ref().and_then(|session| {
                (session.drag == self.resolved())
                    .then(|| session.attached.as_ref().map(|attached| attached.toplevel))
                    .flatten()
            }),
            DragObjPhase::Ended => None,
        }
    }

    fn release_links(&mut self, state: &mut State) {
        let this = self.resolved();
        if let Some(source) = self.source.take() {
            if let Some(mut source) = source.try_ref_mut(state) {
                if source.toplevel_drag() == Some(this) {
                    source.set_toplevel_drag(None);
                }
            }
        }
        if let Some((toplevel, _)) = self.pending.take() {
            if let Some(mut toplevel) = toplevel.try_ref_mut(state) {
                if toplevel.toplevel_drag() == Some(this) {
                    toplevel.set_toplevel_drag(None);
                }
            }
        }
    }
}

impl Object for XdgToplevelDragV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use xdg_toplevel_drag_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                if self.phase == DragObjPhase::Active {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        xdg_toplevel_drag_v1::Error::OngoingDrag as u32,
                        "xdg_toplevel_drag destroyed during an active drag",
                    );
                    return Ok(());
                }
                self.release_links(state);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            Attach { toplevel, x_offset, y_offset } => {
                let toplevel = client.resolve_or_disconnect(toplevel, state, dispatcher)?;
                if self.phase == DragObjPhase::Ended {
                    return Ok(());
                }
                let this_drag = self.resolved();
                if toplevel.try_ref(state).is_some_and(|toplevel| {
                    toplevel
                        .toplevel_drag()
                        .is_some_and(|other| other != this_drag)
                }) {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        xdg_toplevel_drag_v1::Error::ToplevelAttached as u32,
                        "toplevel is already attached to a drag",
                    );
                    return Ok(());
                }
                if let Some(current) = self.current_attachment(state) {
                    if toplevel_is_mapped(state, current) {
                        client.protocol_violation_disconnect(
                            dispatcher,
                            self.id.into(),
                            xdg_toplevel_drag_v1::Error::ToplevelAttached as u32,
                            "a mapped toplevel is already attached",
                        );
                        return Ok(());
                    }
                    if current != toplevel {
                        if let Some(mut current) = current.try_ref_mut(state) {
                            if current.toplevel_drag() == Some(this_drag) {
                                current.set_toplevel_drag(None);
                            }
                        }
                    }
                }
                if let Some(mut toplevel_object) = toplevel.try_ref_mut(state) {
                    toplevel_object.set_toplevel_drag(Some(this_drag));
                }
                let offset = (x_offset, y_offset);
                match self.phase {
                    DragObjPhase::NotStarted => self.pending = Some((toplevel, offset)),
                    DragObjPhase::Active => {
                        crate::input::toplevel_drag::attach(state, dispatcher, toplevel, offset);
                    }
                    DragObjPhase::Ended => {}
                }
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        _client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        let this = self.resolved();
        self.release_links(state);
        crate::input::toplevel_drag::clear_attached_for_drag(state, this);
    }
}

/// A toplevel counts as mapped once its root has a committed buffer.
#[cfg(test)]
mod tests {
    use crate::{
        clients::{
            Client, ClientAction, ClientId, ClientManagerAction, DataSourceClientEvent, RawEvent,
            RawRequest, Resolved,
        },
        io_uring::model::IoUringAction,
        object::{Object, RequestResult},
        protocol_generated::{
            WaylandId, WlDataDeviceId, WlDataSourceId, WlSeatId, WlSurfaceId, XdgSurfaceId,
            XdgToplevelDragManagerV1Id, XdgToplevelDragV1Id, XdgToplevelId, XdgWmBaseId,
        },
        region::RectI,
        runner::Dispatcher,
        seat::SurfaceFocus,
        state::{
            DragNegotiationState, DragPhase, DragSource, DragState, DragTargetState,
            ManagedWindowMode, ObjectId,
        },
        test_support::{
            insert_window, raw_request_from_event, state_for_tests, test_client_and_dispatcher,
            test_output_drm0_wh,
        },
        wayland_objects::{
            wl_data_device::WlDataDevice,
            wl_data_source::{SourceUse, WlDataSource},
            wl_surface::WlSurface,
            xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
            xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1,
        },
        window::WindowId,
    };

    use super::DragObjPhase;

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: Dispatcher,
        manager: ObjectId,
        source: Resolved<WlDataSourceId>,
        surface: Resolved<WlSurfaceId>,
        window: WindowId,
        toplevel: Resolved<XdgToplevelId>,
        device: ObjectId,
    }

    impl Fixture {
        fn new() -> Self {
            let mut state = state_for_tests();
            let (mut client, dispatcher) = test_client_and_dispatcher(1);
            let (manager, _) = client.insert_object(
                &mut state,
                XdgToplevelDragManagerV1::new(XdgToplevelDragManagerV1Id::from(10)),
            );
            let (source, _) = client.insert_object(
                &mut state,
                WlDataSource::new(WlDataSourceId::from(11), client.client_id(), 3),
            );
            let (surface, _) = client.insert_object(
                &mut state,
                WlSurface::new(WlSurfaceId::from(12), client.client_id(), 6),
            );
            state.register_root_surface(surface);
            let window = insert_window(&mut state, surface);
            let (xdg_surface, _) = client.insert_object(
                &mut state,
                XdgSurface::new(
                    XdgSurfaceId::from(80),
                    Resolved::new(
                        client.client_id(),
                        XdgWmBaseId::from(79),
                        ObjectId::from(79usize),
                    ),
                    surface,
                    1,
                ),
            );
            let (toplevel, _) = client.insert_object(
                &mut state,
                XdgToplevel::new(XdgToplevelId::from(13), xdg_surface, 1, window),
            );
            let (device, _) = client.insert_object(
                &mut state,
                WlDataDevice::new(WlDataDeviceId::from(14), WlSeatId::from(1), 3),
            );
            Self {
                state,
                client,
                dispatcher,
                manager: manager.object_id(),
                source,
                surface,
                window,
                toplevel,
                device: device.object_id(),
            }
        }

        fn dispatch(
            &mut self,
            object: ObjectId,
            wire: u32,
            request: RawRequest<'static>,
        ) -> RequestResult {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire));
            self.state.objects.grab(object).on(
                &mut self.client,
                &mut self.state,
                request,
                &mut client_dispatcher,
            )
        }

        fn get_drag(&mut self, new_id: u32) -> RequestResult {
            self.dispatch(self.manager, 10, get_request(10, new_id, 11))
        }

        fn drag(&self, wire: u32) -> Resolved<XdgToplevelDragV1Id> {
            let id = XdgToplevelDragV1Id::from(wire);
            Resolved::new(self.client.client_id(), id, self.client.object_id(id))
        }

        fn phase(&self, wire: u32) -> DragObjPhase {
            self.drag(wire).try_ref(&self.state).unwrap().phase()
        }

        fn arm(&mut self) -> u32 {
            self.state
                .seat
                .test_arm_drag_serial(SurfaceFocus::new(self.surface))
        }

        fn start(&mut self, source: u32, serial: u32) -> RequestResult {
            self.dispatch(self.device, 14, start_request(14, source, 12, serial))
        }

        fn xdg(&self) -> Resolved<XdgSurfaceId> {
            self.toplevel.try_ref(&self.state).unwrap().xdg_surface()
        }

        fn root_pos(&self) -> (i32, i32) {
            self.surface.try_ref(&self.state).unwrap().root_position()
        }

        /// An output, then the window again, owned by the real toplevel.
        fn bind_managed_window(&mut self) {
            self.state.insert_output(test_output_drm0_wh(800, 600));
            self.state.destroy_window(self.window);
            self.state
                .insert_window(self.window, self.surface, self.toplevel.object_id());
        }

        fn set_floating(&mut self) {
            self.state
                .managed_window_state_mut(self.window)
                .unwrap()
                .mode = ManagedWindowMode::Floating;
        }

        fn link_xdg(&mut self) {
            let xdg = self.xdg();
            let toplevel = self.toplevel;
            self.surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_xdg_surface_impl(xdg);
            xdg.try_ref_mut(&mut self.state)
                .unwrap()
                .test_assign_toplevel_role(toplevel);
        }

        fn map(&mut self, width: i32, height: i32) {
            self.surface.try_ref_mut(&mut self.state).unwrap().test_map(
                ObjectId::from(1usize),
                0,
                0,
                width,
                height,
            );
        }

        fn set_xdg_role(&mut self, surface: Resolved<WlSurfaceId>) {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_role(
                    crate::wayland_objects::wl_surface::Role::XdgSurface,
                    &mut client_dispatcher,
                )
                .unwrap();
        }

        /// The fixture window and one other window, both tiled on the only output.
        ///
        /// The fixture window is the one a drag attaches.
        fn tile_with_neighbor(&mut self) -> Resolved<WlSurfaceId> {
            self.state.config.decorations.border.size = 0;
            self.bind_managed_window();
            self.link_xdg();
            self.set_xdg_role(self.surface);
            self.map(200, 120);

            let (neighbor, _) = self.client.insert_object(
                &mut self.state,
                WlSurface::new(WlSurfaceId::from(40), self.client.client_id(), 6),
            );
            self.state.register_root_surface(neighbor);
            let neighbor_window = self.state.allocate_window_id();
            let (xdg, _) = self.client.insert_object(
                &mut self.state,
                XdgSurface::new(
                    XdgSurfaceId::from(81),
                    Resolved::new(
                        self.client.client_id(),
                        XdgWmBaseId::from(79),
                        ObjectId::from(79usize),
                    ),
                    neighbor,
                    1,
                ),
            );
            let (toplevel, _) = self.client.insert_object(
                &mut self.state,
                XdgToplevel::new(XdgToplevelId::from(41), xdg, 1, neighbor_window),
            );
            self.state
                .insert_window(neighbor_window, neighbor, toplevel.object_id());
            self.set_xdg_role(neighbor);
            neighbor
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_xdg_surface_impl(xdg);
            xdg.try_ref_mut(&mut self.state)
                .unwrap()
                .test_assign_toplevel_role(toplevel);
            neighbor.try_ref_mut(&mut self.state).unwrap().test_map(
                ObjectId::from(2usize),
                0,
                0,
                200,
                120,
            );
            let workspace = self
                .state
                .managed_window_state(self.window)
                .unwrap()
                .workspace_id;
            crate::tiling::mark_workspace_dirty(&mut self.state, workspace);
            self.state.relayout_dirty_workspaces();
            self.refresh_geometry(self.surface);
            self.refresh_geometry(neighbor);
            assert!(self.state.window_is_tiled(self.window));
            assert!(self.state.window_is_tiled(neighbor_window));
            neighbor
        }

        fn refresh_geometry(&mut self, surface: Resolved<WlSurfaceId>) {
            self.state.mark_root_geometry_dirty(surface);
            let _ = self.state.committed_root_geometry(surface);
        }

        fn begin_attached(&mut self, x_offset: i32, y_offset: i32) {
            self.get_drag(20).unwrap();
            self.dispatch(
                self.drag(20).object_id(),
                20,
                attach_request(20, 13, x_offset, y_offset),
            )
            .unwrap();
            let serial = self.arm();
            self.start(11, serial).unwrap();
        }

        /// The fixture toplevel stays unmapped so a drag can attach it as a new window.
        fn prepare_unmapped_toplevel(&mut self) {
            self.bind_managed_window();
            self.link_xdg();
            self.set_xdg_role(self.surface);
        }

        fn begin_from(
            &mut self,
            origin: Resolved<WlSurfaceId>,
            origin_wire: u32,
            x_offset: i32,
            y_offset: i32,
        ) {
            self.get_drag(20).unwrap();
            self.dispatch(
                self.drag(20).object_id(),
                20,
                attach_request(20, 13, x_offset, y_offset),
            )
            .unwrap();
            let serial = self
                .state
                .seat
                .test_arm_drag_serial(SurfaceFocus::new(origin));
            self.dispatch(self.device, 14, start_request(14, 11, origin_wire, serial))
                .unwrap();
        }

        fn map_during_drag(&mut self) {
            assert_eq!(self.initial_configure_size(), Some((0, 0)));
            let xdg = self.xdg();
            let mut ack = RawEvent::new(WaylandId::from(80), 4, 1);
            ack.uint(1);
            self.dispatch(xdg.object_id(), 80, raw_request_from_event(ack))
                .unwrap();
            crate::wayland_objects::wl_surface::test_commit_new_buffer(
                &mut self.client,
                &mut self.state,
                &mut self.dispatcher,
                self.surface,
                80,
                60,
            );
        }

        fn add_mapped_toplevel(
            &mut self,
            surface_id: u32,
            xdg_id: u32,
            toplevel_id: u32,
        ) -> (Resolved<WlSurfaceId>, WindowId) {
            let (surface, _) = self.client.insert_object(
                &mut self.state,
                WlSurface::new(WlSurfaceId::from(surface_id), self.client.client_id(), 6),
            );
            self.state.register_root_surface(surface);
            let window = self.state.allocate_window_id();
            let (xdg, _) = self.client.insert_object(
                &mut self.state,
                XdgSurface::new(
                    XdgSurfaceId::from(xdg_id),
                    Resolved::new(
                        self.client.client_id(),
                        XdgWmBaseId::from(79),
                        ObjectId::from(79usize),
                    ),
                    surface,
                    1,
                ),
            );
            let (toplevel, _) = self.client.insert_object(
                &mut self.state,
                XdgToplevel::new(XdgToplevelId::from(toplevel_id), xdg, 1, window),
            );
            self.state
                .insert_window(window, surface, toplevel.object_id());
            self.set_xdg_role(surface);
            surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_xdg_surface_impl(xdg);
            xdg.try_ref_mut(&mut self.state)
                .unwrap()
                .test_assign_toplevel_role(toplevel);
            surface.try_ref_mut(&mut self.state).unwrap().test_map(
                ObjectId::from(surface_id as usize),
                0,
                0,
                200,
                120,
            );
            let workspace = self
                .state
                .managed_window_state(window)
                .unwrap()
                .workspace_id;
            crate::tiling::mark_workspace_dirty(&mut self.state, workspace);
            self.state.relayout_dirty_workspaces();
            self.refresh_geometry(surface);
            (surface, window)
        }

        fn add_output_beside(&mut self) -> crate::state::OutputId {
            let (first, _) = self.state.outputs.iter().next().unwrap();
            let bounds = self.state.output_bounds(first).unwrap();
            self.state
                .insert_output(crate::test_support::test_output_with_crtc_name(
                    crate::state::BackendOutputRef::new(crate::state::DrmId::from(2usize)),
                    2,
                    bounds.x + bounds.width,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                ))
        }

        fn initial_configure_size(&mut self) -> Option<(i32, i32)> {
            let xdg = self.xdg();
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            {
                let mut xdg = xdg.grab(&mut self.state);
                xdg.test_commit(
                    &mut self.client,
                    &mut self.state,
                    &mut client_dispatcher,
                    false,
                    false,
                    None,
                );
            }
            xdg.try_ref(&self.state)
                .unwrap()
                .test_latest_toplevel_configure_size()
        }
    }

    fn get_request(manager: u32, new_id: u32, source: u32) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(manager), 1, 2);
        event.id(XdgToplevelDragV1Id::from(new_id));
        event.id(WlDataSourceId::from(source));
        raw_request_from_event(event)
    }

    fn attach_request(drag: u32, toplevel: u32, x: i32, y: i32) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(drag), 1, 3);
        event.id(XdgToplevelId::from(toplevel));
        event.int(x);
        event.int(y);
        raw_request_from_event(event)
    }

    fn empty_request(object: u32, opcode: u16) -> RawRequest<'static> {
        raw_request_from_event(RawEvent::new(WaylandId::from(object), opcode, 0))
    }

    fn start_request(device: u32, source: u32, origin: u32, serial: u32) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(device), 0, 4);
        event.id(WlDataSourceId::from(source));
        event.id(WlSurfaceId::from(origin));
        event.id(WlSurfaceId::from(0));
        event.uint(serial);
        raw_request_from_event(event)
    }

    fn selection_request(device: u32, source: u32, serial: u32) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(device), 1, 2);
        event.id(WlDataSourceId::from(source));
        event.uint(serial);
        raw_request_from_event(event)
    }

    fn errors(dispatcher: &Dispatcher) -> Vec<(u32, u32)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 16 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            if object == 1 && opcode == 0 {
                let errored = u32::from_ne_bytes(buffer[8..12].try_into().unwrap());
                let code = u32::from_ne_bytes(buffer[12..16].try_into().unwrap());
                sent.push((errored, code));
            }
        });
        sent
    }

    fn closed(dispatcher: &Dispatcher) -> bool {
        dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    fn source_event(
        dispatcher: &Dispatcher,
        event_is: impl Fn(&DataSourceClientEvent) -> bool,
    ) -> bool {
        dispatcher.test_has_queued_action(|action: &ClientAction| match action {
            ClientAction::DataSourceEvent { event, .. } => event_is(event),
            _ => false,
        })
    }

    fn droppable(origin: Resolved<WlSurfaceId>, source: Resolved<WlDataSourceId>) -> DragState {
        DragState {
            source: DragSource::DataSource { source },
            origin_surface: origin,
            icon_surface: None,
            target: Some(DragTargetState { surface: origin }),
            negotiation: DragNegotiationState {
                accepted_mime: Some(String::from("text/plain")),
                chosen_action: 1,
                ..DragNegotiationState::default()
            },
            phase: DragPhase::Dragging,
        }
    }

    #[test]
    fn a_tab_tear_attaches_without_disconnecting() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();

        assert!(!closed(&fix.dispatcher));
        assert!(errors(&fix.dispatcher).is_empty());
        assert_eq!(fix.phase(20), DragObjPhase::Active);
        assert!(fix.state.seat.active_drag().is_some());
        let attached = fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .as_ref()
            .unwrap();
        assert_eq!(attached.offset, (4, 8));
        assert!(!attached.was_mapped_at_attach);
        assert!(attached.start_root_pos.is_none());
        assert!(
            !fix.state
                .seat
                .toplevel_drag
                .as_ref()
                .unwrap()
                .source_window_floating
        );
    }

    #[test]
    fn a_mapped_floating_window_remembers_where_it_started() {
        let mut fix = Fixture::new();
        fix.surface.try_ref_mut(&mut fix.state).unwrap().test_map(
            ObjectId::from(1usize),
            40,
            50,
            80,
            40,
        );
        fix.state
            .insert_output(crate::test_support::test_output_drm0_wh(800, 600));
        fix.state
            .attach_fresh_managed_window(fix.window, fix.surface);
        fix.state.managed_window_state_mut(fix.window).unwrap().mode = ManagedWindowMode::Floating;
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 1, 2))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();

        let session = fix.state.seat.toplevel_drag.as_ref().unwrap();
        assert!(session.source_window_floating);
        let attached = session.attached.as_ref().unwrap();
        assert!(attached.was_mapped_at_attach);
        assert_eq!(attached.start_root_pos, Some((40, 50)));
    }

    #[test]
    fn invalid_source_rejects_a_used_source_and_a_second_drag() {
        let mut fix = Fixture::new();
        fix.source
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .set_used(SourceUse::Selection);
        fix.get_drag(20).unwrap();
        assert_eq!(errors(&fix.dispatcher), vec![(10, 0)]);
        assert!(closed(&fix.dispatcher));

        let mut fix = Fixture::new();
        fix.source
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .set_used(SourceUse::Drag);
        fix.get_drag(20).unwrap();
        assert_eq!(errors(&fix.dispatcher), vec![(10, 0)]);

        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        fix.get_drag(21).unwrap();
        assert_eq!(errors(&fix.dispatcher), vec![(10, 0)]);
        assert!(closed(&fix.dispatcher));
    }

    #[test]
    fn set_selection_rejects_a_toplevel_drag_source() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let mut setup = fix.client.test_dispatcher(&mut fix.dispatcher);
        crate::seat::set_keyboard_focus_from_surface(
            &mut fix.state,
            &mut setup,
            Some(SurfaceFocus::new(fix.surface)),
        );
        drop(setup);
        fix.state
            .seat
            .record_client_serial(fix.client.client_id(), 41);
        fix.dispatch(fix.device, 14, selection_request(14, 11, 41))
            .unwrap();

        assert_eq!(errors(&fix.dispatcher), vec![(11, 1)]);
        assert!(closed(&fix.dispatcher));
        assert!(fix.state.seat.selection_source().is_none());
    }

    #[test]
    fn attaching_a_toplevel_that_already_has_a_drag_is_toplevel_attached() {
        let mut fix = Fixture::new();
        let (source_b, _) = fix.client.insert_object(
            &mut fix.state,
            WlDataSource::new(WlDataSourceId::from(30), fix.client.client_id(), 3),
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.manager, 10, get_request(10, 21, 30))
            .unwrap();
        let _ = source_b;
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        fix.dispatch(fix.drag(21).object_id(), 21, attach_request(21, 13, 0, 0))
            .unwrap();

        assert_eq!(errors(&fix.dispatcher), vec![(21, 0)]);
        assert!(closed(&fix.dispatcher));
    }

    #[test]
    fn a_mapped_attachment_is_not_replaced() {
        let mut fix = Fixture::new();
        fix.surface.try_ref_mut(&mut fix.state).unwrap().test_map(
            ObjectId::from(1usize),
            0,
            0,
            10,
            10,
        );
        let (surface_b, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(31), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(surface_b);
        let window_b = insert_window(&mut fix.state, surface_b);
        let (toplevel_b, _) = fix.client.insert_object(
            &mut fix.state,
            XdgToplevel::new(
                XdgToplevelId::from(32),
                Resolved::new(
                    fix.client.client_id(),
                    XdgSurfaceId::from(81),
                    ObjectId::from(81usize),
                ),
                1,
                window_b,
            ),
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 32, 0, 0))
            .unwrap();

        assert_eq!(errors(&fix.dispatcher), vec![(20, 0)]);
        assert!(closed(&fix.dispatcher));
        assert_eq!(
            fix.drag(20)
                .try_ref(&fix.state)
                .unwrap()
                .pending()
                .unwrap()
                .0
                .wayland_id(),
            fix.toplevel.wayland_id()
        );
        let _ = toplevel_b;
    }

    #[test]
    fn an_unmapped_or_destroyed_attachment_is_replaced() {
        let mut fix = Fixture::new();
        let (surface_b, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(31), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(surface_b);
        let window_b = insert_window(&mut fix.state, surface_b);
        fix.client.insert_object(
            &mut fix.state,
            XdgToplevel::new(
                XdgToplevelId::from(32),
                Resolved::new(
                    fix.client.client_id(),
                    XdgSurfaceId::from(81),
                    ObjectId::from(81usize),
                ),
                1,
                window_b,
            ),
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 32, 3, 4))
            .unwrap();

        assert!(errors(&fix.dispatcher).is_empty());
        assert!(!closed(&fix.dispatcher));
        let (pending, offset) = fix.drag(20).try_ref(&fix.state).unwrap().pending().unwrap();
        assert_eq!(pending.wayland_id(), XdgToplevelId::from(32));
        assert_eq!(offset, (3, 4));
        assert!(fix
            .toplevel
            .try_ref(&fix.state)
            .unwrap()
            .toplevel_drag()
            .is_none());

        let mut fix = Fixture::new();
        let (surface_b, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(31), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(surface_b);
        let window_b = insert_window(&mut fix.state, surface_b);
        let (toplevel_b, _) = fix.client.insert_object(
            &mut fix.state,
            XdgToplevel::new(
                XdgToplevelId::from(32),
                Resolved::new(
                    fix.client.client_id(),
                    XdgSurfaceId::from(81),
                    ObjectId::from(81usize),
                ),
                1,
                window_b,
            ),
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        fix.dispatch(fix.toplevel.object_id(), 13, empty_request(13, 0))
            .unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 32, 0, 0))
            .unwrap();

        assert!(errors(&fix.dispatcher).is_empty());
        assert_eq!(
            fix.drag(20)
                .try_ref(&fix.state)
                .unwrap()
                .pending()
                .unwrap()
                .0,
            toplevel_b
        );
    }

    #[test]
    fn destroy_during_an_active_drag_is_ongoing_drag() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, empty_request(20, 0))
            .unwrap();

        assert_eq!(fix.phase(20), DragObjPhase::Active);
        assert_eq!(errors(&fix.dispatcher), vec![(20, 1)]);
        assert!(closed(&fix.dispatcher));
    }

    #[test]
    fn attach_after_drop_is_ignored_and_destroy_after_cancel_succeeds() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        fix.state
            .seat
            .set_active_drag(droppable(fix.surface, fix.source));
        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 9, 9))
            .unwrap();
        assert!(errors(&fix.dispatcher).is_empty());
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::DndDropPerformed
        )));

        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        crate::seat::cancel_active_drag(&mut fix.state, &mut fix.dispatcher, None);
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        fix.dispatch(fix.drag(20).object_id(), 20, empty_request(20, 0))
            .unwrap();
        assert!(errors(&fix.dispatcher).is_empty());
        assert!(!fix
            .client
            .contains_object(XdgToplevelDragV1Id::from(20), &fix.state));
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::Cancelled
        )));
    }

    #[test]
    fn release_over_nothing_lock_and_a_new_drag_cancel() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::Cancelled
        )));
        assert!(!source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::DndDropPerformed
        )));

        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        crate::seat::gate_for_session_lock(&mut fix.state, &mut fix.dispatcher);
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::Cancelled
        )));

        let mut fix = Fixture::new();
        let (source_b, _) = fix.client.insert_object(
            &mut fix.state,
            WlDataSource::new(WlDataSourceId::from(30), fix.client.client_id(), 3),
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.manager, 10, get_request(10, 21, 30))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        assert_eq!(fix.phase(20), DragObjPhase::Active);
        let serial = fix.arm();
        fix.start(30, serial).unwrap();
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert_eq!(fix.phase(21), DragObjPhase::Active);
        assert!(!closed(&fix.dispatcher));
        let _ = source_b;
    }

    #[test]
    fn destroying_the_source_cancels() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        fix.dispatch(fix.source.object_id(), 11, empty_request(11, 1))
            .unwrap();

        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert!(fix.drag(20).try_ref(&fix.state).unwrap().source().is_none());
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::Cancelled
        )));
    }

    #[test]
    fn pause_cancels_a_drop_that_a_button_release_would_finish() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        fix.drag(20).try_ref_mut(&mut fix.state).unwrap().activate();
        fix.state.seat.toplevel_drag = Some(crate::input::ToplevelDragState {
            drag: fix.drag(20),
            source: fix.source,
            source_window_floating: false,
            attached: None,
        });
        fix.state
            .seat
            .set_active_drag(droppable(fix.surface, fix.source));
        crate::seat::handle_pointer_button(&mut fix.state, &mut fix.dispatcher, 0x110, 1, 0);

        crate::seat::quiesce_for_session_pause(&mut fix.state, &mut fix.dispatcher);

        assert_eq!(fix.phase(20), DragObjPhase::Ended);
        assert!(source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::Cancelled
        )));
        assert!(!source_event(&fix.dispatcher, |event| matches!(
            event,
            DataSourceClientEvent::DndDropPerformed
        )));
    }

    #[test]
    fn unmap_detaches_without_stopping_the_drag() {
        let mut fix = Fixture::new();
        fix.surface.try_ref_mut(&mut fix.state).unwrap().test_map(
            ObjectId::from(1usize),
            0,
            0,
            20,
            20,
        );
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        assert!(fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .is_some());

        let mut client_dispatcher = fix.client.test_dispatcher(&mut fix.dispatcher);
        fix.state
            .objects
            .grab(fix.surface.object_id())
            .on(
                &mut fix.client,
                &mut fix.state,
                {
                    let mut event = RawEvent::new(WaylandId::from(12), 1, 3);
                    event.id(crate::protocol_generated::WlBufferId::from(0));
                    event.int(0);
                    event.int(0);
                    raw_request_from_event(event)
                },
                &mut client_dispatcher,
            )
            .unwrap();
        fix.state
            .objects
            .grab(fix.surface.object_id())
            .on(&mut fix.client, &mut fix.state, empty_request(12, 6), &mut client_dispatcher)
            .unwrap();
        client_dispatcher.drain_post_actions(&mut fix.client, &mut fix.state);

        assert!(fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .is_none());
        assert!(fix.state.seat.active_drag().is_some());
        assert!(fix
            .toplevel
            .try_ref(&fix.state)
            .unwrap()
            .toplevel_drag()
            .is_none());
    }

    #[test]
    fn destroying_the_toplevel_mid_drag_keeps_the_drag_running() {
        let mut fix = Fixture::new();
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 0, 0))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        fix.dispatch(fix.toplevel.object_id(), 13, empty_request(13, 0))
            .unwrap();

        assert!(fix.state.seat.active_drag().is_some());
        assert!(fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .is_none());
        assert_eq!(fix.phase(20), DragObjPhase::Active);
        assert!(!closed(&fix.dispatcher));
    }

    #[test]
    fn a_floating_window_follows_the_pointer_by_its_offset() {
        let mut fix = Fixture::new();
        fix.bind_managed_window();
        fix.set_floating();
        fix.map(80, 60);
        fix.surface
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .set_root_position(30, 40);
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();

        assert_eq!(fix.root_pos(), (96, 192));
        crate::seat::handle_pointer_motion(
            &mut fix.state,
            &mut fix.dispatcher,
            10.0,
            -5.0,
            10.0,
            -5.0,
            1,
            0,
        );
        assert_eq!(fix.root_pos(), (106, 187));
    }

    #[test]
    fn window_geometry_shifts_the_root_and_the_border_does_not() {
        for border in [0, 3] {
            let mut fix = Fixture::new();
            fix.bind_managed_window();
            fix.set_floating();
            fix.link_xdg();
            fix.map(80, 60);
            fix.xdg()
                .try_ref_mut(&mut fix.state)
                .unwrap()
                .test_set_current_window_geometry(Some(RectI {
                    x: 10,
                    y: 20,
                    width: 40,
                    height: 30,
                }));
            fix.state.config.decorations.border.size = border;
            fix.state.mark_root_geometry_dirty(fix.surface);
            fix.state.seat.pointer_pos = (100.0, 200.0);
            fix.get_drag(20).unwrap();
            fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
                .unwrap();
            let serial = fix.arm();
            fix.start(11, serial).unwrap();

            assert_eq!(fix.root_pos(), (86, 172), "border {border}");
            let geometry = fix.state.committed_root_geometry(fix.surface).unwrap();
            assert_eq!(geometry.decoration_insets.left, border, "border {border}");
            assert_eq!(geometry.logical_window_local.unwrap().x, 10);
        }
    }

    #[test]
    fn a_new_window_maps_under_the_pointer_and_starts_floating() {
        let mut fix = Fixture::new();
        fix.bind_managed_window();
        fix.link_xdg();
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();

        assert_eq!(
            fix.state.managed_window_state(fix.window).unwrap().mode,
            ManagedWindowMode::Floating
        );
        assert_eq!(fix.initial_configure_size(), Some((0, 0)));
        let xdg = fix.xdg();
        let mut ack = RawEvent::new(WaylandId::from(80), 4, 1);
        ack.uint(1);
        fix.dispatch(xdg.object_id(), 80, raw_request_from_event(ack))
            .unwrap();

        let (other, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(40), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(other);
        let other_window = fix.state.allocate_window_id();
        fix.state
            .insert_window(other_window, other, ObjectId::from(1_000_001usize));
        let workspace = fix
            .state
            .managed_window_state(fix.window)
            .unwrap()
            .workspace_id;
        fix.state
            .workspaces
            .try_get_mut(workspace)
            .unwrap()
            .layout
            .floating
            .stack = vec![fix.window, other_window];

        crate::wayland_objects::wl_surface::test_commit_new_buffer(
            &mut fix.client,
            &mut fix.state,
            &mut fix.dispatcher,
            fix.surface,
            80,
            60,
        );

        assert_eq!(fix.root_pos(), (96, 192));
        let stack = fix
            .state
            .workspaces
            .try_get(workspace)
            .unwrap()
            .layout
            .floating_windows();
        assert_eq!(stack.last().copied(), Some(fix.window));
    }

    #[test]
    fn the_dragged_window_is_not_the_drop_target() {
        let mut fix = Fixture::new();
        fix.bind_managed_window();
        fix.set_floating();
        fix.map(200, 200);
        fix.state.managed_window_mut(fix.surface).unwrap().mapped = true;

        let (lower, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(40), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(lower);
        let lower_window = fix.state.allocate_window_id();
        fix.state
            .insert_window(lower_window, lower, ObjectId::from(1_000_001usize));
        fix.state
            .managed_window_state_mut(lower_window)
            .unwrap()
            .mode = ManagedWindowMode::Floating;
        lower
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .test_map(ObjectId::from(2usize), 0, 0, 200, 200);
        fix.state.managed_window_mut(lower).unwrap().mapped = true;
        let workspace = fix
            .state
            .managed_window_state(fix.window)
            .unwrap()
            .workspace_id;
        fix.state
            .workspaces
            .try_get_mut(workspace)
            .unwrap()
            .layout
            .floating
            .stack = vec![lower_window, fix.window];

        fix.state.seat.pointer_pos = (50.0, 50.0);
        assert_eq!(
            crate::input::scene_pick_at(&fix.state, (50.0, 50.0))
                .surface_focus
                .map(|focus| focus.target.surface),
            Some(fix.surface)
        );

        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 50, 50))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();

        assert_eq!(
            fix.state
                .seat
                .active_drag()
                .unwrap()
                .target
                .map(|target| target.surface),
            Some(lower)
        );
    }

    #[test]
    fn cancel_restores_a_floating_window_and_drop_keeps_it() {
        let mut fix = Fixture::new();
        fix.bind_managed_window();
        fix.set_floating();
        fix.map(80, 60);
        fix.surface
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .set_root_position(30, 40);
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        assert_eq!(fix.root_pos(), (96, 192));
        crate::seat::cancel_active_drag(&mut fix.state, &mut fix.dispatcher, None);
        assert_eq!(fix.root_pos(), (30, 40));

        let mut fix = Fixture::new();
        fix.bind_managed_window();
        fix.set_floating();
        fix.map(80, 60);
        fix.surface
            .try_ref_mut(&mut fix.state)
            .unwrap()
            .set_root_position(30, 40);
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.get_drag(20).unwrap();
        fix.dispatch(fix.drag(20).object_id(), 20, attach_request(20, 13, 4, 8))
            .unwrap();
        let serial = fix.arm();
        fix.start(11, serial).unwrap();
        crate::seat::handle_pointer_motion(
            &mut fix.state,
            &mut fix.dispatcher,
            10.0,
            -5.0,
            10.0,
            -5.0,
            1,
            0,
        );
        fix.state
            .seat
            .set_active_drag(droppable(fix.surface, fix.source));
        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);
        assert_eq!(fix.root_pos(), (106, 187));
        assert_eq!(fix.phase(20), DragObjPhase::Ended);
    }

    #[test]
    fn a_tiled_window_is_picked_up_immediately_and_ignores_the_offset() {
        let mut fix = Fixture::new();
        let neighbor = fix.tile_with_neighbor();
        let tile = layout_rect(&fix.state, fix.surface);
        let pointer = (tile.x as f64 + 40.0, tile.y as f64 + 30.0);
        fix.state.seat.pointer_pos = pointer;
        let focus = crate::input::SurfaceFocusPick {
            target: SurfaceFocus::new(neighbor),
            rect: crate::input::SurfaceRect { x: 1, y: 2, width: 3, height: 4 },
        };
        fix.state.seat.pointer_focus = Some(focus);
        let cursor = cursor_name(&fix.state);

        fix.begin_attached(4, 8);

        let preview = scaled_preview(tile);
        assert_eq!(fix.root_pos(), preview_origin(pointer, preview));
        assert_ne!(fix.root_pos(), (pointer.0.round() as i32 - 4, pointer.1.round() as i32 - 8));
        let attached = fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .as_ref()
            .unwrap();
        assert!(attached.tiled.is_some_and(|session| session.picked_up));
        assert!(attached.start_root_pos.is_none());
        assert!(fix.state.seat.tiled_drag_origin.is_none());
        assert!(fix.state.seat.pointer_interactive_grab.is_none());
        assert_eq!(fix.state.seat.pointer_focus, Some(focus));
        assert_eq!(cursor_name(&fix.state), cursor);
        assert!(fix.state.is_floating_root_surface(fix.surface));
        assert!(!fix.state.is_tiled_root_surface(fix.surface));

        crate::seat::handle_pointer_motion(
            &mut fix.state,
            &mut fix.dispatcher,
            30.0,
            -10.0,
            30.0,
            -10.0,
            2,
            0,
        );
        let moved = fix.state.seat.pointer_pos;
        assert_eq!(fix.root_pos(), preview_origin(moved, preview));
        assert!(fix.state.seat.pointer_interactive_grab.is_none());
        assert_eq!(cursor_name(&fix.state), cursor);
    }

    #[test]
    fn dropping_a_tiled_toplevel_drag_splits_by_the_pointer_quadrant() {
        let mut fix = Fixture::new();
        let neighbor = fix.tile_with_neighbor();
        let tile = layout_rect(&fix.state, fix.surface);
        fix.state.seat.pointer_pos = (tile.x as f64 + 20.0, tile.y as f64 + 20.0);
        fix.begin_attached(4, 8);
        fix.state.config.tiling.drag.precise_mouse_move = true;
        let stayed = layout_rect(&fix.state, neighbor);
        move_pointer(&mut fix, stayed.x as f64 + stayed.width as f64 / 2.0, stayed.y as f64 + 5.0);

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert!(fix.state.is_tiled_root_surface(fix.surface));
        assert!(fix.state.is_tiled_root_surface(neighbor));
        let (axis, children) = tiled_split(&fix.state, fix.surface);
        assert_eq!(axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(children[0].0, fix.surface);
        assert_eq!(children[1].0, neighbor);
        assert!(fix.state.seat.pointer_interactive_grab.is_none());
    }

    #[test]
    fn releasing_a_tiled_toplevel_drag_over_nothing_still_drops_it() {
        let mut fix = Fixture::new();
        let neighbor = fix.tile_with_neighbor();
        fix.state.config.general.outer_gap = 80;
        let workspace = fix
            .state
            .managed_window_state(fix.window)
            .unwrap()
            .workspace_id;
        crate::tiling::mark_workspace_dirty(&mut fix.state, workspace);
        fix.state.relayout_dirty_workspaces();
        fix.refresh_geometry(fix.surface);
        fix.refresh_geometry(neighbor);
        let tile = layout_rect(&fix.state, fix.surface);
        fix.state.seat.pointer_pos = (tile.x as f64 + 20.0, tile.y as f64 + 20.0);
        fix.begin_attached(0, 0);
        move_pointer(&mut fix, 1.0, 1.0);

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert!(fix.state.is_tiled_root_surface(fix.surface));
        let (axis, children) = tiled_split(&fix.state, fix.surface);
        assert_eq!(axis, crate::tiling::SplitAxis::Horizontal);
        assert_eq!(children[0].0, fix.surface);
        assert_eq!(children[1].0, neighbor);
    }

    #[test]
    fn locking_cancels_a_tiled_toplevel_drag_back_into_its_split() {
        let mut fix = Fixture::new();
        let neighbor = fix.tile_with_neighbor();
        let before = layout_rect(&fix.state, neighbor).width;
        let tile = layout_rect(&fix.state, fix.surface);
        fix.state.seat.pointer_pos = (tile.x as f64 + 20.0, tile.y as f64 + 20.0);
        fix.begin_attached(4, 8);
        assert!(layout_rect(&fix.state, neighbor).width > before);

        crate::seat::gate_for_session_lock(&mut fix.state, &mut fix.dispatcher);

        assert!(fix.state.seat.pointer_interactive_grab.is_none());
        assert!(fix.state.seat.tiled_drag_origin.is_none());
        assert_eq!(layout_rect(&fix.state, neighbor).width, before);
        assert!(fix.state.is_tiled_root_surface(fix.surface));
        assert!(fix.state.is_tiled_root_surface(neighbor));
    }

    #[test]
    fn unmapping_a_tiled_toplevel_drag_does_not_restore_it() {
        let mut fix = Fixture::new();
        let neighbor = fix.tile_with_neighbor();
        let before = layout_rect(&fix.state, neighbor).width;
        let tile = layout_rect(&fix.state, fix.surface);
        fix.state.seat.pointer_pos = (tile.x as f64 + 20.0, tile.y as f64 + 20.0);
        fix.begin_attached(0, 0);
        let grown = layout_rect(&fix.state, neighbor).width;
        assert!(grown > before);

        let mut client_dispatcher = fix.client.test_dispatcher(&mut fix.dispatcher);
        fix.state
            .objects
            .grab(fix.surface.object_id())
            .on(
                &mut fix.client,
                &mut fix.state,
                {
                    let mut event = RawEvent::new(WaylandId::from(12), 1, 3);
                    event.id(crate::protocol_generated::WlBufferId::from(0));
                    event.int(0);
                    event.int(0);
                    raw_request_from_event(event)
                },
                &mut client_dispatcher,
            )
            .unwrap();
        fix.state
            .objects
            .grab(fix.surface.object_id())
            .on(&mut fix.client, &mut fix.state, empty_request(12, 6), &mut client_dispatcher)
            .unwrap();
        client_dispatcher.drain_post_actions(&mut fix.client, &mut fix.state);

        assert!(fix
            .state
            .seat
            .toplevel_drag
            .as_ref()
            .unwrap()
            .attached
            .is_none());
        assert!(!fix.state.is_tiled_root_surface(fix.surface));
        assert!(fix.state.is_tiled_root_surface(neighbor));
        assert_eq!(layout_rect(&fix.state, neighbor).width, grown);
        assert!(fix.state.seat.active_drag().is_some());
    }

    #[test]
    fn a_new_window_from_a_tiled_source_tiles_on_the_pointer_output() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let output_b = fix.add_output_beside();
        let (source, _) = fix.add_mapped_toplevel(40, 81, 41);
        fix.state.seat.pointer_pos = (900.0, 300.0);
        let (destination, _) = fix.add_mapped_toplevel(42, 82, 43);
        let workspace_b = fix.state.active_workspace_for_output(output_b).unwrap();
        assert_eq!(
            fix.state
                .managed_window_state(fix.window)
                .unwrap()
                .workspace_id,
            fix.state.root_surface_workspace(source).unwrap()
        );
        assert_eq!(fix.state.root_surface_workspace(destination).unwrap(), workspace_b);
        let tile = layout_rect(&fix.state, destination);
        fix.state.seat.pointer_pos = (tile.x as f64 + tile.width as f64 / 2.0, tile.y as f64 + 5.0);
        fix.state.config.tiling.drag.precise_mouse_move = true;
        fix.begin_from(source, 40, 4, 8);
        fix.map_during_drag();

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert_eq!(
            fix.state
                .managed_window_state(fix.window)
                .unwrap()
                .workspace_id,
            workspace_b
        );
        assert!(fix.state.is_tiled_root_surface(fix.surface));
        let (axis, children) = tiled_split(&fix.state, fix.surface);
        assert_eq!(axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(children[0].0, fix.surface);
        assert_eq!(children[1].0, destination);
        assert!(fix.state.is_tiled_root_surface(source));
    }

    #[test]
    fn a_new_window_from_a_floating_source_stays_floating_at_the_drop() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let (source, source_window) = fix.add_mapped_toplevel(40, 81, 41);
        fix.state.set_window_floating(source_window, true);
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.begin_from(source, 40, 4, 8);
        fix.map_during_drag();

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert_eq!(fix.root_pos(), (96, 192));
        assert!(fix.state.is_floating_root_surface(fix.surface));
        assert!(!fix.state.is_tiled_root_surface(fix.surface));
        assert_eq!(
            fix.state.managed_window_state(fix.window).unwrap().mode,
            ManagedWindowMode::Floating
        );
    }

    #[test]
    fn a_new_window_with_no_source_window_is_tiled() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let (destination, _) = fix.add_mapped_toplevel(40, 81, 41);
        let (origin, _) = fix.client.insert_object(
            &mut fix.state,
            WlSurface::new(WlSurfaceId::from(50), fix.client.client_id(), 6),
        );
        fix.state.register_root_surface(origin);
        let tile = layout_rect(&fix.state, destination);
        fix.state.seat.pointer_pos = (tile.x as f64 + tile.width as f64 / 2.0, tile.y as f64 + 5.0);
        fix.state.config.tiling.drag.precise_mouse_move = true;
        fix.begin_from(origin, 50, 0, 0);
        assert!(
            !fix.state
                .seat
                .toplevel_drag
                .as_ref()
                .unwrap()
                .source_window_floating
        );
        fix.map_during_drag();

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert!(fix.state.is_tiled_root_surface(fix.surface));
        let (axis, children) = tiled_split(&fix.state, fix.surface);
        assert_eq!(axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(children[0].0, fix.surface);
        assert_eq!(children[1].0, destination);
    }

    #[test]
    fn a_new_window_dropped_on_an_empty_workspace_becomes_its_only_tile() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let output_b = fix.add_output_beside();
        let (source, _) = fix.add_mapped_toplevel(40, 81, 41);
        let workspace_b = fix.state.active_workspace_for_output(output_b).unwrap();
        let bounds = fix.state.output_bounds(output_b).unwrap();
        fix.state.seat.pointer_pos =
            (bounds.x as f64 + bounds.width as f64 / 2.0, bounds.y as f64 + 40.0);
        fix.begin_from(source, 40, 0, 0);
        fix.map_during_drag();

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert_eq!(
            fix.state
                .managed_window_state(fix.window)
                .unwrap()
                .workspace_id,
            workspace_b
        );
        assert!(fix.state.is_tiled_root_surface(fix.surface));
        let layout = &fix.state.workspaces.try_get(workspace_b).unwrap().layout;
        assert_eq!(layout.window_leaves.len(), 1);
        assert!(layout.window_leaves.contains_key(&fix.window));
        assert!(fix.state.is_tiled_root_surface(source));
    }

    #[test]
    fn destroying_a_new_window_mid_drag_does_not_place_it() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let (source, source_window) = fix.add_mapped_toplevel(40, 81, 41);
        let before = layout_rect(&fix.state, source).width;
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.begin_from(source, 40, 0, 0);
        fix.map_during_drag();
        fix.dispatch(fix.toplevel.object_id(), 13, empty_request(13, 0))
            .unwrap();

        crate::seat::complete_drag_drop(&mut fix.state, &mut fix.dispatcher);

        assert!(fix.state.seat.toplevel_drag.is_none());
        assert_eq!(layout_rect(&fix.state, source).width, before);
        assert!(fix.state.window_is_tiled(source_window));
        assert!(fix.state.window_for_root(fix.surface).is_none());
    }

    #[test]
    fn cancelling_a_new_window_leaves_it_floating() {
        let mut fix = Fixture::new();
        fix.prepare_unmapped_toplevel();
        let (source, _) = fix.add_mapped_toplevel(40, 81, 41);
        fix.state.seat.pointer_pos = (100.0, 200.0);
        fix.begin_from(source, 40, 4, 8);
        fix.map_during_drag();
        assert_eq!(fix.root_pos(), (96, 192));

        crate::seat::cancel_active_drag(&mut fix.state, &mut fix.dispatcher, None);

        assert_eq!(fix.root_pos(), (96, 192));
        assert_eq!(
            fix.state.managed_window_state(fix.window).unwrap().mode,
            ManagedWindowMode::Floating
        );
        assert!(!fix.state.is_tiled_root_surface(fix.surface));
    }

    fn layout_rect(state: &crate::state::State, surface: Resolved<WlSurfaceId>) -> RectI {
        state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global
    }

    fn scaled_preview(tile: RectI) -> (i32, i32) {
        (
            ((tile.width as f64) * 0.8489).floor() as i32,
            ((tile.height as f64) * 0.8489).floor() as i32,
        )
    }

    fn preview_origin(pointer: (f64, f64), size: (i32, i32)) -> (i32, i32) {
        (
            (pointer.0 - size.0 as f64 / 2.0).round() as i32,
            (pointer.1 - size.1 as f64 / 2.0).round() as i32,
        )
    }

    fn cursor_name(state: &crate::state::State) -> Option<String> {
        state
            .seat
            .visible_cursor_source()
            .map(|source| match source {
                crate::cursor::CursorSourceRef::Named { name, .. } => name.to_string(),
                crate::cursor::CursorSourceRef::Surface { .. } => String::from("surface"),
            })
    }

    fn move_pointer(fix: &mut Fixture, x: f64, y: f64) {
        let current = fix.state.seat.pointer_pos;
        crate::seat::handle_pointer_motion(
            &mut fix.state,
            &mut fix.dispatcher,
            x - current.0,
            y - current.1,
            x - current.0,
            y - current.1,
            3,
            0,
        );
    }

    fn tiled_split(
        state: &crate::state::State,
        surface: Resolved<WlSurfaceId>,
    ) -> (crate::tiling::SplitAxis, Vec<(Resolved<WlSurfaceId>, f64)>) {
        let window = state.window_for_root(surface).unwrap();
        let workspace_id = state.root_surface_workspace(surface).unwrap();
        let layout = &state.workspaces.try_get(workspace_id).unwrap().layout;
        let leaf_id = layout.window_leaves[&window];
        let crate::tiling::LayoutNode::Leaf(leaf) = &layout.nodes[&leaf_id] else {
            panic!("tiled drag surface is not a leaf");
        };
        let parent_id = leaf.parent.expect("dropped window has a split parent");
        let crate::tiling::LayoutNode::Container(container) = &layout.nodes[&parent_id] else {
            panic!("split parent is a container");
        };
        let children = container
            .children
            .iter()
            .map(|child| {
                let crate::tiling::LayoutNode::Leaf(leaf) = &layout.nodes[&child.node_id] else {
                    panic!("drag split child is a leaf");
                };
                (state.window_root_surface(leaf.window).unwrap(), child.weight)
            })
            .collect();
        (container.axis, children)
    }
}

fn toplevel_is_mapped(state: &State, toplevel: Resolved<XdgToplevelId>) -> bool {
    let Some(toplevel) = toplevel.try_ref(state) else {
        return false;
    };
    let window = toplevel.window();
    state.window_root_surface(window).is_some_and(|root| {
        root.try_ref(state)
            .is_some_and(|surface| surface.has_committed_buffer())
    })
}
