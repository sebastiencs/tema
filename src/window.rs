//! Shell-neutral window identity.
//!
//! A [`WindowId`] is allocated when an `xdg_toplevel` is created and released when that
//! role object goes away. The same `wl_surface` can host a later toplevel; that toplevel
//! receives a new id.

use crate::{
    clients::Resolved,
    protocol_generated::WlSurfaceId,
    runner::Dispatcher,
    state::{ObjectId, State},
};

/// Monotonic id of one managed toplevel. Values start at 1 and stay unique for the
/// process lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WindowId(u64);

impl WindowId {
    #[cfg(test)]
    pub(crate) fn for_tests(raw: u64) -> Self {
        Self(raw)
    }
}

/// The root surface and the shell role object for one window.
#[derive(Clone, Copy, Debug)]
pub struct WindowEntry {
    pub root_surface: Resolved<WlSurfaceId>,
    pub role_object: ObjectId,
}

/// Width and height limits from the shell, in surface coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InteractiveSizeConstraints {
    min_size: Option<(i32, i32)>,
    max_size: Option<(i32, i32)>,
}

impl InteractiveSizeConstraints {
    pub(crate) fn new(min_size: Option<(i32, i32)>, max_size: Option<(i32, i32)>) -> Self {
        Self { min_size, max_size }
    }

    pub fn clamp(self, width: i32, height: i32) -> (i32, i32) {
        let (mut width, mut height) = (width.max(1), height.max(1));
        if let Some((min_width, min_height)) = self.min_size {
            if min_width > 0 {
                width = width.max(min_width);
            }
            if min_height > 0 {
                height = height.max(min_height);
            }
        }
        if let Some((max_width, max_height)) = self.max_size {
            if max_width > 0 {
                width = width.min(max_width);
            }
            if max_height > 0 {
                height = height.min(max_height);
            }
        }
        (width, height)
    }
}

/// Who paints the frame around a toplevel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToplevelDecorationMode {
    ServerSide,
    #[default]
    ClientSide,
}

/// Shell-neutral view of one managed toplevel.
///
/// `XdgToplevel` and the X window's `XToplevel` implement this trait.
pub trait Toplevel {
    /// Window this role object belongs to.
    fn window(&self) -> WindowId;
    /// Title shown by taskbars and window rules.
    fn title(&self) -> &str;
    /// Application id used by window rules.
    fn app_id(&self) -> &str;
    /// Parent window, when this window is transient for another.
    fn parent_window(&self) -> Option<WindowId>;
    /// Minimum and maximum size for interactive resize.
    fn size_constraints(&self) -> InteractiveSizeConstraints;
    /// Minimum size the layout uses when tiling.
    fn minimum_size(&self) -> Option<(i32, i32)>;
    /// The client asked to be maximized.
    fn is_maximized(&self) -> bool;
    /// The client asked to be fullscreen.
    fn is_fullscreen_requested(&self) -> bool;
    /// Frame mode used for geometry and grabs.
    fn decoration_mode(&self) -> ToplevelDecorationMode;
    /// Ask the client to close this window.
    fn request_close(&self, dispatcher: &mut Dispatcher);
    /// Send the size for an interactive resize.
    fn interactive_resize(&self, dispatcher: &mut Dispatcher, size: (i32, i32), resizing: bool);
    /// The compositor moved this window's root. X windows reconfigure from the new position.
    fn position_changed(&self, _dispatcher: &mut Dispatcher) {}
    /// Record a fullscreen request made by the compositor.
    fn set_requested_fullscreen(&mut self, fullscreen: bool);
}

impl State {
    pub(crate) fn allocate_window_id(&mut self) -> WindowId {
        let id = self.next_window_id;
        self.next_window_id = self
            .next_window_id
            .checked_add(1)
            .expect("window id space exhausted");
        WindowId(id)
    }

    pub(crate) fn insert_window(
        &mut self,
        id: WindowId,
        root_surface: Resolved<WlSurfaceId>,
        role_object: ObjectId,
    ) {
        debug_assert!(!self.windows.contains_key(&id), "window id already registered");
        debug_assert!(
            !self.window_by_root.contains_key(&root_surface),
            "root surface already has a window"
        );
        self.window_by_root.insert(root_surface, id);
        self.windows
            .insert(id, WindowEntry { root_surface, role_object });
        self.attach_fresh_managed_window(id, root_surface);
    }

