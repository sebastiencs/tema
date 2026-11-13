//! `xdg_wm_dialog_v1` / `xdg_dialog_v1`. Modal is a stored hint. A dialog with a
//! parent floats at its first initial commit. See `XDG_DIALOG_PLAN.md`.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_dialog_v1, xdg_wm_dialog_v1, Constants, NewId, WaylandId, WlSurfaceId, XdgDialogV1Id,
        XdgToplevelId, XdgWmDialogV1Id,
    },
    runner::Dispatcher,
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, xdg_surface::xdg_toplevel::XdgToplevel},
};

/// Advertised at version 1.
#[derive(Debug)]
pub struct XdgWmDialogV1Global;

impl XdgWmDialogV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgWmDialogV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgWmDialogV1::INTERFACE_NAME, XdgWmDialogV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, XdgWmDialogV1::new(XdgWmDialogV1Id::from(id)));
    }
}

#[derive(Debug)]
pub struct XdgWmDialogV1 {
    id: XdgWmDialogV1Id,
}

impl XdgWmDialogV1 {
    pub(crate) fn new(id: XdgWmDialogV1Id) -> Self {
        Self { id }
    }
}

impl Object for XdgWmDialogV1 {
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
        use xdg_wm_dialog_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetXdgDialog { id, toplevel } => {
                let toplevel = client.resolve_or_disconnect(toplevel, state, dispatcher)?;
                if toplevel
                    .try_ref(state)
                    .is_some_and(|toplevel| toplevel.dialog().is_some())
                {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        xdg_wm_dialog_v1::Error::AlreadyUsed as u32,
                        "xdg_toplevel already has an xdg_dialog_v1",
                    );
                    return Ok(());
                }
                let (dialog, _) = client.insert_object(state, XdgDialogV1::new(id, toplevel));
                if let Some(toplevel) = toplevel.try_ref_mut(state) {
                    toplevel.attach_dialog(dialog);
                }
            }
        }
        Ok(())
    }
}

/// Ancillary object of one toplevel. Inert once that toplevel is destroyed.
#[derive(Debug)]
pub struct XdgDialogV1 {
    id: XdgDialogV1Id,
    toplevel: Option<Resolved<XdgToplevelId>>,
}

impl XdgDialogV1 {
    pub(crate) fn new(id: XdgDialogV1Id, toplevel: Resolved<XdgToplevelId>) -> Self {
        Self { id, toplevel: Some(toplevel) }
    }

    /// The toplevel is being destroyed; later requests do nothing.
    pub(crate) fn toplevel_destroyed(&mut self) {
        self.toplevel = None;
    }

    /// Apply `change` to the live toplevel; mark IPC dirty when the modal value changed.
    fn update_toplevel(
        &mut self,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        change: impl FnOnce(&mut XdgToplevel),
    ) {
        let Some(toplevel) = self.toplevel else {
            return;
        };
        let Some(changed) = toplevel.try_ref_mut(state).map(|toplevel| {
            let before = toplevel.dialog_modal_parent();
            change(toplevel);
            toplevel.dialog_modal_parent() != before
        }) else {
            self.toplevel = None;
            return;
        };
        if changed {
            crate::ipc::mark_dirty(state, dispatcher);
        }
    }
}

impl Object for XdgDialogV1 {
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
        use xdg_dialog_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                self.update_toplevel(state, dispatcher, |toplevel| toplevel.detach_dialog());
                self.toplevel = None;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            SetModal => {
                self.update_toplevel(state, dispatcher, |toplevel| toplevel.set_dialog_modal(true));
            }
            UnsetModal => {
                self.update_toplevel(state, dispatcher, |toplevel| {
                    toplevel.set_dialog_modal(false)
                });
            }
        }
        Ok(())
    }
}

