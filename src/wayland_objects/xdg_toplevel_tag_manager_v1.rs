//! `xdg_toplevel_tag_manager_v1`. Tag and description are stored on the toplevel immediately.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_toplevel_tag_manager_v1, Constants, NewId, WaylandId, XdgToplevelId,
        XdgToplevelTagManagerV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// Advertised at version 1.
#[derive(Debug)]
pub struct XdgToplevelTagManagerV1Global;

impl XdgToplevelTagManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgToplevelTagManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgToplevelTagManagerV1::INTERFACE_NAME, XdgToplevelTagManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            XdgToplevelTagManagerV1 { id: XdgToplevelTagManagerV1Id::from(id) },
        );
    }
}

#[derive(Debug)]
pub struct XdgToplevelTagManagerV1 {
    id: XdgToplevelTagManagerV1Id,
}

impl Object for XdgToplevelTagManagerV1 {
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
        use xdg_toplevel_tag_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            SetToplevelTag { toplevel, tag } => {
                let toplevel = client.resolve_or_disconnect(toplevel, state, dispatcher)?;
                let value = (!tag.is_empty()).then_some(tag);
                let changed = toplevel
                    .try_ref_mut(state)
                    .map(|toplevel| toplevel.set_tag(value))
                    .unwrap_or(false);
                if changed {
                    let toplevel = toplevel.rref(state);
                    let (window, (tag, description, icon)) =
                        (toplevel.window(), toplevel.metadata());
                    crate::capture::note_window_metadata(
                        state,
                        dispatcher,
                        window,
                        tag,
                        description,
                        icon,
                    );
                }
            }
            SetToplevelDescription { toplevel, description } => {
                let toplevel = client.resolve_or_disconnect(toplevel, state, dispatcher)?;
                let value = (!description.is_empty()).then_some(description);
                let changed = toplevel
                    .try_ref_mut(state)
                    .map(|toplevel| toplevel.set_description(value))
                    .unwrap_or(false);
                if changed {
                    let toplevel = toplevel.rref(state);
                    let (window, (tag, description, icon)) =
                        (toplevel.window(), toplevel.metadata());
                    crate::capture::note_window_metadata(
                        state,
                        dispatcher,
                        window,
                        tag,
                        description,
                        icon,
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, ClientId, ClientManagerAction, RawEvent, RawRequest, Resolved},
        globals::{Global, Globals},
        input::SurfaceFocus,
        ipc::{snapshot, IpcServerAction},
        object::{Object, RequestResult},
        protocol_generated::{
            Constants, NewId, WaylandId, XdgSurfaceId, XdgToplevelId, XdgToplevelTagManagerV1Id,
            XdgWmBaseId,
        },
        state::ObjectId,
        test_support::{
            get_toplevel_request, raw_request_from_event, state_for_tests,
            test_client_and_dispatcher,
        },
        wayland_objects::{
            wl_surface::{Role, WlSurface},
            xdg_surface::XdgSurface,
        },
    };

    use super::{XdgToplevelTagManagerV1, XdgToplevelTagManagerV1Global};

    const MANAGER: u32 = 6;
    const TOPLEVEL: u32 = 22;

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: crate::runner::Dispatcher,
        manager: Resolved<XdgToplevelTagManagerV1Id>,
        window: crate::window::WindowId,
        root: Resolved<crate::protocol_generated::WlSurfaceId>,
    }

    fn new_fixture() -> Fixture {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (window, root) = {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let (root, _) = client.insert_object(
                &mut state,
                WlSurface::new(
                    crate::protocol_generated::WlSurfaceId::from(20),
                    client.client_id(),
                    6,
                ),
            );
            {
                let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
                assert!(surface
                    .set_role(Role::XdgSurface, &mut client_dispatcher)
                    .is_ok());
            }
            state.register_root_surface(root);
            let xdg_surface_id = XdgSurfaceId::from(21);
            let (xdg_surface, _) = client.insert_object(
                &mut state,
                XdgSurface::new(
                    xdg_surface_id,
                    Resolved::new(
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
                    &mut client,
                    &mut state,
                    get_toplevel_request(xdg_surface_id, XdgToplevelId::from(TOPLEVEL)),
                    &mut client_dispatcher,
                )
                .unwrap();
            (state.window_for_root(root).unwrap(), root)
        };
        let (manager, _) = client.insert_object(
            &mut state,
            XdgToplevelTagManagerV1 { id: XdgToplevelTagManagerV1Id::from(MANAGER) },
        );
        Fixture { state, client, dispatcher, manager, window, root }
    }

    fn string_request(opcode: u16, toplevel: u32, text: &str) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(MANAGER), opcode, 2);
        event.id(XdgToplevelId::from(toplevel));
        event.string(text);
        raw_request_from_event(event)
    }

