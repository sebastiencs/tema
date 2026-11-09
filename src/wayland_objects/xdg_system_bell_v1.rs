//! `xdg_system_bell_v1`. A ring emits one IPC `bell` event. No sound.

use std::time::Instant;

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_system_bell_v1, Constants, NewId, WaylandId, WlSurfaceId, XdgSystemBellV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// Advertised at version 1.
#[derive(Debug)]
pub struct XdgSystemBellV1Global;

impl XdgSystemBellV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgSystemBellV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgSystemBellV1::INTERFACE_NAME, XdgSystemBellV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, XdgSystemBellV1 { id: XdgSystemBellV1Id::from(id) });
    }
}

#[derive(Debug)]
pub struct XdgSystemBellV1 {
    id: XdgSystemBellV1Id,
}

impl Object for XdgSystemBellV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use xdg_system_bell_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            Ring { surface } => {
                let window = match surface.to_opt() {
                    None => None,
                    Some(surface) => {
                        let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                        let root = state.root_surface_for_surface(surface).unwrap_or(surface);
                        state.window_for_root(root)
                    }
                };
                crate::ipc::bell(state, dispatcher, window, Instant::now());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::{
        clients::{Arguments, Client, ClientId, ClientManagerAction, RawRequest, Resolved},
        globals::{Global, Globals},
        ipc::IpcServerAction,
        object::{Object, RequestResult},
        protocol_generated::{Constants, NewId, WlSubsurfaceId, WlSurfaceId, XdgSystemBellV1Id},
        state::ObjectId,
        test_support::{
            insert_window, insert_wl_subsurface_client1, state_for_tests,
            test_client_and_dispatcher,
        },
        wayland_objects::wl_surface::{Role, WlSurface},
    };

    use super::{XdgSystemBellV1, XdgSystemBellV1Global};

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: crate::runner::Dispatcher,
        bell: Resolved<XdgSystemBellV1Id>,
        toplevel: Resolved<WlSurfaceId>,
        window: crate::window::WindowId,
        child: Resolved<WlSurfaceId>,
        layer: Resolved<WlSurfaceId>,
    }

    fn new_fixture() -> Fixture {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (toplevel, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(5), client.client_id(), 6));
        state.register_root_surface(toplevel);
        let window = insert_window(&mut state, toplevel);
        let (child, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(7), client.client_id(), 6));
        let subsurface = insert_wl_subsurface_client1(
            &mut state,
            WlSubsurfaceId::from(30),
            child.object_id(),
            toplevel.object_id(),
        );
        child
            .try_ref_mut(&mut state)
            .unwrap()
            .assign_subsurface(subsurface, toplevel);
        let (layer, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(8), client.client_id(), 6));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            layer
                .try_ref_mut(&mut state)
                .unwrap()
                .set_role(Role::LayerSurface, &mut client_dispatcher)
                .unwrap();
        }
        state.register_root_surface(layer);
        let (bell, _) =
            client.insert_object(&mut state, XdgSystemBellV1 { id: XdgSystemBellV1Id::from(6) });
        state.ipc_enabled = true;
        let _ = dispatcher.test_take_queued_actions::<IpcServerAction>();
        Fixture {
            state,
            client,
            dispatcher,
            bell,
            toplevel,
            window,
            child,
            layer,
        }
    }

    fn request(object_id: u32, request_id: u16, words: &[u32]) -> RawRequest<'static> {
        RawRequest {
            object_id,
            request_id,
            arguments: Arguments::from_words(VecDeque::from(words.to_vec())),
            fds: None,
        }
    }

    fn dispatch(
        fixture: &mut Fixture,
        object: ObjectId,
        wire_id: u32,
        request_id: u16,
        words: &[u32],
    ) -> RequestResult {
        let request = request(wire_id, request_id, words);
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(wire_id.into());
        fixture.state.objects.grab(object).on(
            &mut fixture.client,
            &mut fixture.state,
            request,
            &mut client_dispatcher,
        )
    }

    fn ring(fixture: &mut Fixture, surface: u32) -> RequestResult {
        let bell = fixture.bell.object_id();
        dispatch(fixture, bell, 6, 1, &[surface])
    }

    fn bells(fixture: &mut Fixture) -> Vec<Option<u64>> {
        fixture
            .dispatcher
            .test_take_queued_actions::<IpcServerAction>()
            .into_iter()
            .map(|action| match action {
                IpcServerAction::Bell { window } => window,
                other => panic!("unexpected ipc action {other:?}"),
            })
            .collect()
    }

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    #[test]
    fn the_bell_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(XdgSystemBellV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = XdgSystemBellV1Global;
        global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        assert!(client.contains_object(XdgSystemBellV1Id::from(6), &state));
        assert_eq!(XdgSystemBellV1Global.interface(), ("xdg_system_bell_v1", 1));
    }

    #[test]
    fn ring_resolves_a_window_or_none_and_disconnects_an_unknown_surface() {
        // Each case uses a fresh fixture. A second ring of the same window inside
        // 100 ms is throttled, so these assertions would hide each other.
        let mut null_ring = new_fixture();
        ring(&mut null_ring, 0).unwrap();
        assert_eq!(bells(&mut null_ring), vec![None]);
        assert!(!closed(&null_ring));

        let mut toplevel = new_fixture();
        let window = toplevel.window.as_u64();
        ring(&mut toplevel, 5).unwrap();
        assert_eq!(bells(&mut toplevel), vec![Some(window)]);
        assert!(!closed(&toplevel));

        let mut child = new_fixture();
        let window = child.window.as_u64();
        ring(&mut child, 7).unwrap();
        assert_eq!(bells(&mut child), vec![Some(window)]);

        let mut layer = new_fixture();
        ring(&mut layer, 8).unwrap();
        assert_eq!(bells(&mut layer), vec![None]);
        assert!(!closed(&layer));

        let mut unknown = new_fixture();
        assert!(ring(&mut unknown, 99).is_err());
        assert!(bells(&mut unknown).is_empty());
        assert!(closed(&unknown));
    }
}
