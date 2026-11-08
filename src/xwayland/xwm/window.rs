//! One X window and the events that create, map, move, and destroy it.
//!
//! A window stays unmapped until `MapNotify`. `ConfigureRequest` is stored in
//! `pending` until the window is shown, and applied on `MapRequest`.
//!
//! A paired, mapped window becomes a tema window once its first property batch
//! has arrived. The layout owns that window's root position. An
//! override-redirect window stays unmanaged at its X geometry.

use crate::{
    clients::Resolved,
    drm::request_outputs_for_rect,
    protocol_generated::WlSurfaceId,
    region::RectI,
    runner::{Dispatcher, Redispatcher},
    state::ObjectId,
    window::{Toplevel, WindowId},
    x11::{
        change_property_atoms, change_property_u32s, event_bytes, Atom, Atoms,
        ChangeWindowAttributes, ChangeWindowAttributesAux, ClientIdMask, ClientIdSpec,
        ClientMessage, ClientMessageData, ConfigWindow, ConfigureNotify, ConfigureRequest,
        ConfigureWindow, ConfigureWindowAux, CreateNotify, DestroyNotify, Event, EventMask,
        KillClient, MapNotify, MapRequest, MapWindow, ResQueryClientIds, ResQueryClientIdsReply,
        SendEvent, StackMode, UnmapNotify, Window, XReplyError,
    },
    State,
};

use super::{
    super::{surfaces::SurfaceSerials, SurfaceEvent},
    props,
    toplevel::XToplevel,
    Wm, WmCtx, WmEvent, WmFailure, WmPhase, Xwayland, XwaylandAction,
};

impl PartialOrd for Window {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Window {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

pub(super) const NORMAL: u32 = 1;
const ICONIC: u32 = 3;
const WITHDRAWN: u32 = 0;
const NET_WM_STATE_REMOVE: u32 = 0;
const NET_WM_STATE_ADD: u32 = 1;
const NET_WM_STATE_TOGGLE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl Rect {
    fn from_event(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x: i32::from(x),
            y: i32::from(y),
            width: u32::from(width),
            height: u32::from(height),
        }
    }

