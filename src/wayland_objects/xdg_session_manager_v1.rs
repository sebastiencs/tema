//! `xdg_session_manager_v1`. Create and restore application sessions.

use std::collections::BTreeMap;

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_session_manager_v1, Constants, NewId, WaylandId, XdgSessionManagerV1Id, XdgSessionV1Id,
    },
    session::{LiveSession, SessionReason},
    session_store::SessionStore,
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// Advertised at version 1 while session management is enabled and a store exists.
#[derive(Debug)]
pub struct XdgSessionManagerV1Global;

impl XdgSessionManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgSessionManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgSessionManagerV1::INTERFACE_NAME, XdgSessionManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, XdgSessionManagerV1 { id: XdgSessionManagerV1Id::from(id) });
    }
}

#[derive(Debug)]
pub struct XdgSessionManagerV1 {
    id: XdgSessionManagerV1Id,
}

impl Object for XdgSessionManagerV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use xdg_session_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetSession { id, reason, session_id } => {
                let reason = match reason {
                    1 => SessionReason::Launch,
                    2 => SessionReason::Recover,
                    3 => SessionReason::SessionRestore,
                    _ => {
                        client.protocol_violation_disconnect(
                            dispatcher,
                            self.id.into(),
                            xdg_session_manager_v1::Error::InvalidReason as u32,
                            "invalid session reason",
                        );
                        return Ok(());
                    }
                };
                if !session_id.is_empty() {
                    if state
                        .live_sessions
                        .get(&session_id)
                        .is_some_and(|live| live.client == client.client_id())
                    {
                        client.protocol_violation_disconnect(
                            dispatcher,
                            self.id.into(),
                            xdg_session_manager_v1::Error::InUse as u32,
                            "session is in use",
                        );
                        return Ok(());
                    }
                    if state.live_sessions.contains_key(&session_id) {
                        crate::session::take_over_live_session(state, dispatcher, &session_id);
                    }
                }
                let (session_id, created) = if session_id.is_empty() {
                    (SessionStore::new_session_id(), true)
                } else if state
                    .session_store
                    .as_ref()
                    .is_some_and(|store| store.has_session(&session_id))
                {
                    (session_id, false)
                } else {
                    (SessionStore::new_session_id(), true)
                };
                let object = crate::wayland_objects::xdg_session_v1::open(
                    client,
                    state,
                    dispatcher,
                    id,
                    session_id.clone(),
                    created,
                );
                if let Some(store) = state.session_store.as_mut() {
                    store.touch_session(&session_id, crate::session::unix_now());
                }
                state.live_sessions.insert(
                    session_id,
                    LiveSession {
                        client: client.client_id(),
                        object,
                        reason,
                        toplevels: BTreeMap::new(),
                    },
                );
                crate::session_store::model::note_dirty(state, dispatcher);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        clients::{Client, ClientAction, ClientId, ClientManagerAction, RawEvent, RawRequest},
        io_uring::model::IoUringAction,
        object::{Object, RequestResult},
        protocol_generated::{
            WaylandId, WlSurfaceId, XdgSessionManagerV1Id, XdgSessionV1Id, XdgSurfaceId,
            XdgToplevelId, XdgToplevelSessionV1Id, XdgWmBaseId,
        },
        runner::Dispatcher,
        session_store::{model::SessionStoreAction, SessionStore, ToplevelRecord},
        state::{ManagedWindowMode, ManagedWindowState, ObjectId, WorkspaceId},
        test_support::{
            get_toplevel_request, raw_request_from_event, state_for_tests,
            test_client_and_dispatcher,
        },
        visual_animation::{
            FocusVisualAnimation, WindowGeometryAnimation, WindowLifecycleAnimation,
            WindowVisualBase,
        },
        wayland_objects::{
            wl_surface::{Role, WlSurface},
            xdg_surface::XdgSurface,
        },
        window::WindowId,
    };

    use super::XdgSessionManagerV1;

    const MANAGER: u32 = 6;
    const SESSION: u32 = 30;
    const TOPLEVEL_SESSION: u32 = 40;

    struct ToplevelFixture {
        window: WindowId,
        xdg_surface: crate::clients::Resolved<XdgSurfaceId>,
        toplevel: crate::clients::Resolved<XdgToplevelId>,
    }

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: Dispatcher,
        manager: crate::clients::Resolved<XdgSessionManagerV1Id>,
        toplevel: ToplevelFixture,
    }

    fn new_fixture() -> Fixture {
        let mut state = state_for_tests();
        state.session_store = Some(SessionStore::load(PathBuf::from("missing-sessions.json")));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let toplevel = install_toplevel(&mut client, &mut state, &mut dispatcher, 20, 21, 22);
        let (manager, _) = client.insert_object(
            &mut state,
            XdgSessionManagerV1 { id: XdgSessionManagerV1Id::from(MANAGER) },
        );
        Fixture { state, client, dispatcher, manager, toplevel }
    }

    fn install_toplevel(
        client: &mut Client,
        state: &mut crate::state::State,
        dispatcher: &mut Dispatcher,
        surface: u32,
        xdg: u32,
        toplevel: u32,
    ) -> ToplevelFixture {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let (root, _) = client.insert_object(
            state,
            WlSurface::new(WlSurfaceId::from(surface), client.client_id(), 6),
        );
        {
            let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
            assert!(surface
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .is_ok());
        }
        state.register_root_surface(root);
        let xdg_surface_id = XdgSurfaceId::from(xdg);
        let (xdg_surface, _) = client.insert_object(
            state,
            XdgSurface::new(
                xdg_surface_id,
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    ObjectId::from(1_000_000usize),
                ),
                root,
                7,
            ),
        );
        state
            .objects
            .grab_object::<WlSurface>(root.object_id())
            .set_xdg_surface_impl(xdg_surface);
        state
            .objects
            .grab_object::<XdgSurface>(xdg_surface.object_id())
            .on(
                client,
                state,
                get_toplevel_request(xdg_surface_id, XdgToplevelId::from(toplevel)),
                &mut client_dispatcher,
            )
            .unwrap();
        ToplevelFixture {
            window: state.window_for_root(root).unwrap(),
            xdg_surface,
            toplevel: client
                .resolve(XdgToplevelId::from(toplevel), state)
                .unwrap(),
        }
    }

    fn dispatch(
        client: &mut Client,
        state: &mut crate::state::State,
        dispatcher: &mut Dispatcher,
        object: ObjectId,
        wire: u32,
        request: RawRequest<'static>,
    ) -> RequestResult {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire));
        state
            .objects
            .grab(object)
            .on(client, state, request, &mut client_dispatcher)
    }

    fn get_session(
        manager: u32,
        new_id: u32,
        reason: u32,
        session_id: Option<&str>,
    ) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(manager), 1, 3);
        event.id(XdgSessionV1Id::from(new_id));
        event.uint(reason);
        match session_id {
            Some(session_id) => event.string(session_id),
            None => event.null_string(),
        }
        raw_request_from_event(event)
    }

    fn toplevel_request(
        session: u32,
        opcode: u16,
        new_id: u32,
        toplevel: u32,
        name: &str,
    ) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(session), opcode, 3);
        event.id(XdgToplevelSessionV1Id::from(new_id));
        event.id(XdgToplevelId::from(toplevel));
        event.string(name);
        raw_request_from_event(event)
    }

    fn name_request(object: u32, opcode: u16, name: &str) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(object), opcode, 1);
        event.string(name);
        raw_request_from_event(event)
    }

    fn empty_request(object: u32, opcode: u16) -> RawRequest<'static> {
        raw_request_from_event(RawEvent::new(WaylandId::from(object), opcode, 0))
    }

    fn messages(dispatcher: &Dispatcher) -> Vec<(u32, u32, Vec<u8>)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            sent.push((object, opcode, buffer[8..].to_vec()));
        });
        sent
    }

    fn body_string(body: &[u8]) -> String {
        if body.len() < 4 {
            return String::new();
        }
        let len = u32::from_ne_bytes(body[0..4].try_into().unwrap()) as usize;
        if len == 0 {
            return String::new();
        }
        let bytes = &body[4..4 + len - 1];
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn error_codes(dispatcher: &Dispatcher) -> Vec<u32> {
        messages(dispatcher)
            .into_iter()
            .filter_map(|(object, opcode, body)| {
                if object != 1 || opcode != 0 || body.len() < 8 {
                    return None;
                }
                Some(u32::from_ne_bytes(body[4..8].try_into().unwrap()))
            })
            .collect()
    }

    fn closed(dispatcher: &Dispatcher, client: u64) -> bool {
        dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(client))
        })
    }

    fn session_event(dispatcher: &Dispatcher, object: u32, opcode: u32) -> Option<String> {
        messages(dispatcher)
            .into_iter()
            .find_map(|(id, event, body)| {
                (id == object && event == opcode).then(|| body_string(&body))
            })
    }

    fn mark_mapped(state: &mut crate::state::State, window: WindowId) {
        state.replace_managed_window_state(
            window,
            ManagedWindowState {
                workspace_id: WorkspaceId::from_parts(1, 1),
                mode: ManagedWindowMode::Tiled,
                fullscreen: false,
                mapped: true,
                awaiting_initial_layout: false,
                remembered_floating_state: None,
                restore_floating_state_on_next_configure: false,
                hold_floating_layout: false,
                resize_anchor: None,
                floating_frame_origin: None,
                tiled_content_basis: None,
                urgent: false,
                focus_animation: FocusVisualAnimation::settled(WindowVisualBase {
                    content_alpha: 255,
                    dim_alpha: 0,
                    shadow_alpha: 0,
                    border_alpha: 0,
                }),
                lifecycle_animation: WindowLifecycleAnimation::idle(),
                geometry_animation: WindowGeometryAnimation::unarmed(),
            },
        );
    }

    fn is_session_id(id: &str) -> bool {
        id.len() == 64
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    fn open_null(fixture: &mut Fixture, new_id: u32) -> String {
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.manager.object_id(),
            MANAGER,
            get_session(MANAGER, new_id, 1, None),
        )
        .unwrap();
        let created = session_event(&fixture.dispatcher, new_id, 0).unwrap();
        assert!(is_session_id(&created), "{created}");
        created
    }

    #[test]
    fn null_id_is_created_and_a_known_id_is_restored() {
        let mut fixture = new_fixture();
        let created = open_null(&mut fixture, SESSION);
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .has_session(&created));
        assert!(fixture
            .dispatcher
            .test_has_queued_action(|action: &SessionStoreAction| {
                matches!(action, SessionStoreAction::Arm)
            }));

        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            empty_request(SESSION, 0),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .has_session(&created));
        assert!(fixture.state.live_sessions.get(&created).is_none());

        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.manager.object_id(),
            MANAGER,
            get_session(MANAGER, 31, 2, Some(&created)),
        )
        .unwrap();
        assert_eq!(session_event(&fixture.dispatcher, 31, 1), Some(String::new()));
        assert_eq!(
            fixture.state.live_sessions.get(&created).unwrap().client,
            ClientId::from_raw(1)
        );
    }

    #[test]
    fn an_unknown_id_is_replaced_with_a_new_one() {
        let mut fixture = new_fixture();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.manager.object_id(),
            MANAGER,
            get_session(MANAGER, SESSION, 3, Some("not-a-real-session")),
        )
        .unwrap();
        let created = session_event(&fixture.dispatcher, SESSION, 0).unwrap();
        assert_ne!(created, "not-a-real-session");
        assert!(is_session_id(&created), "{created}");
        assert!(fixture.state.live_sessions.contains_key(&created));
        assert!(!fixture
            .state
            .live_sessions
            .contains_key("not-a-real-session"));
    }

    #[test]
    fn invalid_reason_and_in_use_disconnect() {
        let mut fixture = new_fixture();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.manager.object_id(),
            MANAGER,
            get_session(MANAGER, SESSION, 0, None),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![3]);
        assert!(closed(&fixture.dispatcher, 1));
        assert!(fixture
            .client
            .resolve::<XdgSessionV1Id>(XdgSessionV1Id::from(SESSION), &fixture.state)
            .is_none());

        let mut fixture = new_fixture();
        let created = open_null(&mut fixture, SESSION);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.manager.object_id(),
            MANAGER,
            get_session(MANAGER, 31, 1, Some(&created)),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture.dispatcher, 1));
        assert!(fixture
            .client
            .resolve::<XdgSessionV1Id>(XdgSessionV1Id::from(31), &fixture.state)
            .is_none());
    }

    #[test]
    fn add_and_rename_report_name_errors() {
        let mut fixture = new_fixture();
        open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();

        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, ""),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![3]);
        assert!(closed(&fixture.dispatcher, 1));

        let mut fixture = new_fixture();
        let _session_id = open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        let second = install_toplevel(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            23,
            24,
            25,
        );
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, 41, 25, "main"),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture.dispatcher, 1));
        assert!(second.toplevel.rref(&fixture.state).session().is_none());

        let mut fixture = new_fixture();
        open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        install_toplevel(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            23,
            24,
            25,
        );
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, 41, 25, "other"),
        )
        .unwrap();
        let before = fixture
            .dispatcher
            .test_count_queued_action(|action: &SessionStoreAction| {
                matches!(action, SessionStoreAction::Arm)
            });
        let other = fixture
            .client
            .resolve(XdgToplevelSessionV1Id::from(41), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            other.object_id(),
            41,
            name_request(41, 1, "other"),
        )
        .unwrap();
        assert_eq!(
            fixture
                .dispatcher
                .test_count_queued_action(|action: &SessionStoreAction| {
                    matches!(action, SessionStoreAction::Arm)
                }),
            before
        );
        assert_eq!(other.rref(&fixture.state).name(), "other");

        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            other.object_id(),
            41,
            name_request(41, 1, "main"),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture.dispatcher, 1));
        assert_eq!(other.rref(&fixture.state).name(), "other");

        let mut fixture = new_fixture();
        open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        let linked = fixture
            .client
            .resolve(XdgToplevelSessionV1Id::from(TOPLEVEL_SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            linked.object_id(),
            TOPLEVEL_SESSION,
            name_request(TOPLEVEL_SESSION, 1, ""),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![3]);
        assert!(closed(&fixture.dispatcher, 1));
    }

    #[test]
    fn restore_rejects_a_mapped_xdg_surface_and_a_second_add() {
        let mut fixture = new_fixture();
        open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        fixture
            .toplevel
            .xdg_surface
            .try_ref_mut(&mut fixture.state)
            .unwrap()
            .test_set_initial_commit_complete(true);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 3, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![2]);
        assert!(closed(&fixture.dispatcher, 1));

        let mut fixture = new_fixture();
        open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, 41, 22, "other"),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher), vec![4]);
        assert!(closed(&fixture.dispatcher, 1));
    }

    #[test]
    fn a_mapped_add_stores_the_name_and_a_stored_restore_stays_pending() {
        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        mark_mapped(&mut fixture.state, fixture.toplevel.window);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&session_id, "main")
            .is_some());
        assert!(fixture
            .dispatcher
            .test_has_queued_action(|action: &SessionStoreAction| {
                matches!(action, SessionStoreAction::Arm)
            }));

        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        let record = ToplevelRecord { fullscreen: true, ..ToplevelRecord::default() };
        fixture
            .state
            .session_store
            .as_mut()
            .unwrap()
            .put_toplevel(&session_id, "main", record);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 3, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        let pending = fixture
            .toplevel
            .toplevel
            .rref(&fixture.state)
            .pending_session_restore()
            .unwrap();
        assert_eq!(pending.0, session_id);
        assert!(pending.1.fullscreen);
        assert!(
            fixture
                .state
                .session_store
                .as_ref()
                .unwrap()
                .toplevel(&session_id, "main")
                .unwrap()
                .fullscreen
        );
    }

    #[test]
    fn remove_deletes_the_store_entry_and_destroy_keeps_it() {
        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            empty_request(SESSION, 1),
        )
        .unwrap();
        assert!(!fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .has_session(&session_id));
        assert!(fixture.state.live_sessions.get(&session_id).is_none());

        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        mark_mapped(&mut fixture.state, fixture.toplevel.window);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            empty_request(SESSION, 0),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .has_session(&session_id));
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&session_id, "main")
            .is_some());
        assert!(fixture
            .client
            .resolve(XdgToplevelSessionV1Id::from(TOPLEVEL_SESSION), &fixture.state)
            .unwrap()
            .rref(&fixture.state)
            .is_inert());
    }

    #[test]
    fn another_client_replaces_a_live_session() {
        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        let toplevel_session = fixture
            .client
            .resolve(XdgToplevelSessionV1Id::from(TOPLEVEL_SESSION), &fixture.state)
            .unwrap();

        let mut client_b = Client::new_for_tests(ClientId::from_raw(2));
        let (manager_b, _) = client_b.insert_object(
            &mut fixture.state,
            XdgSessionManagerV1 { id: XdgSessionManagerV1Id::from(MANAGER) },
        );
        dispatch(
            &mut client_b,
            &mut fixture.state,
            &mut fixture.dispatcher,
            manager_b.object_id(),
            MANAGER,
            get_session(MANAGER, 50, 1, Some(&session_id)),
        )
        .unwrap();

        assert!(fixture
            .dispatcher
            .test_has_queued_action_for::<ClientAction, ClientId>(
                ClientId::from_raw(1),
                |action| matches!(action, ClientAction::SessionReplaced { .. })
            ));
        assert!(session.rref(&fixture.state).is_inert());
        assert!(toplevel_session.rref(&fixture.state).is_inert());
        assert_eq!(session_event(&fixture.dispatcher, 50, 1), Some(String::new()));
        assert_eq!(
            fixture.state.live_sessions.get(&session_id).unwrap().client,
            ClientId::from_raw(2)
        );

        let errors = error_codes(&fixture.dispatcher).len();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            toplevel_session.object_id(),
            TOPLEVEL_SESSION,
            name_request(TOPLEVEL_SESSION, 1, "renamed"),
        )
        .unwrap();
        assert_eq!(error_codes(&fixture.dispatcher).len(), errors);
        assert_eq!(toplevel_session.rref(&fixture.state).name(), "main");
        assert!(!closed(&fixture.dispatcher, 1));
    }

    #[test]
    fn remove_toplevel_drops_one_name() {
        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        mark_mapped(&mut fixture.state, fixture.toplevel.window);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            name_request(SESSION, 4, "missing"),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&session_id, "main")
            .is_some());
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            name_request(SESSION, 4, "main"),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&session_id, "main")
            .is_none());
        assert!(fixture
            .client
            .resolve(XdgToplevelSessionV1Id::from(TOPLEVEL_SESSION), &fixture.state)
            .unwrap()
            .rref(&fixture.state)
            .is_inert());
        assert!(fixture
            .toplevel
            .toplevel
            .rref(&fixture.state)
            .session()
            .is_none());
    }

    #[test]
    fn unmapped_add_waits_to_sample() {
        let mut fixture = new_fixture();
        let session_id = open_null(&mut fixture, SESSION);
        let session = fixture
            .client
            .resolve(XdgSessionV1Id::from(SESSION), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            session.object_id(),
            SESSION,
            toplevel_request(SESSION, 2, TOPLEVEL_SESSION, 22, "main"),
        )
        .unwrap();
        assert!(fixture
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&session_id, "main")
            .is_none());
    }
}