    fn dispatch(fixture: &mut Fixture, request: RawRequest<'static>) -> RequestResult {
        let manager = fixture.manager.object_id();
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(MANAGER));
        fixture.state.objects.grab(manager).on(
            &mut fixture.client,
            &mut fixture.state,
            request,
            &mut client_dispatcher,
        )
    }

    fn set_tag(fixture: &mut Fixture, text: &str) -> RequestResult {
        dispatch(fixture, string_request(1, TOPLEVEL, text))
    }

    fn set_description(fixture: &mut Fixture, text: &str) -> RequestResult {
        dispatch(fixture, string_request(2, TOPLEVEL, text))
    }

    fn map(fixture: &mut Fixture) {
        crate::capture::map_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
    }

    fn focused_tag(fixture: &mut Fixture) -> Option<String> {
        fixture.state.seat.keyboard_focus = Some(SurfaceFocus::new(fixture.root));
        snapshot::build(&fixture.state)
            .focused_window
            .and_then(|window| window.tag)
    }

    fn focused_description(fixture: &mut Fixture) -> Option<String> {
        fixture.state.seat.keyboard_focus = Some(SurfaceFocus::new(fixture.root));
        snapshot::build(&fixture.state)
            .focused_window
            .and_then(|window| window.description)
    }

    fn take_refreshes(fixture: &mut Fixture) -> usize {
        fixture
            .dispatcher
            .test_take_queued_actions::<IpcServerAction>()
            .into_iter()
            .filter(|action| matches!(action, IpcServerAction::Refresh))
            .count()
    }

    fn stored_tag(fixture: &Fixture) -> Option<&str> {
        fixture.state.toplevel(fixture.window).unwrap().tag()
    }

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    #[test]
    fn the_tag_manager_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(XdgToplevelTagManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = XdgToplevelTagManagerV1Global;
        global.bind(&mut client, &mut state, NewId::from(MANAGER), 1, &mut client_dispatcher);
        assert!(client.contains_object(XdgToplevelTagManagerV1Id::from(MANAGER), &state));
        assert_eq!(XdgToplevelTagManagerV1Global.interface(), ("xdg_toplevel_tag_manager_v1", 1));
    }

    #[test]
    fn a_tag_set_before_map_is_in_the_snapshot() {
        let mut fixture = new_fixture();
        fixture.state.ipc_enabled = true;
        set_tag(&mut fixture, "main").unwrap();
        assert_eq!(take_refreshes(&mut fixture), 0);
        assert_eq!(stored_tag(&fixture), Some("main"));

        map(&mut fixture);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
        assert_eq!(focused_description(&mut fixture), None);
    }

    #[test]
    fn a_tag_change_after_map_marks_dirty_and_a_repeat_does_not() {
        let mut fixture = new_fixture();
        map(&mut fixture);
        fixture.state.ipc_enabled = true;
        assert_eq!(take_refreshes(&mut fixture), 0);

        set_tag(&mut fixture, "main").unwrap();
        assert_eq!(take_refreshes(&mut fixture), 1);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
        fixture.state.ipc_dirty = false;

        set_tag(&mut fixture, "main").unwrap();
        assert_eq!(take_refreshes(&mut fixture), 0);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
    }

    #[test]
    fn an_empty_string_clears_and_description_is_independent() {
        let mut fixture = new_fixture();
        set_tag(&mut fixture, "main").unwrap();
        set_description(&mut fixture, "the main window").unwrap();
        map(&mut fixture);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
        assert_eq!(focused_description(&mut fixture).as_deref(), Some("the main window"));

        fixture.state.ipc_enabled = true;
        let _ = take_refreshes(&mut fixture);
        fixture.state.ipc_dirty = false;

        set_description(&mut fixture, "settings").unwrap();
        assert_eq!(take_refreshes(&mut fixture), 1);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
        assert_eq!(focused_description(&mut fixture).as_deref(), Some("settings"));
        fixture.state.ipc_dirty = false;

        set_tag(&mut fixture, "").unwrap();
        assert_eq!(take_refreshes(&mut fixture), 1);
        assert_eq!(focused_tag(&mut fixture), None);
        assert_eq!(focused_description(&mut fixture).as_deref(), Some("settings"));
        assert_eq!(stored_tag(&fixture), None);
    }

    #[test]
    fn destroying_the_manager_keeps_the_tag() {
        let mut fixture = new_fixture();
        set_tag(&mut fixture, "main").unwrap();
        let request = raw_request_from_event(RawEvent::new(WaylandId::from(MANAGER), 0, 0));
        dispatch(&mut fixture, request).unwrap();
        assert!(!fixture
            .client
            .contains_object(XdgToplevelTagManagerV1Id::from(MANAGER), &fixture.state));
        assert_eq!(stored_tag(&fixture), Some("main"));
        assert!(!closed(&fixture));

        map(&mut fixture);
        assert_eq!(focused_tag(&mut fixture).as_deref(), Some("main"));
    }

    #[test]
    fn an_unknown_toplevel_disconnects() {
        let mut fixture = new_fixture();
        assert!(dispatch(&mut fixture, string_request(1, 99, "main")).is_err());
        assert!(closed(&fixture));
        assert_eq!(stored_tag(&fixture), None);
        assert_eq!(take_refreshes(&mut fixture), 0);
    }
}
