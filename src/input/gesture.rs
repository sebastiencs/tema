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

/// Travel needed before a bound swipe runs its action, in logical px.
pub(crate) const GESTURE_TRIGGER_DISTANCE: f64 = 64.0;
/// Travel before a bound horizontal swipe decides whether to track, logical px.
pub(crate) const WORKSPACE_SWIPE_START_DISTANCE: f64 = 16.0;
/// Finger travel (logical px) for a full slide (Hyprland's `workspace_swipe_distance`).
pub(crate) const WORKSPACE_SWIPE_DISTANCE: f64 = 300.0;
/// Progress at or past which a slow release commits.
pub(crate) const WORKSPACE_SWIPE_COMMIT_PROGRESS: f64 = 0.5;
/// Release speed (logical px/ms along the binding direction) that decides by direction alone.
pub(crate) const WORKSPACE_SWIPE_FLING_VELOCITY: f64 = 0.5;
/// A release this long (ms) after the last update has no fling.
pub(crate) const WORKSPACE_SWIPE_FLING_MAX_AGE_MS: u32 = 50;

/// A swipe tema owns through `[gestures]`. It never reaches clients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OwnedSwipe {
    pub(crate) fingers: u32,
    /// Accumulated libinput deltas since `begin`, logical px.
    pub(crate) dx: f64,
    pub(crate) dy: f64,
    pub(crate) mode: OwnedSwipeMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkspaceSwipe {
    pub(crate) start: crate::state::WorkspaceSwipeStart,
    /// `-1.0` for a `left` binding, `+1.0` for `right`: travel = sign * dx.
    pub(crate) sign: f64,
    pub(crate) progress: f64,
    pub(crate) travel: f64,
    pub(crate) last_time: u32,
    /// Logical px/ms along the travel, exponential moving average.
    pub(crate) velocity: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OwnedSwipeMode {
    /// Travel is below the start distance; the direction is still open.
    Undecided,
    /// The direction is decided when the fingers lift.
    Trigger,
    /// Progress follows the fingers on a workspace slide.
    Workspace(WorkspaceSwipe),
    /// The gesture ends without running anything.
    Aborted,
}

/// The libinput gesture in progress on the seat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ActivePointerGesture {
    pub(crate) kind: PointerGestureKind,
    /// Surface whose client was sent `begin`. Only its client matters after
    /// `begin`. `None` when the gesture was swallowed or has been cancelled:
    /// later updates and the end from libinput are dropped.
    pub(crate) target: Option<Resolved<WlSurfaceId>>,
    /// Set when tema owns the swipe through a `[gestures]` binding.
    pub(crate) owned: Option<OwnedSwipe>,
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
    abort_owned_swipe(state, dispatcher);
    cancel_pointer_gesture(state, dispatcher, time);
    if kind == PointerGestureKind::Swipe
        && !state.session_lock.input_gated()
        && state.config.swipe_fingers_bound(fingers)
    {
        state.seat.active_pointer_gesture = Some(ActivePointerGesture {
            kind,
            target: None,
            owned: Some(OwnedSwipe { fingers, dx: 0.0, dy: 0.0, mode: OwnedSwipeMode::Undecided }),
        });
        return;
    }
    let target = gesture_target(state);
    state.seat.active_pointer_gesture = Some(ActivePointerGesture { kind, target, owned: None });
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
    if state
        .seat
        .active_pointer_gesture
        .is_some_and(|active| active.kind == PointerGestureKind::Swipe && active.owned.is_some())
    {
        owned_swipe_update(state, dispatcher, time, dx, dy);
        return;
    }
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
    if let Some(owned) = active.owned {
        state.seat.active_pointer_gesture = None;
        match owned.mode {
            OwnedSwipeMode::Workspace(swipe) => {
                release_workspace_swipe(state, dispatcher, swipe, time, cancelled)
            }
            OwnedSwipeMode::Trigger | OwnedSwipeMode::Undecided if !cancelled => {
                run_swipe_binding(state, dispatcher, owned)
            }
            _ => {}
        }
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

fn swipe_direction(dx: f64, dy: f64) -> Option<crate::config::SwipeDirection> {
    use crate::config::SwipeDirection;
    if dx.abs().max(dy.abs()) < GESTURE_TRIGGER_DISTANCE {
        return None;
    }
    Some(if dx.abs() >= dy.abs() {
        if dx < 0.0 {
            SwipeDirection::Left
        } else {
            SwipeDirection::Right
        }
    } else if dy < 0.0 {
        SwipeDirection::Up
    } else {
        SwipeDirection::Down
    })
}

fn run_swipe_binding(state: &mut State, dispatcher: &mut Dispatcher, owned: OwnedSwipe) {
    let Some(direction) = swipe_direction(owned.dx, owned.dy) else {
        return;
    };
    let Some(action) = state.config.binding_for_swipe(owned.fingers, direction) else {
        return;
    };
    log::info!(
        "seat executing gesture binding fingers={} direction={direction:?} action={action:?}",
        owned.fingers
    );
    crate::config::execute_action(state, dispatcher, action);
}

fn owned_swipe(state: &mut State) -> Option<&mut OwnedSwipe> {
    state.seat.active_pointer_gesture.as_mut()?.owned.as_mut()
}

fn owned_swipe_update(state: &mut State, dispatcher: &mut Dispatcher, time: u32, dx: f64, dy: f64) {
    let Some(owned) = owned_swipe(state) else {
        return;
    };
    owned.dx += dx;
    owned.dy += dy;
    let owned = *owned;
    match owned.mode {
        OwnedSwipeMode::Undecided => {
            if owned.dx.abs().max(owned.dy.abs()) < WORKSPACE_SWIPE_START_DISTANCE {
                return;
            }
            let mode = start_workspace_swipe(state, owned, time);
            if let Some(current) = owned_swipe(state) {
                current.mode = mode;
            }
            if let OwnedSwipeMode::Workspace(swipe) = mode {
                let outputs = state.workspace_group_output_ids(swipe.start.group_id);
                crate::wayland_objects::ext_workspace_manager_v1::publish_workspace_state_changed(
                    state, dispatcher, outputs,
                );
                track_workspace_swipe(state, dispatcher, time);
            }
        }
        OwnedSwipeMode::Workspace(_) => track_workspace_swipe(state, dispatcher, time),
        OwnedSwipeMode::Trigger | OwnedSwipeMode::Aborted => {}
    }
}

/// Decide, at the start distance, whether this swipe tracks a workspace slide.
fn start_workspace_swipe(state: &mut State, owned: OwnedSwipe, time: u32) -> OwnedSwipeMode {
    use crate::{
        config::{CompositorAction, SwipeDirection},
        visual_animation::WorkspaceTransitionDirection,
    };
    if owned.dx.abs() < owned.dy.abs() {
        return OwnedSwipeMode::Trigger;
    }
    let (direction, sign) =
        if owned.dx < 0.0 { (SwipeDirection::Left, -1.0) } else { (SwipeDirection::Right, 1.0) };
    let transition_direction = match state.config.binding_for_swipe(owned.fingers, direction) {
        Some(CompositorAction::WorkspaceNext) => WorkspaceTransitionDirection::Next,
        Some(CompositorAction::WorkspacePrevious) => WorkspaceTransitionDirection::Previous,
        _ => return OwnedSwipeMode::Trigger,
    };
    let Some(start) = state.begin_workspace_swipe(transition_direction) else {
        return OwnedSwipeMode::Trigger;
    };
    OwnedSwipeMode::Workspace(WorkspaceSwipe {
        start,
        sign,
        progress: 0.0,
        travel: 0.0,
        last_time: time,
        velocity: 0.0,
    })
}

/// Apply the accumulated travel to the tracked slide.
fn track_workspace_swipe(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    let Some(owned) = owned_swipe(state).copied() else {
        return;
    };
    let OwnedSwipeMode::Workspace(mut swipe) = owned.mode else {
        return;
    };
    let travel = swipe.sign * owned.dx;
    let dt = time.wrapping_sub(swipe.last_time);
    if dt > 0 && dt < u32::MAX / 2 {
        let instant = (travel - swipe.travel) / f64::from(dt);
        swipe.velocity = 0.5 * swipe.velocity + 0.5 * instant;
        swipe.last_time = time;
    }
    swipe.travel = travel;
    swipe.progress = (travel / WORKSPACE_SWIPE_DISTANCE).clamp(0.0, 1.0);
    let progress = crate::visual_animation::UnitValue::saturating_from_f64(swipe.progress);
    let mode = match state.set_workspace_swipe_progress(swipe.start, progress) {
        None => OwnedSwipeMode::Aborted,
        Some(changed) => {
            if changed {
                let outputs = state.workspace_group_output_ids(swipe.start.group_id);
                crate::drm::request_outputs(state, outputs, dispatcher);
            }
            OwnedSwipeMode::Workspace(swipe)
        }
    };
    if let Some(current) = owned_swipe(state) {
        current.mode = mode;
    }
}

/// Fingers lifted on a tracked slide.
fn release_workspace_swipe(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    swipe: WorkspaceSwipe,
    time: u32,
    cancelled: bool,
) {
    let velocity = if time.wrapping_sub(swipe.last_time) > WORKSPACE_SWIPE_FLING_MAX_AGE_MS {
        0.0
    } else {
        swipe.velocity
    };
    let commit = !cancelled
        && if velocity.abs() >= WORKSPACE_SWIPE_FLING_VELOCITY {
            velocity > 0.0
        } else {
            swipe.progress >= WORKSPACE_SWIPE_COMMIT_PROGRESS
        };
    settle_tracked_swipe(state, dispatcher, swipe, commit);
}

fn settle_tracked_swipe(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    swipe: WorkspaceSwipe,
    commit: bool,
) {
    use crate::state::WorkspaceSwipeSettle;
    let group_id = swipe.start.group_id;
    match state.settle_workspace_swipe(swipe.start, commit) {
        None => {}
        Some(WorkspaceSwipeSettle::Committed) => {
            log::info!("workspace swipe committed on group {group_id:?}");
            crate::config::finish_workspace_activation(state, dispatcher, group_id);
        }
        Some(WorkspaceSwipeSettle::Returning) => {
            let outputs = state.workspace_group_output_ids(group_id);
            crate::drm::request_outputs(state, outputs, dispatcher);
            crate::drm::kick_visual_animation_pump(dispatcher);
        }
        Some(WorkspaceSwipeSettle::Dropped) => {
            let outputs = state.workspace_group_output_ids(group_id);
            crate::wayland_objects::ext_workspace_manager_v1::publish_workspace_state_changed(
                state,
                dispatcher,
                outputs.iter().copied(),
            );
            crate::drm::request_outputs(state, outputs, dispatcher);
        }
    }
}

/// Stop an owned swipe: it ends without running anything. A tracked workspace
/// slide is cancelled (it returns to its origin) unless it is already gone.
pub(crate) fn abort_owned_swipe(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(owned) = owned_swipe(state) else {
        return;
    };
    let mode = std::mem::replace(&mut owned.mode, OwnedSwipeMode::Aborted);
    if let OwnedSwipeMode::Workspace(swipe) = mode {
        settle_tracked_swipe(state, dispatcher, swipe, false);
    }
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
        config::{CompositorAction, GestureBinding, SwipeDirection},
        input::types::{PointerFocus, SurfaceRect},
        process::ProcessManagerAction,
        protocol_generated::{ExtSessionLockV1Id, WlSurfaceId},
        runner::Dispatcher,
        seat::SeatClientEvent,
        session_lock::PreLockFocus,
        state::{DragPhase, DragSource, DragState, ObjectId, OutputId, State},
        test_support::{fake_focus, state_for_tests, test_client_and_dispatcher},
        visual_animation::WorkspaceTransitionDirection,
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
            Some(ActivePointerGesture {
                kind: PointerGestureKind::Swipe,
                target: Some(surface),
                owned: None,
            })
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

    fn bind_swipe(state: &mut State, fingers: u32, direction: SwipeDirection) {
        state.config.gesture_bindings.push(GestureBinding {
            description: format!("swipe:{fingers}:{direction:?}"),
            fingers,
            direction,
            action: CompositorAction::Spawn { command: "true".to_string() },
        });
    }

    fn spawned(dispatcher: &Dispatcher) -> bool {
        dispatcher.test_has_queued_action(|action: &ProcessManagerAction| {
            matches!(action, ProcessManagerAction::Spawn(_))
        })
    }

    #[test]
    fn bound_finger_count_is_owned_and_never_reaches_clients() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Left);
        focus(&mut state, 7, 50);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        swipe_update(&mut state, &mut dispatcher, 11, -40.0);
        swipe_update(&mut state, &mut dispatcher, 12, -40.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert!(gesture_events(&mut dispatcher, 7).is_empty());
        assert!(spawned(&dispatcher));
        assert!(state.seat.active_pointer_gesture.is_none());
    }

    #[test]
    fn short_or_unbound_direction_runs_nothing() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Left);
        focus(&mut state, 7, 50);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        swipe_update(&mut state, &mut dispatcher, 11, -50.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);
        assert!(!spawned(&dispatcher));

        begin_swipe(&mut state, &mut dispatcher, 20, 3);
        swipe_update(&mut state, &mut dispatcher, 21, 100.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);
        assert!(!spawned(&dispatcher));
        assert!(gesture_events(&mut dispatcher, 7).is_empty());
    }

    #[test]
    fn vertical_dominant_swipe_uses_the_vertical_binding() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Down);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        handle_pointer_gesture_swipe_update(&mut state, &mut dispatcher, 11, 30.0, 80.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert!(spawned(&dispatcher));
    }

    #[test]
    fn unbound_finger_count_goes_to_clients() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Left);
        focus(&mut state, 7, 50);

        begin_swipe(&mut state, &mut dispatcher, 10, 4);

        assert!(matches!(
            gesture_events(&mut dispatcher, 7).as_slice(),
            [SeatClientEvent::PointerGestureBegin {
                kind: PointerGestureKind::Swipe,
                fingers: 4,
                ..
            }]
        ));
    }

    #[test]
    fn cancelled_or_aborted_owned_swipe_runs_nothing() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Left);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);
        swipe_update(&mut state, &mut dispatcher, 11, -100.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, true);
        assert!(!spawned(&dispatcher));

        begin_swipe(&mut state, &mut dispatcher, 20, 3);
        swipe_update(&mut state, &mut dispatcher, 21, -100.0);
        abort_owned_swipe(&mut state, &mut dispatcher);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);
        assert!(!spawned(&dispatcher));
    }

    #[test]
    fn session_lock_never_owns_a_swipe() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        bind_swipe(&mut state, 3, SwipeDirection::Left);
        state.session_lock.install(
            ClientId::from_raw(7),
            Resolved::new(
                ClientId::from_raw(7),
                ExtSessionLockV1Id::from(9),
                ObjectId::from(9usize),
            ),
            false,
            PreLockFocus::default(),
            std::iter::empty::<OutputId>(),
        );
        focus(&mut state, 7, 50);

        begin_swipe(&mut state, &mut dispatcher, 10, 3);

        assert!(matches!(
            gesture_events(&mut dispatcher, 7).as_slice(),
            [SeatClientEvent::PointerGestureBegin { .. }]
        ));
    }

    /// Output with a four-workspace group, bound to the example config's swipes.
    fn workspace_setup(state: &mut State) -> OutputId {
        state.config.animations.set_workspace_duration_msec(250);
        let output = state.insert_output(crate::test_support::test_output_drm0_wh(200, 100));
        state.config.gesture_bindings = vec![
            GestureBinding {
                description: "swipe:3:left".into(),
                fingers: 3,
                direction: SwipeDirection::Left,
                action: CompositorAction::WorkspaceNext,
            },
            GestureBinding {
                description: "swipe:3:right".into(),
                fingers: 3,
                direction: SwipeDirection::Right,
                action: CompositorAction::WorkspacePrevious,
            },
        ];
        output
    }

    fn group_of(state: &State, output: OutputId) -> crate::state::WorkspaceGroup {
        let group_id = state.workspace_group_for_output(output).unwrap();
        state.workspace_groups.try_get(group_id).unwrap().clone()
    }

    fn workspaces_of(state: &State, output: OutputId) -> Vec<crate::state::WorkspaceId> {
        group_of(state, output).workspaces
    }

    fn active_of(state: &State, output: OutputId) -> crate::state::WorkspaceId {
        group_of(state, output).active_workspace.unwrap()
    }

    fn transition_of(
        state: &State,
        output: OutputId,
    ) -> Option<crate::visual_animation::WorkspaceGroupTransition> {
        group_of(state, output).transition
    }

    fn owned_mode(state: &State) -> OwnedSwipeMode {
        state
            .seat
            .active_pointer_gesture
            .and_then(|active| active.owned)
            .unwrap()
            .mode
    }

    fn frame_requested(dispatcher: &Dispatcher, output: OutputId) -> bool {
        dispatcher.test_has_queued_action_for(
            crate::scheduler::output_scheduler::output_scheduler_id(output),
            |action: &crate::scheduler::output_scheduler::OutputSchedulerAction| {
                matches!(
                    action,
                    crate::scheduler::output_scheduler::OutputSchedulerAction::RequestFrame { .. }
                )
            },
        )
    }

    fn release_at(state: &mut State, dispatcher: &mut Dispatcher, time: u32, cancelled: bool) {
        handle_pointer_gesture_end(state, dispatcher, PointerGestureKind::Swipe, time, cancelled);
    }

    #[test]
    fn small_travel_does_not_start_tracking() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -10.0);

        assert_eq!(owned_mode(&state), OwnedSwipeMode::Undecided);
        assert!(transition_of(&state, output).is_none());
    }

    #[test]
    fn left_swipe_tracks_the_next_workspace() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);

        let transition = transition_of(&state, output).unwrap();
        assert!(transition.is_tracking());
        assert_eq!(transition.to_workspace(), workspaces[1]);
        assert_eq!(transition.direction(), WorkspaceTransitionDirection::Next);
        let visible = crate::scene::visibility::output_workspace_ids(
            &state,
            output,
            crate::scene::WorkspaceParticipationPolicy::Steady,
        );
        assert!(visible.contains(&workspaces[0]) && visible.contains(&workspaces[1]));

        swipe_update(&mut state, &mut dispatcher, 10, -120.0);

        let transition = transition_of(&state, output).unwrap();
        assert_eq!(
            transition.progress(),
            crate::visual_animation::UnitValue::saturating_from_f64(0.5)
        );
        assert_eq!(active_of(&state, output), workspaces[0]);
        assert!(frame_requested(&dispatcher, output));
    }

    #[test]
    fn slow_release_past_half_commits() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        for time in [0, 10, 20, 30, 40, 50] {
            swipe_update(&mut state, &mut dispatcher, time, -30.0);
        }
        release_at(&mut state, &mut dispatcher, 110, false);

        assert_eq!(active_of(&state, output), workspaces[1]);
        let transition = transition_of(&state, output).unwrap();
        assert_eq!(transition.goal(), crate::visual_animation::UnitValue::ONE);
        assert!(!transition.is_tracking());
    }

    #[test]
    fn slow_release_before_half_cancels() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        swipe_update(&mut state, &mut dispatcher, 10, -60.0);
        release_at(&mut state, &mut dispatcher, 70, false);

        assert_eq!(active_of(&state, output), workspaces[0]);
        if let Some(transition) = transition_of(&state, output) {
            assert_eq!(transition.goal(), crate::visual_animation::UnitValue::ZERO);
            assert!(!transition.is_tracking());
        }
    }

    #[test]
    fn fling_commits_below_half() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        swipe_update(&mut state, &mut dispatcher, 10, -45.0);
        swipe_update(&mut state, &mut dispatcher, 20, -45.0);
        release_at(&mut state, &mut dispatcher, 25, false);

        assert_eq!(active_of(&state, output), workspaces[1]);
    }

    #[test]
    fn fling_back_cancels_above_half() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        swipe_update(&mut state, &mut dispatcher, 100, -180.0);
        swipe_update(&mut state, &mut dispatcher, 105, 15.0);
        swipe_update(&mut state, &mut dispatcher, 110, 15.0);
        release_at(&mut state, &mut dispatcher, 112, false);

        assert_eq!(active_of(&state, output), workspaces[0]);
        if let Some(transition) = transition_of(&state, output) {
            assert_eq!(transition.goal(), crate::visual_animation::UnitValue::ZERO);
        }
    }

    #[test]
    fn reversing_past_zero_clamps_to_the_original_workspace() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -60.0);
        swipe_update(&mut state, &mut dispatcher, 10, 150.0);

        let transition = transition_of(&state, output).unwrap();
        assert_eq!(transition.progress(), crate::visual_animation::UnitValue::ZERO);
        assert!(transition.is_tracking());
        assert_eq!(transition.from_workspace(), workspaces[0]);
        assert_eq!(transition.to_workspace(), workspaces[1]);
    }

    #[test]
    fn right_swipe_on_the_first_workspace_wraps_to_the_last() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, 30.0);

        let transition = transition_of(&state, output).unwrap();
        assert_eq!(transition.to_workspace(), *workspaces.last().unwrap());
        assert_eq!(transition.direction(), WorkspaceTransitionDirection::Previous);
    }

    #[test]
    fn keybinding_mid_swipe_aborts_tracking() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        crate::config::execute_action(
            &mut state,
            &mut dispatcher,
            CompositorAction::WorkspaceIndex { index: 3 },
        );
        swipe_update(&mut state, &mut dispatcher, 10, -30.0);

        assert_eq!(owned_mode(&state), OwnedSwipeMode::Aborted);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);
        assert_eq!(active_of(&state, output), workspaces[2]);
    }

    #[test]
    fn session_lock_mid_swipe_returns_the_slide() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        swipe_update(&mut state, &mut dispatcher, 10, -90.0);
        crate::seat::gate_for_session_lock(&mut state, &mut dispatcher);

        assert_eq!(owned_mode(&state), OwnedSwipeMode::Aborted);
        let transition = transition_of(&state, output).unwrap();
        assert_eq!(transition.goal(), crate::visual_animation::UnitValue::ZERO);
        assert_eq!(active_of(&state, output), workspaces[0]);
    }

    #[test]
    fn vertical_or_other_action_falls_back_to_trigger() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        state.config.gesture_bindings = vec![GestureBinding {
            description: "swipe:3:left".into(),
            fingers: 3,
            direction: SwipeDirection::Left,
            action: CompositorAction::Spawn { command: "true".to_string() },
        }];

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        assert_eq!(owned_mode(&state), OwnedSwipeMode::Trigger);
        assert!(transition_of(&state, output).is_none());
        swipe_update(&mut state, &mut dispatcher, 10, -120.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert!(spawned(&dispatcher));
    }

    #[test]
    fn swipe_during_a_running_slide_snaps_and_tracks() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);
        crate::config::execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceNext);
        assert_eq!(active_of(&state, output), workspaces[1]);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);

        let transition = transition_of(&state, output).unwrap();
        assert!(transition.is_tracking());
        assert_eq!(transition.from_workspace(), workspaces[1]);
        assert_eq!(transition.to_workspace(), workspaces[2]);
    }

    #[test]
    fn disabled_workspace_animation_falls_back_to_trigger() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);
        state.config.animations.set_workspace_duration_msec(0);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        assert_eq!(owned_mode(&state), OwnedSwipeMode::Trigger);
        swipe_update(&mut state, &mut dispatcher, 10, -120.0);
        end(&mut state, &mut dispatcher, PointerGestureKind::Swipe, false);

        assert_eq!(active_of(&state, output), workspaces[1]);
    }

    #[test]
    fn new_begin_mid_swipe_returns_the_slide() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        swipe_update(&mut state, &mut dispatcher, 10, -120.0);
        handle_pointer_gesture_begin(&mut state, &mut dispatcher, PointerGestureKind::Pinch, 20, 2);

        let transition = transition_of(&state, output).unwrap();
        assert!(!transition.is_tracking());
        assert_eq!(transition.goal(), crate::visual_animation::UnitValue::ZERO);
        assert_eq!(active_of(&state, output), workspaces[0]);
    }

    #[test]
    fn vt_switch_mid_swipe_drops_the_slide() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let output = workspace_setup(&mut state);
        let workspaces = workspaces_of(&state, output);

        begin_swipe(&mut state, &mut dispatcher, 0, 3);
        swipe_update(&mut state, &mut dispatcher, 0, -30.0);
        state.session_active = false;
        crate::seat::quiesce_for_session_pause(&mut state, &mut dispatcher);

        assert!(transition_of(&state, output).is_none());
        assert_eq!(active_of(&state, output), workspaces[0]);
    }
}
