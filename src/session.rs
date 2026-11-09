//! Live xdg-session-management sessions, and when the manager global is advertised.

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    clients::{Client, ClientAction, ClientId, ClientManagerAction, Resolved},
    protocol_generated::{Constants, XdgSessionV1Id, XdgToplevelSessionV1Id},
    runner::Dispatcher,
    session_store::ToplevelRecord,
    state::State,
    wayland_objects::xdg_session_manager_v1::{XdgSessionManagerV1, XdgSessionManagerV1Global},
    window::WindowId,
};

pub(crate) enum SessionReason {
    Launch,
    Recover,
    SessionRestore,
}

pub(crate) struct LiveToplevel {
    pub object: Resolved<XdgToplevelSessionV1Id>,
    pub window: WindowId,
}

pub(crate) struct LiveSession {
    pub client: ClientId,
    pub object: Resolved<XdgSessionV1Id>,
    pub reason: SessionReason,
    pub toplevels: BTreeMap<String, LiveToplevel>,
}

/// Empty layout. Phase 8 fills this from the window.
pub(crate) fn sample(_state: &State, _window: WindowId) -> ToplevelRecord {
    ToplevelRecord::default()
}

pub(crate) fn sample_into_store(state: &mut State, session_id: &str, name: &str, window: WindowId) {
    let record = sample(state, window);
    if let Some(store) = state.session_store.as_mut() {
        store.put_toplevel(session_id, name, record);
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The toplevel is grabbed by its destroy path. This only updates the session side.
pub(crate) fn toplevel_gone(
    state: &mut State,
    _client: &mut Client,
    session: Resolved<XdgToplevelSessionV1Id>,
    window: WindowId,
) {
    let Some((session_id, name, inert)) = session.try_ref(state).map(|object| {
        (object.session_id().to_string(), object.name().to_string(), object.is_inert())
    }) else {
        return;
    };
    if inert {
        return;
    }
    sample_into_store(state, &session_id, &name, window);
    if let Some(object) = session.try_ref_mut(state) {
        object.make_inert();
    }
    if let Some(live) = state.live_sessions.get_mut(&session_id) {
        live.toplevels.remove(&name);
    }
}

pub(crate) fn detach_toplevel_session(
    state: &mut State,
    session: Resolved<XdgToplevelSessionV1Id>,
) {
    let toplevel = session
        .try_ref_mut(state)
        .and_then(|object| object.make_inert());
    if let Some(toplevel) = toplevel {
        if let Some(toplevel) = toplevel.try_ref_mut(state) {
            toplevel.set_session(None);
        }
    }
}

/// Drop a live session. `sample` writes each toplevel before its session object goes inert.
pub(crate) fn release_live_session(state: &mut State, session_id: &str, sample: bool) {
    let Some(live) = state.live_sessions.remove(session_id) else {
        return;
    };
    for (name, toplevel) in live.toplevels {
        if sample {
            sample_into_store(state, session_id, &name, toplevel.window);
        }
        detach_toplevel_session(state, toplevel.object);
    }
}

/// Another client is taking this id. The old client hears `replaced` from its own queue.
pub(crate) fn take_over_live_session(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    session_id: &str,
) {
    let Some(live) = state.live_sessions.remove(session_id) else {
        return;
    };
    dispatcher.dispatch_to(live.client, ClientAction::SessionReplaced { session: live.object });
    for (name, toplevel) in live.toplevels {
        sample_into_store(state, session_id, &name, toplevel.window);
        detach_toplevel_session(state, toplevel.object);
    }
    if let Some(session) = live.object.try_ref_mut(state) {
        session.set_inert();
    }
}

pub(crate) fn sync_session_manager_global(state: &mut State, dispatcher: &mut Dispatcher) {
    let want = state.config.session_management.enabled && state.session_store.is_some();
    match (want, state.session_manager_global) {
        (true, None) => {
            let global = XdgSessionManagerV1Global::new();
            let (interface, version) = global.interface();
            let name = state.insert_global(global);
            state.session_manager_global = Some(name);
            dispatcher.dispatch(ClientManagerAction::RegistryGlobalAdded {
                name: name.as_u32(),
                interface: interface.to_string(),
                version,
            });
        }
        (false, Some(name)) => {
            let _ = state.remove_global(name);
            state.session_manager_global = None;
            dispatcher.dispatch(ClientManagerAction::RegistryGlobalRemoved { name: name.as_u32() });
        }
        (true, Some(_)) | (false, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{session_store::SessionStore, test_support::state_for_tests},
        std::path::PathBuf,
    };

    #[test]
    fn sync_session_manager_global_adds_then_withdraws() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());

        state.session_store = Some(SessionStore::load(PathBuf::from("missing-sessions.json")));
        state.config.session_management.enabled = false;
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());

        state.config.session_management.enabled = true;
        sync_session_manager_global(&mut state, &mut dispatcher);
        let name = state.session_manager_global.unwrap();
        assert_eq!(
            state
                .globals
                .name_for_interface(XdgSessionManagerV1::INTERFACE_NAME),
            Some(name)
        );
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(
                action,
                ClientManagerAction::RegistryGlobalAdded {
                    name: added,
                    interface,
                    version: 1,
                } if *added == name.as_u32() && interface == XdgSessionManagerV1::INTERFACE_NAME
            )
        }));
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::RegistryGlobalAdded { .. })
            }),
            1
        );

        state.session_store = None;
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());
        assert!(state
            .globals
            .name_for_interface(XdgSessionManagerV1::INTERFACE_NAME)
            .is_none());
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::RegistryGlobalRemoved { name: removed } if *removed == name.as_u32())
        }));
    }
}
