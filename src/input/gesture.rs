//! `zwp_pointer_gestures_v1` resource bookkeeping and gesture routing.

use crate::{
    clients::{ClientDispatcher, ClientId, Resolved},
    protocol_generated::{
        WaylandId, WlSurfaceId, ZwpPointerGestureHoldV1Id, ZwpPointerGesturePinchV1Id,
        ZwpPointerGestureSwipeV1Id,
    },
    runner::Dispatcher,
    seat::{dispatch_client_event, SeatClientEvent},
    state::State,
};

/// One gesture object created through `zwp_pointer_gestures_v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerGestureResource {
    Swipe(Resolved<ZwpPointerGestureSwipeV1Id>),
    Pinch(Resolved<ZwpPointerGesturePinchV1Id>),
    Hold(Resolved<ZwpPointerGestureHoldV1Id>),
}

impl PointerGestureResource {
    pub(crate) fn wayland_id(self) -> WaylandId {
        match self {
            Self::Swipe(resource) => resource.wayland_id().into(),
            Self::Pinch(resource) => resource.wayland_id().into(),
            Self::Hold(resource) => resource.wayland_id().into(),
        }
    }

    pub(crate) fn kind(self) -> PointerGestureKind {
        match self {
            Self::Swipe(_) => PointerGestureKind::Swipe,
            Self::Pinch(_) => PointerGestureKind::Pinch,
            Self::Hold(_) => PointerGestureKind::Hold,
        }
    }

