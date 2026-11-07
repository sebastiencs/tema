//! One X window and the events that create, map, move, and destroy it.
//!
//! A window stays unmapped until `MapNotify`. `ConfigureRequest` is stored in
//! `pending` until the window is shown, and applied on `MapRequest`.

use crate::{
    clients::Resolved,
    drm::request_outputs_for_rect,
    protocol_generated::WlSurfaceId,
    region::RectI,
    x11::{
        change_property_atoms, change_property_u32s, Atom, Atoms, ChangeWindowAttributes,
        ChangeWindowAttributesAux, ClientMessage, ConfigWindow, ConfigureNotify, ConfigureRequest,
        ConfigureWindow, ConfigureWindowAux, CreateNotify, DestroyNotify, Event, EventMask,
        MapNotify, MapRequest, MapWindow, UnmapNotify, Window,
    },
};

use super::{super::SurfaceEvent, props, Wm, WmCtx, WmFailure, WmPhase};

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

const NORMAL: u32 = 1;
const WITHDRAWN: u32 = 0;

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
            Event::DestroyNotify(event) => {
                self.forget(event, ctx);
                Ok(())
            }
            Event::MapRequest(event) => self.on_map_request(event, atoms),
            Event::MapNotify(event) => {
                self.on_map(event, ctx);
                Ok(())
            }
            Event::UnmapNotify(event) => self.on_unmap(event, atoms.WM_STATE, ctx),
            Event::ConfigureRequest(event) => {
                self.on_configure_request(event);
                Ok(())
            }
            Event::ConfigureNotify(event) => {
                self.on_configure(event, ctx);
                Ok(())
            }
            Event::ClientMessage(event) => self.on_serial(event, atoms.WL_SURFACE_SERIAL, ctx),
            Event::PropertyNotify(event) => self.on_property_notify(event, ctx),
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
        if event.window == check || self.windows.contains_key(&event.window) {
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
            },
        );
        self.send_request(&ChangeWindowAttributes {
            window: event.window,
            value_list: ChangeWindowAttributesAux::default()
                .event_mask(EventMask::PropertyChange.union(EventMask::FocusChange)),
        })
    }

    fn forget(&mut self, event: DestroyNotify, ctx: &mut WmCtx<'_>) {
        let Some(window) = self.windows.remove(&event.window) else { return };
        if let Some(serial) = window.serial {
            if self.by_serial.get(&serial) == Some(&event.window) {
                self.by_serial.remove(&serial);
            }
        }
        if window.shown {
            hide(window.surface, window.geometry, ctx);
        }
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
        self.sync_shown(id, geometry, None, ctx);
        self.load_properties(id, ctx)
    }

    fn unpair(&mut self, id: Window, ctx: &mut WmCtx<'_>) {
        let Some(geometry) = self.windows.get(&id).map(|window| window.geometry) else { return };
        let detached = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.surface.take());
        self.sync_shown(id, geometry, detached, ctx);
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
        self.send_request(&MapWindow { window: event.window })
    }

    fn on_map(&mut self, event: MapNotify, ctx: &mut WmCtx<'_>) {
        let geometry = {
            let Some(window) = self.windows.get_mut(&event.window) else { return };
            window.mapped = true;
            window.override_redirect = event.override_redirect != 0;
            window.geometry
        };
        self.sync_shown(event.window, geometry, None, ctx);
    }

    fn on_unmap(
        &mut self,
        event: UnmapNotify,
        wm_state: Atom,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let geometry = {
            let Some(window) = self.windows.get_mut(&event.window) else { return Ok(()) };
            window.mapped = false;
            window.geometry
        };
        self.write_state(event.window, wm_state, WITHDRAWN)?;
        self.sync_shown(event.window, geometry, None, ctx);
        Ok(())
    }

    fn on_configure_request(&mut self, event: ConfigureRequest) {
        let Some(window) = self.windows.get_mut(&event.window) else { return };
        if window.shown {
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
        self.sync_shown(event.window, previous, None, ctx);
    }

    fn sync_shown(
        &mut self,
        id: Window,
        previous: Rect,
        detached: Option<Resolved<WlSurfaceId>>,
        ctx: &mut WmCtx<'_>,
    ) {
        let Some(window) = self.windows.get(&id) else { return };
        let attached = window.surface;
        let surface = attached.or(detached);
        let geometry = window.geometry;
        let was_shown = window.shown;
        let want = attached.is_some() && window.mapped;
        if want {
            if !was_shown || previous != geometry {
                if let Some(surface) = surface {
                    if let Some(mut wl_surface) = surface.try_grab(ctx.state) {
                        wl_surface.set_root_position(geometry.x, geometry.y);
                        wl_surface.set_xwayland_shown(true);
                    }
                    ctx.state.mark_root_geometry_dirty(surface);
                    damage(previous, ctx);
                    damage(geometry, ctx);
                }
                if let Some(window) = self.windows.get_mut(&id) {
                    window.shown = true;
                }
            }
        } else if was_shown {
            hide(surface, geometry, ctx);
            if let Some(window) = self.windows.get_mut(&id) {
                window.shown = false;
            }
        }
    }

    fn write_state(&mut self, window: Window, wm_state: Atom, state: u32) -> Result<(), WmFailure> {
        let change = change_property_u32s(window, wm_state, wm_state, &[state, 0]);
        self.send_request(&change.request())
    }

    fn send_request(
        &mut self,
        request: &impl crate::x11::Request<Reply = crate::x11::NoReply>,
    ) -> Result<(), WmFailure> {
        match self.conn.send_void(request) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }
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