    fn has_size(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// Tema window created for one managed X window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Managed {
    pub(super) window: WindowId,
    pub(super) object: ObjectId,
    pub(super) configured: Option<Rect>,
    pub(super) floating_size: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Hidden,
    Unmanaged,
    Managed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct XWindow {
    pub(super) id: Window,
    pub(super) override_redirect: bool,
    pub(super) geometry: Rect,
    pub(super) pending: Rect,
    pub(super) mapped: bool,
    pub(super) serial: Option<u64>,
    pub(super) surface: Option<Resolved<WlSurfaceId>>,
    pub(super) shown: bool,
    pub(super) props: props::WindowProps,
    pub(super) managed: Option<Managed>,
    pub(super) props_outstanding: usize,
    pub(super) props_loaded: bool,
    /// Client pid from X-Resource. `None` until a reply names one.
    pub(super) pid: Option<u32>,
}

impl Wm {
    pub(super) fn on_core_event(
        &mut self,
        event: Event,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (check, atoms) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            (ready.check_window, ready.atoms)
        };
        match event {
            Event::CreateNotify(event) => self.on_create(event, check),
            Event::DestroyNotify(event) => self.forget(event, ctx),
            Event::MapRequest(event) => self.on_map_request(event, atoms),
            Event::MapNotify(event) => self.on_map(event, ctx),
            Event::UnmapNotify(event) => self.on_unmap(event, atoms, ctx),
            Event::ConfigureRequest(event) => {
                self.on_configure_request(event);
                Ok(())
            }
            Event::ConfigureNotify(event) => {
                self.on_configure(event, ctx);
                Ok(())
            }
            Event::ClientMessage(event) => self.on_client_message(event, atoms, ctx),
            Event::PropertyNotify(event) => self.on_property_notify(event, ctx),
            Event::FocusIn(event) => self.on_focus_in(event, ctx),
            Event::XFixesSelectionNotify(event) => self.on_selection_owner(event, ctx),
            Event::SelectionNotify(event) => self.on_selection_notify(event, ctx),
            Event::SelectionClear(event) => self.on_selection_clear(event, ctx),
            Event::SelectionRequest(event) => self.on_selection_request(event, ctx),
            _ => Ok(()),
        }
    }

    pub(in crate::xwayland) fn on_surface_event(
        &mut self,
        event: &SurfaceEvent,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        match *event {
            SurfaceEvent::SerialAssigned { serial, surface } => {
                let Some(&window) = self.by_serial.get(&serial) else { return Ok(()) };
                self.pair(window, surface, serial, ctx)
            }
            SurfaceEvent::Destroyed { serial, surface } => {
                let Some(&window) = self.by_serial.get(&serial) else { return Ok(()) };
                let paired = self
                    .windows
                    .get(&window)
                    .is_some_and(|window| window.surface == Some(surface));
                if paired {
                    self.unpair(window, ctx);
                }
                Ok(())
            }
        }
    }

    fn on_create(&mut self, event: CreateNotify, check: Window) -> Result<(), WmFailure> {
        if event.window == check
            || self.selection_window(event.window)
            || self.windows.contains_key(&event.window)
        {
            return Ok(());
        }
        let rect = Rect::from_event(event.x, event.y, event.width, event.height);
        self.windows.insert(
            event.window,
            XWindow {
                id: event.window,
                override_redirect: event.override_redirect != 0,
                geometry: rect,
                pending: rect,
                mapped: false,
                serial: None,
                surface: None,
                shown: false,
                props: props::WindowProps::default(),
                managed: None,
                props_outstanding: 0,
                props_loaded: false,
                pid: None,
            },
        );
        self.send_request(&ChangeWindowAttributes {
            window: event.window,
            value_list: ChangeWindowAttributesAux::default()
                .event_mask(EventMask::PropertyChange.union(EventMask::FocusChange)),
        })
    }

    fn forget(&mut self, event: DestroyNotify, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        self.drop_outgoing_requestor(event.window, ctx)?;
        let dropped_map = remove_window(&mut self.map_order, event.window);
        let dropped_stack = remove_window(&mut self.stack, event.window);
        let focused = self.focus == Some(event.window);
        let Some(window) = self.windows.remove(&event.window) else { return Ok(()) };
        if focused {
            self.focus = None;
        }
        if let Some(serial) = window.serial {
            if self.by_serial.get(&serial) == Some(&event.window) {
                self.by_serial.remove(&serial);
            }
        }
        let managed = window.managed;
        let close = window
            .surface
            .and_then(|root| crate::input::focus::focused_window_close(ctx.state, root));
        if let Some(managed) = managed {
            crate::capture::unmap_window(ctx.state, ctx.dispatcher, managed.window);
            ctx.state.destroy_window(managed.window);
            ctx.state.remove_object(managed.object);
        }
        if let Some(close) = close {
            crate::input::focus::refocus_after_focused_window_unmap(
                ctx.state,
                ctx.dispatcher,
                close,
            );
        } else if window.shown && managed.is_none() {
            release_unmanaged_keyboard_focus(window.surface, ctx);
        }
        if window.shown || managed.is_some() {
            hide(window.surface, window.geometry, ctx);
        }
        let atoms = self.ready_atoms();
        if dropped_map {
            if let Some(atoms) = atoms {
                self.write_client_list(atoms)?;
            }
        }
        if dropped_stack {
            if let Some(atoms) = atoms {
                self.write_stacking_list(atoms)?;
            }
        }
        if focused {
            self.release_x_focus()?;
        }
        Ok(())
    }

    fn on_client_message(
        &mut self,
        event: ClientMessage,
        atoms: Atoms,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if event.format != 32 || !self.windows.contains_key(&event.window) {
            return Ok(());
        }
        if event.r#type == atoms.WL_SURFACE_SERIAL {
            self.on_serial(event, atoms.WL_SURFACE_SERIAL, ctx)
        } else if event.r#type == atoms._NET_WM_STATE {
            self.on_net_wm_state(event, atoms, ctx)
        } else if event.r#type == atoms.WM_CHANGE_STATE {
            self.on_wm_change_state(event, atoms, ctx)
        } else if event.r#type == atoms._NET_ACTIVE_WINDOW {
            self.on_net_active_window(event.window, ctx)
        } else {
            Ok(())
        }
    }

    fn on_net_wm_state(
        &mut self,
        event: ClientMessage,
        atoms: Atoms,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let words = event.data.data32();
        if !matches!(words[0], NET_WM_STATE_REMOVE | NET_WM_STATE_ADD | NET_WM_STATE_TOGGLE) {
            return Ok(());
        }
        let Some(mut props) = self
            .windows
            .get(&event.window)
            .map(|window| window.props.clone())
        else {
            return Ok(());
        };
        let was_fullscreen = props.fullscreen;
        let was_hidden = props.hidden;
        let mut changed = false;
        for word in [words[1], words[2]] {
            if apply_net_state(&mut props, &atoms, Atom(word), words[0]) {
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let fullscreen = props.fullscreen;
        let rising = !was_hidden && props.hidden;
        if rising {
            props.hidden = was_hidden;
        }
        if let Some(window) = self.windows.get_mut(&event.window) {
            window.props = props;
        }
        if fullscreen != was_fullscreen {
            if let Some(window_id) = self.managed_window(Some(event.window)) {
                ctx.state.set_window_fullscreen(window_id, fullscreen);
                ctx.dispatcher
                    .dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
            }
        }
        if rising {
            self.request_iconify(event.window, atoms, ctx)?;
        }
        self.refresh_managed(event.window, ctx);
        self.write_net_wm_state(event.window, atoms)
    }

    fn on_wm_change_state(
        &mut self,
        event: ClientMessage,
        atoms: Atoms,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        match event.data.data32()[0] {
            ICONIC => self.request_iconify(event.window, atoms, ctx),
            NORMAL => {
                if let Some(window) = self.windows.get_mut(&event.window) {
                    window.props.hidden = false;
                }
                self.write_state(event.window, atoms.WM_STATE, NORMAL)?;
                self.write_net_wm_state(event.window, atoms)
            }
            _ => Ok(()),
        }
    }

    fn request_iconify(
        &mut self,
        id: Window,
        atoms: Atoms,
        ctx: &WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let iconic = !self.focus_is_root(id, ctx);
        if let Some(window) = self.windows.get_mut(&id) {
            window.props.hidden = iconic;
        }
        self.write_state(id, atoms.WM_STATE, if iconic { ICONIC } else { NORMAL })?;
        self.write_net_wm_state(id, atoms)
    }

    fn focus_is_root(&self, id: Window, ctx: &WmCtx<'_>) -> bool {
        let Some(root) = self.windows.get(&id).and_then(|window| window.surface) else {
            return false;
        };
        let Some(focus) = ctx.state.seat.keyboard_focus.as_ref() else { return false };
        ctx.state.root_surface_for_surface(focus.surface) == Some(root)
    }

    pub(super) fn write_net_wm_state(&mut self, id: Window, atoms: Atoms) -> Result<(), WmFailure> {
        let Some(props) = self.windows.get(&id).map(|window| window.props.clone()) else {
            return Ok(());
        };
        let mut state = props::net_wm_state(&props, &atoms);
        if self.focus == Some(id) {
            state.push(atoms._NET_WM_STATE_FOCUSED);
        }
        let change = change_property_atoms(id, atoms._NET_WM_STATE, atoms.ATOM, &state);
        self.send_request(&change.request())
    }

    fn on_serial(
        &mut self,
        event: ClientMessage,
        serial_atom: Atom,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if event.format != 32 || event.r#type != serial_atom {
            return Ok(());
        }
        if !self.windows.contains_key(&event.window) {
            return Ok(());
        }
        let words = event.data.data32();
        let serial = u64::from(words[0]) | (u64::from(words[1]) << 32);
        if serial <= self.last_serial {
            log::debug!("ignoring WL_SURFACE_SERIAL {serial} on {:?}", event.window);
            return Ok(());
        }
        self.last_serial = serial;
        if let Some(previous) = self
            .windows
            .get(&event.window)
            .and_then(|window| window.serial)
        {
            self.by_serial.remove(&previous);
        }
        self.unpair(event.window, ctx);
        if let Some(window) = self.windows.get_mut(&event.window) {
            window.serial = Some(serial);
        }
        self.by_serial.insert(serial, event.window);
        if let Some(surface) = ctx.surfaces.get(serial) {
            self.pair(event.window, surface, serial, ctx)?;
        }
        Ok(())
    }

    fn pair(
        &mut self,
        id: Window,
        surface: Resolved<WlSurfaceId>,
        serial: u64,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let matches = surface.try_ref(ctx.state).is_some_and(|wl_surface| {
            wl_surface.has_xwayland_role() && wl_surface.xwayland_serial() == Some(serial)
        });
        if !matches {
            return Ok(());
        }
        let geometry = {
            let Some(window) = self.windows.get_mut(&id) else { return Ok(()) };
            window.surface = Some(surface);
            window.geometry
        };
        log::debug!("X window {id:?} paired with {surface:?}");
        self.sync_presence(id, geometry, None, ctx);
        self.load_properties(id, ctx)?;
        self.query_pid(id, ctx)
    }

    /// Asks X-Resource for this window's client pid. A missing extension sends nothing.
    fn query_pid(&mut self, id: Window, ctx: &WmCtx<'_>) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
        if ready.extensions.res.is_none() {
            return Ok(());
        }
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::ClientPid { window: id, result },
        });
        let spec = ClientIdSpec { client: id.0, mask: ClientIdMask::LocalClientPID };
        match self
            .conn
            .send(&ResQueryClientIds { specs: &[spec] }, redispatch)
        {
            Ok(_) => Ok(()),
            Err(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }

    pub(super) fn on_client_pid(
        &mut self,
        window: Window,
        result: Result<ResQueryClientIdsReply, XReplyError>,
    ) -> Result<(), WmFailure> {
        let Some(tracked) = self.windows.get_mut(&window) else { return Ok(()) };
        tracked.pid = match result {
            Err(_) => None,
            Ok(reply) => reply.ids.into_iter().find_map(|id| {
                if id.spec.mask.contains(ClientIdMask::LocalClientPID) {
                    id.value.first().copied()
                } else {
                    None
                }
            }),
        };
        Ok(())
    }

    fn unpair(&mut self, id: Window, ctx: &mut WmCtx<'_>) {
        let Some(geometry) = self.windows.get(&id).map(|window| window.geometry) else { return };
        let detached = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.surface.take());
        self.sync_presence(id, geometry, detached, ctx);
    }

    fn on_map_request(&mut self, event: MapRequest, atoms: Atoms) -> Result<(), WmFailure> {
        let Some(window) = self.windows.get(&event.window) else { return Ok(()) };
        let pending = window.pending;
        let state = props::net_wm_state(&window.props, &atoms);
        let net_wm_state =
            change_property_atoms(event.window, atoms._NET_WM_STATE, atoms.ATOM, &state);
        self.write_state(event.window, atoms.WM_STATE, NORMAL)?;
        if pending.has_size() {
            self.send_request(&ConfigureWindow {
                window: event.window,
                value_list: ConfigureWindowAux::default().x(0).y(0).width(1).height(1),
            })?;
            self.send_request(&ConfigureWindow {
                window: event.window,
                value_list: ConfigureWindowAux::default()
                    .x(pending.x)
                    .y(pending.y)
                    .width(pending.width)
                    .height(pending.height),
            })?;
        }
        self.send_request(&net_wm_state.request())?;
        self.track_map_request(event.window, atoms)?;
        self.send_request(&MapWindow { window: event.window })
    }

    fn on_map(&mut self, event: MapNotify, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let geometry = {
            let Some(window) = self.windows.get_mut(&event.window) else { return Ok(()) };
            window.mapped = true;
            window.override_redirect = event.override_redirect != 0;
            window.geometry
        };
        self.sync_presence(event.window, geometry, None, ctx);
        let Some(atoms) = self.ready_atoms() else { return Ok(()) };
        self.restack(event.window, atoms)
    }

    fn on_unmap(
        &mut self,
        event: UnmapNotify,
        atoms: Atoms,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let geometry = {
            let Some(window) = self.windows.get_mut(&event.window) else { return Ok(()) };
            window.mapped = false;
            window.geometry
        };
        self.write_state(event.window, atoms.WM_STATE, WITHDRAWN)?;
        if remove_window(&mut self.map_order, event.window) {
            self.write_client_list(atoms)?;
        }
        self.sync_presence(event.window, geometry, None, ctx);
        Ok(())
    }

    fn on_configure_request(&mut self, event: ConfigureRequest) {
        let Some(window) = self.windows.get_mut(&event.window) else { return };
        if window.shown || window.managed.is_some() {
            return;
        }
        if event.value_mask.contains(ConfigWindow::X) {
            window.pending.x = i32::from(event.x);
        }
        if event.value_mask.contains(ConfigWindow::Y) {
            window.pending.y = i32::from(event.y);
        }
        if event.value_mask.contains(ConfigWindow::Width) {
            window.pending.width = u32::from(event.width);
        }
        if event.value_mask.contains(ConfigWindow::Height) {
            window.pending.height = u32::from(event.height);
        }
    }

    fn on_configure(&mut self, event: ConfigureNotify, ctx: &mut WmCtx<'_>) {
        let previous = {
            let Some(window) = self.windows.get_mut(&event.window) else { return };
            let previous = window.geometry;
            window.override_redirect = event.override_redirect != 0;
            window.geometry = Rect::from_event(event.x, event.y, event.width, event.height);
            if !window.shown {
                window.pending = window.geometry;
            }
            previous
        };
        self.sync_presence(event.window, previous, None, ctx);
    }

    /// The first property batch finished, or one property of a managed window changed.
    pub(super) fn note_property_batch(&mut self, id: Window, finished: bool, ctx: &mut WmCtx<'_>) {
        if finished {
            if let Some(window) = self.windows.get_mut(&id) {
                window.props_loaded = true;
            }
            if let Some(geometry) = self.windows.get(&id).map(|window| window.geometry) {
                self.sync_presence(id, geometry, None, ctx);
            }
        }
        self.refresh_managed(id, ctx);
    }

    /// Drops every tema window this WM created.
    pub(super) fn release_managed(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        let surfaces = SurfaceSerials::default();
        let mut ctx = WmCtx { run: 0, state, dispatcher, surfaces: &surfaces, client: None };
        let leaving = self
            .windows
            .values_mut()
            .filter_map(|window| {
                let managed = window.managed.take()?;
                window.shown = false;
                Some((managed, window.surface, window.geometry))
            })
            .collect::<Vec<_>>();
        let mut workspaces = Vec::new();
        for (managed, surface, geometry) in leaving {
            if let Some(workspace_id) = ctx
                .state
                .managed_window_state(managed.window)
                .map(|managed_window| managed_window.workspace_id)
            {
                if !workspaces.contains(&workspace_id) {
                    workspaces.push(workspace_id);
                }
            }
            let close =
                surface.and_then(|root| crate::input::focus::focused_window_close(ctx.state, root));
            crate::capture::unmap_window(ctx.state, ctx.dispatcher, managed.window);
            ctx.state.destroy_window(managed.window);
            ctx.state.remove_object(managed.object);
            if let Some(close) = close {
                crate::input::focus::refocus_after_focused_window_unmap(
                    ctx.state,
                    ctx.dispatcher,
                    close,
                );
            }
            hide(surface, geometry, &mut ctx);
        }
        for workspace_id in workspaces {
            publish_layout(Some(workspace_id), &mut ctx);
        }
    }

    fn sync_presence(
        &mut self,
        id: Window,
        previous: Rect,
        detached: Option<Resolved<WlSurfaceId>>,
        ctx: &mut WmCtx<'_>,
    ) {
        let Some(window) = self.windows.get(&id) else { return };
        let surface = window.surface.or(detached);
        let geometry = window.geometry;
        let want = wanted(window);
        let have = if window.managed.is_some() {
            Presence::Managed
        } else if window.shown {
            Presence::Unmanaged
        } else {
            Presence::Hidden
        };
        if have == want {
            if have == Presence::Unmanaged && previous != geometry {
                place(surface, geometry, ctx);
                damage(previous, ctx);
                damage(geometry, ctx);
            }
            return;
        }
        if have == Presence::Managed {
            self.leave_managed(id, surface, geometry, ctx);
        } else if have == Presence::Unmanaged {
            release_unmanaged_keyboard_focus(surface, ctx);
            hide(surface, geometry, ctx);
            if let Some(window) = self.windows.get_mut(&id) {
                window.shown = false;
            }
        }
        if previous != geometry && have != Presence::Hidden {
            damage(previous, ctx);
        }
        match want {
            Presence::Managed => self.enter_managed(id, ctx),
            Presence::Unmanaged => self.show_unmanaged(id, surface, geometry, ctx),
            Presence::Hidden => {}
        }
    }

    fn enter_managed(&mut self, id: Window, ctx: &mut WmCtx<'_>) {
        let Some((surface, props, transient)) = self.windows.get(&id).and_then(|window| {
            Some((window.surface?, window.props.clone(), window.props.transient_for))
        }) else {
            return;
        };
        let parent = self.managed_window(transient);
        let atoms = self.ready_atoms();
        let window_id = ctx.state.allocate_window_id();
        let mut toplevel = XToplevel::new(window_id, id, ctx.run);
        if let Some(atoms) = atoms {
            toplevel.refresh(&props, parent, &atoms);
        }
        let (_, object) = ctx.state.insert_object(toplevel);
        ctx.state.insert_window(window_id, surface, object);
        reveal(Some(surface), ctx);
        let floating = match &self.phase {
            WmPhase::Ready(ready) => props::wants_floating(&props, &ready.atoms),
            _ => false,
        };
        let fullscreen = props.fullscreen;
        if let Some(window) = self.windows.get_mut(&id) {
            window.shown = true;
            window.managed = Some(Managed {
                window: window_id,
                object,
                configured: None,
                floating_size: None,
            });
        }
        if floating {
            ctx.state.set_window_floating(window_id, true);
            self.place_floating(id, ctx);
        } else if fullscreen {
            ctx.state.set_window_fullscreen(window_id, true);
        }
        self.publish_layout_for(window_id, ctx);
        if surface
            .try_ref(ctx.state)
            .is_some_and(|surface| surface.has_committed_buffer())
        {
            crate::capture::map_window(ctx.state, ctx.dispatcher, window_id);
            crate::capture::sync_layout_shell_state(ctx.state, ctx.dispatcher, window_id);
            crate::capture::sync_activated_from_keyboard_focus(ctx.state, ctx.dispatcher);
        }
        if ctx.state.window_is_mapped(window_id)
            && crate::wayland_objects::wl_surface::should_focus_new_toplevel_on_map(
                ctx.state, surface,
            )
        {
            crate::input::set_keyboard_focus(
                ctx.state,
                ctx.dispatcher,
                Some(crate::input::SurfaceFocus::new(surface)),
            );
        }
        self.refresh_transient_children(id, ctx);
    }

    fn place_floating(&mut self, id: Window, ctx: &mut WmCtx<'_>) {
        let Some((surface, geometry, tema_window, transient)) =
            self.windows.get(&id).and_then(|window| {
                Some((
                    window.surface?,
                    window.geometry,
                    window.managed?.window,
                    window.props.transient_for,
                ))
            })
        else {
            return;
        };
        let insets = crate::root_geometry::effective_decoration_insets(ctx.state, surface);
        let content_width = i32::try_from(geometry.width).unwrap_or(i32::MAX);
        let content_height = i32::try_from(geometry.height).unwrap_or(i32::MAX);
        let mut frame_width = content_width
            .saturating_add(insets.left)
            .saturating_add(insets.right);
        let mut frame_height = content_height
            .saturating_add(insets.top)
            .saturating_add(insets.bottom);
        let workspace_id = ctx
            .state
            .managed_window_state(tema_window)
            .map(|managed| managed.workspace_id);
        let Some(bounds) =
            workspace_id.and_then(|workspace_id| usable_bounds(ctx.state, workspace_id))
        else {
            return;
        };
        let parent = self
            .managed_window(transient)
            .filter(|parent| parent_shares_workspace(ctx.state, *parent, workspace_id));
        let anchor = parent
            .and_then(|parent| parent_frame(ctx.state, parent))
            .unwrap_or(bounds);
        if frame_width > bounds.width {
            frame_width = bounds.width;
        }
        if frame_height > bounds.height {
            frame_height = bounds.height;
        }
        let frame_x = clamp_origin(
            anchor.x + (anchor.width - frame_width) / 2,
            frame_width,
            bounds.x,
            bounds.width,
        );
        let frame_y = clamp_origin(
            anchor.y + (anchor.height - frame_height) / 2,
            frame_height,
            bounds.y,
            bounds.height,
        );
        if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
            wl_surface.set_root_position(
                frame_x.saturating_add(insets.left),
                frame_y.saturating_add(insets.top),
            );
        }
        ctx.state.mark_root_geometry_dirty(surface);
        let placed_width = frame_width
            .saturating_sub(insets.left)
            .saturating_sub(insets.right);
        let placed_height = frame_height
            .saturating_sub(insets.top)
            .saturating_sub(insets.bottom);
        if placed_width != content_width || placed_height != content_height {
            if let Some(managed) = self
                .windows
                .get_mut(&id)
                .and_then(|window| window.managed.as_mut())
            {
                managed.floating_size = Some((
                    u32::try_from(placed_width.max(1)).unwrap_or(u32::MAX),
                    u32::try_from(placed_height.max(1)).unwrap_or(u32::MAX),
                ));
            }
        }
        ctx.dispatcher
            .dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
    }

    fn leave_managed(
        &mut self,
        id: Window,
        surface: Option<Resolved<WlSurfaceId>>,
        geometry: Rect,
        ctx: &mut WmCtx<'_>,
    ) {
        let Some(managed) = self.windows.get(&id).and_then(|window| window.managed) else {
            return;
        };
        let workspace_id = ctx
            .state
            .managed_window_state(managed.window)
            .map(|managed_window| managed_window.workspace_id);
        let Some(managed) = self.windows.get_mut(&id).and_then(|window| {
            window.shown = false;
            window.managed.take()
        }) else {
            return;
        };
        let close =
            surface.and_then(|root| crate::input::focus::focused_window_close(ctx.state, root));
        crate::capture::unmap_window(ctx.state, ctx.dispatcher, managed.window);
        ctx.state.destroy_window(managed.window);
        ctx.state.remove_object(managed.object);
        if let Some(close) = close {
            crate::input::focus::refocus_after_focused_window_unmap(
                ctx.state,
                ctx.dispatcher,
                close,
            );
        }
        hide(surface, geometry, ctx);
        publish_layout(workspace_id, ctx);
        self.refresh_transient_children(id, ctx);
    }

    fn show_unmanaged(
        &mut self,
        id: Window,
        surface: Option<Resolved<WlSurfaceId>>,
        geometry: Rect,
        ctx: &mut WmCtx<'_>,
    ) {
        place(surface, geometry, ctx);
        damage(geometry, ctx);
        self.shown_order = self.shown_order.saturating_add(1);
        let order = self.shown_order;
        if let Some(surface) = surface {
            if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
                wl_surface.set_xwayland_shown_order(order);
            }
        }
        if let Some(window) = self.windows.get_mut(&id) {
            window.shown = true;
        }
    }

    fn refresh_managed(&mut self, id: Window, ctx: &mut WmCtx<'_>) {
        let Some((object, window_id, props, transient)) =
            self.windows.get(&id).and_then(|window| {
                Some((
                    window.managed?.object,
                    window.managed?.window,
                    window.props.clone(),
                    window.props.transient_for,
                ))
            })
        else {
            return;
        };
        let parent = self.managed_window(transient);
        let atoms = self.ready_atoms();
        let (minimum_changed, title, app_id, parent_window) = {
            let Some(atoms) = atoms else { return };
            let Some(toplevel) = ctx
                .state
                .objects
                .try_get_mut(object)
                .and_then(|object| object.downcast_mut::<XToplevel>())
            else {
                return;
            };
            let minimum_changed = toplevel.refresh(&props, parent, &atoms);
            (
                minimum_changed,
                toplevel.title().to_string(),
                toplevel.app_id().to_string(),
                toplevel.parent_window(),
            )
        };
        crate::capture::retitle_window(ctx.state, ctx.dispatcher, window_id, title);
        crate::capture::reapp_window(ctx.state, ctx.dispatcher, window_id, app_id);
        crate::capture::note_window_parent(ctx.state, ctx.dispatcher, window_id, parent_window);
        if minimum_changed
            && (ctx.state.window_is_tiled(window_id) || ctx.state.window_is_fullscreen(window_id))
        {
            let workspace_id = ctx
                .state
                .managed_window_state(window_id)
                .map(|managed_window| managed_window.workspace_id);
            if let Some(workspace_id) = workspace_id {
                crate::tiling::mark_workspace_dirty(ctx.state, workspace_id);
            }
            ctx.dispatcher
                .dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
        }
    }

    /// Refresh every managed window that names `parent` in `WM_TRANSIENT_FOR`.
    fn refresh_transient_children(&mut self, parent: Window, ctx: &mut WmCtx<'_>) {
        let children = self
            .windows
            .iter()
            .filter(|(_, window)| {
                window.props.transient_for == Some(parent) && window.managed.is_some()
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for child in children {
            self.refresh_managed(child, ctx);
        }
    }

    fn publish_layout_for(&mut self, window: WindowId, ctx: &mut WmCtx<'_>) {
        let workspace_id = ctx
            .state
            .managed_window_state(window)
            .map(|managed_window| managed_window.workspace_id);
        publish_layout(workspace_id, ctx);
    }

    fn managed_window(&self, x_window: Option<Window>) -> Option<WindowId> {
        self.windows
            .get(&x_window?)?
            .managed
            .map(|managed| managed.window)
    }

    pub(super) fn reconfigure(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let ids = self.windows.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.reconfigure_window(id, ctx)?;
        }
        let managed = self
            .windows
            .values()
            .filter_map(|window| window.managed.map(|managed| managed.window))
            .collect::<Vec<_>>();
        for window in managed {
            crate::capture::sync_layout_shell_state(ctx.state, ctx.dispatcher, window);
        }
        Ok(())
    }

    pub(super) fn resize(
        &mut self,
        id: Window,
        width: i32,
        height: i32,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let Some(managed) = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.managed.as_mut())
        else {
            return Ok(());
        };
        managed.floating_size = Some((at_least_one(width), at_least_one(height)));
        self.reconfigure_window(id, ctx)
    }

    fn reconfigure_window(&mut self, id: Window, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(target) = self.configure_target(id, ctx) else { return Ok(()) };
        let configured = self
            .windows
            .get(&id)
            .and_then(|window| window.managed)
            .and_then(|managed| managed.configured);
        if configured == Some(target) {
            return Ok(());
        }
        self.send_request(&ConfigureWindow {
            window: id,
            value_list: ConfigureWindowAux::default()
                .x(target.x)
                .y(target.y)
                .width(target.width)
                .height(target.height)
                .border_width(0),
        })?;
        if let Some(managed) = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.managed.as_mut())
        {
            managed.configured = Some(target);
        }
        Ok(())
    }

    fn configure_target(&mut self, id: Window, ctx: &mut WmCtx<'_>) -> Option<Rect> {
        let window = self.windows.get(&id)?;
        let managed = window.managed?;
        let surface = window.surface?;
        let geometry = window.geometry;
        let floating_size = managed.floating_size;
        let tema_window = managed.window;
        let has_buffer = surface.try_ref(ctx.state)?.has_committed_buffer();
        if !has_buffer
            || !crate::scene::visibility::root_has_protocol_visibility(ctx.state, surface)
        {
            return None;
        }
        if let Some(layout) = ctx.state.tiled_window_geometry(tema_window) {
            let content = layout.content_rect_global();
            return Some(Rect {
                x: content.x,
                y: content.y,
                width: at_least_one(content.width),
                height: at_least_one(content.height),
            });
        }
        let (x, y) = surface.try_ref(ctx.state)?.root_position();
        let (width, height) = if let Some(remembered) =
            ctx.state.take_window_floating_restore(tema_window)
        {
            (at_least_one(remembered.configure_size.0), at_least_one(remembered.configure_size.1))
        } else if let Some((width, height)) = floating_size {
            (width.max(1), height.max(1))
        } else {
            (geometry.width.max(1), geometry.height.max(1))
        };
        Some(Rect { x, y, width, height })
    }

    pub(super) fn close(&mut self, id: Window) -> Result<(), WmFailure> {
        let Some(protocols) = self
            .windows
            .get(&id)
            .map(|window| window.props.protocols.clone())
        else {
            return Ok(());
        };
        let delete_window = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            ready.atoms.WM_DELETE_WINDOW
        };
        if protocols.contains(&delete_window) {
            self.send_protocol_message(id, delete_window)
        } else {
            self.send_request(&KillClient { resource: id.0 })
        }
    }

    /// `WM_PROTOCOLS` `ClientMessage` delivered with `SendEvent`. The time word is `CurrentTime`.
    pub(super) fn send_protocol_message(
        &mut self,
        id: Window,
        atom: Atom,
    ) -> Result<(), WmFailure> {
        let wm_protocols = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            ready.atoms.WM_PROTOCOLS
        };
        let mut data = ClientMessageData::default();
        data.set_data32([atom.0, 0, 0, 0, 0]);
        let message = ClientMessage {
            send_event: true,
            sequence: 0,
            format: 32,
            window: id,
            r#type: wm_protocols,
            data,
        };
        let event = event_bytes(|writer| message.serialize(writer));
        self.send_request(&SendEvent {
            propagate: 0,
            destination: id,
            event_mask: EventMask::NoEvent,
            event: &event,
        })
    }

    /// Moves a normal window to the top of the stack. Activation calls this.
    pub(super) fn move_to_top(&mut self, id: Window) {
        let normal = self
            .windows
            .get(&id)
            .is_some_and(|window| !window.override_redirect);
        if !normal {
            return;
        }
        self.stack.retain(|window| *window != id);
        self.stack.push(id);
    }

    fn track_map_request(&mut self, id: Window, atoms: Atoms) -> Result<(), WmFailure> {
        let normal = self
            .windows
            .get(&id)
            .is_some_and(|window| !window.override_redirect);
        if !normal {
            return Ok(());
        }
        self.move_to_top(id);
        if !self.map_order.contains(&id) {
            self.map_order.push(id);
        }
        self.write_client_list(atoms)
    }

    pub(super) fn restack(&mut self, id: Window, atoms: Atoms) -> Result<(), WmFailure> {
        let Some(index) = self.stack.iter().position(|window| *window == id) else {
            return Ok(());
        };
        let below = index.checked_sub(1).map(|below| self.stack[below]);
        let above = self.stack.get(index + 1).copied();
        let value_list = if let Some(sibling) = below {
            ConfigureWindowAux::default()
                .sibling(sibling)
                .stack_mode(StackMode::Above)
        } else if let Some(sibling) = above {
            ConfigureWindowAux::default()
                .sibling(sibling)
                .stack_mode(StackMode::Below)
        } else {
            ConfigureWindowAux::default().stack_mode(StackMode::Above)
        };
        self.send_request(&ConfigureWindow { window: id, value_list })?;
        self.write_stacking_list(atoms)
    }

    pub(super) fn ready_atoms(&self) -> Option<Atoms> {
        match &self.phase {
            WmPhase::Ready(ready) => Some(ready.atoms),
            _ => None,
        }
    }

    fn write_client_list(&mut self, atoms: Atoms) -> Result<(), WmFailure> {
        let ids: Vec<u32> = self.map_order.iter().map(|window| window.0).collect();
        self.write_window_list(atoms._NET_CLIENT_LIST, &ids, atoms)
    }

    fn write_stacking_list(&mut self, atoms: Atoms) -> Result<(), WmFailure> {
        let ids: Vec<u32> = self.stack.iter().map(|window| window.0).collect();
        self.write_window_list(atoms._NET_CLIENT_LIST_STACKING, &ids, atoms)
    }

    fn write_window_list(
        &mut self,
        property: Atom,
        ids: &[u32],
        atoms: Atoms,
    ) -> Result<(), WmFailure> {
        let root = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            ready.root
        };
        let change = change_property_u32s(root, property, atoms.WINDOW, ids);
        self.send_request(&change.request())
    }

    pub(super) fn write_state(
        &mut self,
        window: Window,
        wm_state: Atom,
        state: u32,
    ) -> Result<(), WmFailure> {
        let change = change_property_u32s(window, wm_state, wm_state, &[state, 0]);
        self.send_request(&change.request())
    }

    pub(super) fn send_request(
        &mut self,
        request: &impl crate::x11::Request<Reply = crate::x11::NoReply>,
    ) -> Result<(), WmFailure> {
        self.send_request_sequence(request).map(|_| ())
    }

    /// Sends a void request and returns its sequence. A send error fails the WM.
    pub(super) fn send_request_sequence(
        &mut self,
        request: &impl crate::x11::Request<Reply = crate::x11::NoReply>,
    ) -> Result<u64, WmFailure> {
        match self.conn.send_void(request) {
            Ok(sequence) => Ok(sequence),
            Err(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }
}

fn remove_window(windows: &mut Vec<Window>, id: Window) -> bool {
    let before = windows.len();
    windows.retain(|window| *window != id);
    windows.len() != before
}

fn apply_net_state(props: &mut props::WindowProps, atoms: &Atoms, atom: Atom, action: u32) -> bool {
    let slot = if atom == atoms._NET_WM_STATE_MODAL {
        &mut props.modal
    } else if atom == atoms._NET_WM_STATE_FULLSCREEN {
        &mut props.fullscreen
    } else if atom == atoms._NET_WM_STATE_MAXIMIZED_VERT {
        &mut props.maximized_vert
    } else if atom == atoms._NET_WM_STATE_MAXIMIZED_HORZ {
        &mut props.maximized_horz
    } else if atom == atoms._NET_WM_STATE_HIDDEN {
        &mut props.hidden
    } else {
        return false;
    };
    let new = match action {
        NET_WM_STATE_REMOVE => false,
        NET_WM_STATE_ADD => true,
        NET_WM_STATE_TOGGLE => !*slot,
        _ => return false,
    };
    let changed = *slot != new;
    *slot = new;
    changed
}

fn usable_bounds(state: &State, workspace_id: crate::state::WorkspaceId) -> Option<RectI> {
    let output_id = state.workspace_output_id(workspace_id)?;
    crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(state, output_id)
        .or_else(|| state.output_bounds(output_id))
}

fn parent_shares_workspace(
    state: &State,
    parent: WindowId,
    workspace_id: Option<crate::state::WorkspaceId>,
) -> bool {
    state.window_is_mapped(parent)
        && state
            .managed_window_state(parent)
            .is_some_and(|managed| Some(managed.workspace_id) == workspace_id)
}

fn parent_frame(state: &State, parent: WindowId) -> Option<RectI> {
    let surface = state.window_root_surface(parent)?;
    state
        .committed_root_geometry(surface)
        .map(|geometry| geometry.layout_rect_global)
}

fn clamp_origin(origin: i32, size: i32, bounds_origin: i32, bounds_size: i32) -> i32 {
    if bounds_size <= size {
        return bounds_origin;
    }
    origin.clamp(bounds_origin, bounds_origin + bounds_size - size)
}

fn at_least_one(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0).max(1)
}

