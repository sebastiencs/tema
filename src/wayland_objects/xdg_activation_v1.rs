use crate::{
    clients::{Client, ClientDispatcher, ClientManagerAction, RawRequest},
    drm::{request_active_output, request_outputs_for_rect},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wl_display, xdg_activation_v1, Constants, NewId, WaylandId, XdgActivationV1Id,
    },
    seat::{resolve_activation_target, set_keyboard_focus_from_surface},
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, xdg_activation_token_v1::XdgActivationTokenV1},
};

#[derive(Debug)]
pub struct XdgActivationV1Global;

impl XdgActivationV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self {})
    }
}

impl Global for XdgActivationV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgActivationV1::INTERFACE_NAME, XdgActivationV1::INTERFACE_VERSION)
    }
    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let id = XdgActivationV1Id::from(id);
        client.insert_object(state, XdgActivationV1 { id });
    }
}

#[derive(Debug)]
pub struct XdgActivationV1 {
    id: XdgActivationV1Id,
}

impl XdgActivationV1 {
    pub fn new(id: XdgActivationV1Id) -> Self {
        Self { id }
    }
}

impl Object for XdgActivationV1 {
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
        use xdg_activation_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetActivationToken { id } => {
                client.insert_object(state, XdgActivationTokenV1::new(id));
            }
            Activate { token, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                let Some(_) = state.activation_tokens.consume(&token) else {
                    log::debug!(
                        "ignoring activation request with unknown or expired token from client {:?}",
                        client.client_id()
                    );
                    return Ok(());
                };
                let Some(focus) = resolve_activation_target(state, surface) else {
                    log::debug!(
                        "ignoring activation request for non-activatable surface {:?}",
                        surface
                    );
                    return Ok(());
                };
                let window = state.window_for_root(focus.surface);
                let workspace_id = state.root_surface_workspace(focus.surface);
                if let (Some(window), Some(workspace_id)) = (window, workspace_id) {
                    if !state.workspace_is_active(workspace_id) {
                        if state.set_window_urgent(window, true) {
                            dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
                        }
                        return Ok(());
                    }
                }
                let raised = state
                    .window_for_root(focus.surface)
                    .is_some_and(|window| state.raise_window(window));
                if !raised && !state.raise_root_surface(focus.surface) {
                    return Ok(());
                }
                set_keyboard_focus_from_surface(state, dispatcher, Some(focus));
                if let Some(bounds) = state
                    .root_surface_for_surface(focus.surface)
                    .and_then(|root_surface| state.root_surface_surface_bounds_global(root_surface))
                {
                    request_outputs_for_rect(state, bounds, dispatcher);
                } else {
                    request_active_output(state, dispatcher);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{XdgActivationV1, XdgActivationV1Global},
        crate::{
            clients::{Client, ClientId, ClientManagerAction, RawEvent},
            io_uring::model::IoUringAction,
            object::{Object, RequestResult},
            protocol_generated::{
                ext_workspace_handle_v1, Constants, ExtWorkspaceManagerV1Id, WlSeatId, WlSurfaceId,
                XdgActivationTokenV1Id, XdgActivationV1Id,
            },
            runner::Dispatcher,
            seat::{handle_keyboard_key, set_keyboard_focus_from_surface},
            state::{BackendOutputRef, DrmId, GrabbedObject, ObjectId, State, WorkspaceId},
            test_support::{
                focus_for_surface, state_for_tests, test_client_and_dispatcher,
                test_output_with_crtc_name,
            },
            wayland_objects::{
                ext_workspace_manager_v1::ExtWorkspaceManagerV1, wl_seat::WlSeat,
                wl_surface::WlSurface,
            },
        },
    };

    fn insert_mapped_surface(
        state: &mut State,
        client: &mut Client,
        surface_id: WlSurfaceId,
        buffer_object_id: usize,
    ) -> ObjectId {
        let (surface_resolved, _) =
            client.insert_object(state, WlSurface::new(surface_id, client.client_id(), 6));
        let object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);
        state
            .objects
            .get_mut(object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(buffer_object_id), 0, 0, 100, 100);
        object_id
    }

