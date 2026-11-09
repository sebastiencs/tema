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

use crate::{runner::Dispatcher, state::State};

/// Coalesce every IPC-visible change in this runner turn into one [`IpcServerAction::Refresh`].
pub(crate) fn mark_dirty(state: &mut State, dispatcher: &mut Dispatcher) {
    if !state.ipc_enabled || state.ipc_dirty {
        return;
    }
    state.ipc_dirty = true;
    dispatcher.dispatch(IpcServerAction::Refresh);
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
}