fn wanted(window: &XWindow) -> Presence {
    if window.surface.is_none() || !window.mapped {
        return Presence::Hidden;
    }
    if window.override_redirect {
        return Presence::Unmanaged;
    }
    if window.props_loaded {
        return Presence::Managed;
    }
    Presence::Hidden
}

fn reveal(surface: Option<Resolved<WlSurfaceId>>, ctx: &mut WmCtx<'_>) {
    let Some(surface) = surface else { return };
    if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
        wl_surface.set_xwayland_shown(true);
    }
    ctx.state.mark_root_geometry_dirty(surface);
}

fn place(surface: Option<Resolved<WlSurfaceId>>, geometry: Rect, ctx: &mut WmCtx<'_>) {
    reveal(surface, ctx);
    let Some(surface) = surface else { return };
    if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
        wl_surface.set_root_position(geometry.x, geometry.y);
    }
}

fn publish_layout(workspace_id: Option<crate::state::WorkspaceId>, ctx: &mut WmCtx<'_>) {
    if let Some(workspace_id) = workspace_id {
        crate::tiling::mark_workspace_dirty(ctx.state, workspace_id);
    }
    let opened = ctx.state.relayout_dirty_workspaces();
    if !opened.is_empty() {
        crate::drm::kick_visual_animation_pump(ctx.dispatcher);
    }
    if workspace_id.is_some() {
        ctx.dispatcher
            .dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
    }
}

fn release_unmanaged_keyboard_focus(surface: Option<Resolved<WlSurfaceId>>, ctx: &mut WmCtx<'_>) {
    let Some(surface) = surface else { return };
    let Some(focus) = ctx.state.seat.keyboard_focus else { return };
    let root = ctx
        .state
        .root_surface_for_surface(focus.surface)
        .unwrap_or(focus.surface);
    if root != surface {
        return;
    }
    crate::input::set_keyboard_focus(ctx.state, ctx.dispatcher, None);
    crate::input::focus::focus_after_client_disconnect(ctx.state, ctx.dispatcher);
}

fn hide(surface: Option<Resolved<WlSurfaceId>>, geometry: Rect, ctx: &mut WmCtx<'_>) {
    if let Some(surface) = surface {
        if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
            wl_surface.set_xwayland_shown(false);
        }
    }
    damage(geometry, ctx);
}

fn damage(rect: Rect, ctx: &mut WmCtx<'_>) {
    let Ok(width) = i32::try_from(rect.width) else { return };
    let Ok(height) = i32::try_from(rect.height) else { return };
    let Some(rect) = RectI::new(rect.x, rect.y, width, height) else { return };
    request_outputs_for_rect(ctx.state, rect, ctx.dispatcher);
}
