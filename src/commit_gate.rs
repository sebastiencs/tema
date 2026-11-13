//! Per-surface fifo barriers and timed commit waits, and the timerfd that wakes
//! the commit queues when a wait ends. A barrier is set when an update with
//! `wp_fifo_v1.set_barrier` applies, and it clears on the next latch of the
//! surface's output that shows the surface (or after a fallback deadline).

use std::{
    collections::BTreeMap,
    os::fd::{AsFd, AsRawFd},
    time::{Duration, Instant},
};

use nix::sys::timerfd::{self, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

use crate::{
    clients::{ClientAction, ClientId, Resolved},
    io_uring::{model::IoUringAction, ring::RingId},
    protocol_generated::WlSurfaceId,
    runner::{Dispatcher, Model, ModelFailure, Redispatcher},
    state::{OutputId, State},
};

pub(crate) const FIFO_FALLBACK_SHORT: Duration = Duration::from_millis(25);
pub(crate) const FIFO_FALLBACK_LONG_MIN: Duration = Duration::from_millis(100);
const FIFO_FALLBACK_LONG_REFRESHES: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FifoBarrier {
    pub(crate) seq: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LatchKind {
    Hard,
    Soft,
}

/// A barrier as it was when a frame latched. `in_frame` tells whether the
/// barrier's surface was one of the frame's callback surfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NotedBarrier {
    pub(crate) surface: Resolved<WlSurfaceId>,
    pub(crate) seq: u64,
    pub(crate) in_frame: bool,
}

#[derive(Debug, Default)]
pub(crate) struct CommitGate {
    barriers: BTreeMap<Resolved<WlSurfaceId>, FifoBarrier>,
    /// Surfaces whose queue head waits for a time.
    timed: BTreeMap<Resolved<WlSurfaceId>, Instant>,
    next_seq: u64,
    #[cfg(test)]
    pub(crate) test_now: Option<Instant>,
}

impl CommitGate {
    pub(crate) fn now(&self) -> Instant {
        #[cfg(test)]
        {
            if let Some(now) = self.test_now {
                return now;
            }
        }
        Instant::now()
    }

    pub(crate) fn has_barrier(&self, surface: Resolved<WlSurfaceId>) -> bool {
        self.barriers.contains_key(&surface)
    }

    pub(crate) fn has_any_barrier(&self) -> bool {
        !self.barriers.is_empty()
    }

    pub(crate) fn barrier(&self, surface: Resolved<WlSurfaceId>) -> Option<FifoBarrier> {
        self.barriers.get(&surface).copied()
    }

    pub(crate) fn barriers(
        &self,
    ) -> impl Iterator<Item = (Resolved<WlSurfaceId>, FifoBarrier)> + '_ {
        self.barriers
            .iter()
            .map(|(surface, barrier)| (*surface, *barrier))
    }

    /// Sets the surface's barrier, replacing any older one, and returns its sequence number.
    pub(crate) fn insert_barrier(
        &mut self,
        surface: Resolved<WlSurfaceId>,
        deadline: Instant,
    ) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.barriers.insert(surface, FifoBarrier { seq, deadline });
        seq
    }

    /// Removes the barrier only if it is still the one with `seq`.
    pub(crate) fn remove_barrier(&mut self, surface: Resolved<WlSurfaceId>, seq: u64) -> bool {
        if self
            .barriers
            .get(&surface)
            .is_some_and(|barrier| barrier.seq == seq)
        {
            self.barriers.remove(&surface);
            true
        } else {
            false
        }
    }

    /// Moves the deadline of the barrier with `seq` forward to `deadline` if that is earlier.
    pub(crate) fn shorten_barrier(
        &mut self,
        surface: Resolved<WlSurfaceId>,
        seq: u64,
        deadline: Instant,
    ) {
        if let Some(barrier) = self.barriers.get_mut(&surface) {
            if barrier.seq == seq {
                barrier.deadline = barrier.deadline.min(deadline);
            }
        }
    }

    pub(crate) fn wait_until(&mut self, surface: Resolved<WlSurfaceId>, at: Instant) {
        self.timed.insert(surface, at);
    }

    /// Removes every barrier and timed wait that is due at `now`, and returns their
    /// surfaces, each once.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<Resolved<WlSurfaceId>> {
        let mut due = Vec::new();
        self.barriers.retain(|surface, barrier| {
            if barrier.deadline <= now {
                due.push(*surface);
                false
            } else {
                true
            }
        });
        self.timed.retain(|surface, at| {
            if *at <= now {
                due.push(*surface);
                false
            } else {
                true
            }
        });
        due.sort();
        due.dedup();
        due
    }

    /// The earliest deadline of any barrier or timed wait.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.barriers
            .values()
            .map(|barrier| barrier.deadline)
            .chain(self.timed.values().copied())
            .min()
    }

    pub(crate) fn forget_surface(&mut self, surface: Resolved<WlSurfaceId>) {
        self.barriers.remove(&surface);
        self.timed.remove(&surface);
    }

    pub(crate) fn remove_client(&mut self, client: ClientId) {
        self.barriers
            .retain(|surface, _| surface.client_id() != client);
        self.timed
            .retain(|surface, _| surface.client_id() != client);
    }
}

