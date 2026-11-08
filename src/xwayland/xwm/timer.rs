//! One monotonic timer for clipboard transfer deadlines.
//!
//! Each in-flight step stores an absolute deadline. The timerfd wakes at the
//! soonest of them. A read completion is [`WmEvent::TransferTimer`].

use std::{
    collections::BTreeMap,
    os::fd::{AsFd, AsRawFd},
    time::{Duration, Instant},
};

use nix::sys::timerfd::{self, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

use crate::{
    io_uring::{model::IoUringAction, ring::RingId},
    runner::{Dispatcher, Redispatcher},
    xwayland::{Xwayland, XwaylandAction},
};

use super::WmEvent;

/// A transfer step dies after this much silence.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct TransferTimer {
    fd: TimerFd,
    deadlines: BTreeMap<u64, Instant>,
    read: Option<RingId>,
    armed: Option<Instant>,
    run: u64,
}

impl TransferTimer {
    pub(super) fn new(run: u64) -> Self {
        let fd = TimerFd::new(
            timerfd::ClockId::CLOCK_MONOTONIC,
            TimerFlags::TFD_CLOEXEC | TimerFlags::TFD_NONBLOCK,
        )
        .expect("failed to create selection transfer timerfd");
        Self { fd, deadlines: BTreeMap::new(), read: None, armed: None, run }
    }

    /// Set `id` to expire at `now + 5s` and arm the soonest deadline.
    pub(super) fn touch(&mut self, id: u64, now: Instant, dispatcher: &mut Dispatcher) {
        self.deadlines.insert(id, now + IDLE_TIMEOUT);
        self.arm(dispatcher);
    }

    /// Drop `id` and arm whatever remains.
    pub(super) fn remove(&mut self, id: u64, dispatcher: &mut Dispatcher) {
        self.deadlines.remove(&id);
        self.arm(dispatcher);
    }

