use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    input::gesture::PointerGestureResource,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wl_display, zwp_pointer_gestures_v1, Constants, NewId, WaylandId, WlPointerId,
        ZwpPointerGestureHoldV1Id, ZwpPointerGesturePinchV1Id, ZwpPointerGestureSwipeV1Id,
        ZwpPointerGesturesV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_display::WlDisplay, zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
        zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
        zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
    },
};

#[derive(Debug)]
pub struct ZwpPointerGesturesV1Global;

impl ZwpPointerGesturesV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for ZwpPointerGesturesV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (ZwpPointerGesturesV1::INTERFACE_NAME, ZwpPointerGesturesV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            ZwpPointerGesturesV1 { id: ZwpPointerGesturesV1Id::from(id), version },
        );
    }
}

#[derive(Debug)]
pub struct ZwpPointerGesturesV1 {
    id: ZwpPointerGesturesV1Id,
    version: u32,
}

impl Object for ZwpPointerGesturesV1 {
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
        use zwp_pointer_gestures_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            GetSwipeGesture { id, pointer } => {
                let pointer =
                    client.resolve_or_disconnect(WlPointerId::from(pointer), state, dispatcher)?;
                let (swipe, _) =
                    client.insert_object(state, ZwpPointerGestureSwipeV1::new(id, pointer));
                state
                    .seat
                    .register_pointer_gesture(pointer, PointerGestureResource::Swipe(swipe));
            }
            GetPinchGesture { id, pointer } => {
                let pointer =
                    client.resolve_or_disconnect(WlPointerId::from(pointer), state, dispatcher)?;
                let (pinch, _) =
                    client.insert_object(state, ZwpPointerGesturePinchV1::new(id, pointer));
                state
                    .seat
                    .register_pointer_gesture(pointer, PointerGestureResource::Pinch(pinch));
            }
            Release => {
                if self.version < zwp_pointer_gestures_v1::RELEASE_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "zwp_pointer_gestures_v1.release requires version >= 2",
                    );
                    return Ok(());
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetHoldGesture { id, pointer } => {
                if self.version < zwp_pointer_gestures_v1::GET_HOLD_GESTURE_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "zwp_pointer_gestures_v1.get_hold_gesture requires version >= 3",
                    );
                    return Ok(());
                }
                let pointer =
                    client.resolve_or_disconnect(WlPointerId::from(pointer), state, dispatcher)?;
                let (hold, _) =
                    client.insert_object(state, ZwpPointerGestureHoldV1::new(id, pointer));
                state
                    .seat
                    .register_pointer_gesture(pointer, PointerGestureResource::Hold(hold));
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
        input::gesture::PointerGestureResource,
        object::RequestResult,
        protocol_generated::{
            Constants, NewId, WlPointerId, ZwpPointerGestureHoldV1Id, ZwpPointerGesturePinchV1Id,
            ZwpPointerGestureSwipeV1Id, ZwpPointerGesturesV1Id,
        },
        state::{ObjectId, State},
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::wl_pointer::WlPointer,
    };

    use super::{ZwpPointerGesturesV1, ZwpPointerGesturesV1Global};

    struct Fixture {
        state: State,
        client: Client,
        dispatcher: crate::runner::Dispatcher,
        pointer: Resolved<WlPointerId>,
        manager: Resolved<ZwpPointerGesturesV1Id>,
    }

    /// The manager is bound through the global, so `version` goes through `bind`.
    fn fixture(version: u32) -> Fixture {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        state.seat.register_pointer(pointer);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            ZwpPointerGesturesV1Global.bind(
                &mut client,
                &mut state,
                NewId::from(6),
                version,
                &mut client_dispatcher,
            );
        }
        let manager = client
            .resolve(ZwpPointerGesturesV1Id::from(6), &state)
            .expect("manager is bound");
        Fixture { state, client, dispatcher, pointer, manager }
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

    fn manager_request(fixture: &mut Fixture, request_id: u16, words: &[u32]) -> RequestResult {
        let manager = fixture.manager.object_id();
        dispatch(fixture, manager, 6, request_id, words)
    }

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    #[test]
    fn pointer_gestures_global_is_advertised_at_version_three() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(ZwpPointerGesturesV1::INTERFACE_NAME));
        assert_eq!(ZwpPointerGesturesV1Global.interface(), ("zwp_pointer_gestures_v1", 3));

        // Bound at version 2, so get_hold_gesture (since 3) must disconnect.
        let mut fixture = fixture(2);
        manager_request(&mut fixture, 3, &[9, 5]).unwrap();
        assert!(closed(&fixture));
    }

    #[test]
    fn each_get_request_registers_one_object_of_its_kind() {
        let mut fixture = fixture(3);
        manager_request(&mut fixture, 0, &[7, 5]).unwrap();
        manager_request(&mut fixture, 1, &[8, 5]).unwrap();
        manager_request(&mut fixture, 3, &[9, 5]).unwrap();

        let swipe = fixture
            .client
            .resolve(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state)
            .unwrap();
        let pinch = fixture
            .client
            .resolve(ZwpPointerGesturePinchV1Id::from(8), &fixture.state)
            .unwrap();
        let hold = fixture
            .client
            .resolve(ZwpPointerGestureHoldV1Id::from(9), &fixture.state)
            .unwrap();
        assert_eq!(
            fixture.state.seat.pointer_gestures(fixture.pointer),
            &[
                PointerGestureResource::Swipe(swipe),
                PointerGestureResource::Pinch(pinch),
                PointerGestureResource::Hold(hold),
            ]
        );
    }

    #[test]
    fn destroying_a_gesture_object_unregisters_it() {
        let mut fixture = fixture(3);
        manager_request(&mut fixture, 0, &[7, 5]).unwrap();
        manager_request(&mut fixture, 1, &[8, 5]).unwrap();
        let swipe = fixture
            .client
            .resolve(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state)
            .unwrap();
        let pinch = fixture
            .client
            .resolve(ZwpPointerGesturePinchV1Id::from(8), &fixture.state)
            .unwrap();

        dispatch(&mut fixture, swipe.object_id(), 7, 0, &[]).unwrap();

        assert_eq!(
            fixture.state.seat.pointer_gestures(fixture.pointer),
            &[PointerGestureResource::Pinch(pinch)]
        );
        assert!(!fixture
            .client
            .contains_object(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state));
    }

    #[test]
    fn release_keeps_created_gestures() {
        let mut fixture = fixture(2);
        manager_request(&mut fixture, 0, &[7, 5]).unwrap();
        let swipe = fixture
            .client
            .resolve(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state)
            .unwrap();

        manager_request(&mut fixture, 2, &[]).unwrap();

        assert!(!fixture
            .client
            .contains_object(ZwpPointerGesturesV1Id::from(6), &fixture.state));
        assert!(fixture
            .client
            .contains_object(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state));
        assert_eq!(
            fixture.state.seat.pointer_gestures(fixture.pointer),
            &[PointerGestureResource::Swipe(swipe)]
        );
        assert!(!closed(&fixture));
    }

    #[test]
    fn release_below_version_two_disconnects() {
        let mut fixture = fixture(1);
        manager_request(&mut fixture, 2, &[]).unwrap();
        assert!(closed(&fixture));
    }

    #[test]
    fn get_hold_gesture_below_version_three_disconnects() {
        let mut fixture = fixture(2);
        manager_request(&mut fixture, 3, &[9, 5]).unwrap();
        assert!(closed(&fixture));
        assert!(!fixture
            .client
            .contains_object(ZwpPointerGestureHoldV1Id::from(9), &fixture.state));
    }

    #[test]
    fn releasing_the_wl_pointer_or_disconnecting_forgets_its_gestures() {
        let mut fixture = fixture(3);
        manager_request(&mut fixture, 0, &[7, 5]).unwrap();
        fixture.state.seat.unregister_pointer(fixture.pointer);
        assert!(fixture
            .state
            .seat
            .pointer_gestures(fixture.pointer)
            .is_empty());

        fixture.state.seat.register_pointer(fixture.pointer);
        manager_request(&mut fixture, 1, &[8, 5]).unwrap();
        assert_eq!(fixture.state.seat.pointer_gestures(fixture.pointer).len(), 1);

        fixture.state.seat.remove_client(ClientId::from_raw(1));
        assert!(fixture
            .state
            .seat
            .pointer_gestures(fixture.pointer)
            .is_empty());
    }
}