/// Center a window floated as a dialog, once, on the map after that decision.
pub(crate) fn place_on_map(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    root: Resolved<WlSurfaceId>,
) {
    let Some(window) = state.window_for_root(root) else {
        return;
    };
    let Some(role) = state.window_role_object(window) else {
        return;
    };
    let Some((center, parent)) = state
        .objects
        .try_get_mut(role)
        .and_then(|object| object.downcast_mut::<XdgToplevel>())
        .map(|toplevel| (toplevel.take_center_on_map(), toplevel.dialog_parent_window()))
    else {
        return;
    };
    if !center || crate::input::toplevel_drag::tracks_root(state, root) {
        return;
    }
    let Some(managed) = state.managed_window_state(window) else {
        return;
    };
    if managed.mode != crate::state::ManagedWindowMode::Floating || managed.fullscreen {
        return;
    }
    let workspace_id = managed.workspace_id;
    let Some(output_id) = state.workspace_output_id(workspace_id) else {
        return;
    };
    let Some(bounds) =
        crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(state, output_id)
            .or_else(|| state.output_bounds(output_id))
    else {
        return;
    };
    let Some(geometry) = state.committed_root_geometry(root) else {
        return;
    };
    let anchor = parent
        .filter(|parent| {
            state.window_is_mapped(*parent)
                && state
                    .managed_window_state(*parent)
                    .is_some_and(|managed| managed.workspace_id == workspace_id)
        })
        .and_then(|parent| state.window_root_surface(parent))
        .and_then(|parent_root| state.committed_root_geometry(parent_root))
        .map(|parent| parent.layout_rect_global)
        .unwrap_or(bounds);
    let frame = geometry.layout_rect_global;
    let x = clamp_origin(
        anchor.x + (anchor.width - frame.width) / 2,
        frame.width,
        bounds.x,
        bounds.width,
    );
    let y = clamp_origin(
        anchor.y + (anchor.height - frame.height) / 2,
        frame.height,
        bounds.y,
        bounds.height,
    );
    let root_pos = (geometry.root_pos.0 + x - frame.x, geometry.root_pos.1 + y - frame.y);
    crate::input::grab::place_grabbed_root(state, dispatcher, root, root_pos);
}