pub(crate) fn refresh_period(refresh_millihz: u32) -> Option<Duration> {
    (refresh_millihz > 0)
        .then(|| Duration::from_nanos(1_000_000_000_000 / u64::from(refresh_millihz)))
}

fn output_period(state: &State, output: OutputId) -> Option<Duration> {
    state
        .outputs
        .try_get(output)
        .and_then(|o| refresh_period(o.current_mode.refresh_millihz.max(0) as u32))
}

pub(crate) fn fallback_deadline(state: &State, output: Option<OutputId>, now: Instant) -> Instant {
    let long = output.filter(|_| state.session_active).and_then(|output| {
        let snapshot = state.frame_scheduler.get(&output)?;
        if snapshot.clock.mode == crate::scheduler::frame_clock::FrameClockMode::Passive {
            return None;
        }
        let period = output_period(state, output)?;
        Some((period * FIFO_FALLBACK_LONG_REFRESHES).max(FIFO_FALLBACK_LONG_MIN))
    });
    now + long.unwrap_or(FIFO_FALLBACK_SHORT)
}

/// An update with `set_barrier` was applied (the surface is grabbed by the caller).
pub(crate) fn set_barrier(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    surface: Resolved<WlSurfaceId>,
    active_output: Option<OutputId>,
) {
    let now = state.commit_gate.now();
    let deadline = fallback_deadline(state, active_output, now);
    state.commit_gate.insert_barrier(surface, deadline);
    if let Some(output) = active_output {
        crate::drm::request_output(state, output, dispatcher);
    }
    dispatcher.dispatch(CommitGateAction::Arm);
}

const MAX_PREDICTION_PERIODS: u32 = 8;
const MAX_VBLANK_MARGIN: Duration = Duration::from_millis(2);