    /// Drops the window from both maps. An id that is already gone leaves the maps unchanged.
    pub(crate) fn destroy_window(&mut self, id: WindowId) {
        let Some(entry) = self.windows.remove(&id) else {
            return;
        };
        if self.window_by_root.get(&entry.root_surface).copied() == Some(id) {
            self.window_by_root.remove(&entry.root_surface);
        }
        self.drop_managed_window(id, entry.root_surface);
    }

    pub(crate) fn window_for_root(&self, root: Resolved<WlSurfaceId>) -> Option<WindowId> {
        self.window_by_root.get(&root).copied()
    }

    pub(crate) fn window_root_surface(&self, id: WindowId) -> Option<Resolved<WlSurfaceId>> {
        self.windows.get(&id).map(|entry| entry.root_surface)
    }

    pub(crate) fn window_role_object(&self, id: WindowId) -> Option<ObjectId> {
        self.windows.get(&id).map(|entry| entry.role_object)
    }

    /// Role object for `window`.
    ///
    /// Panics when that object is grabbed. A request handler uses `self` instead.
    pub(crate) fn toplevel(&self, window: WindowId) -> Option<&dyn Toplevel> {
        let role = self.window_role_object(window)?;
        self.objects.try_get(role)?.as_toplevel()
    }

    /// Mutable form of [`toplevel`](State::toplevel). The same grab rule applies.
    pub(crate) fn toplevel_mut(&mut self, window: WindowId) -> Option<&mut dyn Toplevel> {
        let role = self.window_role_object(window)?;
        self.objects.try_get_mut(role)?.as_toplevel_mut()
    }

    /// Toplevel for the root surface that currently hosts `root`.
    pub(crate) fn toplevel_for_root(&self, root: Resolved<WlSurfaceId>) -> Option<&dyn Toplevel> {
        self.toplevel(self.window_for_root(root)?)
    }