    /// Deadlines at or before `now`, earliest first. Those ids are removed.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<u64> {
        let mut due: Vec<(Instant, u64)> = self
            .deadlines
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, at)| (*at, *id))
            .collect();
        due.sort_unstable();
        for (_, id) in &due {
            self.deadlines.remove(id);
        }
        due.into_iter().map(|(_, id)| id).collect()
    }

    fn deadline(&self, id: u64) -> Option<Instant> {
        self.deadlines.get(&id).copied()
    }

    /// The in-flight timer read completed. The next arm may submit another.
    pub(super) fn read_finished(&mut self) {
        self.read = None;
    }

    pub(super) fn arm(&mut self, dispatcher: &mut Dispatcher) {
        let Some(deadline) = self.deadlines.values().copied().min() else {
            self.armed = None;
            let _ = self.fd.unset();
            return;
        };
        self.armed = Some(deadline);
        // A zero `it_value` disarms a timerfd, so a deadline that is already
        // due waits one nanosecond and is reported on the next completion.
        let delay = deadline
            .checked_duration_since(Instant::now())
            .filter(|delay| !delay.is_zero())
            .unwrap_or(Duration::from_nanos(1));
        let expiration = Expiration::OneShot(delay.into());
        if let Err(error) = self.fd.set(expiration, TimerSetTimeFlags::empty()) {
            log::warn!("failed to arm selection transfer timer: {error}");
        }
        self.dispatch_read(dispatcher);
    }

    fn dispatch_read(&mut self, dispatcher: &mut Dispatcher) {
        if self.read.is_some() {
            return;
        }
        let read = RingId::fresh();
        self.read = Some(read);
        let run = self.run;
        dispatcher.dispatch(IoUringAction::Read {
            id: Some(read),
            fd: self.fd.as_fd().as_raw_fd(),
            offset: 0,
            length: std::mem::size_of::<u64>(),
            dispatch_to: Redispatcher::to::<Xwayland>(move |_result| XwaylandAction::Wm {
                run,
                event: WmEvent::TransferTimer,
            }),
        });
    }

    /// Disarm the timerfd and cancel the parked read.
    pub(super) fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.armed = None;
        let _ = self.fd.unset();
        if let Some(read) = self.read.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: read });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use {
        super::{
            super::{tests::open_wm, WmCtx, WmEvent},
            *,
        },
        crate::{
            io_uring::{model::IoUringAction, ring::ReadWriteCompleted},
            runner::Dispatcher,
            xwayland::XwaylandAction,
        },
    };

    fn completed(buffer: Box<[u8]>) -> ReadWriteCompleted {
        ReadWriteCompleted { fd: 0, offset: 0, buffer }
    }

    #[test]
    fn expire_returns_due_ids_and_leaves_the_rest() {
        let mut timer = TransferTimer::new(1);
        let mut dispatcher = Dispatcher::default();
        let start = Instant::now();
        timer.touch(1, start, &mut dispatcher);
        timer.touch(2, start + Duration::from_secs(3), &mut dispatcher);

        assert!(timer.expire(start + Duration::from_secs(4)).is_empty());
        assert_eq!(timer.expire(start + Duration::from_secs(5)), vec![1]);
        assert_eq!(timer.deadline(2), Some(start + Duration::from_secs(8)));
        assert_eq!(timer.expire(start + Duration::from_secs(8)), vec![2]);
        assert!(timer.deadline(2).is_none());
    }

    #[test]
    fn touching_extends_the_deadline_and_removing_drops_it() {
        let mut timer = TransferTimer::new(1);
        let mut dispatcher = Dispatcher::default();
        let start = Instant::now();
        timer.touch(7, start, &mut dispatcher);
        assert_eq!(timer.deadline(7), Some(start + IDLE_TIMEOUT));

        timer.touch(7, start + Duration::from_secs(2), &mut dispatcher);
        assert_eq!(timer.deadline(7), Some(start + Duration::from_secs(7)));
        assert_eq!(timer.armed, Some(start + Duration::from_secs(7)));

        timer.remove(7, &mut dispatcher);
        assert!(timer.deadline(7).is_none());
        assert!(timer.armed.is_none());
        assert!(timer.expire(start + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn arming_reads_eight_bytes_once_and_stop_cancels_that_read() {
        let mut timer = TransferTimer::new(4);
        let mut dispatcher = Dispatcher::default();
        let start = Instant::now();
        timer.touch(1, start, &mut dispatcher);
        timer.touch(1, start + Duration::from_secs(1), &mut dispatcher);

        let mut reads = dispatcher.test_take_queued_actions::<IoUringAction>();
        assert_eq!(reads.len(), 1);
        let IoUringAction::Read { id, length, dispatch_to, .. } = reads.pop().unwrap() else {
            panic!("arm submits a read");
        };
        assert_eq!(length, 8);
        assert_eq!(id, Some(timer.read.expect("parked read")));

        dispatcher.redispatch_to(dispatch_to, Ok(completed(Box::new([0; 8]))));
        let event = dispatcher.test_take_queued_actions::<XwaylandAction>();
        assert!(matches!(
            event.as_slice(),
            [XwaylandAction::Wm { run: 4, event: WmEvent::TransferTimer }]
        ));

        timer.stop(&mut dispatcher);
        let cancel = dispatcher.test_take_queued_actions::<IoUringAction>();
        assert!(matches!(
            cancel.as_slice(),
            [IoUringAction::Cancel { target }] if id == Some(*target)
        ));
    }

    #[test]
    fn the_wm_expires_a_deadline_at_the_given_instant() {
        let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
        let start = Instant::now();
        wm.timer.touch(4, start, &mut dispatcher);
        {
            let mut ctx = WmCtx {
                run: 1,
                state: &mut state,
                dispatcher: &mut dispatcher,
                surfaces: &surfaces,
                client: None,
            };
            wm.test_expire_transfers(start + Duration::from_secs(4), &mut ctx);
        }
        assert_eq!(wm.timer.deadline(4), Some(start + IDLE_TIMEOUT));
        {
            let mut ctx = WmCtx {
                run: 1,
                state: &mut state,
                dispatcher: &mut dispatcher,
                surfaces: &surfaces,
                client: None,
            };
            wm.test_expire_transfers(start + IDLE_TIMEOUT, &mut ctx);
        }
        assert!(wm.timer.deadline(4).is_none());
    }
}