/// The time at which an update with `target` may apply. It is the target itself
/// unless the output's last present is trustworthy and on a fixed grid; then it
/// is the first predicted vblank at or after `target + margin`, plus `margin`.
/// Late is allowed, early is not.
pub(crate) fn ready_at_with_now(
    state: &State,
    output: Option<OutputId>,
    output_count: usize,
    target: Instant,
    now: Instant,
) -> Instant {
    if output_count > 1 {
        return target;
    }
    let Some(snapshot) = output.and_then(|o| state.frame_scheduler.get(&o)) else {
        return target;
    };
    let clock = &snapshot.clock;
    if clock.mode != crate::scheduler::frame_clock::FrameClockMode::Fixed || clock.tearing {
        return target;
    }
    let (Some(last), Some(period)) =
        (clock.last_present, refresh_period(clock.nominal_refresh_millihz))
    else {
        return target;
    };
    if !(last.flags.refresh_is_constant && !last.flags.async_flip && last.flags.kernel_timestamp) {
        return target;
    }
    let base = last.present_time;
    if now.saturating_duration_since(base) > period * MAX_PREDICTION_PERIODS {
        return target;
    }
    let margin = (period / 4).min(MAX_VBLANK_MARGIN);
    let Some(goal) = target.checked_add(margin) else {
        return target;
    };
    if goal <= base {
        return base;
    }
    let period_ns = period.as_nanos();
    let k = (goal - base).as_nanos().div_ceil(period_ns); // >= 1
    let previous_ns = u64::try_from((k - 1) * period_ns).unwrap_or(u64::MAX);
    base.checked_add(Duration::from_nanos(previous_ns))
        .and_then(|vblank| vblank.checked_add(margin))
        .unwrap_or(target)
}

pub(crate) fn wait_until(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    surface: Resolved<WlSurfaceId>,
    at: Instant,
) {
    state.commit_gate.wait_until(surface, at);
    dispatcher.dispatch(CommitGateAction::Arm);
}

/// Called right after a frame was submitted or ended without presenting. It
/// runs inside the DRM submit path, so it reads only the gate: the barriers'
/// surfaces are read later, by the `OutputLatched` action. Each barrier's
/// sequence number is recorded now, so a barrier set after this latch is not
/// cleared by it.
pub(crate) fn note_latched(
    gate: &CommitGate,
    output_id: OutputId,
    surfaces: &[Resolved<WlSurfaceId>],
    kind: LatchKind,
    dispatcher: &mut Dispatcher,
) {
    if !gate.has_any_barrier() {
        return;
    }
    let barriers = gate
        .barriers()
        .map(|(surface, barrier)| NotedBarrier {
            surface,
            seq: barrier.seq,
            in_frame: surfaces.contains(&surface),
        })
        .collect();
    dispatcher.dispatch(CommitGateAction::OutputLatched { output_id, kind, barriers });
}

/// Which noted barriers this latch acts on, and how. A barrier whose surface is
/// in the frame latches with `kind` when the surface's active output is this
/// output or it has none. A barrier whose surface is on this output but not in
/// the frame (occluded) latches softly. Others are left alone.
fn classify_latch(
    state: &State,
    output_id: OutputId,
    kind: LatchKind,
    barriers: &[NotedBarrier],
) -> Vec<(Resolved<WlSurfaceId>, u64, LatchKind)> {
    barriers
        .iter()
        .filter_map(|noted| {
            let active = noted
                .surface
                .try_ref(state)
                .and_then(|surface| surface.active_output_id());
            let on_this_output = active == Some(output_id);
            if noted.in_frame && (on_this_output || active.is_none()) {
                Some((noted.surface, noted.seq, kind))
            } else if on_this_output {
                Some((noted.surface, noted.seq, LatchKind::Soft))
            } else {
                None
            }
        })
        .collect()
}

fn on_output_latched(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    output_id: OutputId,
    latched: Vec<(Resolved<WlSurfaceId>, u64, LatchKind)>,
    now: Instant,
) {
    let period = output_period(state, output_id).unwrap_or(FIFO_FALLBACK_SHORT);
    for (surface, seq, kind) in latched {
        match kind {
            LatchKind::Hard => {
                if state.commit_gate.remove_barrier(surface, seq) {
                    dispatcher.dispatch_to(surface.client_id(), ClientAction::FlushCommitQueues);
                }
            }
            LatchKind::Soft => state
                .commit_gate
                .shorten_barrier(surface, seq, now + period),
        }
    }
}

pub(crate) struct CommitGateModel {
    fd: TimerFd,
    read: Option<RingId>,
    stopped: bool,
}

