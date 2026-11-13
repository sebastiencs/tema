use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_display, wl_registry, WaylandId, WlRegistryId},
    state::{GrabbedObject, State},
};

#[derive(Debug)]
pub struct WlRegistry {
    id: WlRegistryId,
}

impl WlRegistry {
    pub fn new(id: WlRegistryId) -> Self {
        Self { id }
    }

    pub fn send_global(
        &self,
        dispatcher: &mut ClientDispatcher,
        name: u32,
        interface: String,
        version: u32,
    ) {
        self.send_event(wl_registry::Event::Global { name, interface, version }, dispatcher);
    }

    pub fn send_global_remove(&self, dispatcher: &mut ClientDispatcher, name: u32) {
        self.send_event(wl_registry::Event::GlobalRemove { name }, dispatcher);
    }

    pub fn announce(
        registry: crate::clients::Resolved<WlRegistryId>,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        class: crate::globals::ClientClass,
    ) {
        let advertised = state
            .globals
            .iter_for(class)
            .map(|(name, global)| {
                let (interface_name, interface_version) = global.interface();
                (name, interface_name.to_string(), interface_version)
            })
            .collect::<Vec<_>>();
        for (name, interface_name, interface_version) in advertised {
            registry.rref(state).send_global(
                dispatcher,
                name.as_u32(),
                interface_name,
                interface_version,
            );
            state.globals.note_offer(name, registry);
        }
    }
}

impl Object for WlRegistry {
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
        match client.parse::<Self>(req, dispatcher)? {
            wl_registry::Request::Bind { name, id, requested_version } => {
                let Some(mut global) = state.globals.try_grab_global(name) else {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidObject as u32,
                        "invalid global name",
                    );
                    return Ok(());
                };
                if !client.global_class().may_see(&**global) {
                    drop(global);
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidObject as u32,
                        "invalid global name",
                    );
                    return Ok(());
                }