    fn insert_manager(
        state: &mut State,
        client: &mut Client,
        manager_id: XdgActivationV1Id,
    ) -> ObjectId {
        let (manager_resolved, _) = client.insert_object(state, XdgActivationV1::new(manager_id));
        manager_resolved.object_id()
    }

    fn insert_seat(state: &mut State, client: &mut Client, seat_id: WlSeatId) {
        client.insert_object(state, WlSeat::new(seat_id));
    }

    fn get_activation_token_request(
        manager_id: XdgActivationV1Id,
        token_id: XdgActivationTokenV1Id,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(manager_id.into(), 1, 1);
        event.id(token_id);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn activate_request(
        manager_id: XdgActivationV1Id,
        token: &str,
        surface_id: WlSurfaceId,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(manager_id.into(), 2, 2);
        event.string(token.to_string());
        event.id(surface_id);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn set_serial_request(
        token_id: XdgActivationTokenV1Id,
        serial: u32,
        seat_id: WlSeatId,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(token_id.into(), 0, 2);
        event.uint(serial);
        event.id(seat_id);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn set_surface_request(
        token_id: XdgActivationTokenV1Id,
        surface_id: WlSurfaceId,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(token_id.into(), 2, 1);
        event.id(surface_id);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn commit_request(token_id: XdgActivationTokenV1Id) -> crate::clients::RawRequest<'static> {
        let event = RawEvent::new(token_id.into(), 3, 0);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn output_workspaces(state: &mut State) -> (WorkspaceId, WorkspaceId) {
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(0usize)),
            1,
            0,
            0,
            100,
            100,
        ));
        let group_id = state.workspace_group_for_output(output_id).unwrap();
        let workspaces = state.workspace_groups[group_id].workspaces.clone();
        (workspaces[0], workspaces[1])
    }

    fn press_for_serial(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        surface_id: WlSurfaceId,
        object_id: ObjectId,
    ) -> u32 {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        set_keyboard_focus_from_surface(
            state,
            &mut client_dispatcher,
            Some(focus_for_surface(client.client_id(), surface_id, object_id)),
        );
        handle_keyboard_key(state, &mut client_dispatcher, 30, 1, 100);
        state.seat.last_keyboard_trigger_serial().unwrap()
    }

    fn issue_token(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        manager_id: XdgActivationV1Id,
        token_id: XdgActivationTokenV1Id,
        seat_id: WlSeatId,
        requester: WlSurfaceId,
        serial: u32,
    ) -> String {
        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(dispatcher);
            manager
                .on(
                    client,
                    state,
                    get_activation_token_request(manager_id, token_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        for request in [
            set_surface_request(token_id, requester),
            set_serial_request(token_id, serial, seat_id),
            commit_request(token_id),
        ] {
            let token = state.objects.grab_object::<
                crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1,
            >(client.object_id(token_id));
            let mut client_dispatcher = client.test_dispatcher(dispatcher);
            token
                .on(client, state, request, &mut client_dispatcher)
                .unwrap();
        }
        state.activation_tokens.first_token().unwrap()
    }

    fn activate(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        manager_id: XdgActivationV1Id,
        token: &str,
        target: WlSurfaceId,
    ) {
        let mut manager = state
            .objects
            .grab_object::<XdgActivationV1>(client.object_id(manager_id));
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        manager
            .on(client, state, activate_request(manager_id, token, target), &mut client_dispatcher)
            .unwrap();
    }

    fn sync_workspaces(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        manager_id: ExtWorkspaceManagerV1Id,
    ) {
        let mut manager = state
            .objects
            .grab_object::<ExtWorkspaceManagerV1>(client.object_id(manager_id));
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        manager.sync(client, state, &mut client_dispatcher, true);
    }

    fn workspace_state_values(dispatcher: &Dispatcher, object: u32) -> Vec<u32> {
        let mut values = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            let mut offset = 0;
            while offset + 8 <= buffer.len() {
                let id = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
                let header = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
                let size = (header >> 16) as usize;
                let opcode = header as u16;
                if size < 8 || offset + size > buffer.len() {
                    break;
                }
                if id == object && opcode == 3 && offset + 12 <= offset + size {
                    values.push(u32::from_ne_bytes(
                        buffer[offset + 8..offset + 12].try_into().unwrap(),
                    ));
                }
                offset += size;
            }
        });
        values
    }

    fn workspace_state_changed(dispatcher: &Dispatcher) -> usize {
        dispatcher.test_count_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        })
    }

    #[test]
    fn valid_token_commit_and_activate_raises_and_focuses_target() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let requester_surface_id = WlSurfaceId::from(10u32);
        let target_surface_id = WlSurfaceId::from(11u32);
        let target_object_id =
            insert_mapped_surface(&mut state, &mut client, target_surface_id, 101);
        let requester_object_id =
            insert_mapped_surface(&mut state, &mut client, requester_surface_id, 100);
        let manager_id = XdgActivationV1Id::from(20u32);
        let token_id = XdgActivationTokenV1Id::from(21u32);
        let seat_id = WlSeatId::from(22u32);

        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus_from_surface(
                &mut state,
                &mut client_dispatcher,
                Some(focus_for_surface(
                    client.client_id(),
                    requester_surface_id,
                    requester_object_id,
                )),
            );
            handle_keyboard_key(&mut state, &mut client_dispatcher, 30, 1, 100);
        }
        let serial = state.seat.last_keyboard_trigger_serial().unwrap();

        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                get_activation_token_request(manager_id, token_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_surface_request(token_id, requester_surface_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_serial_request(token_id, serial, seat_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(&mut client, &mut state, commit_request(token_id), &mut client_dispatcher);
        }

        let token = state.activation_tokens.first_token().unwrap();
        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                activate_request(manager_id, &token, target_surface_id),
                &mut client_dispatcher,
            );
        }

        assert_eq!(state.activation_tokens.tracked_count(), 0);
        assert_eq!(
            state
                .root_surfaces()
                .into_iter()
                .map(|surface| surface.object_id())
                .collect::<Vec<_>>(),
            vec![requester_object_id, target_object_id]
        );
        assert_eq!(
            state.seat.keyboard_focus(),
            Some(focus_for_surface(client.client_id(), target_surface_id, target_object_id,))
        );
    }

    #[test]
    fn invalid_serial_commit_returns_untracked_token() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let surface_id = WlSurfaceId::from(30u32);
        let manager_id = XdgActivationV1Id::from(31u32);
        let token_id = XdgActivationTokenV1Id::from(32u32);
        let seat_id = WlSeatId::from(33u32);

        insert_mapped_surface(&mut state, &mut client, surface_id, 200);
        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                get_activation_token_request(manager_id, token_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_surface_request(token_id, surface_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_serial_request(token_id, 9999, seat_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(&mut client, &mut state, commit_request(token_id), &mut client_dispatcher);
        }

        assert_eq!(state.activation_tokens.tracked_count(), 0);
    }

    #[test]
    fn double_commit_keeps_single_tracked_token() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let surface_id = WlSurfaceId::from(40u32);
        let manager_id = XdgActivationV1Id::from(41u32);
        let token_id = XdgActivationTokenV1Id::from(42u32);
        let seat_id = WlSeatId::from(43u32);

        insert_mapped_surface(&mut state, &mut client, surface_id, 300);
        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus_from_surface(
                &mut state,
                &mut client_dispatcher,
                Some(focus_for_surface(
                    client.client_id(),
                    surface_id,
                    client.object_id(surface_id),
                )),
            );
            handle_keyboard_key(&mut state, &mut client_dispatcher, 30, 1, 100);
        }
        let serial = state.seat.last_keyboard_trigger_serial().unwrap();

        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                get_activation_token_request(manager_id, token_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_surface_request(token_id, surface_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_serial_request(token_id, serial, seat_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(&mut client, &mut state, commit_request(token_id), &mut client_dispatcher);
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(&mut client, &mut state, commit_request(token_id), &mut client_dispatcher);
        }

        assert_eq!(state.activation_tokens.tracked_count(), 1);
    }

    #[test]
    fn expired_token_is_ignored_on_activate() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let requester_surface_id = WlSurfaceId::from(50u32);
        let target_surface_id = WlSurfaceId::from(51u32);
        let requester_object_id =
            insert_mapped_surface(&mut state, &mut client, requester_surface_id, 400);
        let target_object_id =
            insert_mapped_surface(&mut state, &mut client, target_surface_id, 401);
        let manager_id = XdgActivationV1Id::from(52u32);
        let token_id = XdgActivationTokenV1Id::from(53u32);
        let seat_id = WlSeatId::from(54u32);

        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus_from_surface(
                &mut state,
                &mut client_dispatcher,
                Some(focus_for_surface(
                    client.client_id(),
                    requester_surface_id,
                    requester_object_id,
                )),
            );
            handle_keyboard_key(&mut state, &mut client_dispatcher, 30, 1, 100);
        }
        let serial = state.seat.last_keyboard_trigger_serial().unwrap();

        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                get_activation_token_request(manager_id, token_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_surface_request(token_id, requester_surface_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(
                &mut client,
                &mut state,
                set_serial_request(token_id, serial, seat_id),
                &mut client_dispatcher,
            );
        }
        {
            let token = state
                .objects
                .grab_object::<crate::wayland_objects::xdg_activation_token_v1::XdgActivationTokenV1>(
                    client.object_id(token_id),
                );
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            token.on(&mut client, &mut state, commit_request(token_id), &mut client_dispatcher);
        }

        let token = state.activation_tokens.first_token().unwrap();
        assert!(state.activation_tokens.expire_now(&token));
        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                activate_request(manager_id, &token, target_surface_id),
                &mut client_dispatcher,
            );
        }

        assert_eq!(
            state
                .root_surfaces()
                .into_iter()
                .map(|surface| surface.object_id())
                .collect::<Vec<_>>(),
            vec![requester_object_id, target_object_id]
        );
        assert_eq!(
            state.seat.keyboard_focus(),
            Some(focus_for_surface(client.client_id(), requester_surface_id, requester_object_id,))
        );
    }

    #[test]
    fn unknown_token_is_ignored_on_activate() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(5);
        let requester_surface_id = WlSurfaceId::from(60u32);
        let target_surface_id = WlSurfaceId::from(61u32);
        let requester_object_id =
            insert_mapped_surface(&mut state, &mut client, requester_surface_id, 500);
        let target_object_id =
            insert_mapped_surface(&mut state, &mut client, target_surface_id, 501);
        let manager_id = XdgActivationV1Id::from(62u32);

        insert_manager(&mut state, &mut client, manager_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus_from_surface(
                &mut state,
                &mut client_dispatcher,
                Some(focus_for_surface(
                    client.client_id(),
                    requester_surface_id,
                    requester_object_id,
                )),
            );
        }
        {
            let mut manager = state
                .objects
                .grab_object::<XdgActivationV1>(client.object_id(manager_id));
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            manager.on(
                &mut client,
                &mut state,
                activate_request(manager_id, "does-not-exist", target_surface_id),
                &mut client_dispatcher,
            );
        }

        assert_eq!(
            state
                .root_surfaces()
                .into_iter()
                .map(|surface| surface.object_id())
                .collect::<Vec<_>>(),
            vec![requester_object_id, target_object_id]
        );
        assert_eq!(
            state.seat.keyboard_focus(),
            Some(focus_for_surface(client.client_id(), requester_surface_id, requester_object_id,))
        );
    }

    #[test]
    fn global_is_registered() {
        let global = XdgActivationV1Global::new();
        assert_eq!(
            global.interface(),
            (XdgActivationV1::INTERFACE_NAME, XdgActivationV1::INTERFACE_VERSION)
        );
    }

    #[test]
    fn activation_on_another_workspace_sets_urgent_without_focus() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let (_active, inactive) = output_workspaces(&mut state);
        let requester_surface_id = WlSurfaceId::from(70u32);
        let target_surface_id = WlSurfaceId::from(71u32);
        let requester_object_id =
            insert_mapped_surface(&mut state, &mut client, requester_surface_id, 700);
        let target_object_id =
            insert_mapped_surface(&mut state, &mut client, target_surface_id, 701);
        let target = client.resolve(target_surface_id, &state).unwrap();
        let window = crate::test_support::insert_window(&mut state, target);
        assert!(state.assign_window_to_workspace(window, inactive));
        let manager_id = XdgActivationV1Id::from(72u32);
        let token_id = XdgActivationTokenV1Id::from(73u32);
        let seat_id = WlSeatId::from(74u32);
        let workspace_manager_id = ExtWorkspaceManagerV1Id::from(75u32);
        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        client.insert_object(&mut state, ExtWorkspaceManagerV1::new(workspace_manager_id, 1));
        sync_workspaces(&mut state, &mut client, &mut dispatcher, workspace_manager_id);
        let handle = client
            .resolve(workspace_manager_id, &state)
            .unwrap()
            .rref(&state)
            .workspace_handle_id(inactive)
            .unwrap();
        let focus =
            focus_for_surface(client.client_id(), requester_surface_id, requester_object_id);
        let serial = press_for_serial(
            &mut state,
            &mut client,
            &mut dispatcher,
            requester_surface_id,
            requester_object_id,
        );
        let token = issue_token(
            &mut state,
            &mut client,
            &mut dispatcher,
            manager_id,
            token_id,
            seat_id,
            requester_surface_id,
            serial,
        );

        activate(&mut state, &mut client, &mut dispatcher, manager_id, &token, target_surface_id);

        assert!(state.managed_window_state(window).unwrap().urgent);
        assert!(state.workspace_is_urgent(inactive));
        assert!(!state.workspace_is_active(inactive));
        assert_eq!(state.seat.keyboard_focus(), Some(focus));
        assert_eq!(workspace_state_changed(&dispatcher), 1);
        sync_workspaces(&mut state, &mut client, &mut dispatcher, workspace_manager_id);
        let urgent = ext_workspace_handle_v1::State::Urgent as u32;
        let active_bit = ext_workspace_handle_v1::State::Active as u32;
        assert_eq!(
            workspace_state_values(&dispatcher, u32::from(handle.wayland_id())),
            vec![0, urgent]
        );

        assert!(state.activate_workspace(inactive));
        set_keyboard_focus_from_surface(
            &mut state,
            &mut client.test_dispatcher(&mut dispatcher),
            Some(focus_for_surface(client.client_id(), target_surface_id, target_object_id)),
        );
        assert!(!state.managed_window_state(window).unwrap().urgent);
        assert!(!state.workspace_is_urgent(inactive));
        assert!(state.workspace_is_active(inactive));
        sync_workspaces(&mut state, &mut client, &mut dispatcher, workspace_manager_id);
        assert_eq!(
            workspace_state_values(&dispatcher, u32::from(handle.wayland_id())),
            vec![0, urgent, active_bit]
        );
    }

    #[test]
    fn activation_on_the_active_workspace_still_focuses() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let (active, _) = output_workspaces(&mut state);
        let requester_surface_id = WlSurfaceId::from(80u32);
        let target_surface_id = WlSurfaceId::from(81u32);
        let requester_object_id =
            insert_mapped_surface(&mut state, &mut client, requester_surface_id, 800);
        let target_object_id =
            insert_mapped_surface(&mut state, &mut client, target_surface_id, 801);
        let target = client.resolve(target_surface_id, &state).unwrap();
        let window = crate::test_support::insert_window(&mut state, target);
        assert_eq!(state.managed_window_state(window).unwrap().workspace_id, active);
        state
            .workspaces
            .try_get_mut(active)
            .unwrap()
            .layout
            .floating
            .stack
            .push(window);
        let manager_id = XdgActivationV1Id::from(82u32);
        let token_id = XdgActivationTokenV1Id::from(83u32);
        let seat_id = WlSeatId::from(84u32);
        insert_manager(&mut state, &mut client, manager_id);
        insert_seat(&mut state, &mut client, seat_id);
        let serial = press_for_serial(
            &mut state,
            &mut client,
            &mut dispatcher,
            requester_surface_id,
            requester_object_id,
        );
        let token = issue_token(
            &mut state,
            &mut client,
            &mut dispatcher,
            manager_id,
            token_id,
            seat_id,
            requester_surface_id,
            serial,
        );

        activate(&mut state, &mut client, &mut dispatcher, manager_id, &token, target_surface_id);

        assert!(!state.managed_window_state(window).unwrap().urgent);
        assert!(!state.workspace_is_urgent(active));
        assert_eq!(workspace_state_changed(&dispatcher), 0);
        assert_eq!(
            state.seat.keyboard_focus(),
            Some(focus_for_surface(client.client_id(), target_surface_id, target_object_id))
        );
    }
}