pub(crate) enum CommitGateAction {
    Arm,
    Fired,
    OutputLatched { output_id: OutputId, kind: LatchKind, barriers: Vec<NotedBarrier> },
    Stop,
}

impl CommitGateModel {
    pub(crate) fn new() -> Self {
        let fd = TimerFd::new(
            timerfd::ClockId::CLOCK_MONOTONIC,
            TimerFlags::TFD_CLOEXEC | TimerFlags::TFD_NONBLOCK,
        )
        .expect("failed to create commit gate timerfd");
        Self { fd, read: None, stopped: false }
    }

    fn service(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        let now = state.commit_gate.now();
        for surface in state.commit_gate.take_due(now) {
            dispatcher.dispatch_to(surface.client_id(), ClientAction::FlushCommitQueues);
        }
        match state.commit_gate.next_deadline() {
            None => {
                let _ = self.fd.unset();
            }
            Some(at) => {
                let wait = at
                    .saturating_duration_since(now)
                    .max(Duration::from_micros(1));
                if let Err(error) = self
                    .fd
                    .set(Expiration::OneShot(wait.into()), TimerSetTimeFlags::empty())
                {
                    log::warn!("failed to arm commit gate timer: {error}");
                }
                if self.read.is_none() {
                    let id = RingId::fresh();
                    self.read = Some(id);
                    dispatcher.dispatch(IoUringAction::Read {
                        id: Some(id),
                        fd: self.fd.as_fd().as_raw_fd(),
                        offset: 0,
                        length: std::mem::size_of::<u64>(),
                        dispatch_to: Redispatcher::to::<CommitGateModel>(|_| {
                            CommitGateAction::Fired
                        }),
                    });
                }
            }
        }
    }
}

impl Model for CommitGateModel {
    type Action = CommitGateAction;