                let (_, max_version) = global.interface();
                let version = requested_version.min(max_version);
                global.bind(client, state, id, version, dispatcher);
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        if let Some(registry) = client.resolve(self.id, state) {
            state.globals.release_registry(registry);
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, ClientAction, ClientId, ClientManagerAction, RawEvent},
            globals::{Global, GlobalName},
            io_uring::model::IoUringAction,
            protocol_generated::{NewId, WaylandId, WlDisplayId, WlRegistryId},
            runner::{Dispatcher, Model},
            state::State,
            test_support::state_for_tests,
            wayland_objects::wl_display::WlDisplay,
        },
    };

    struct XwaylandOnlyGlobal;

    impl Global for XwaylandOnlyGlobal {
        fn interface(&self) -> (&'static str, u32) {
            ("xwayland-only", 1)
        }

        fn xwayland_only(&self) -> bool {
            true
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

    fn request(object: u32, opcode: u16) -> RawEvent {
        RawEvent::new(WaylandId::from(object), opcode, 8)
    }

    fn dispatch_request<K>(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        object: K,
        event: RawEvent,
    ) where
        K: crate::object::IdToObject + Into<WaylandId> + Copy,
    {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(object.into());
        client.grab_object(object, state).on(
            client,
            state,
            (&*event.into_bytes()).try_into().unwrap(),
            &mut client_dispatcher,
        );
    }

    fn registry(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
    ) -> WlRegistryId {
        client.insert_object(state, WlDisplay::default());
        let mut event = request(1, 1);
        event.id(WlRegistryId::from(2));
        dispatch_request(client, state, dispatcher, WlDisplayId::from(1u32), event);
        WlRegistryId::from(2)
    }

    fn sent(dispatcher: &Dispatcher, text: &str) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Sendmsg { buffer, .. } = action {
                if buffer
                    .windows(text.len())
                    .any(|window| window == text.as_bytes())
                {
                    found = true;
                }
            }
        });
        found
    }

    fn offered(client: &Client, state: &State, name: GlobalName) -> bool {
        let registry = client.resolve(WlRegistryId::from(2u32), state).unwrap();
        state.globals.was_offered(name, registry)
    }

    #[test]
    fn a_normal_registry_hides_an_xwayland_global_and_the_xwayland_registry_lists_it() {
        let mut state = state_for_tests();
        let name = state.insert_global(Box::new(XwaylandOnlyGlobal));
        let mut normal = Client::new_for_tests(ClientId::from_raw(1));
        let mut xwayland = Client::new_xwayland_for_tests(ClientId::from_raw(2));
        let mut normal_dispatcher = Dispatcher::default();
        let mut xwayland_dispatcher = Dispatcher::default();

        registry(&mut normal, &mut state, &mut normal_dispatcher);
        registry(&mut xwayland, &mut state, &mut xwayland_dispatcher);

        assert!(!offered(&normal, &state, name));
        assert!(!sent(&normal_dispatcher, "xwayland-only"));
        assert!(offered(&xwayland, &state, name));
        assert!(sent(&xwayland_dispatcher, "xwayland-only"));
    }

    #[test]
    fn binding_an_xwayland_global_disconnects_a_normal_client() {
        let mut state = state_for_tests();
        let name = state.insert_global(Box::new(XwaylandOnlyGlobal));
        let mut normal = Client::new_for_tests(ClientId::from_raw(1));
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut normal, &mut state, &mut dispatcher);

        let mut bind = request(u32::from(registry_id), 0);
        bind.uint(name.as_u32());
        bind.string("xwayland-only");
        bind.uint(1);
        bind.id(9u32);
        dispatch_request(&mut normal, &mut state, &mut dispatcher, registry_id, bind);

        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == normal.client_id())
        }));
        assert!(state.globals.try_grab_global(name.as_u32()).is_some());

        let mut xwayland = Client::new_xwayland_for_tests(ClientId::from_raw(2));
        let mut xwayland_dispatcher = Dispatcher::default();
        let xwayland_registry = registry(&mut xwayland, &mut state, &mut xwayland_dispatcher);
        let mut bind = request(u32::from(xwayland_registry), 0);
        bind.uint(name.as_u32());
        bind.string("xwayland-only");
        bind.uint(1);
        bind.id(9u32);
        dispatch_request(
            &mut xwayland,
            &mut state,
            &mut xwayland_dispatcher,
            xwayland_registry,
            bind,
        );
        assert!(!xwayland_dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { .. })
        }));
    }

    #[test]
    fn a_global_added_later_is_sent_only_to_the_xwayland_client() {
        let mut state = state_for_tests();
        let mut normal = Client::new_for_tests(ClientId::from_raw(1));
        let mut xwayland = Client::new_xwayland_for_tests(ClientId::from_raw(2));
        let mut normal_dispatcher = Dispatcher::default();
        let mut xwayland_dispatcher = Dispatcher::default();
        registry(&mut normal, &mut state, &mut normal_dispatcher);
        registry(&mut xwayland, &mut state, &mut xwayland_dispatcher);
        let name = state.insert_global(Box::new(XwaylandOnlyGlobal));

        let action = ClientAction::RegistryGlobalAdded {
            name: name.as_u32(),
            interface: "xwayland-only".to_string(),
            version: 1,
        };
        normal
            .process(&mut state, action, &mut normal_dispatcher)
            .unwrap();
        xwayland
            .process(
                &mut state,
                ClientAction::RegistryGlobalAdded {
                    name: name.as_u32(),
                    interface: "xwayland-only".to_string(),
                    version: 1,
                },
                &mut xwayland_dispatcher,
            )
            .unwrap();

        assert!(!offered(&normal, &state, name));
        assert!(offered(&xwayland, &state, name));
    }

    struct HiddenGlobal;

    impl Global for HiddenGlobal {
        fn interface(&self) -> (&'static str, u32) {
            ("zwlr_gamma_control_manager_v1", 1)
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

    struct UnlistedGlobal;

    impl Global for UnlistedGlobal {
        fn interface(&self) -> (&'static str, u32) {
            ("test-global", 1)
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

    fn sandboxed_client(id: u64) -> Client {
        Client::new_sandboxed_for_tests(
            ClientId::from_raw(id),
            crate::security_context::SecurityContextMetadata::default(),
        )
    }

    #[test]
    fn a_sandboxed_registry_hides_unlisted_globals() {
        let mut state = state_for_tests();
        let layer_shell = state
            .globals
            .name_for_interface("zwlr_layer_shell_v1")
            .unwrap();
        let mut sandboxed = sandboxed_client(1);
        let mut normal = Client::new_for_tests(ClientId::from_raw(2));
        let mut sandboxed_dispatcher = Dispatcher::default();
        let mut normal_dispatcher = Dispatcher::default();

        registry(&mut sandboxed, &mut state, &mut sandboxed_dispatcher);
        registry(&mut normal, &mut state, &mut normal_dispatcher);

        assert!(!sent(&sandboxed_dispatcher, "zwlr_layer_shell_v1"));
        assert!(!sent(&sandboxed_dispatcher, "ext_session_lock_manager_v1"));
        assert!(!sent(&sandboxed_dispatcher, "ext_idle_notifier_v1"));
        assert!(sent(&sandboxed_dispatcher, "wl_compositor"));
        assert!(sent(&sandboxed_dispatcher, "xdg_wm_base"));
        assert!(sent(&sandboxed_dispatcher, "zxdg_output_manager_v1"));
        assert!(!offered(&sandboxed, &state, layer_shell));
        assert!(sent(&normal_dispatcher, "zwlr_layer_shell_v1"));
    }

    #[test]
    fn a_sandboxed_client_binding_a_hidden_global_is_disconnected() {
        let mut state = state_for_tests();
        let layer_shell = state
            .globals
            .name_for_interface("zwlr_layer_shell_v1")
            .unwrap();
        let mut sandboxed = sandboxed_client(1);
        let mut dispatcher = Dispatcher::default();
        let registry_id = registry(&mut sandboxed, &mut state, &mut dispatcher);

        let mut bind = request(u32::from(registry_id), 0);
        bind.uint(layer_shell.as_u32());
        bind.string("zwlr_layer_shell_v1");
        bind.uint(1);
        bind.id(9u32);
        dispatch_request(&mut sandboxed, &mut state, &mut dispatcher, registry_id, bind);

        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == sandboxed.client_id())
        }));
        assert!(state
            .globals
            .try_grab_global(layer_shell.as_u32())
            .is_some());
    }

    #[test]
    fn a_sandboxed_client_never_sees_an_xwayland_only_global() {
        let mut state = state_for_tests();
        let name = state.insert_global(Box::new(XwaylandOnlyGlobal));
        let mut sandboxed = sandboxed_client(1);
        let mut dispatcher = Dispatcher::default();

        registry(&mut sandboxed, &mut state, &mut dispatcher);

        assert!(!offered(&sandboxed, &state, name));
        assert!(!sent(&dispatcher, "xwayland-only"));
    }

    #[test]
    fn a_sandboxed_client_does_not_see_an_unknown_global() {
        let mut state = state_for_tests();
        let name = state.insert_global(Box::new(UnlistedGlobal));
        let mut sandboxed = sandboxed_client(1);
        let mut normal = Client::new_for_tests(ClientId::from_raw(2));
        let mut sandboxed_dispatcher = Dispatcher::default();
        let mut normal_dispatcher = Dispatcher::default();

        registry(&mut sandboxed, &mut state, &mut sandboxed_dispatcher);
        registry(&mut normal, &mut state, &mut normal_dispatcher);

        assert!(!offered(&sandboxed, &state, name));
        assert!(!sent(&sandboxed_dispatcher, "test-global"));
        assert!(offered(&normal, &state, name));
        assert!(sent(&normal_dispatcher, "test-global"));
    }

    #[test]
    fn a_hidden_global_added_later_is_not_sent_to_a_sandboxed_client() {
        let mut state = state_for_tests();
        let mut sandboxed = sandboxed_client(1);
        let mut normal = Client::new_for_tests(ClientId::from_raw(2));
        let mut sandboxed_dispatcher = Dispatcher::default();
        let mut normal_dispatcher = Dispatcher::default();
        registry(&mut sandboxed, &mut state, &mut sandboxed_dispatcher);
        registry(&mut normal, &mut state, &mut normal_dispatcher);
        let name = state.insert_global(Box::new(HiddenGlobal));

        let action = || ClientAction::RegistryGlobalAdded {
            name: name.as_u32(),
            interface: "zwlr_gamma_control_manager_v1".to_string(),
            version: 1,
        };
        sandboxed
            .process(&mut state, action(), &mut sandboxed_dispatcher)
            .unwrap();
        normal
            .process(&mut state, action(), &mut normal_dispatcher)
            .unwrap();

        assert!(!offered(&sandboxed, &state, name));
        assert!(!sent(&sandboxed_dispatcher, "zwlr_gamma_control_manager_v1"));
        assert!(offered(&normal, &state, name));
    }

    #[test]
    fn a_hidden_interface_announced_for_a_vanished_global_is_not_sent_to_a_sandboxed_client() {
        let mut state = state_for_tests();
        let name = state.insert_global(Box::new(HiddenGlobal));
        assert!(state.globals.remove(name).is_some());
        let action = || ClientAction::RegistryGlobalAdded {
            name: name.as_u32(),
            interface: "zwlr_gamma_control_manager_v1".to_string(),
            version: 1,
        };
        let mut sandboxed = sandboxed_client(1);
        let mut normal = Client::new_for_tests(ClientId::from_raw(2));
        let mut sandboxed_dispatcher = Dispatcher::default();
        let mut normal_dispatcher = Dispatcher::default();
        registry(&mut sandboxed, &mut state, &mut sandboxed_dispatcher);
        registry(&mut normal, &mut state, &mut normal_dispatcher);

        sandboxed
            .process(&mut state, action(), &mut sandboxed_dispatcher)
            .unwrap();
        normal
            .process(&mut state, action(), &mut normal_dispatcher)
            .unwrap();

        assert!(!sent(&sandboxed_dispatcher, "zwlr_gamma_control_manager_v1"));
        assert!(sent(&normal_dispatcher, "zwlr_gamma_control_manager_v1"));
    }
}
