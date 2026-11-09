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
            object_id: ObjectId::from(0usize),
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
                    .then_some(session.attached.map(|attached| attached.toplevel))
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
        runner::Dispatcher,
        seat::SurfaceFocus,
        state::{
            DragNegotiationState, DragPhase, DragSource, DragState, DragTargetState,
            ManagedWindowMode, ObjectId,
        },
        test_support::{
            insert_window, raw_request_from_event, state_for_tests, test_client_and_dispatcher,
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
        let attached = fix.state.seat.toplevel_drag.unwrap().attached.unwrap();
        assert_eq!(attached.offset, (4, 8));
        assert!(!attached.was_mapped_at_attach);
        assert!(attached.start_root_pos.is_none());
        assert!(!fix.state.seat.toplevel_drag.unwrap().source_window_floating);
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

        let session = fix.state.seat.toplevel_drag.unwrap();
        assert!(session.source_window_floating);
        let attached = session.attached.unwrap();
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
        assert!(fix.state.seat.toplevel_drag.unwrap().attached.is_some());

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

        assert!(fix.state.seat.toplevel_drag.unwrap().attached.is_none());
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
        assert!(fix.state.seat.toplevel_drag.unwrap().attached.is_none());
        assert_eq!(fix.phase(20), DragObjPhase::Active);
        assert!(!closed(&fix.dispatcher));
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
