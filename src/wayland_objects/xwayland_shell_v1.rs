use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{xwayland_shell_v1, Constants, NewId, WaylandId, XwaylandShellV1Id},
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_display::WlDisplay, wl_surface::WlSurface, xwayland_surface_v1::XwaylandSurfaceV1,
    },
};

#[derive(Debug)]
pub struct XwaylandShellV1Global;

impl XwaylandShellV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XwaylandShellV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XwaylandShellV1::INTERFACE_NAME, XwaylandShellV1::INTERFACE_VERSION)
    }

    fn xwayland_only(&self) -> bool {
        true
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        log::debug!("Xwayland client {:?} bound xwayland_shell_v1", client.client_id());
        client.insert_object(state, XwaylandShellV1 { id: id.into() });
    }
}

#[derive(Debug)]
pub struct XwaylandShellV1 {
    id: XwaylandShellV1Id,
}

impl Object for XwaylandShellV1 {
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
        use xwayland_shell_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetXwaylandSurface { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                {
                    let mut wl_surface = surface.grab(state);
                    if wl_surface.xwayland_surface_object().is_some() {
                        drop(wl_surface);
                        client.protocol_violation_disconnect(
                            dispatcher,
                            self.id.into(),
                            xwayland_shell_v1::Error::Role as u32,
                            "wl_surface already has an xwayland_surface_v1",
                        );
                        return Ok(());
                    }
                    if !wl_surface.assign_xwayland_role() {
                        drop(wl_surface);
                        client.protocol_violation_disconnect(
                            dispatcher,
                            self.id.into(),
                            xwayland_shell_v1::Error::Role as u32,
                            "wl_surface already has another role",
                        );
                        return Ok(());
                    }
                }
                let (xsurface, _) =
                    client.insert_object(state, XwaylandSurfaceV1::new(id, surface));
                surface.grab(state).set_xwayland_surface_object(xsurface);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, ClientId, ClientManagerAction, RawEvent},
            globals::GlobalName,
            io_uring::model::IoUringAction,
            protocol_generated::{
                WaylandId, WlDisplayId, WlRegistryId, WlSurfaceId, XwaylandSurfaceV1Id,
            },
            runner::Dispatcher,
            test_support::state_for_tests,
            wayland_objects::{wl_display::WlDisplay, wl_surface::Role},
        },
    };

    fn request(object: u32, opcode: u16) -> RawEvent {
        RawEvent::new(WaylandId::from(object), opcode, 8)
    }

    fn dispatch_request<K>(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        object: K,
        event: RawEvent,
    ) where
        K: crate::object::IdToObject + Into<WaylandId> + Copy,
    {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(object.into());
        client.grab_object(object, state).on(
            client,
            state,
            (&*event.into_bytes()).try_into().unwrap(),
            &mut client_dispatcher,
        );
    }

    fn registry(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
    ) -> WlRegistryId {
        client.insert_object(state, WlDisplay::default());
        let mut event = request(1, 1);
        event.id(WlRegistryId::from(2));
        dispatch_request(client, state, dispatcher, WlDisplayId::from(1u32), event);
        WlRegistryId::from(2)
    }

    fn shell_name(state: &State) -> GlobalName {
        state
            .globals
            .iter()
            .find(|(_, global)| global.interface().0 == XwaylandShellV1::INTERFACE_NAME)
            .map(|(name, _)| name)
            .expect("xwayland_shell_v1 is registered")
    }

    fn bind_shell(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        registry_id: WlRegistryId,
        shell_id: u32,
    ) {
        let mut bind = request(u32::from(registry_id), 0);
        bind.uint(shell_name(state).as_u32());
        bind.string(XwaylandShellV1::INTERFACE_NAME);
        bind.uint(1);
        bind.id(shell_id);
        dispatch_request(client, state, dispatcher, registry_id, bind);
    }

    fn insert_surface(client: &mut Client, state: &mut State, id: u32) -> Resolved<WlSurfaceId> {
        client
            .insert_object(state, WlSurface::new(WlSurfaceId::from(id), client.client_id(), 6))
            .0
    }

    fn get_xwayland_surface(shell_id: u32, id: u32, surface_id: u32) -> RawEvent {
        let mut event = request(shell_id, 1);
        event.id(id);
        event.id(surface_id);
        event
    }

    fn closed(dispatcher: &Dispatcher, client: &Client) -> bool {
        dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == client.client_id())
        })
    }

    fn sent(dispatcher: &Dispatcher, text: &str) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Sendmsg { buffer, .. } = action {
                if buffer
                    .windows(text.len())
                    .any(|window| window == text.as_bytes())
                {
                    found = true;
                }
            }
        });
        found
    }

    fn offered(client: &Client, state: &State, name: GlobalName) -> bool {
        let registry = client.resolve(WlRegistryId::from(2u32), state).unwrap();
        state.globals.was_offered(name, registry)
    }

    #[test]
    fn get_xwayland_surface_assigns_the_role_and_links_the_object() {
        let mut state = state_for_tests();
        let mut xwayland = Client::new_xwayland_for_tests(ClientId::from_raw(4));
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut xwayland, &mut state, &mut dispatcher);
        bind_shell(&mut xwayland, &mut state, &mut dispatcher, registry_id, 3);
        let surface = insert_surface(&mut xwayland, &mut state, 4);

        dispatch_request(
            &mut xwayland,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 5, 4),
        );

        assert!(surface.rref(&state).has_xwayland_role());
        assert_eq!(
            surface
                .rref(&state)
                .xwayland_surface_object()
                .map(|object| object.wayland_id()),
            Some(XwaylandSurfaceV1Id::from(5))
        );
        assert!(xwayland.contains_object(XwaylandSurfaceV1Id::from(5), &state));
        assert!(!closed(&dispatcher, &xwayland));

        let mut serial = request(5, 0);
        serial.uint(1);
        serial.uint(0);
        dispatch_request(
            &mut xwayland,
            &mut state,
            &mut dispatcher,
            XwaylandSurfaceV1Id::from(5),
            serial,
        );
        assert!(surface.rref(&state).xwayland_serial().is_none());
        assert!(!closed(&dispatcher, &xwayland));

        dispatch_request(
            &mut xwayland,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            request(3, 0),
        );
        assert!(!xwayland.contains_object(XwaylandShellV1Id::from(3), &state));
        assert!(xwayland.contains_object(XwaylandSurfaceV1Id::from(5), &state));
        assert!(surface.rref(&state).has_xwayland_role());
    }

    #[test]
    fn get_xwayland_surface_on_an_xdg_surface_is_a_role_error() {
        let mut state = state_for_tests();
        let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(4));
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut client, &mut state, &mut dispatcher);
        bind_shell(&mut client, &mut state, &mut dispatcher, registry_id, 3);
        let surface = insert_surface(&mut client, &mut state, 4);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            assert!(surface
                .grab(&mut state)
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .is_ok());
        }

        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 5, 4),
        );

        assert!(closed(&dispatcher, &client));
        assert_eq!(surface.rref(&state).role(), Role::XdgSurface);
        assert!(!client.contains_object(XwaylandSurfaceV1Id::from(5), &state));
    }

    #[test]
    fn a_second_get_xwayland_surface_is_a_role_error() {
        let mut state = state_for_tests();
        let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(4));
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut client, &mut state, &mut dispatcher);
        bind_shell(&mut client, &mut state, &mut dispatcher, registry_id, 3);
        let surface = insert_surface(&mut client, &mut state, 4);
        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 5, 4),
        );

        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 6, 4),
        );

        assert!(closed(&dispatcher, &client));
        assert_eq!(
            surface
                .rref(&state)
                .xwayland_surface_object()
                .map(|object| object.wayland_id()),
            Some(XwaylandSurfaceV1Id::from(5))
        );
        assert!(!client.contains_object(XwaylandSurfaceV1Id::from(6), &state));
    }

    #[test]
    fn a_new_xwayland_surface_is_accepted_after_the_role_object_is_destroyed() {
        let mut state = state_for_tests();
        let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(4));
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut client, &mut state, &mut dispatcher);
        bind_shell(&mut client, &mut state, &mut dispatcher, registry_id, 3);
        let surface = insert_surface(&mut client, &mut state, 4);
        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 5, 4),
        );

        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandSurfaceV1Id::from(5),
            request(5, 1),
        );

        assert!(surface.rref(&state).has_xwayland_role());
        assert!(surface.rref(&state).xwayland_surface_object().is_none());
        assert!(!client.contains_object(XwaylandSurfaceV1Id::from(5), &state));

        dispatch_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            XwaylandShellV1Id::from(3),
            get_xwayland_surface(3, 6, 4),
        );

        assert!(!closed(&dispatcher, &client));
        assert!(surface.rref(&state).has_xwayland_role());
        assert_eq!(
            surface
                .rref(&state)
                .xwayland_surface_object()
                .map(|object| object.wayland_id()),
            Some(XwaylandSurfaceV1Id::from(6))
        );
    }

    #[test]
    fn a_normal_registry_hides_xwayland_shell_v1() {
        let mut state = state_for_tests();
        let name = shell_name(&state);
        let mut normal = Client::new_for_tests(ClientId::from_raw(1));
        let mut xwayland = Client::new_xwayland_for_tests(ClientId::from_raw(2));
        let mut normal_dispatcher = Dispatcher::default();
        let mut xwayland_dispatcher = Dispatcher::default();

        registry(&mut normal, &mut state, &mut normal_dispatcher);
        registry(&mut xwayland, &mut state, &mut xwayland_dispatcher);

        assert!(!offered(&normal, &state, name));
        assert!(!sent(&normal_dispatcher, "xwayland_shell_v1"));
        assert!(offered(&xwayland, &state, name));
        assert!(sent(&xwayland_dispatcher, "xwayland_shell_v1"));
    }
}
