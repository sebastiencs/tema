use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        zwp_keyboard_shortcuts_inhibit_manager_v1, Constants, NewId, WaylandId, WlSeatId,
        WlSurfaceId, ZwpKeyboardShortcutsInhibitManagerV1Id, ZwpKeyboardShortcutsInhibitorV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_display::WlDisplay, zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    },
};

/// Advertised at version 1. Inhibitors are granted on creation (unless the surface
/// was denied by the user, see `WlSurface::shortcuts_inhibit_denied`).
#[derive(Debug)]
pub struct ZwpKeyboardShortcutsInhibitManagerV1Global;

impl ZwpKeyboardShortcutsInhibitManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for ZwpKeyboardShortcutsInhibitManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (
            ZwpKeyboardShortcutsInhibitManagerV1::INTERFACE_NAME,
            ZwpKeyboardShortcutsInhibitManagerV1::INTERFACE_VERSION,
        )
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            ZwpKeyboardShortcutsInhibitManagerV1 {
                id: ZwpKeyboardShortcutsInhibitManagerV1Id::from(id),
            },
        );
    }
}

#[derive(Debug)]
pub struct ZwpKeyboardShortcutsInhibitManagerV1 {
    id: ZwpKeyboardShortcutsInhibitManagerV1Id,
}

