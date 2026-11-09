//! xdg-session-management. Phase 7 replaces the stand-in global and adds the call sites.

use crate::{
    clients::{Client, ClientDispatcher, ClientManagerAction},
    globals::Global,
    protocol_generated::{Constants, NewId},
    runner::Dispatcher,
    state::State,
    wayland_objects::xdg_session_manager_v1::XdgSessionManagerV1,
};

/// Stand-in for `XdgSessionManagerV1Global`. Nothing advertises it until phase 7.
struct SessionManagerGlobal;

impl SessionManagerGlobal {
    fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for SessionManagerGlobal {
    fn interface(&self) -> (&'static str, u32) {
        (XdgSessionManagerV1::INTERFACE_NAME, XdgSessionManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        _client: &mut Client,
        _state: &mut State,
        _id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
    }
}

pub(crate) fn sync_session_manager_global(state: &mut State, dispatcher: &mut Dispatcher) {
    let want = state.config.session_management.enabled && state.session_store.is_some();
    match (want, state.session_manager_global) {
        (true, None) => {
            let global = SessionManagerGlobal::new();
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
