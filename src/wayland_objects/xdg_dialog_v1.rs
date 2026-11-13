//! `xdg_wm_dialog_v1` / `xdg_dialog_v1`. Modal is a stored hint. A dialog with a
//! parent floats at its first initial commit. See `XDG_DIALOG_PLAN.md`.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_dialog_v1, xdg_wm_dialog_v1, Constants, NewId, WaylandId, XdgDialogV1Id, XdgToplevelId,
        XdgWmDialogV1Id,
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
            get_toplevel_request, raw_request_from_event, state_for_tests,
            test_client_and_dispatcher,
        },
        wayland_objects::{
            wl_surface::{Role, WlSurface},
            xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
        },
        window::{Toplevel, WindowId},
    };

    use super::{XdgDialogV1, XdgWmDialogV1, XdgWmDialogV1Global};

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
}