impl Object for ZwpKeyboardShortcutsInhibitManagerV1 {
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
        use zwp_keyboard_shortcuts_inhibit_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            InhibitShortcuts { id, surface, seat } => {
                let surface =
                    client.resolve_or_disconnect(WlSurfaceId::from(surface), state, dispatcher)?;
                client.resolve_or_disconnect(WlSeatId::from(seat), state, dispatcher)?;
                if surface.rref(state).shortcuts_inhibitor().is_some() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        zwp_keyboard_shortcuts_inhibit_manager_v1::Error::AlreadyInhibited as u32,
                        "keyboard shortcuts are already inhibited for this surface and seat",
                    );
                    return Ok(());
                }
                let granted = !surface.rref(state).shortcuts_inhibit_denied();
                let (inhibitor, _) = client.insert_object(
                    state,
                    ZwpKeyboardShortcutsInhibitorV1::new(
                        ZwpKeyboardShortcutsInhibitorV1Id::from(id),
                        surface,
                        granted,
                    ),
                );
                surface
                    .try_ref_mut(state)
                    .expect("shortcuts inhibitor surface")
                    .set_shortcuts_inhibitor(inhibitor);
                if granted {
                    inhibitor.rref(state).send_state(true, dispatcher);
                }
                log::info!(
                    "keyboard shortcuts inhibitor client={:?} surface={:?} granted={}",
                    client.client_id(),
                    surface,
                    granted,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::{
        clients::{
            Arguments, Client, ClientAction, ClientId, ClientManagerAction, RawRequest, Resolved,
        },
        globals::{Global, Globals},
        io_uring::model::IoUringAction,
        object::{Object, RequestResult},
        protocol_generated::{
            Constants, NewId, WlSeatId, WlSurfaceId, ZwpKeyboardShortcutsInhibitManagerV1Id,
            ZwpKeyboardShortcutsInhibitorV1Id,
        },
        runner::{Dispatcher, Model},
        state::{ObjectId, State},
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::{
            wl_seat::WlSeat, wl_surface::WlSurface,
            zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        },
    };

    use super::{ZwpKeyboardShortcutsInhibitManagerV1, ZwpKeyboardShortcutsInhibitManagerV1Global};

    struct Fixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
        manager: Resolved<ZwpKeyboardShortcutsInhibitManagerV1Id>,
    }

    fn new_fixture() -> Fixture {
        let mut state = state_for_tests();
        let (mut client, dispatcher) = test_client_and_dispatcher(1);
        let (surface, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(5), client.client_id(), 6));
        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(3)));
        let (manager, _) = client.insert_object(
            &mut state,
            ZwpKeyboardShortcutsInhibitManagerV1 {
                id: ZwpKeyboardShortcutsInhibitManagerV1Id::from(6),
            },
        );
        Fixture { state, client, dispatcher, surface, manager }
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

    /// `inhibit_shortcuts(id, surface 5, seat 3)` on the manager (object 6).
    fn inhibit(fixture: &mut Fixture, id: u32) -> RequestResult {
        let manager = fixture.manager.object_id();
        dispatch(fixture, manager, 6, 1, &[id, 5, 3])
    }

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    /// Wire events queued for sending: `(object, opcode, arg0, arg1)`, missing args are 0.
    fn sent(dispatcher: &Dispatcher) -> Vec<(u32, u32, u32, u32)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            let arg = if buffer.len() >= 12 {
                u32::from_ne_bytes(buffer[8..12].try_into().unwrap())
            } else {
                0
            };
            let second = if buffer.len() >= 16 {
                u32::from_ne_bytes(buffer[12..16].try_into().unwrap())
            } else {
                0
            };
            sent.push((object, opcode, arg, second));
        });
        sent
    }

    #[test]
    fn the_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(ZwpKeyboardShortcutsInhibitManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = ZwpKeyboardShortcutsInhibitManagerV1Global;
        global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        assert!(client.contains_object(ZwpKeyboardShortcutsInhibitManagerV1Id::from(6), &state));
        assert_eq!(
            ZwpKeyboardShortcutsInhibitManagerV1Global.interface(),
            ("zwp_keyboard_shortcuts_inhibit_manager_v1", 1)
        );
    }

    #[test]
    fn inhibit_creates_an_active_inhibitor() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        assert!(fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state));
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(fixture.surface.rref(&fixture.state).shortcuts_inhibitor(), Some(inhibitor));
        assert!(inhibitor.rref(&fixture.state).user_enabled());
        assert_eq!(sent(&fixture.dispatcher), vec![(7, 0, 0, 0)]);
        assert!(!closed(&fixture));
    }

    #[test]
    fn a_second_inhibitor_on_the_surface_is_already_inhibited() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        inhibit(&mut fixture, 8).unwrap();
        assert!(closed(&fixture));
        assert!(!fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(8), &fixture.state));
        let attached = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(fixture.surface.rref(&fixture.state).shortcuts_inhibitor(), Some(attached));
        assert!(sent(&fixture.dispatcher).contains(&(1, 0, 6, 0)));
    }

    #[test]
    fn a_new_inhibitor_is_allowed_after_the_old_one_is_destroyed() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        let first = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap()
            .object_id();
        dispatch(&mut fixture, first, 7, 0, &[]).unwrap();
        inhibit(&mut fixture, 8).unwrap();
        let second = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(8), &fixture.state)
            .unwrap();
        assert_eq!(fixture.surface.rref(&fixture.state).shortcuts_inhibitor(), Some(second));
        assert!(sent(&fixture.dispatcher).contains(&(8, 0, 0, 0)));
        assert!(!closed(&fixture));
    }

    #[test]
    fn an_unknown_seat_disconnects() {
        let mut fixture = new_fixture();
        let manager = fixture.manager.object_id();
        let result = dispatch(&mut fixture, manager, 6, 1, &[7, 5, 99]);
        assert!(result.is_err());
        assert!(closed(&fixture));
        assert!(!fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .shortcuts_inhibitor()
            .is_none());
        assert!(!sent(&fixture.dispatcher)
            .iter()
            .any(|(object, ..)| *object == 7));
    }

    #[test]
    fn an_unknown_surface_disconnects() {
        let mut fixture = new_fixture();
        let manager = fixture.manager.object_id();
        let result = dispatch(&mut fixture, manager, 6, 1, &[7, 99, 3]);
        assert!(result.is_err());
        assert!(closed(&fixture));
        assert!(!fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .shortcuts_inhibitor()
            .is_none());
        assert!(!sent(&fixture.dispatcher)
            .iter()
            .any(|(object, ..)| *object == 7));
    }

    #[test]
    fn destroying_the_manager_keeps_the_inhibitor() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        let manager = fixture.manager.object_id();
        dispatch(&mut fixture, manager, 6, 0, &[]).unwrap();
        assert!(!fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitManagerV1Id::from(6), &fixture.state));
        assert!(fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .shortcuts_inhibitor()
            .is_some());
        assert!(!closed(&fixture));
    }

    #[test]
    fn destroying_the_surface_leaves_an_inert_inhibitor() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        {
            let mut surface = fixture
                .state
                .objects
                .grab_object::<WlSurface>(fixture.surface.object_id());
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
            surface.delete_self(&mut fixture.client, &mut fixture.state, &mut client_dispatcher);
            client_dispatcher.drain_post_actions(&mut fixture.client, &mut fixture.state);
        }
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .expect("inhibitor stays until the client destroys it");
        assert!(inhibitor.rref(&fixture.state).surface().is_none());
        assert!(!sent(&fixture.dispatcher)
            .iter()
            .any(|(object, opcode, ..)| *object == 7 && *opcode == 1));
        let object = inhibitor.object_id();
        dispatch(&mut fixture, object, 7, 0, &[]).unwrap();
        assert!(!fixture
            .client
            .contains_object(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state));
        assert!(!closed(&fixture));
    }

    #[test]
    fn client_disconnect_clears_the_surface_association() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        {
            let mut inhibitor = fixture
                .state
                .objects
                .try_grab_object::<ZwpKeyboardShortcutsInhibitorV1>(inhibitor.object_id())
                .unwrap();
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
            inhibitor.on_client_disconnect(
                &mut fixture.client,
                &mut fixture.state,
                &mut client_dispatcher,
            );
        }
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .shortcuts_inhibitor()
            .is_none());
    }

    #[test]
    fn a_denied_surface_creates_a_disabled_inhibitor() {
        let mut fixture = new_fixture();
        fixture
            .surface
            .ref_mut(&mut fixture.state)
            .set_shortcuts_inhibit_denied(true);
        inhibit(&mut fixture, 7).unwrap();
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(fixture.surface.rref(&fixture.state).shortcuts_inhibitor(), Some(inhibitor));
        assert!(!inhibitor.rref(&fixture.state).user_enabled());
        assert!(sent(&fixture.dispatcher).is_empty());
    }

    #[test]
    fn the_state_action_sends_inactive_then_active() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        fixture
            .dispatcher
            .test_take_queued_actions::<IoUringAction>();

        for active in [false, true] {
            fixture
                .client
                .process(
                    &mut fixture.state,
                    ClientAction::ShortcutsInhibitorState { inhibitor, active },
                    &mut fixture.dispatcher,
                )
                .unwrap();
        }
        assert_eq!(sent(&fixture.dispatcher), vec![(7, 1, 0, 0), (7, 0, 0, 0)]);
    }

    #[test]
    fn the_state_action_for_a_destroyed_inhibitor_sends_nothing() {
        let mut fixture = new_fixture();
        inhibit(&mut fixture, 7).unwrap();
        let inhibitor = fixture
            .client
            .resolve(ZwpKeyboardShortcutsInhibitorV1Id::from(7), &fixture.state)
            .unwrap();
        let object = inhibitor.object_id();
        dispatch(&mut fixture, object, 7, 0, &[]).unwrap();
        fixture
            .dispatcher
            .test_take_queued_actions::<IoUringAction>();

        fixture
            .client
            .process(
                &mut fixture.state,
                ClientAction::ShortcutsInhibitorState { inhibitor, active: false },
                &mut fixture.dispatcher,
            )
            .unwrap();
        assert!(sent(&fixture.dispatcher).is_empty());
    }
}
