//! X input focus for managed windows.
//!
//! The seat's keyboard focus picks the target. A managed X root is activated.
//! Any other X root is left alone. A Wayland surface, or no focus, clears X focus.

use crate::{
    clients::Resolved,
    protocol_generated::WlSurfaceId,
    x11::{
        change_property_u32s, Atoms, FocusIn, InputFocus, NotifyDetail, NotifyMode, SetInputFocus,
        Window,
    },
};

use super::{window::NORMAL, Wm, WmCtx, WmFailure, WmPhase};

impl Wm {
    pub(super) fn on_keyboard_focus(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(focus) = ctx.state.seat.keyboard_focus else {
            return self.activate(None);
        };
        let root = ctx
            .state
            .root_surface_for_surface(focus.surface)
            .unwrap_or(focus.surface);
        let x_root = root
            .try_ref(ctx.state)
            .is_some_and(|surface| surface.has_xwayland_role());
        if !x_root {
            return self.activate(None);
        }
        let managed = self.windows.iter().find_map(|(&id, window)| {
            (window.surface == Some(root) && window.managed.is_some() && !window.override_redirect)
                .then_some(id)
        });
        if let Some(id) = managed {
            self.activate(Some(id))
        } else {
            Ok(())
        }
    }

    fn activate(&mut self, target: Option<Window>) -> Result<(), WmFailure> {
        if target == self.focus {
            return Ok(());
        }
        let (atoms, root) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            (ready.atoms, ready.root)
        };
        if let Some(id) = target {
            let eligible = self
                .windows
                .get(&id)
                .is_some_and(|window| !window.override_redirect && window.managed.is_some());
            if !eligible {
                return Ok(());
            }
            let hidden = self
                .windows
                .get(&id)
                .is_some_and(|window| window.props.hidden);
            if hidden {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.props.hidden = false;
                }
                self.write_state(id, atoms.WM_STATE, NORMAL)?;
            }
        }
        self.write_active(root, atoms, target)?;
        let previous = std::mem::replace(&mut self.focus, target);
        if let Some(previous) = previous {
            if self.windows.contains_key(&previous) {
                self.write_net_wm_state(previous, atoms)?;
            }
        }
        self.send_focus(target, atoms)?;
        if let Some(id) = target {
            self.write_net_wm_state(id, atoms)?;
            self.move_to_top(id);
            self.restack(id, atoms)?;
        }
        Ok(())
    }

    /// `_NET_ACTIVE_WINDOW` becomes 0 and X focus becomes `None`.
    pub(super) fn release_x_focus(&mut self) -> Result<(), WmFailure> {
        let (atoms, root) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            (ready.atoms, ready.root)
        };
        self.write_active(root, atoms, None)?;
        self.send_input_focus(Window::None)
    }

    fn write_active(
        &mut self,
        root: Window,
        atoms: Atoms,
        target: Option<Window>,
    ) -> Result<(), WmFailure> {
        let id = target.map(|window| window.0).unwrap_or(0);
        let change = change_property_u32s(root, atoms._NET_ACTIVE_WINDOW, atoms.WINDOW, &[id]);
        self.send_request(&change.request())
    }

    fn send_focus(&mut self, target: Option<Window>, atoms: Atoms) -> Result<(), WmFailure> {
        let Some(id) = target else {
            return self.send_input_focus(Window::None);
        };
        let (input, take_focus) = {
            let Some(window) = self.windows.get(&id) else { return Ok(()) };
            let take_focus = window.props.protocols.contains(&atoms.WM_TAKE_FOCUS);
            (window.props.input_hint, take_focus)
        };
        if input {
            self.send_input_focus(id)?;
        }
        if take_focus {
            self.send_protocol_message(id, atoms.WM_TAKE_FOCUS)?;
        }
        Ok(())
    }

    fn send_input_focus(&mut self, focus: Window) -> Result<(), WmFailure> {
        let sequence = self.send_request_sequence(&SetInputFocus {
            revert_to: InputFocus::PointerRoot,
            focus,
            time: 0,
        })?;
        self.last_input_sequence = sequence;
        Ok(())
    }

    pub(super) fn on_focus_in(
        &mut self,
        event: FocusIn,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if event.mode == NotifyMode::Grab
            || event.mode == NotifyMode::Ungrab
            || event.detail == NotifyDetail::Pointer
        {
            return Ok(());
        }
        let Some(atoms) = self.ready_atoms() else { return Ok(()) };
        if self.focus_in_is_acceptable(&event, ctx) {
            self.apply_accepted_focus_in(event.event, atoms, ctx)
        } else {
            self.send_focus(self.focus, atoms)
        }
    }

    fn focus_in_is_acceptable(&self, event: &FocusIn, ctx: &WmCtx<'_>) -> bool {
        let Some(incoming) = self.windows.get(&event.event) else { return false };
        if incoming.surface.is_none() {
            return false;
        }
        let incoming_pid = incoming.pid;
        let managed = incoming.managed.is_some();
        let shown = incoming.shown;
        let surface = incoming.surface;
        let Some(current) = self.focus else { return false };
        let Some(current_pid) = self.windows.get(&current).and_then(|window| window.pid) else {
            return false;
        };
        let Some(incoming_pid) = incoming_pid else { return false };
        if incoming_pid != current_pid {
            return false;
        }
        if self.conn.widen_event_sequence(event.sequence) < self.last_input_sequence {
            return false;
        }
        if managed {
            let Some(surface) = surface else { return false };
            crate::scene::visibility::root_has_protocol_visibility(ctx.state, surface)
        } else {
            shown
        }
    }

    fn apply_accepted_focus_in(
        &mut self,
        id: Window,
        atoms: Atoms,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let previous = self.focus.replace(id);
        if let Some(previous) = previous {
            if self
                .windows
                .get(&previous)
                .is_some_and(|window| window.managed.is_some())
            {
                self.write_net_wm_state(previous, atoms)?;
            }
        }
        let (surface, managed, override_redirect) = {
            let Some(window) = self.windows.get(&id) else { return Ok(()) };
            (window.surface, window.managed.is_some(), window.override_redirect)
        };
        if managed {
            self.write_net_wm_state(id, atoms)?;
        }
        if managed && !override_redirect {
            if let Some(surface) = surface {
                ctx.state.raise_root_surface(surface);
                crate::input::set_keyboard_focus(
                    ctx.state,
                    ctx.dispatcher,
                    Some(crate::input::SurfaceFocus::new(surface)),
                );
            }
        }
        Ok(())
    }

    pub(super) fn on_net_active_window(
        &mut self,
        id: Window,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let Some(current) = self.focus else { return Ok(()) };
        let Some(current_pid) = self.windows.get(&current).and_then(|window| window.pid) else {
            return Ok(());
        };
        let Some(surface) = self.activation_target(id, current_pid) else { return Ok(()) };
        if !crate::scene::visibility::root_has_protocol_visibility(ctx.state, surface) {
            return Ok(());
        }
        ctx.state.raise_root_surface(surface);
        crate::input::set_keyboard_focus(
            ctx.state,
            ctx.dispatcher,
            Some(crate::input::SurfaceFocus::new(surface)),
        );
        Ok(())
    }

    /// A managed, same-PID window that a client may activate. Override-redirect windows are excluded.
    fn activation_target(&self, id: Window, current_pid: u32) -> Option<Resolved<WlSurfaceId>> {
        let window = self.windows.get(&id)?;
        if window.override_redirect || window.managed.is_none() || window.pid != Some(current_pid) {
            return None;
        }
        window.surface
    }
}
