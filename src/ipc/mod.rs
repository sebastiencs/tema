//! IPC for clients that need compositor state Wayland does not expose.
//!
//! [`proto`] is the wire format. `temactl` compiles that file by path, so it
//! depends only on `std`, `serde`, and `serde_json`. [`snapshot`] reads
//! compositor state. [`server`] accepts connections and [`connection`] answers
//! one client.

pub(crate) mod connection;
pub(crate) mod proto;
pub(crate) mod server;
pub(crate) mod snapshot;

pub(crate) use server::{IpcServer, IpcServerAction};

use std::time::{Duration, Instant};

use crate::{runner::Dispatcher, state::State, window::WindowId};

/// Coalesce every IPC-visible change in this runner turn into one [`IpcServerAction::Refresh`].
pub(crate) fn mark_dirty(state: &mut State, dispatcher: &mut Dispatcher) {
    if !state.ipc_enabled || state.ipc_dirty {
        return;
    }
    state.ipc_dirty = true;
    dispatcher.dispatch(IpcServerAction::Refresh);
}

/// One `bell` event per window per 100 ms. `None` is a ring with no surface.
pub(crate) fn bell(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    window: Option<WindowId>,
    now: Instant,
) {
    if !state.ipc_enabled {
        return;
    }
    if let Some(last) = state.bell_last.get(&window) {
        if now.saturating_duration_since(*last) < Duration::from_millis(100) {
            return;
        }
    }
    state.bell_last.insert(window, now);
    dispatcher.dispatch(IpcServerAction::Bell { window: window.map(WindowId::as_u64) });
}

#[cfg(test)]
mod tests {
    use crate::runner::Dispatcher;

    use super::IpcServerAction;

    #[test]
    fn display_name_matches_the_wayland_socket() {
        assert_eq!(super::proto::DISPLAY_NAME, crate::clients::WAYLAND_DISPLAY_NAME);
    }

    #[test]
    fn mark_dirty_queues_one_refresh_until_the_flag_clears() {
        let mut state = crate::test_support::state_for_tests();
        let mut dispatcher = Dispatcher::default();
        super::mark_dirty(&mut state, &mut dispatcher);
        assert!(!state.ipc_dirty);
        assert_eq!(dispatcher.test_count_queued_action::<IpcServerAction>(|_| true), 0);

        state.ipc_enabled = true;
        super::mark_dirty(&mut state, &mut dispatcher);
        super::mark_dirty(&mut state, &mut dispatcher);
        assert!(state.ipc_dirty);
        assert_eq!(
            dispatcher.test_count_queued_action::<IpcServerAction>(|action| {
                matches!(action, IpcServerAction::Refresh)
            }),
            1
        );

        state.ipc_dirty = false;
        super::mark_dirty(&mut state, &mut dispatcher);
        assert_eq!(
            dispatcher.test_count_queued_action::<IpcServerAction>(|action| {
                matches!(action, IpcServerAction::Refresh)
            }),
            2
        );
    }

    #[test]
    fn bell_throttles_per_window_and_destroy_clears_the_entry() {
        use std::time::Duration;

        let mut state = crate::test_support::state_for_tests();
        state.ipc_enabled = true;
        let mut dispatcher = Dispatcher::default();
        let start = std::time::Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let window_a = crate::window::WindowId::for_tests(4);
        let window_b = crate::window::WindowId::for_tests(9);

        super::bell(&mut state, &mut dispatcher, None, at(0));
        super::bell(&mut state, &mut dispatcher, None, at(99));
        super::bell(&mut state, &mut dispatcher, Some(window_a), at(0));
        super::bell(&mut state, &mut dispatcher, Some(window_a), at(50));
        super::bell(&mut state, &mut dispatcher, Some(window_b), at(50));
        assert_eq!(
            bell_windows(&mut dispatcher),
            vec![None, Some(window_a.as_u64()), Some(window_b.as_u64())]
        );

        super::bell(&mut state, &mut dispatcher, Some(window_a), at(100));
        assert_eq!(bell_windows(&mut dispatcher), vec![Some(window_a.as_u64())]);

        state.destroy_window(window_a);
        super::bell(&mut state, &mut dispatcher, Some(window_a), at(150));
        assert_eq!(bell_windows(&mut dispatcher), vec![Some(window_a.as_u64())]);
    }

    fn bell_windows(dispatcher: &mut Dispatcher) -> Vec<Option<u64>> {
        dispatcher
            .test_take_queued_actions::<IpcServerAction>()
            .into_iter()
            .map(|action| match action {
                IpcServerAction::Bell { window } => window,
                other => panic!("unexpected ipc action {other:?}"),
            })
            .collect()
    }
}