    pub(crate) fn send_begin(
        self,
        state: &State,
        serial: u32,
        time: u32,
        surface: Resolved<WlSurfaceId>,
        fingers: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        match self {
            Self::Swipe(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_begin(serial, time, surface, fingers, dispatcher);
                }
            }
            Self::Pinch(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_begin(serial, time, surface, fingers, dispatcher);
                }
            }
            Self::Hold(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_begin(serial, time, surface, fingers, dispatcher);
                }
            }
        }
    }

    pub(crate) fn send_end(
        self,
        state: &State,
        serial: u32,
        time: u32,
        cancelled: bool,
        dispatcher: &mut ClientDispatcher,
    ) {
        match self {
            Self::Swipe(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_end(serial, time, cancelled, dispatcher);
                }
            }
            Self::Pinch(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_end(serial, time, cancelled, dispatcher);
                }
            }
            Self::Hold(resource) => {
                if let Some(object) = resource.try_ref(state) {
                    object.send_end(serial, time, cancelled, dispatcher);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerGestureKind {
    Swipe,
    Pinch,
    Hold,
}

/// The libinput gesture in progress on the seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivePointerGesture {
    pub(crate) kind: PointerGestureKind,
    /// Surface whose client was sent `begin`. Only its client matters after
    /// `begin`. `None` when the gesture was swallowed or has been cancelled:
    /// later updates and the end from libinput are dropped.
    pub(crate) target: Option<Resolved<WlSurfaceId>>,
}

/// Surface a client gesture starting now would go to.
fn gesture_target(state: &State) -> Option<Resolved<WlSurfaceId>> {
    if state.seat.active_drag().is_some() || state.seat.pointer_interactive_grab.is_some() {
        return None;
    }
    state.seat.pointer_focus.map(|focus| focus.target.surface)
}

pub(crate) fn handle_pointer_gesture_begin(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    kind: PointerGestureKind,
    time: u32,
    fingers: u32,
) {
    cancel_pointer_gesture(state, dispatcher, time);
    let target = gesture_target(state);
    state.seat.active_pointer_gesture = Some(ActivePointerGesture { kind, target });
    let Some(surface) = target else {
        return;
    };
    let serial = state.seat.next_serial();
    dispatch_client_event(
        dispatcher,
        surface.client_id(),
        SeatClientEvent::PointerGestureBegin { kind, serial, time, surface, fingers },
    );
}

/// Client for an update or end of `kind`. Cancels the gesture first when the
/// client that `begin` would pick now is not the target's client.
fn live_target(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    kind: PointerGestureKind,
    time: u32,
) -> Option<ClientId> {
    let active = state.seat.active_pointer_gesture?;
    if active.kind != kind {
        return None;
    }
    let client_id = active.target?.client_id();
    if gesture_target(state).map(|surface| surface.client_id()) == Some(client_id) {
        return Some(client_id);
    }
    cancel_pointer_gesture(state, dispatcher, time);
    None
}

pub(crate) fn handle_pointer_gesture_swipe_update(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    dx: f64,
    dy: f64,
) {
    let Some(client_id) = live_target(state, dispatcher, PointerGestureKind::Swipe, time) else {
        return;
    };
    dispatch_client_event(
        dispatcher,
        client_id,
        SeatClientEvent::PointerGestureSwipeUpdate { time, dx: dx as f32, dy: dy as f32 },
    );
}

pub(crate) fn handle_pointer_gesture_pinch_update(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    dx: f64,
    dy: f64,
    scale: f64,
    rotation: f64,
) {
    let Some(client_id) = live_target(state, dispatcher, PointerGestureKind::Pinch, time) else {
        return;
    };
    dispatch_client_event(
        dispatcher,
        client_id,
        SeatClientEvent::PointerGesturePinchUpdate {
            time,
            dx: dx as f32,
            dy: dy as f32,
            scale: scale as f32,
            rotation: rotation as f32,
        },
    );
}

pub(crate) fn handle_pointer_gesture_end(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    kind: PointerGestureKind,
    time: u32,
    cancelled: bool,
) {
    let Some(active) = state.seat.active_pointer_gesture else {
        return;
    };
    if active.kind != kind {
        return;
    }
    let client_id = live_target(state, dispatcher, kind, time);
    state.seat.active_pointer_gesture = None;
    let Some(client_id) = client_id else {
        return;
    };
    let serial = state.seat.next_serial();
    dispatch_client_event(
        dispatcher,
        client_id,
        SeatClientEvent::PointerGestureEnd { kind, serial, time, cancelled },
    );
}

/// End the gesture in progress as cancelled for the client that got its
/// `begin`. libinput's remaining updates and end are dropped.
pub(crate) fn cancel_pointer_gesture(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    let Some(active) = state.seat.active_pointer_gesture.as_mut() else {
        return;
    };
    let Some(surface) = active.target.take() else {
        return;
    };
    let kind = active.kind;
    let serial = state.seat.next_serial();
    dispatch_client_event(
        dispatcher,
        surface.client_id(),
        SeatClientEvent::PointerGestureEnd { kind, serial, time, cancelled: true },
    );
}

/// Pointer focus is about to change to `new_client` (`None`: no focus). Cancel
/// unless the gesture's client keeps pointer focus.
pub(crate) fn cancel_pointer_gesture_unless_client(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    new_client: Option<ClientId>,
) {
    let keeps = state
        .seat
        .active_pointer_gesture
        .and_then(|active| active.target)
        .is_some_and(|surface| Some(surface.client_id()) == new_client);
    if !keeps {
        cancel_pointer_gesture(state, dispatcher, time);
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{ClientAction, ClientId, Resolved},
        input::types::{PointerFocus, SurfaceRect},
        protocol_generated::WlSurfaceId,
        runner::Dispatcher,
        seat::SeatClientEvent,
        state::{DragPhase, DragSource, DragState, ObjectId, State},
        test_support::{fake_focus, state_for_tests, test_client_and_dispatcher},
        wayland_objects::wl_surface::WlSurface,
    };

    use super::*;

    fn focus(state: &mut State, client: u64, surface: u32) {
        state.seat.pointer_focus = Some(PointerFocus {
            target: fake_focus(ClientId::from_raw(client), WlSurfaceId::from(surface)),
            rect: SurfaceRect { x: 0, y: 0, width: 10, height: 10 },
        });
    }

    /// Every seat event queued for `client`, in order.
    fn seat_events(dispatcher: &mut Dispatcher, client: u64) -> Vec<SeatClientEvent> {
        dispatcher
            .test_take_actions_for::<ClientAction, ClientId>(ClientId::from_raw(client))
            .into_iter()
            .filter_map(|action| match action {
                ClientAction::SeatEvent { event } => Some(event),
                _ => None,
            })
            .collect()
    }

    fn is_gesture(event: &SeatClientEvent) -> bool {
        matches!(
            event,
            SeatClientEvent::PointerGestureBegin { .. }
                | SeatClientEvent::PointerGestureSwipeUpdate { .. }
                | SeatClientEvent::PointerGesturePinchUpdate { .. }
                | SeatClientEvent::PointerGestureEnd { .. }
        )
    }

    /// Gesture events queued for `client`, in order.
    fn gesture_events(dispatcher: &mut Dispatcher, client: u64) -> Vec<SeatClientEvent> {
        seat_events(dispatcher, client)
            .into_iter()
            .filter(is_gesture)
            .collect()
    }

    fn begin_swipe(state: &mut State, dispatcher: &mut Dispatcher, time: u32, fingers: u32) {
        handle_pointer_gesture_begin(state, dispatcher, PointerGestureKind::Swipe, time, fingers);
    }

    fn swipe_update(state: &mut State, dispatcher: &mut Dispatcher, time: u32, dx: f64) {
        handle_pointer_gesture_swipe_update(state, dispatcher, time, dx, 0.0);
    }

    fn end(
        state: &mut State,
        dispatcher: &mut Dispatcher,
        kind: PointerGestureKind,
        cancelled: bool,
    ) {
        handle_pointer_gesture_end(state, dispatcher, kind, 12, cancelled);
    }

    #[test]
    fn begin_goes_to_the_pointer_focus() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        let surface = fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)).surface;
        let events = gesture_events(&mut dispatcher, 7);
        assert_eq!(events.len(), 1);
        let SeatClientEvent::PointerGestureBegin { kind, time, surface: begun, fingers, .. } =
            events[0]
        else {
            panic!("expected begin, got {:?}", events[0]);
        };
        assert_eq!((kind, time, begun, fingers), (PointerGestureKind::Swipe, 10, surface, 3));
        assert_eq!(
            state.seat.active_pointer_gesture,
            Some(ActivePointerGesture { kind: PointerGestureKind::Swipe, target: Some(surface) })
        );
    }

    #[test]
    fn updates_and_end_follow_the_begin() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);

        handle_pointer_gesture_begin(&mut state, &mut dispatcher, PointerGestureKind::Pinch, 10, 2);
        handle_pointer_gesture_pinch_update(&mut state, &mut dispatcher, 11, 1.5, -2.0, 1.25, 4.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Pinch, false);

        let events = gesture_events(&mut dispatcher, 7);
        assert_eq!(events.len(), 3);
        let SeatClientEvent::PointerGestureBegin { serial: begin_serial, .. } = events[0] else {
            panic!("expected begin, got {:?}", events[0]);
        };
        let SeatClientEvent::PointerGesturePinchUpdate { time, dx, dy, scale, rotation } =
            events[1]
        else {
            panic!("expected pinch update, got {:?}", events[1]);
        };
        assert_eq!((time, dx, dy, scale, rotation), (11, 1.5, -2.0, 1.25, 4.0));
        let SeatClientEvent::PointerGestureEnd {
            kind: PointerGestureKind::Pinch,
            serial: end_serial,
            time: 12,
            cancelled: false,
        } = events[2]
        else {
            panic!("expected pinch end, got {:?}", events[2]);
        };
        assert_ne!(begin_serial, end_serial);
        assert!(state.seat.active_pointer_gesture.is_none());
    }

    #[test]
    fn libinput_cancel_is_forwarded() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);

        handle_pointer_gesture_begin(&mut state, &mut dispatcher, PointerGestureKind::Hold, 10, 3);
        end(&mut state, &mut dispatcher, PointerGestureKind::Hold, true);

        let events = gesture_events(&mut dispatcher, 7);
        assert!(matches!(
            events.last(),
            Some(SeatClientEvent::PointerGestureEnd {
                kind: PointerGestureKind::Hold,
                cancelled: true,
                ..
            })
        ));
    }

    #[test]
    fn begin_without_focus_swallows_the_whole_gesture() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();

        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        focus(&mut state, 7, 50);
        swipe_update(&mut state, &mut dispatcher, 11, 2.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert!(gesture_events(&mut dispatcher, 7).is_empty());
        assert!(state.seat.active_pointer_gesture.is_none());
    }

    #[test]
    fn begin_during_a_drag_is_swallowed() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        let source_client_id = ClientId::from_raw(7);
        state.seat.set_active_drag(DragState {
            source: DragSource::None { initiator_client_id: source_client_id },
            origin_surface: Resolved::new(
                source_client_id,
                WlSurfaceId::from(10),
                ObjectId::from(10usize),
            ),
            icon_surface: None,
            target: None,
            negotiation: Default::default(),
            phase: DragPhase::Dragging,
        });

        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        assert!(gesture_events(&mut dispatcher, 7).is_empty());
        assert_eq!(
            state
                .seat
                .active_pointer_gesture
                .map(|active| active.target),
            Some(None)
        );
    }

    #[test]
    fn focus_moving_to_another_client_cancels() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        focus(&mut state, 8, 60);

        swipe_update(&mut state, &mut dispatcher, 11, 2.0);
        swipe_update(&mut state, &mut dispatcher, 12, 2.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        let events = gesture_events(&mut dispatcher, 7);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            SeatClientEvent::PointerGestureEnd {
                kind: PointerGestureKind::Swipe,
                cancelled: true,
                ..
            }
        ));
        assert!(gesture_events(&mut dispatcher, 8).is_empty());
    }

    #[test]
    fn focus_moving_within_the_client_keeps_the_gesture() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        focus(&mut state, 7, 51);

        swipe_update(&mut state, &mut dispatcher, 11, 3.0);

        let events = gesture_events(&mut dispatcher, 7);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            SeatClientEvent::PointerGestureSwipeUpdate { dx, .. } if dx == 3.0
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, SeatClientEvent::PointerGestureEnd { .. })));
    }

    #[test]
    fn clearing_focus_cancels_before_leave() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        crate::seat::quiesce_for_session_pause(&mut state, &mut dispatcher);

        let events = seat_events(&mut dispatcher, 7);
        let cancel = events.iter().position(|event| {
            matches!(event, SeatClientEvent::PointerGestureEnd { cancelled: true, .. })
        });
        let leave = events
            .iter()
            .position(|event| matches!(event, SeatClientEvent::PointerLeave { .. }));
        assert!(cancel.is_some() && leave.is_some() && cancel < leave, "{events:?}");

        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);
        assert!(gesture_events(&mut dispatcher, 7).is_empty());
    }

    #[test]
    fn end_of_another_kind_is_ignored() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        end(&mut state, &mut dispatcher, PointerGestureKind::Pinch, false);

        assert_eq!(gesture_events(&mut dispatcher, 7).len(), 1);
        assert_eq!(
            state.seat.active_pointer_gesture.map(|active| active.kind),
            Some(PointerGestureKind::Swipe)
        );
    }

    #[test]
    fn a_new_begin_cancels_the_previous_gesture() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        handle_pointer_gesture_begin(&mut state, &mut dispatcher, PointerGestureKind::Pinch, 11, 2);

        let events = gesture_events(&mut dispatcher, 7);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[1],
            SeatClientEvent::PointerGestureEnd {
                kind: PointerGestureKind::Swipe,
                cancelled: true,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            SeatClientEvent::PointerGestureBegin { kind: PointerGestureKind::Pinch, .. }
        ));
    }

    #[test]
    fn client_removal_drops_the_target() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        let _ = state.seat.remove_client(ClientId::from_raw(7));
        assert_eq!(
            state
                .seat
                .active_pointer_gesture
                .map(|active| active.target),
            Some(None)
        );
        // Drop the begin that was sent before the client went away.
        assert_eq!(gesture_events(&mut dispatcher, 7).len(), 1);
        focus(&mut state, 7, 50);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert!(gesture_events(&mut dispatcher, 7).is_empty());
    }

    #[test]
    fn refresh_to_no_focus_cancels_before_leave() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        focus(&mut state, 7, 50);
        begin_swipe(&mut state, &mut dispatcher, 4, 3);

        crate::input::refresh_pointer_focus(&mut state, &mut dispatcher, 5, false);

        let events = seat_events(&mut dispatcher, 7);
        let cancel = events.iter().position(|event| {
            matches!(event, SeatClientEvent::PointerGestureEnd { cancelled: true, time: 5, .. })
        });
        let leave = events
            .iter()
            .position(|event| matches!(event, SeatClientEvent::PointerLeave { .. }));
        assert!(cancel.is_some() && leave.is_some() && cancel < leave, "{events:?}");
    }

    #[test]
    fn destroyed_focus_surface_cancels() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(50), ClientId::from_raw(7), 6),
        );
        state.seat.pointer_focus = Some(PointerFocus {
            target: crate::input::SurfaceFocus::new(surface),
            rect: SurfaceRect { x: 0, y: 0, width: 10, height: 10 },
        });
        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            crate::input::focus::leave_destroyed_surface_focus(
                &mut state,
                &mut client_dispatcher,
                surface,
            );
        }

        let events = gesture_events(&mut dispatcher, 7);
        assert!(matches!(
            events.last(),
            Some(SeatClientEvent::PointerGestureEnd { cancelled: true, .. })
        ));
        assert!(state.seat.pointer_focus.is_none());
    }
}