    const STOP: Option<Self::Action> = Some(CommitGateAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> Result<(), ModelFailure> {
        if self.stopped {
            if matches!(action, CommitGateAction::Fired) {
                self.read = None;
            }
            return Ok(());
        }
        match action {
            CommitGateAction::Arm => self.service(state, dispatcher),
            CommitGateAction::Fired => {
                self.read = None;
                self.service(state, dispatcher);
            }
            CommitGateAction::OutputLatched { output_id, kind, barriers } => {
                let now = state.commit_gate.now();
                let latched = classify_latch(state, output_id, kind, &barriers);
                on_output_latched(state, dispatcher, output_id, latched, now);
                self.service(state, dispatcher);
            }
            CommitGateAction::Stop => {
                self.stopped = true;
                let _ = self.fd.unset();
                if let Some(id) = self.read.take() {
                    dispatcher.dispatch(IoUringAction::Cancel { target: id });
                }
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
            scheduler::{
                frame_clock::{FrameClock, FrameClockMode, PresentFlags, PresentSample},
                output_scheduler::OutputSchedulerSnapshot,
            },
            state::{BackendOutputRef, DrmId, ObjectId},
            test_support::{
                insert_root_surface, resolved_wl_surface, state_for_tests, test_output,
            },
        },
    };

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn key(client: u64, wl_id: u32, object: u32) -> Resolved<WlSurfaceId> {
        Resolved::new(
            ClientId::from_raw(client),
            WlSurfaceId::from(wl_id),
            ObjectId::from_parts(object, 0),
        )
    }

    fn snapshot(
        mode: FrameClockMode,
        millihz: u32,
        tearing: bool,
        last: Option<(Instant, PresentFlags)>,
    ) -> OutputSchedulerSnapshot {
        let mut clock = FrameClock::new();
        clock.update_mode(mode, millihz);
        clock.set_tearing(tearing);
        if let Some((present_time, flags)) = last {
            clock.notify_presented(PresentSample {
                present_time,
                sequence: 1,
                flags,
                timing: None,
            });
        }
        OutputSchedulerSnapshot {
            clock: clock.snapshot(),
            blocked_on_pipeline: false,
            flip_watchdog_deadline: None,
            render_dispatched: false,
            session_active: true,
            presented_frames: 0,
            discarded_frames: 0,
            watchdog_timeouts: 0,
            last_predicted_present: None,
            last_actual_present: None,
            last_present_error_ns: None,
            mode_transition_count: 0,
            vrr_transition_count: 0,
        }
    }

    fn test_out(state: &mut State, drm: usize, refresh_millihz: i32) -> OutputId {
        let mut output = test_output(
            BackendOutputRef::new(DrmId::from(drm)),
            drm as u32,
            0,
            0,
            1920,
            1080,
            "out",
        );
        output.current_mode.refresh_millihz = refresh_millihz;
        state.insert_output(output)
    }

    /// A root surface on `out`, returned as its queue key.
    fn surface_on(state: &mut State, out: OutputId, wl_id: u32) -> Resolved<WlSurfaceId> {
        let object = insert_root_surface(state, wl_id);
        let surface = resolved_wl_surface(state, object);
        let _ = surface.ref_mut(state).update_output_membership(vec![out]);
        surface
    }

    #[test]
    fn insert_barrier_replaces_with_a_new_sequence() {
        let mut gate = CommitGate::default();
        let surface = key(1, 5, 1);
        let t0 = Instant::now();
        let first = gate.insert_barrier(surface, t0 + ms(10));
        let second = gate.insert_barrier(surface, t0 + ms(20));
        assert_ne!(first, second);
        let barrier = gate.barrier(surface).unwrap();
        assert_eq!(barrier.seq, second);
        assert_eq!(barrier.deadline, t0 + ms(20));
    }

    #[test]
    fn fallback_without_an_output_is_short() {
        let state = state_for_tests();
        let now = Instant::now();
        assert_eq!(fallback_deadline(&state, None, now), now + FIFO_FALLBACK_SHORT);
    }

    #[test]
    fn fallback_on_a_scheduled_60hz_output_is_long() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let now = Instant::now();
        state
            .frame_scheduler
            .insert(out, snapshot(FrameClockMode::Fixed, 60_000, false, None));
        assert_eq!(fallback_deadline(&state, Some(out), now), now + ms(100));
    }

    #[test]
    fn fallback_is_short_when_passive_inactive_or_unscheduled() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let now = Instant::now();
        assert_eq!(fallback_deadline(&state, Some(out), now), now + FIFO_FALLBACK_SHORT);

        state
            .frame_scheduler
            .insert(out, snapshot(FrameClockMode::Passive, 60_000, false, None));
        assert_eq!(fallback_deadline(&state, Some(out), now), now + FIFO_FALLBACK_SHORT);

        state
            .frame_scheduler
            .insert(out, snapshot(FrameClockMode::Fixed, 60_000, false, None));
        state.session_active = false;
        assert_eq!(fallback_deadline(&state, Some(out), now), now + FIFO_FALLBACK_SHORT);
    }

