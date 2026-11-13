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

    use {
        super::{ZwpPointerGesturesV1, ZwpPointerGesturesV1Global},
        crate::{
            clients::ClientAction,
            input::gesture::PointerGestureKind,
            io_uring::model::IoUringAction,
            protocol_generated::WlSurfaceId,
            runner::{Dispatcher, Model},
            seat::SeatClientEvent,
            wayland_objects::{
                wl_surface::WlSurface, zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
                zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
                zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
            },
            wire_scale::WireScale,
        },
    };

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

    /// Client 3 with a real surface 4. Pointer 5 owns swipe 7, pinch 8 and
    /// hold 9; pointer 10 owns swipe 11.
    struct WriterFixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
    }

    fn writer_fixture() -> WriterFixture {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(3), 6),
        );
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        state.seat.register_pointer(pointer);
        let (swipe, _) = client.insert_object(
            &mut state,
            ZwpPointerGestureSwipeV1::new(ZwpPointerGestureSwipeV1Id::from(7), pointer),
        );
        let (pinch, _) = client.insert_object(
            &mut state,
            ZwpPointerGesturePinchV1::new(ZwpPointerGesturePinchV1Id::from(8), pointer),
        );
        let (hold, _) = client.insert_object(
            &mut state,
            ZwpPointerGestureHoldV1::new(ZwpPointerGestureHoldV1Id::from(9), pointer),
        );
        state
            .seat
            .register_pointer_gesture(pointer, PointerGestureResource::Swipe(swipe));
        state
            .seat
            .register_pointer_gesture(pointer, PointerGestureResource::Pinch(pinch));
        state
            .seat
            .register_pointer_gesture(pointer, PointerGestureResource::Hold(hold));

        let (other, _) = client.insert_object(&mut state, WlPointer::new(WlPointerId::from(10), 9));
        state.seat.register_pointer(other);
        let (other_swipe, _) = client.insert_object(
            &mut state,
            ZwpPointerGestureSwipeV1::new(ZwpPointerGestureSwipeV1Id::from(11), other),
        );
        state
            .seat
            .register_pointer_gesture(other, PointerGestureResource::Swipe(other_swipe));

        WriterFixture { state, client, dispatcher, surface }
    }

    fn fixed_bits(value: f32) -> u32 {
        ((value * 256.0).round() as i32) as u32
    }

    fn words(buffer: &[u8]) -> Vec<u32> {
        buffer
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    fn send(fixture: &mut WriterFixture, event: SeatClientEvent) {
        fixture
            .client
            .process(&mut fixture.state, ClientAction::SeatEvent { event }, &mut fixture.dispatcher)
            .unwrap();
    }

    /// Number of queued `sendmsg` buffers whose words satisfy `pred`.
    fn count_sent(fixture: &mut WriterFixture, pred: impl Fn(&[u32]) -> bool) -> usize {
        fixture
            .dispatcher
            .test_count_queued_action::<IoUringAction>(|action| {
                matches!(action, IoUringAction::Sendmsg { buffer, .. } if pred(&words(buffer)))
            })
    }

    fn destroy_swipe_seven(fixture: &mut WriterFixture) {
        let object = fixture
            .client
            .resolve(ZwpPointerGestureSwipeV1Id::from(7), &fixture.state)
            .expect("swipe 7 exists")
            .object_id();
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(7u32.into());
        fixture
            .state
            .objects
            .grab(object)
            .on(&mut fixture.client, &mut fixture.state, request(7, 0, &[]), &mut client_dispatcher)
            .unwrap();
    }

    #[test]
    fn begin_reaches_every_object_of_its_kind_only() {
        let mut fixture = writer_fixture();
        let surface = fixture.surface;
        send(
            &mut fixture,
            SeatClientEvent::PointerGestureBegin {
                kind: PointerGestureKind::Swipe,
                serial: 20,
                time: 30,
                surface,
                fingers: 3,
            },
        );

        assert_eq!(count_sent(&mut fixture, |w| w == [7, 24 << 16, 20, 30, 4, 3]), 1);
        assert_eq!(count_sent(&mut fixture, |w| w == [11, 24 << 16, 20, 30, 4, 3]), 1);
        assert_eq!(count_sent(&mut fixture, |w| matches!(w.first(), Some(8 | 9))), 0);
    }

    #[test]
    fn swipe_update_deltas_follow_the_wire_scale() {
        let mut fixture = writer_fixture();
        fixture.client.set_wire_scale_for_tests(WireScale::new(2));
        send(
            &mut fixture,
            SeatClientEvent::PointerGestureSwipeUpdate { time: 5, dx: 1.5, dy: -0.25 },
        );

        let expected = [fixed_bits(3.0), fixed_bits(-0.5)];
        assert_eq!(
            count_sent(&mut fixture, |w| w == [7, (20 << 16) | 1, 5, expected[0], expected[1]]),
            1
        );
        assert_eq!(
            count_sent(&mut fixture, |w| w == [11, (20 << 16) | 1, 5, expected[0], expected[1]]),
            1
        );
    }

    #[test]
    fn pinch_update_scales_deltas_but_not_scale_or_rotation() {
        let mut fixture = writer_fixture();
        fixture.client.set_wire_scale_for_tests(WireScale::new(2));
        send(
            &mut fixture,
            SeatClientEvent::PointerGesturePinchUpdate {
                time: 5,
                dx: 1.0,
                dy: 2.0,
                scale: 1.5,
                rotation: -10.0,
            },
        );

        assert_eq!(
            count_sent(&mut fixture, |w| {
                w == [
                    8,
                    (28 << 16) | 1,
                    5,
                    fixed_bits(2.0),
                    fixed_bits(4.0),
                    fixed_bits(1.5),
                    fixed_bits(-10.0),
                ]
            }),
            1
        );
    }

    #[test]
    fn end_encodes_opcode_and_cancelled_flag() {
        let mut fixture = writer_fixture();
        send(
            &mut fixture,
            SeatClientEvent::PointerGestureEnd {
                kind: PointerGestureKind::Hold,
                serial: 21,
                time: 6,
                cancelled: true,
            },
        );
        assert_eq!(count_sent(&mut fixture, |w| w == [9, (20 << 16) | 1, 21, 6, 1]), 1);

        send(
            &mut fixture,
            SeatClientEvent::PointerGestureEnd {
                kind: PointerGestureKind::Swipe,
                serial: 22,
                time: 6,
                cancelled: false,
            },
        );
        assert_eq!(count_sent(&mut fixture, |w| w == [7, (20 << 16) | 2, 22, 6, 0]), 1);
        assert_eq!(count_sent(&mut fixture, |w| w == [11, (20 << 16) | 2, 22, 6, 0]), 1);
    }

    #[test]
    fn begin_for_a_dead_surface_is_skipped() {
        let mut fixture = writer_fixture();
        let dead =
            Resolved::new(ClientId::from_raw(3), WlSurfaceId::from(99), ObjectId::from(999usize));
        send(
            &mut fixture,
            SeatClientEvent::PointerGestureBegin {
                kind: PointerGestureKind::Swipe,
                serial: 20,
                time: 30,
                surface: dead,
                fingers: 3,
            },
        );

        assert_eq!(count_sent(&mut fixture, |_| true), 0);
    }

    #[test]
    fn a_destroyed_gesture_object_gets_nothing() {
        let mut fixture = writer_fixture();
        destroy_swipe_seven(&mut fixture);
        let surface = fixture.surface;
        send(
            &mut fixture,
            SeatClientEvent::PointerGestureBegin {
                kind: PointerGestureKind::Swipe,
                serial: 23,
                time: 30,
                surface,
                fingers: 3,
            },
        );

        assert_eq!(count_sent(&mut fixture, |w| w == [11, 24 << 16, 23, 30, 4, 3]), 1);
        assert_eq!(count_sent(&mut fixture, |w| matches!(w.first(), Some(7))), 0);
    }
}