/// Same rule as X11 placement (`src/xwayland/xwm/window.rs` `clamp_origin`).
fn clamp_origin(origin: i32, size: i32, bounds_origin: i32, bounds_size: i32) -> i32 {
    if bounds_size <= size {
        return bounds_origin;
    }
    origin.clamp(bounds_origin, bounds_origin + bounds_size - size)
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, ClientId, ClientManagerAction, RawEvent, RawRequest, Resolved},
        globals::{Global, Globals},
        object::{Object, RequestResult},
        protocol_generated::{
            Constants, NewId, WaylandId, WlSurfaceId, XdgDialogV1Id, XdgSurfaceId, XdgToplevelId,
            XdgWmBaseId, XdgWmDialogV1Id,
        },
        runner::Dispatcher,
        state::{ObjectId, State},
        test_support::{
            get_toplevel_request, insert_window, raw_request_from_event, state_for_tests,
            test_client_and_dispatcher, test_output_drm0_wh,
        },
        wayland_objects::{
            wl_surface::{Role, WlSurface},
            xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
        },
        window::{Toplevel, WindowId},
    };

    use {
        super::{XdgDialogV1, XdgWmDialogV1, XdgWmDialogV1Global},
        crate::{
            input::SurfaceFocus,
            ipc::{snapshot, IpcServerAction},
        },
    };

    const MANAGER: u32 = 6;

    struct Opened {
        window: WindowId,
        root: Resolved<WlSurfaceId>,
        xdg_surface_id: XdgSurfaceId,
        xdg_surface_object: ObjectId,
        toplevel_id: XdgToplevelId,
    }

    /// Copy of `window.rs` tests `open_toplevel`.
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

    /// Copy of `window.rs` tests `destroy_request`.
    fn destroy_request(toplevel_id: XdgToplevelId) -> RawRequest<'static> {
        raw_request_from_event(RawEvent::new(toplevel_id.into(), 0, 0))
    }

    struct Fixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        manager: ObjectId,
        parent: Opened,
        child: Opened,
    }

    impl Fixture {
        fn new() -> Self {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
            let (parent, child) = {
                let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
                let parent =
                    open_toplevel(&mut state, &mut client, &mut client_dispatcher, 20, 21, 22);
                let child =
                    open_toplevel(&mut state, &mut client, &mut client_dispatcher, 30, 31, 32);
                (parent, child)
            };
            let (manager, _) = client
                .insert_object(&mut state, XdgWmDialogV1::new(XdgWmDialogV1Id::from(MANAGER)));
            Self {
                state,
                client,
                dispatcher,
                manager: manager.object_id(),
                parent,
                child,
            }
        }

        fn dispatch(
            &mut self,
            object: ObjectId,
            wire: u32,
            request: RawRequest<'static>,
        ) -> RequestResult {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire));
            self.state.objects.grab(object).on(
                &mut self.client,
                &mut self.state,
                request,
                &mut client_dispatcher,
            )
        }

        fn toplevel_object(&self, wire: u32) -> ObjectId {
            self.client
                .resolve(XdgToplevelId::from(wire), &self.state)
                .unwrap()
                .object_id()
        }

        fn dialog_object(&self, wire: u32) -> ObjectId {
            self.client
                .resolve(XdgDialogV1Id::from(wire), &self.state)
                .unwrap()
                .object_id()
        }

        fn child_xdg_toplevel(&self) -> &XdgToplevel {
            self.client
                .resolve(XdgToplevelId::from(32), &self.state)
                .unwrap()
                .try_ref(&self.state)
                .unwrap()
        }

        fn attached_dialog(&self) -> Option<Resolved<XdgDialogV1Id>> {
            self.child_xdg_toplevel().dialog()
        }

        fn child_modal_parent(&self) -> Option<WindowId> {
            self.state
                .toplevel(self.child.window)
                .unwrap()
                .modal_parent()
        }

        fn closed(&self) -> bool {
            self.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
            })
        }

        /// `xdg_wm_dialog_v1.get_xdg_dialog(dialog, toplevel)` on the manager.
        fn get_dialog(&mut self, dialog: u32, toplevel: u32) -> RequestResult {
            let mut event = RawEvent::new(WaylandId::from(MANAGER), 1, 2);
            event.id(XdgDialogV1Id::from(dialog));
            event.id(XdgToplevelId::from(toplevel));
            let manager = self.manager;
            self.dispatch(manager, MANAGER, raw_request_from_event(event))
        }

        /// `xdg_dialog_v1` requests with no arguments: 0 destroy, 1 set_modal, 2 unset_modal.
        fn dialog_request(&mut self, dialog: u32, opcode: u16) -> RequestResult {
            let object = self.dialog_object(dialog);
            let event = RawEvent::new(WaylandId::from(dialog), opcode, 0);
            self.dispatch(object, dialog, raw_request_from_event(event))
        }

        fn set_modal(&mut self, dialog: u32) -> RequestResult {
            self.dialog_request(dialog, 1)
        }

        fn unset_modal(&mut self, dialog: u32) -> RequestResult {
            self.dialog_request(dialog, 2)
        }

        fn destroy_dialog(&mut self, dialog: u32) -> RequestResult {
            self.dialog_request(dialog, 0)
        }

        /// `xdg_toplevel.set_parent(parent)`; wire 0 is null.
        fn set_parent(&mut self, child: u32, parent: u32) -> RequestResult {
            let mut event = RawEvent::new(WaylandId::from(child), 1, 1);
            event.id(XdgToplevelId::from(parent));
            let object = self.toplevel_object(child);
            self.dispatch(object, child, raw_request_from_event(event))
        }

        fn destroy_toplevel(&mut self, toplevel: u32) -> RequestResult {
            let object = self.toplevel_object(toplevel);
            self.dispatch(object, toplevel, destroy_request(XdgToplevelId::from(toplevel)))
        }
    }

    #[test]
    fn the_dialog_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(XdgWmDialogV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = XdgWmDialogV1Global;
        global.bind(&mut client, &mut state, NewId::from(MANAGER), 1, &mut client_dispatcher);
        assert!(client.contains_object(XdgWmDialogV1Id::from(MANAGER), &state));
        assert_eq!(XdgWmDialogV1Global.interface(), ("xdg_wm_dialog_v1", 1));
    }

    #[test]
    fn a_second_dialog_for_the_same_toplevel_is_already_used() {
        let mut fixture = Fixture::new();
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.get_dialog(41, 32).is_ok());
        assert!(fixture.closed());
    }

    #[test]
    fn a_new_dialog_is_allowed_after_the_old_one_is_destroyed() {
        let mut fixture = Fixture::new();
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.destroy_dialog(40).is_ok());
        assert!(fixture.get_dialog(41, 32).is_ok());
        assert!(!fixture.closed());
        assert_eq!(
            fixture.attached_dialog(),
            fixture
                .client
                .resolve(XdgDialogV1Id::from(41), &fixture.state),
        );
    }

    #[test]
    fn modal_counts_only_with_a_parent() {
        let mut fixture = Fixture::new();
        let parent_window = fixture.parent.window;
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        assert_eq!(fixture.child_modal_parent(), None);

        assert!(fixture.set_parent(32, 22).is_ok());
        assert_eq!(fixture.child_modal_parent(), Some(parent_window));

        assert!(fixture.unset_modal(40).is_ok());
        assert_eq!(fixture.child_modal_parent(), None);

        assert!(fixture.set_modal(40).is_ok());
        assert_eq!(fixture.child_modal_parent(), Some(parent_window));

        assert!(fixture.set_parent(32, 0).is_ok());
        assert_eq!(fixture.child_modal_parent(), None);
    }

    #[test]
    fn destroying_the_dialog_unapplies_modal() {
        let mut fixture = Fixture::new();
        assert!(fixture.set_parent(32, 22).is_ok());
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        assert!(fixture.destroy_dialog(40).is_ok());
        assert_eq!(fixture.child_modal_parent(), None);
        assert_eq!(fixture.attached_dialog(), None);
        assert!(!fixture.closed());
    }

    #[test]
    fn destroying_the_toplevel_makes_the_dialog_inert() {
        let mut fixture = Fixture::new();
        assert!(fixture.set_parent(32, 22).is_ok());
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        assert!(fixture.destroy_toplevel(32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        assert!(!fixture.closed());
        assert!(fixture.destroy_dialog(40).is_ok());
        assert!(!fixture.closed());
    }

    #[test]
    fn destroying_the_manager_keeps_the_dialog() {
        let mut fixture = Fixture::new();
        let parent_window = fixture.parent.window;
        assert!(fixture.get_dialog(40, 32).is_ok());
        let manager = fixture.manager;
        assert!(fixture
            .dispatch(
                manager,
                MANAGER,
                raw_request_from_event(RawEvent::new(WaylandId::from(MANAGER), 0, 0))
            )
            .is_ok());
        assert!(fixture.set_parent(32, 22).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        assert_eq!(fixture.child_modal_parent(), Some(parent_window));
    }

    #[test]
    fn an_unknown_toplevel_disconnects() {
        let mut fixture = Fixture::new();
        assert!(fixture.get_dialog(40, 99).is_err());
        assert!(fixture.closed());
    }

    fn take_refreshes(fixture: &mut Fixture) -> usize {
        fixture
            .dispatcher
            .test_take_queued_actions::<IpcServerAction>()
            .into_iter()
            .filter(|action| matches!(action, IpcServerAction::Refresh))
            .count()
    }

    /// The `modal` value of the window that `focus` gives keyboard focus to.
    fn focused_modal(fixture: &mut Fixture, focus: Resolved<WlSurfaceId>) -> Option<bool> {
        fixture.state.seat.keyboard_focus = Some(SurfaceFocus::new(focus));
        snapshot::build(&fixture.state)
            .focused_window
            .map(|window| window.modal)
    }

    #[test]
    fn the_snapshot_reports_a_focused_modal_dialog() {
        let mut fixture = Fixture::new();
        assert!(fixture.set_parent(32, 22).is_ok());
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        let (parent_window, child_window) = (fixture.parent.window, fixture.child.window);
        crate::capture::map_window(&mut fixture.state, &mut fixture.dispatcher, parent_window);
        crate::capture::map_window(&mut fixture.state, &mut fixture.dispatcher, child_window);
        let (parent_root, child_root) = (fixture.parent.root, fixture.child.root);

        assert_eq!(focused_modal(&mut fixture, child_root), Some(true));
        assert_eq!(focused_modal(&mut fixture, parent_root), Some(false));

        // Destroying the parent leaves the dialog without a live parent.
        assert!(fixture.destroy_toplevel(22).is_ok());
        assert_eq!(focused_modal(&mut fixture, child_root), Some(false));
    }

    #[test]
    fn a_modal_change_marks_dirty_and_a_repeat_does_not() {
        let mut fixture = Fixture::new();
        assert!(fixture.set_parent(32, 22).is_ok());
        assert!(fixture.get_dialog(40, 32).is_ok());
        assert!(fixture.set_modal(40).is_ok());
        let child_window = fixture.child.window;
        crate::capture::map_window(&mut fixture.state, &mut fixture.dispatcher, child_window);
        fixture.state.ipc_enabled = true;
        take_refreshes(&mut fixture);
        fixture.state.ipc_dirty = false;

        assert!(fixture.unset_modal(40).is_ok());
        assert_eq!(take_refreshes(&mut fixture), 1);
        fixture.state.ipc_dirty = false;

        assert!(fixture.unset_modal(40).is_ok());
        assert_eq!(take_refreshes(&mut fixture), 0);
    }

    /// Trimmed copy of the drag fixture: one child toplevel (wire 13) that maps
    /// through the commit path, and a dialog manager (wire 10).
    struct MapFixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        manager: ObjectId,
        surface: Resolved<WlSurfaceId>,
        window: WindowId,
        toplevel: Resolved<XdgToplevelId>,
    }

    impl MapFixture {
        fn new() -> Self {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
            let (manager, _) =
                client.insert_object(&mut state, XdgWmDialogV1::new(XdgWmDialogV1Id::from(10)));
            let (surface, _) = client.insert_object(
                &mut state,
                WlSurface::new(WlSurfaceId::from(12), client.client_id(), 6),
            );
            state.register_root_surface(surface);
            let window = insert_window(&mut state, surface);
            let (xdg_surface, _) = client.insert_object(
                &mut state,
                XdgSurface::new(
                    XdgSurfaceId::from(80),
                    Resolved::new(
                        client.client_id(),
                        XdgWmBaseId::from(79),
                        ObjectId::from(79usize),
                    ),
                    surface,
                    1,
                ),
            );
            let (toplevel, _) = client.insert_object(
                &mut state,
                XdgToplevel::new(XdgToplevelId::from(13), xdg_surface, 1, window),
            );
            Self {
                state,
                client,
                dispatcher,
                manager: manager.object_id(),
                surface,
                window,
                toplevel,
            }
        }

        fn dispatch(
            &mut self,
            object: ObjectId,
            wire: u32,
            request: RawRequest<'static>,
        ) -> RequestResult {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire));
            self.state.objects.grab(object).on(
                &mut self.client,
                &mut self.state,
                request,
                &mut client_dispatcher,
            )
        }

        fn xdg(&self) -> Resolved<XdgSurfaceId> {
            self.toplevel.try_ref(&self.state).unwrap().xdg_surface()
        }

        fn root_pos(&self) -> (i32, i32) {
            self.surface.try_ref(&self.state).unwrap().root_position()
        }

        /// An output, then the child window again, owned by the real toplevel.
        fn bind_managed_window(&mut self) {
            self.state.insert_output(test_output_drm0_wh(800, 600));
            self.state.destroy_window(self.window);
            self.state
                .insert_window(self.window, self.surface, self.toplevel.object_id());
        }

        fn link_xdg(&mut self) {
            let xdg = self.xdg();
            let toplevel = self.toplevel;
            self.surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_xdg_surface_impl(xdg);
            xdg.try_ref_mut(&mut self.state)
                .unwrap()
                .test_assign_toplevel_role(toplevel);
        }

        fn set_xdg_role(&mut self, surface: Resolved<WlSurfaceId>) {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .unwrap();
        }

        fn refresh_geometry(&mut self, surface: Resolved<WlSurfaceId>) {
            self.state.mark_root_geometry_dirty(surface);
            let _ = self.state.committed_root_geometry(surface);
        }

        /// A mapped toplevel (`test_map`), tiled on the only workspace.
        fn add_mapped_toplevel(
            &mut self,
            surface_id: u32,
            xdg_id: u32,
            toplevel_id: u32,
        ) -> (Resolved<WlSurfaceId>, WindowId) {
            let (surface, _) = self.client.insert_object(
                &mut self.state,
                WlSurface::new(WlSurfaceId::from(surface_id), self.client.client_id(), 6),
            );
            self.state.register_root_surface(surface);
            let window = self.state.allocate_window_id();
            let (xdg, _) = self.client.insert_object(
                &mut self.state,
                XdgSurface::new(
                    XdgSurfaceId::from(xdg_id),
                    Resolved::new(
                        self.client.client_id(),
                        XdgWmBaseId::from(79),
                        ObjectId::from(79usize),
                    ),
                    surface,
                    1,
                ),
            );
            let (toplevel, _) = self.client.insert_object(
                &mut self.state,
                XdgToplevel::new(XdgToplevelId::from(toplevel_id), xdg, 1, window),
            );
            self.state
                .insert_window(window, surface, toplevel.object_id());
            self.set_xdg_role(surface);
            surface
                .try_ref_mut(&mut self.state)
                .unwrap()
                .set_xdg_surface_impl(xdg);
            xdg.try_ref_mut(&mut self.state)
                .unwrap()
                .test_assign_toplevel_role(toplevel);
            surface.try_ref_mut(&mut self.state).unwrap().test_map(
                ObjectId::from(surface_id as usize),
                0,
                0,
                200,
                120,
            );
            let workspace = self
                .state
                .managed_window_state(window)
                .unwrap()
                .workspace_id;
            crate::tiling::mark_workspace_dirty(&mut self.state, workspace);
            self.state.relayout_dirty_workspaces();
            self.refresh_geometry(surface);
            (surface, window)
        }

        /// Bind the output and the child, then add the mapped parent (wire 40/81/41).
        fn setup(&mut self) -> (Resolved<WlSurfaceId>, WindowId) {
            self.bind_managed_window();
            self.link_xdg();
            self.set_xdg_role(self.surface);
            self.add_mapped_toplevel(40, 81, 41)
        }

        /// `xdg_wm_dialog_v1.get_xdg_dialog(20, 13)` on the manager.
        fn make_dialog(&mut self) {
            let mut event = RawEvent::new(WaylandId::from(10), 1, 2);
            event.id(XdgDialogV1Id::from(20));
            event.id(XdgToplevelId::from(13));
            let manager = self.manager;
            self.dispatch(manager, 10, raw_request_from_event(event))
                .unwrap();
        }

        /// `xdg_toplevel.set_parent(parent_wire)` on the child.
        fn set_parent(&mut self, parent_wire: u32) {
            let mut event = RawEvent::new(WaylandId::from(13), 1, 1);
            event.id(XdgToplevelId::from(parent_wire));
            let object = self.toplevel.object_id();
            self.dispatch(object, 13, raw_request_from_event(event))
                .unwrap();
        }

        fn initial_configure_size(&mut self) -> Option<(i32, i32)> {
            let xdg = self.xdg();
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            let role_changes = {
                let mut xdg = xdg.grab(&mut self.state);
                xdg.on_commit(
                    &mut self.client,
                    &mut self.state,
                    &mut client_dispatcher,
                    None,
                    false,
                    false,
                    None,
                )
            };
            let size = xdg
                .try_ref(&self.state)
                .unwrap()
                .test_latest_toplevel_configure_size();
            if let Some(role_changes) = role_changes {
                let mut xdg = xdg.grab(&mut self.state);
                xdg.apply_commit_role_changes(
                    &mut self.client,
                    &mut self.state,
                    &mut client_dispatcher,
                    role_changes,
                );
            }
            size
        }

        /// Ack the first configure and commit a 80x60 buffer: the child maps.
        fn map_child(&mut self) {
            let xdg = self.xdg();
            let mut ack = RawEvent::new(WaylandId::from(80), 4, 1);
            ack.uint(1);
            self.dispatch(xdg.object_id(), 80, raw_request_from_event(ack))
                .unwrap();
            crate::wayland_objects::wl_surface::test_commit_new_buffer(
                &mut self.client,
                &mut self.state,
                &mut self.dispatcher,
                self.surface,
                80,
                60,
            );
            self.refresh_geometry(self.surface);
        }

        /// A null-buffer commit: the child unmaps.
        fn unmap_child(&mut self) {
            crate::wayland_objects::wl_surface::test_commit_null_buffer(
                &mut self.client,
                &mut self.state,
                &mut self.dispatcher,
                self.surface,
            );
        }

        fn is_floating(&self) -> bool {
            self.state.managed_window_state(self.window).unwrap().mode
                == crate::state::ManagedWindowMode::Floating
        }

        fn is_tiled(&self) -> bool {
            self.state.managed_window_state(self.window).unwrap().mode
                == crate::state::ManagedWindowMode::Tiled
        }

        fn center_pending(&self) -> bool {
            self.toplevel
                .try_ref(&self.state)
                .unwrap()
                .test_center_on_map()
        }

        fn child_geometry(&self) -> crate::region::RectI {
            self.state
                .committed_root_geometry(self.surface)
                .unwrap()
                .layout_rect_global
        }
    }

    #[test]
    fn a_dialog_with_a_parent_floats_and_gets_a_zero_configure() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        fixture.make_dialog();
        assert_eq!(fixture.initial_configure_size(), Some((0, 0)));
        assert!(fixture.is_floating());
        assert!(fixture.center_pending());
    }

    #[test]
    fn a_dialog_without_a_parent_tiles() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.make_dialog();
        assert_ne!(fixture.initial_configure_size(), Some((0, 0)));
        assert!(fixture.is_tiled());
    }

    #[test]
    fn a_parented_toplevel_without_a_dialog_tiles() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        assert_ne!(fixture.initial_configure_size(), Some((0, 0)));
        assert!(fixture.is_tiled());
    }

    #[test]
    fn a_dialog_is_centered_over_its_parent_on_first_map() {
        let mut fixture = MapFixture::new();
        let (parent, parent_window) = fixture.setup();
        fixture.state.set_window_floating(parent_window, true);
        crate::input::grab::place_grabbed_root(
            &mut fixture.state,
            &mut fixture.dispatcher,
            parent,
            (40, 50),
        );
        fixture.refresh_geometry(parent);
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();
        fixture.map_child();

        let workspace = fixture
            .state
            .managed_window_state(fixture.window)
            .unwrap()
            .workspace_id;
        let output = fixture.state.workspace_output_id(workspace).unwrap();
        let p = fixture
            .state
            .committed_root_geometry(parent)
            .unwrap()
            .layout_rect_global;
        let b = crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(
            &fixture.state,
            output,
        )
        .unwrap();
        let c = fixture.child_geometry();
        assert_eq!(c.x, p.x + (p.width - c.width) / 2);
        assert_eq!(c.y, p.y + (p.height - c.height) / 2);
        assert_ne!(c.x, b.x + (b.width - c.width) / 2);
        assert!(!fixture.center_pending());
    }

    /// GTK maps a dialog with its 20 px CSD shadow (window geometry at 20,20 in a
    /// larger buffer), then drops the shadow on the next commit (geometry 0,0).
    /// The frame tema centered must not move.
    #[test]
    fn dropping_the_csd_shadow_after_map_keeps_the_dialog_frame_in_place() {
        let mut fixture = MapFixture::new();
        let (parent, parent_window) = fixture.setup();
        fixture.state.set_window_floating(parent_window, true);
        crate::input::grab::place_grabbed_root(
            &mut fixture.state,
            &mut fixture.dispatcher,
            parent,
            (40, 50),
        );
        fixture.refresh_geometry(parent);
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();

        let set_geometry = |fixture: &mut MapFixture, x: i32, y: i32| {
            let xdg = fixture.xdg();
            let mut request = RawEvent::new(WaylandId::from(80), 3, 4);
            request.int(x);
            request.int(y);
            request.int(80);
            request.int(60);
            fixture
                .dispatch(xdg.object_id(), 80, raw_request_from_event(request))
                .unwrap();
        };
        let xdg = fixture.xdg();
        let mut ack = RawEvent::new(WaylandId::from(80), 4, 1);
        ack.uint(1);
        fixture
            .dispatch(xdg.object_id(), 80, raw_request_from_event(ack))
            .unwrap();
        set_geometry(&mut fixture, 20, 20);
        crate::wayland_objects::wl_surface::test_commit_new_buffer(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.surface,
            120,
            100,
        );
        fixture.refresh_geometry(fixture.surface);
        let centered = fixture.child_geometry();
        let root_with_shadow = fixture.root_pos();

        set_geometry(&mut fixture, 0, 0);
        crate::wayland_objects::wl_surface::test_commit_new_buffer(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.surface,
            80,
            60,
        );
        fixture.refresh_geometry(fixture.surface);

        assert_eq!(fixture.child_geometry(), centered);
        assert_eq!(fixture.root_pos(), (root_with_shadow.0 + 20, root_with_shadow.1 + 20));
    }

    #[test]
    fn a_left_resize_keeps_the_right_frame_edge_when_the_shadow_is_dropped() {
        let mut fixture = MapFixture::new();
        let (parent, parent_window) = fixture.setup();
        fixture.state.set_window_floating(parent_window, true);
        crate::input::grab::place_grabbed_root(
            &mut fixture.state,
            &mut fixture.dispatcher,
            parent,
            (40, 50),
        );
        fixture.refresh_geometry(parent);
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();

        let set_geometry = |fixture: &mut MapFixture, x: i32, y: i32, w: i32, h: i32| {
            let xdg = fixture.xdg();
            let mut request = RawEvent::new(WaylandId::from(80), 3, 4);
            request.int(x);
            request.int(y);
            request.int(w);
            request.int(h);
            fixture
                .dispatch(xdg.object_id(), 80, raw_request_from_event(request))
                .unwrap();
        };
        let xdg = fixture.xdg();
        let mut ack = RawEvent::new(WaylandId::from(80), 4, 1);
        ack.uint(1);
        fixture
            .dispatch(xdg.object_id(), 80, raw_request_from_event(ack))
            .unwrap();
        set_geometry(&mut fixture, 20, 20, 80, 60);
        crate::wayland_objects::wl_surface::test_commit_new_buffer(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.surface,
            120,
            100,
        );
        fixture.refresh_geometry(fixture.surface);
        let centered = fixture.child_geometry();

        fixture.state.note_floating_resize_anchor(
            fixture.window,
            crate::protocol_generated::xdg_toplevel::ResizeEdge::Left as u32,
        );
        set_geometry(&mut fixture, 0, 0, 60, 60);
        crate::wayland_objects::wl_surface::test_commit_new_buffer(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.surface,
            60,
            60,
        );
        fixture.refresh_geometry(fixture.surface);
        let resized = fixture.child_geometry();
        assert_eq!(resized.x2(), centered.x2());
        assert_eq!(resized.y, centered.y);
        assert_eq!(resized.width, centered.width - 20);
    }

    #[test]
    fn a_mapped_floating_dialog_adopts_its_frame_origin() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();
        fixture.map_child();

        let g = fixture.child_geometry();
        assert_eq!(
            fixture
                .state
                .managed_window_state(fixture.window)
                .unwrap()
                .floating_frame_origin,
            Some((g.x, g.y))
        );
    }

    #[test]
    fn a_restored_floating_window_takes_its_remembered_frame_origin() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();
        fixture.map_child();

        fixture.state.set_window_floating(fixture.window, false);
        let r = fixture
            .state
            .managed_window_state(fixture.window)
            .unwrap()
            .remembered_floating_state
            .unwrap()
            .layout_rect;
        fixture.state.set_window_floating(fixture.window, true);

        assert_eq!(
            fixture
                .state
                .managed_window_state(fixture.window)
                .unwrap()
                .floating_frame_origin,
            Some((r.x, r.y))
        );
    }

    #[test]
    fn a_dialog_whose_parent_is_elsewhere_is_centered_on_the_output() {
        let mut fixture = MapFixture::new();
        let (_parent, parent_window) = fixture.setup();
        let parent_workspace = fixture
            .state
            .managed_window_state(parent_window)
            .unwrap()
            .workspace_id;
        let group = fixture
            .state
            .workspaces
            .try_get(parent_workspace)
            .unwrap()
            .group
            .unwrap();
        let other = fixture
            .state
            .create_workspace(Some(group), Some("other".to_string()));
        fixture
            .state
            .assign_window_to_workspace(parent_window, other);

        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();
        fixture.map_child();

        let workspace = fixture
            .state
            .managed_window_state(fixture.window)
            .unwrap()
            .workspace_id;
        let output = fixture.state.workspace_output_id(workspace).unwrap();
        let b = crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(
            &fixture.state,
            output,
        )
        .unwrap();
        let c = fixture.child_geometry();
        assert_eq!(c.x, b.x + (b.width - c.width) / 2);
        assert_eq!(c.y, b.y + (b.height - c.height) / 2);
    }

    #[test]
    fn a_window_already_floating_is_not_recentered() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture
            .state
            .managed_window_state_mut(fixture.window)
            .unwrap()
            .mode = crate::state::ManagedWindowMode::Floating;
        fixture.initial_configure_size();
        assert!(!fixture.center_pending());
        let before = fixture.root_pos();
        fixture.map_child();
        assert_eq!(fixture.root_pos(), before);
    }

    #[test]
    fn a_remap_does_not_float_again() {
        let mut fixture = MapFixture::new();
        fixture.setup();
        fixture.set_parent(41);
        fixture.make_dialog();
        fixture.initial_configure_size();
        fixture.map_child();
        fixture
            .state
            .managed_window_state_mut(fixture.window)
            .unwrap()
            .mode = crate::state::ManagedWindowMode::Tiled;
        fixture.unmap_child();
        // Unmapping cleared the parent; the client sets it again.
        fixture.set_parent(41);
        fixture.initial_configure_size();
        assert!(fixture.is_tiled());
        assert!(!fixture.center_pending());
    }
}