    #[test]
    fn fallback_on_a_24hz_output_is_four_refreshes() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 24_000);
        state
            .frame_scheduler
            .insert(out, snapshot(FrameClockMode::Fixed, 24_000, false, None));
        let now = Instant::now();
        assert_eq!(
            fallback_deadline(&state, Some(out), now),
            now + Duration::from_nanos(4 * 41_666_666)
        );
    }

    #[test]
    fn take_due_returns_expired_barriers_and_timed_surfaces() {
        let mut gate = CommitGate::default();
        let t0 = Instant::now();
        let (a, b, c) = (key(1, 5, 1), key(1, 6, 2), key(1, 7, 3));
        gate.insert_barrier(a, t0 + ms(10));
        gate.insert_barrier(b, t0 + ms(100));
        gate.wait_until(c, t0 + ms(5));
        assert_eq!(gate.take_due(t0 + ms(20)), vec![a, c]);
        assert!(!gate.has_barrier(a));
        assert!(gate.has_barrier(b));
        assert!(gate.take_due(t0 + ms(20)).is_empty());
    }

    #[test]
    fn next_deadline_is_the_earliest_of_both_maps() {
        let mut gate = CommitGate::default();
        let t0 = Instant::now();
        assert_eq!(gate.next_deadline(), None);
        gate.insert_barrier(key(1, 5, 1), t0 + ms(30));
        gate.wait_until(key(1, 6, 2), t0 + ms(10));
        assert_eq!(gate.next_deadline(), Some(t0 + ms(10)));
    }

    #[test]
    fn hard_latch_clears_and_flushes() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let now = state.commit_gate.now();
        let seq = state
            .commit_gate
            .insert_barrier(s, now + Duration::from_secs(1));
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, out, &[s], LatchKind::Hard, &mut d);
        assert!(d.test_has_queued_action::<CommitGateAction>(|a| {
            matches!(a, CommitGateAction::OutputLatched { kind: LatchKind::Hard, barriers, .. }
                if barriers.as_slice() == [NotedBarrier { surface: s, seq, in_frame: true }])
        }));
        let latched = classify_latch(
            &state,
            out,
            LatchKind::Hard,
            &[NotedBarrier { surface: s, seq, in_frame: true }],
        );
        assert_eq!(latched, vec![(s, seq, LatchKind::Hard)]);
        on_output_latched(&mut state, &mut d, out, latched, now);
        assert!(state.commit_gate.barrier(s).is_none());
        assert!(d
            .test_has_queued_action_for::<ClientAction, _>(s.client_id(), |a: &ClientAction| {
                matches!(a, ClientAction::FlushCommitQueues)
            }));
    }

    #[test]
    fn soft_latch_shortens_to_one_refresh() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let now = state.commit_gate.now();
        let seq = state
            .commit_gate
            .insert_barrier(s, now + Duration::from_secs(1));
        let mut d = Dispatcher::default();
        on_output_latched(&mut state, &mut d, out, vec![(s, seq, LatchKind::Soft)], now);
        assert_eq!(
            state.commit_gate.barrier(s).unwrap().deadline,
            now + Duration::from_nanos(16_666_666)
        );
    }

    #[test]
    fn tearing_latch_does_not_clear() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let now = state.commit_gate.now();
        let seq = state
            .commit_gate
            .insert_barrier(s, now + Duration::from_secs(1));
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, out, &[s], LatchKind::Soft, &mut d);
        assert!(d.test_has_queued_action::<CommitGateAction>(|a| {
            matches!(a, CommitGateAction::OutputLatched { kind: LatchKind::Soft, barriers, .. }
                if barriers.as_slice() == [NotedBarrier { surface: s, seq, in_frame: true }])
        }));
        let latched = classify_latch(
            &state,
            out,
            LatchKind::Soft,
            &[NotedBarrier { surface: s, seq, in_frame: true }],
        );
        assert_eq!(latched, vec![(s, seq, LatchKind::Soft)]);
        on_output_latched(&mut state, &mut d, out, latched, now);
        assert!(state.commit_gate.has_barrier(s));
    }

    #[test]
    fn occluded_surface_on_the_output_gets_a_soft_latch() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let seq = state
            .commit_gate
            .insert_barrier(s, state.commit_gate.now() + ms(500));
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, out, &[], LatchKind::Hard, &mut d);
        assert!(d.test_has_queued_action::<CommitGateAction>(|a| {
            matches!(a, CommitGateAction::OutputLatched { barriers, .. }
                if barriers.as_slice() == [NotedBarrier { surface: s, seq, in_frame: false }])
        }));
        let latched = classify_latch(
            &state,
            out,
            LatchKind::Hard,
            &[NotedBarrier { surface: s, seq, in_frame: false }],
        );
        assert_eq!(latched, vec![(s, seq, LatchKind::Soft)]);
    }

    #[test]
    fn latch_on_another_output_keeps_the_barrier() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let other = test_out(&mut state, 2, 60_000);
        let s = surface_on(&mut state, out, 5);
        state
            .commit_gate
            .insert_barrier(s, state.commit_gate.now() + ms(500));
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, other, &[s], LatchKind::Hard, &mut d);
        let seq = state.commit_gate.barrier(s).unwrap().seq;
        let latched = classify_latch(
            &state,
            other,
            LatchKind::Hard,
            &[NotedBarrier { surface: s, seq, in_frame: true }],
        );
        assert!(latched.is_empty());
        assert!(state.commit_gate.has_barrier(s));
    }

    #[test]
    fn stale_latch_keeps_a_newer_barrier() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let now = state.commit_gate.now();
        let old = state.commit_gate.insert_barrier(s, now + ms(500));
        let newer = state.commit_gate.insert_barrier(s, now + ms(500));
        let mut d = Dispatcher::default();
        on_output_latched(&mut state, &mut d, out, vec![(s, old, LatchKind::Hard)], now);
        assert_eq!(state.commit_gate.barrier(s).unwrap().seq, newer);
        assert!(!d.test_has_queued_action_for::<ClientAction, _>(s.client_id(), |_| true));
    }

    #[test]
    fn note_latched_dispatches_nothing_without_barriers() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, out, &[s], LatchKind::Hard, &mut d);
        assert!(!d.test_has_queued_action::<CommitGateAction>(|_| true));
    }

    #[test]
    fn stopped_model_ignores_arm_and_latch() {
        let mut state = state_for_tests();
        let now = Instant::now();
        state.commit_gate.wait_until(key(1, 5, 1), now + ms(50));
        let mut model = CommitGateModel::new();
        let mut d = Dispatcher::default();
        model
            .process(&mut state, CommitGateAction::Arm, &mut d)
            .unwrap();
        assert!(d.test_has_queued_action::<IoUringAction>(|_| true));

        let mut stopped = CommitGateModel::new();
        let mut d = Dispatcher::default();
        stopped
            .process(&mut state, CommitGateAction::Stop, &mut d)
            .unwrap();
        stopped
            .process(&mut state, CommitGateAction::Arm, &mut d)
            .unwrap();
        assert!(!d.test_has_queued_action::<IoUringAction>(|_| true));
    }

    #[test]
    fn remove_client_drops_its_entries() {
        let mut gate = CommitGate::default();
        let t0 = Instant::now();
        let (kept, gone) = (key(1, 5, 1), key(2, 6, 2));
        gate.insert_barrier(kept, t0 + ms(10));
        gate.insert_barrier(gone, t0 + ms(10));
        gate.wait_until(gone, t0 + ms(20));
        gate.remove_client(ClientId::from_raw(2));
        assert!(gate.has_barrier(kept));
        assert!(!gate.has_barrier(gone));
        assert_eq!(gate.next_deadline(), Some(t0 + ms(10)));
    }

    const P: Duration = Duration::from_nanos(16_666_666);
    const M: Duration = Duration::from_millis(2);

    fn good() -> PresentFlags {
        PresentFlags {
            zero_copy: false,
            refresh_is_constant: true,
            async_flip: false,
            kernel_timestamp: true,
        }
    }

    /// An output with a snapshot whose last present is at `last`, and the state holding it.
    fn ready_fixture(
        millihz: u32,
        mode: FrameClockMode,
        tearing: bool,
        last: Option<(Instant, PresentFlags)>,
    ) -> (State, OutputId) {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, millihz as i32);
        state
            .frame_scheduler
            .insert(out, snapshot(mode, millihz, tearing, last));
        (state, out)
    }

    #[test]
    fn target_between_vblanks() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), b + P * 5 + M);
    }

    #[test]
    fn target_just_before_a_vblank() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = b + P * 6 - ms(1);
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), b + P * 6 + M);
    }

    #[test]
    fn target_well_before_a_vblank() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = b + P * 6 - ms(3);
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), b + P * 5 + M);
    }

    #[test]
    fn grid_late_by_one_ms_still_not_early() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        // The real vblanks are 1 ms after the sampled present time.
        let real_vblank = b + ms(1) + P * 6;
        let result = ready_at_with_now(&state, Some(out), 1, real_vblank, t0);
        assert_eq!(result, b + P * 6 + M);
        assert!(result > real_vblank);
    }

    #[test]
    fn target_before_last_present_is_base() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        // target + margin <= base: the last present is already late enough.
        assert_eq!(ready_at_with_now(&state, Some(out), 1, b - ms(3), t0), b);
        // target + margin = base + 1 ms: one margin after the last vblank, never before it.
        assert_eq!(ready_at_with_now(&state, Some(out), 1, b - ms(1), t0), b + M);
    }

    #[test]
    fn stale_history_returns_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 9;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = t0 + ms(50);
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);
    }

    #[test]
    fn async_last_sample_returns_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let flags = PresentFlags { async_flip: true, ..good() };
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, flags)));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);
    }

    #[test]
    fn non_kernel_timestamp_returns_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let flags = PresentFlags { kernel_timestamp: false, ..good() };
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, flags)));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);
    }

    #[test]
    fn variable_refresh_returns_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) =
            ready_fixture(60_000, FrameClockMode::Variable, false, Some((b, good())));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);
    }

    #[test]
    fn tearing_returns_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, true, Some((b, good())));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);
    }

    #[test]
    fn two_outputs_return_the_target() {
        let t0 = Instant::now();
        let b = t0 - P * 3;
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = b + P * 5 + P / 2;
        assert_eq!(ready_at_with_now(&state, Some(out), 2, target, t0), target);
    }

    #[test]
    fn no_present_no_snapshot_no_output_return_the_target() {
        let t0 = Instant::now();
        let target = t0 + ms(40);
        let (state, out) = ready_fixture(60_000, FrameClockMode::Fixed, false, None);
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), target);

        let empty = state_for_tests();
        assert_eq!(ready_at_with_now(&empty, None, 1, target, t0), target);
        let mut unscheduled = state_for_tests();
        let out = test_out(&mut unscheduled, 1, 60_000);
        assert_eq!(ready_at_with_now(&unscheduled, Some(out), 1, target, t0), target);
    }

    #[test]
    fn a_240hz_output_uses_a_quarter_period_margin() {
        let t0 = Instant::now();
        let p = Duration::from_nanos(4_166_666);
        let margin = Duration::from_nanos(1_041_666);
        let b = t0 - p;
        let (state, out) = ready_fixture(240_000, FrameClockMode::Fixed, false, Some((b, good())));
        let target = b + p * 2 + Duration::from_nanos(1);
        assert_eq!(ready_at_with_now(&state, Some(out), 1, target, t0), b + p * 2 + margin);
    }

    #[test]
    fn a_barrier_set_after_the_latch_survives_it() {
        let mut state = state_for_tests();
        let out = test_out(&mut state, 1, 60_000);
        let s = surface_on(&mut state, out, 5);
        let now = state.commit_gate.now();
        state.commit_gate.insert_barrier(s, now + ms(500));
        let mut d = Dispatcher::default();
        note_latched(&state.commit_gate, out, &[s], LatchKind::Hard, &mut d);
        let newer = state.commit_gate.insert_barrier(s, now + ms(500));
        let barriers = d
            .test_take_queued_actions::<CommitGateAction>()
            .into_iter()
            .find_map(|a| match a {
                CommitGateAction::OutputLatched { barriers, .. } => Some(barriers),
                _ => None,
            })
            .unwrap();
        let latched = classify_latch(&state, out, LatchKind::Hard, &barriers);
        on_output_latched(&mut state, &mut d, out, latched, now);
        assert_eq!(state.commit_gate.barrier(s).unwrap().seq, newer);
    }
}