    /// Ask the window's client to close it.
    ///
    /// Returns false when `window` has no toplevel.
    pub(crate) fn request_window_close(
        &self,
        window: WindowId,
        dispatcher: &mut Dispatcher,
    ) -> bool {
        let Some(toplevel) = self.toplevel(window) else {
            return false;
        };
        toplevel.request_close(dispatcher);
        true
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, ClientAction, ClientId, RawEvent, Resolved},
            object::Object,
            protocol_generated::{WlSurfaceId, XdgSurfaceId, XdgToplevelId, XdgWmBaseId},
            runner::Model,
            state::ObjectId,
            test_support::{
                get_toplevel_request, insert_root_surface, raw_request_from_event,
                resolved_wl_surface, state_for_tests, test_client_and_dispatcher,
            },
            wayland_objects::{
                wl_surface::{Role, WlSurface},
                xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
            },
        },
    };

    struct Opened {
        window: WindowId,
        root: Resolved<WlSurfaceId>,
        xdg_surface_id: XdgSurfaceId,
        xdg_surface_object: ObjectId,
        toplevel_id: XdgToplevelId,
    }

    fn open_toplevel(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        wl_surface_id: u32,
        xdg_surface_id: u32,
        toplevel_id: u32,
    ) -> Opened {
        let wl_surface_id = WlSurfaceId::from(wl_surface_id);
        let xdg_surface_id = XdgSurfaceId::from(xdg_surface_id);
        let toplevel_id = XdgToplevelId::from(toplevel_id);
        let (root, _) =
            client.insert_object(state, WlSurface::new(wl_surface_id, client.client_id(), 6));
        {
            let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
            assert!(surface.set_role(Role::XdgSurface, dispatcher).is_ok());
        }
        state.register_root_surface(root);
        let (xdg_surface, _) = client.insert_object(
            state,
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
        {
            let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
            surface.set_xdg_surface_impl(xdg_surface);
        }
        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface.object_id());
            xdg_surface
                .on(client, state, get_toplevel_request(xdg_surface_id, toplevel_id), dispatcher)
                .unwrap();
        }
        let window = state.window_for_root(root).unwrap();
        Opened {
            window,
            root,
            xdg_surface_id,
            xdg_surface_object: xdg_surface.object_id(),
            toplevel_id,
        }
    }

    fn destroy_request(toplevel_id: XdgToplevelId) -> crate::clients::RawRequest<'static> {
        raw_request_from_event(RawEvent::new(toplevel_id.into(), 0, 0))
    }

    fn destroy_xdg_surface(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        xdg_surface_id: XdgSurfaceId,
        xdg_surface_object: ObjectId,
    ) {
        let xdg_surface = state.objects.grab_object::<XdgSurface>(xdg_surface_object);
        xdg_surface
            .on(
                client,
                state,
                raw_request_from_event(RawEvent::new(xdg_surface_id.into(), 0, 0)),
                dispatcher,
            )
            .unwrap();
    }

    fn assert_registered(state: &State, opened: &Opened) {
        assert_eq!(state.window_root_surface(opened.window), Some(opened.root));
        assert_eq!(state.window_for_root(opened.root), Some(opened.window));
        let role = state.window_role_object(opened.window).unwrap();
        let toplevel = state.objects.try_ref_object::<XdgToplevel>(role).unwrap();
        assert_eq!(toplevel.window(), opened.window);
        assert_eq!(toplevel.toplevel_id(), opened.toplevel_id);
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.window_by_root.len(), 1);
    }

    fn assert_no_windows(state: &State, root: Resolved<WlSurfaceId>) {
        assert!(state.window_for_root(root).is_none());
        assert!(state.windows.is_empty());
        assert!(state.window_by_root.is_empty());
    }

    #[test]
    fn get_toplevel_registers_one_window_for_the_root() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        assert_eq!(opened.window, WindowId(1));
        assert_registered(&state, &opened);
    }

    #[test]
    fn destroying_the_toplevel_releases_both_maps() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        {
            let toplevel = client.grab_object(opened.toplevel_id, &mut state);
            toplevel
                .on(
                    &mut client,
                    &mut state,
                    destroy_request(opened.toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert_no_windows(&state, opened.root);
        state.destroy_window(opened.window);
        assert_no_windows(&state, opened.root);
    }

    #[test]
    fn a_new_toplevel_on_the_same_root_gets_a_new_id() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        {
            let toplevel = client.grab_object(opened.toplevel_id, &mut state);
            toplevel
                .on(
                    &mut client,
                    &mut state,
                    destroy_request(opened.toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        destroy_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            opened.xdg_surface_id,
            opened.xdg_surface_object,
        );

        let xdg_surface_id = XdgSurfaceId::from(13);
        let toplevel_id = XdgToplevelId::from(14);
        let (xdg_surface, _) = client.insert_object(
            &mut state,
            XdgSurface::new(
                xdg_surface_id,
                Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    ObjectId::from(1_000_000usize),
                ),
                opened.root,
                7,
            ),
        );
        {
            let mut surface = state
                .objects
                .grab_object::<WlSurface>(opened.root.object_id());
            assert!(surface
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .is_ok());
            surface.set_xdg_surface_impl(xdg_surface);
        }
        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface.object_id());
            xdg_surface
                .on(
                    &mut client,
                    &mut state,
                    get_toplevel_request(xdg_surface_id, toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }

        let again = state.window_for_root(opened.root).unwrap();
        assert!(again > opened.window);
        assert_eq!(state.window_root_surface(opened.window), None);
        assert_eq!(state.windows.len(), 1);
        let role = state.window_role_object(again).unwrap();
        assert_eq!(
            state
                .objects
                .try_ref_object::<XdgToplevel>(role)
                .unwrap()
                .toplevel_id(),
            toplevel_id
        );
    }

    #[test]
    fn client_disconnect_releases_the_window() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        drop(client_dispatcher);
        client
            .process(&mut state, ClientAction::Close, &mut dispatcher)
            .unwrap();
        assert_no_windows(&state, opened.root);
    }

    #[test]
    fn unregistering_the_root_releases_the_window() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        state.unregister_root_surface(opened.root);
        assert_no_windows(&state, opened.root);
        {
            let toplevel = client.grab_object(opened.toplevel_id, &mut state);
            toplevel
                .on(
                    &mut client,
                    &mut state,
                    destroy_request(opened.toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert_no_windows(&state, opened.root);
    }

    #[test]
    fn a_role_less_root_and_a_layer_root_have_no_window() {
        let mut state = state_for_tests();
        let (client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let role_less = insert_root_surface(&mut state, 20);
        let role_less = resolved_wl_surface(&state, role_less);
        assert!(state.window_for_root(role_less).is_none());

        let layer = insert_root_surface(&mut state, 21);
        {
            let mut surface = state.objects.grab_object::<WlSurface>(layer);
            assert!(surface
                .set_role(Role::LayerSurface, &mut client_dispatcher)
                .is_ok());
        }
        let layer = resolved_wl_surface(&state, layer);
        assert!(state.window_for_root(layer).is_none());
        assert!(state.windows.is_empty());
    }

    #[test]
    fn size_constraints_clamp_to_the_shell_limits() {
        let constraints = InteractiveSizeConstraints::new(Some((40, 20)), Some((100, 80)));
        assert_eq!(constraints.clamp(10, 90), (40, 80));
        assert_eq!(InteractiveSizeConstraints::default().clamp(0, 0), (1, 1));
    }

    fn toplevel_request(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        toplevel_id: XdgToplevelId,
        request: crate::clients::RawRequest<'static>,
    ) {
        let toplevel = client.grab_object(toplevel_id, state);
        toplevel.on(client, state, request, dispatcher).unwrap();
    }

    #[test]
    fn the_trait_reports_xdg_toplevel_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);

        let mut title = RawEvent::new(opened.toplevel_id.into(), 2, 1);
        title.string("Terminal");
        toplevel_request(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            opened.toplevel_id,
            raw_request_from_event(title),
        );
        let mut app_id = RawEvent::new(opened.toplevel_id.into(), 3, 1);
        app_id.string("app.terminal");
        toplevel_request(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            opened.toplevel_id,
            raw_request_from_event(app_id),
        );
        let mut max_size = RawEvent::new(opened.toplevel_id.into(), 7, 2);
        max_size.int(200);
        max_size.int(180);
        toplevel_request(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            opened.toplevel_id,
            raw_request_from_event(max_size),
        );
        let mut min_size = RawEvent::new(opened.toplevel_id.into(), 8, 2);
        min_size.int(40);
        min_size.int(20);
        toplevel_request(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            opened.toplevel_id,
            raw_request_from_event(min_size),
        );
        toplevel_request(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            opened.toplevel_id,
            raw_request_from_event(RawEvent::new(opened.toplevel_id.into(), 9, 0)),
        );

        let role = state.window_role_object(opened.window).unwrap();
        state
            .objects
            .try_ref_object_mut::<XdgToplevel>(role)
            .unwrap()
            .test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        state
            .toplevel_mut(opened.window)
            .unwrap()
            .set_requested_fullscreen(true);

        let toplevel = state.toplevel(opened.window).unwrap();
        assert_eq!(toplevel.window(), opened.window);
        assert_eq!(toplevel.title(), "Terminal");
        assert_eq!(toplevel.app_id(), "app.terminal");
        assert_eq!(
            toplevel.size_constraints(),
            InteractiveSizeConstraints::new(Some((40, 20)), Some((200, 180)))
        );
        assert_eq!(toplevel.minimum_size(), Some((40, 20)));
        assert!(toplevel.is_maximized());
        assert!(toplevel.is_fullscreen_requested());
        assert_eq!(toplevel.decoration_mode(), ToplevelDecorationMode::ServerSide);
        assert_eq!(state.toplevel_for_root(opened.root).unwrap().title(), "Terminal");
        assert!(state.toplevel(WindowId::for_tests(99)).is_none());
    }

    #[test]
    fn request_close_and_interactive_resize_queue_for_that_client() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 10, 11, 12);
        drop(client_dispatcher);

        state
            .toplevel(opened.window)
            .unwrap()
            .request_close(&mut dispatcher);
        state.toplevel(opened.window).unwrap().interactive_resize(
            &mut dispatcher,
            (320, 200),
            true,
        );

        assert!(dispatcher.test_has_queued_action_for::<ClientAction, _>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::RequestToplevelClose { toplevel }
                    if toplevel.wayland_id() == opened.toplevel_id
            ),
        ));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, _>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::InteractiveResizeConfigure {
                    xdg_toplevel,
                    width: 320,
                    height: 200,
                    resizing: true,
                } if xdg_toplevel.wayland_id() == opened.toplevel_id
            ),
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, _>(
            ClientId::from_raw(99),
            |action| matches!(
                action,
                ClientAction::RequestToplevelClose { .. }
                    | ClientAction::InteractiveResizeConfigure { .. }
            ),
        ));
    }

    #[test]
    fn close_on_a_focused_xdg_window_queues_close() {
        let mut state = state_for_tests();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(1usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 20, 21, 22);
        drop(client_dispatcher);
        state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(opened.root));

        crate::config::execute_action(
            &mut state,
            &mut dispatcher,
            crate::config::CompositorAction::Close,
        );

        assert!(dispatcher.test_has_queued_action_for::<ClientAction, _>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::RequestToplevelClose { toplevel }
                    if toplevel.wayland_id() == opened.toplevel_id
            ),
        ));
    }

    #[test]
    fn close_on_a_focused_layer_surface_queues_nothing() {
        let mut state = state_for_tests();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(1usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(5);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (root, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(30), client.client_id(), 6),
        );
        {
            let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
            assert!(surface
                .set_role(Role::LayerSurface, &mut client_dispatcher)
                .is_ok());
        }
        drop(client_dispatcher);
        state.register_root_surface(root);
        state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(root));

        crate::config::execute_action(
            &mut state,
            &mut dispatcher,
            crate::config::CompositorAction::Close,
        );

        assert!(!dispatcher.test_has_queued_action::<ClientAction>(|action| {
            matches!(action, ClientAction::RequestToplevelClose { .. })
        }));
    }

    fn insert_test_output(state: &mut State) {
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(1usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
    }

    #[test]
    fn managed_state_exists_only_for_toplevel_roots() {
        let mut state = state_for_tests();
        insert_test_output(&mut state);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(6);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let role_less = insert_root_surface(&mut state, 40);
        let role_less = resolved_wl_surface(&state, role_less);
        assert!(state.managed_window(role_less).is_none());

        let layer = insert_root_surface(&mut state, 41);
        {
            let mut surface = state.objects.grab_object::<WlSurface>(layer);
            assert!(surface
                .set_role(Role::LayerSurface, &mut client_dispatcher)
                .is_ok());
        }
        let layer = resolved_wl_surface(&state, layer);
        assert!(state.managed_window(layer).is_none());

        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 42, 43, 44);
        let managed = state.managed_window(opened.root).unwrap();
        assert_eq!(managed.mode, crate::state::ManagedWindowMode::Tiled);
        assert!(managed.awaiting_initial_layout);
        assert!(!managed.fullscreen);
        assert!(!managed.mapped);
        assert_eq!(managed.workspace_id, state.root_surface_workspace(opened.root).unwrap());
    }

    #[test]
    fn recreating_a_toplevel_starts_from_tiled_state() {
        let mut state = state_for_tests();
        insert_test_output(&mut state);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 50, 51, 52);
        {
            let managed = state.managed_window_mut(opened.root).unwrap();
            managed.mode = crate::state::ManagedWindowMode::Floating;
            managed.awaiting_initial_layout = false;
            managed.mapped = true;
        }
        {
            let toplevel = client.grab_object(opened.toplevel_id, &mut state);
            toplevel
                .on(
                    &mut client,
                    &mut state,
                    destroy_request(opened.toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(state.managed_window(opened.root).is_none());
        destroy_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            opened.xdg_surface_id,
            opened.xdg_surface_object,
        );

        let xdg_surface_id = XdgSurfaceId::from(53);
        let toplevel_id = XdgToplevelId::from(54);
        let (xdg_surface, _) = client.insert_object(
            &mut state,
            XdgSurface::new(
                xdg_surface_id,
                Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    ObjectId::from(1_000_000usize),
                ),
                opened.root,
                7,
            ),
        );
        {
            let mut surface = state
                .objects
                .grab_object::<WlSurface>(opened.root.object_id());
            assert!(surface
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .is_ok());
            surface.set_xdg_surface_impl(xdg_surface);
        }
        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface.object_id());
            xdg_surface
                .on(
                    &mut client,
                    &mut state,
                    get_toplevel_request(xdg_surface_id, toplevel_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }

        let again = state.window_for_root(opened.root).unwrap();
        assert!(again > opened.window);
        let managed = state.managed_window(opened.root).unwrap();
        assert_eq!(managed.mode, crate::state::ManagedWindowMode::Tiled);
        assert!(managed.awaiting_initial_layout);
        assert!(!managed.mapped);
        assert!(!managed.fullscreen);
    }

    #[test]
    fn moving_a_window_keeps_its_state() {
        let mut state = state_for_tests();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(1usize));
        let output = state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(&mut state, &mut client, &mut client_dispatcher, 60, 61, 62);
        {
            let managed = state.managed_window_mut(opened.root).unwrap();
            managed.mode = crate::state::ManagedWindowMode::Floating;
            managed.awaiting_initial_layout = false;
            managed.mapped = true;
        }
        let group = state.workspace_group_for_output(output).unwrap();
        let other = state.create_workspace(Some(group), Some("extra".to_string()));
        assert!(state.assign_root_surface_to_workspace(opened.root, other));

        assert_eq!(state.window_for_root(opened.root), Some(opened.window));
        let managed = state.managed_window(opened.root).unwrap();
        assert_eq!(managed.workspace_id, other);
        assert_eq!(managed.mode, crate::state::ManagedWindowMode::Floating);
        assert!(!managed.awaiting_initial_layout);
        assert!(managed.mapped);
    }

    fn destroyed_window(
        state: &mut State,
    ) -> (WindowId, Resolved<WlSurfaceId>, crate::state::WorkspaceId) {
        insert_test_output(state);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(90);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let opened = open_toplevel(state, &mut client, &mut client_dispatcher, 80, 81, 82);
        let workspace = state.root_surface_workspace(opened.root).unwrap();
        state.destroy_window(opened.window);
        (opened.window, opened.root, workspace)
    }

    #[test]
    fn managed_state_ignores_a_destroyed_window() {
        let mut state = state_for_tests();
        let (window, _, _) = destroyed_window(&mut state);
        assert!(state.managed_window_state(window).is_none());
        assert!(state.managed_window_state_mut(window).is_none());
        assert!(state.take_managed_window(window).is_none());
        assert!(!state.managed_window_states().any(|(id, _)| id == window));
    }

    #[test]
    fn mode_changes_ignore_a_destroyed_window() {
        let mut state = state_for_tests();
        let (window, _, _) = destroyed_window(&mut state);
        assert!(!state.set_window_floating(window, true));
        assert!(!state.set_window_fullscreen(window, true));
        assert!(!state.float_window_for_drag(window));
        assert!(!state.retile_window_after_drag(window));
        assert!(state.take_window_floating_restore(window).is_none());
    }

    #[test]
    fn window_queries_ignore_a_destroyed_window() {
        let mut state = state_for_tests();
        let (window, root, _) = destroyed_window(&mut state);
        state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(root));
        assert_eq!(state.focused_root_surface(), Some(root));
        assert!(state.focused_window().is_none());
        assert!(!state.window_is_mapped(window));
        assert!(!state.window_is_fullscreen(window));
        assert!(!state.window_is_tiled(window));
        assert!(!state.window_is_floating(window));
        assert!(state.tiled_window_geometry(window).is_none());
        assert!(!state.prepare_window_initial_layout(window));
        state.confirm_window_mapped(window);
    }

    #[test]
    fn raise_resize_and_workspace_move_ignore_a_destroyed_window() {
        let mut state = state_for_tests();
        let (window, _, workspace) = destroyed_window(&mut state);
        let resize_state = crate::tiling::test_tiled_resize_state();
        assert!(!state.raise_window(window));
        assert!(!state.resize_tiled_window(window, 0, 1, 0));
        assert!(state.begin_tiled_window_resize(window, 0).is_none());
        assert!(!state.resize_tiled_window_from_state(window, resize_state, 1, 0));
        assert!(!state.assign_window_to_workspace(window, workspace));
        assert!(!crate::tiling::window_is_tiled(&state, window));
        assert!(!crate::tiling::focus_window(&mut state, window));
        assert!(!crate::tiling::raise_window(&mut state, window));
        assert!(!crate::tiling::set_window_floating(&mut state, window, true));
        assert!(!crate::tiling::resize_tiled_window(&mut state, window, 0, 1, 0));
    }

    #[test]
    fn a_plain_surface_is_not_a_toplevel() {
        let mut state = state_for_tests();
        let surface = insert_root_surface(&mut state, 30);
        assert!(state
            .objects
            .try_get(surface)
            .unwrap()
            .as_toplevel()
            .is_none());
        assert!(state
            .objects
            .try_get_mut(surface)
            .unwrap()
            .as_toplevel_mut()
            .is_none());
    }
}
