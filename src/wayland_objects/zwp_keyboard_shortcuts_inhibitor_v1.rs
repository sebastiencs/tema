//! `zwp_keyboard_shortcuts_inhibitor_v1`. One per surface (tema has one seat).

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        zwp_keyboard_shortcuts_inhibitor_v1, WaylandId, WlSurfaceId,
        ZwpKeyboardShortcutsInhibitorV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// Surface destruction drops the association and leaves this object inert until
/// the client destroys it (same model as `ZwpIdleInhibitorV1`).
#[derive(Debug)]
pub struct ZwpKeyboardShortcutsInhibitorV1 {
    id: ZwpKeyboardShortcutsInhibitorV1Id,
    surface: Option<Resolved<WlSurfaceId>>,
    /// Protocol "active" state. Flipped only by the user toggle; never by focus.
    user_enabled: bool,
}

impl ZwpKeyboardShortcutsInhibitorV1 {
    pub(crate) fn new(
        id: ZwpKeyboardShortcutsInhibitorV1Id,
        surface: Resolved<WlSurfaceId>,
        user_enabled: bool,
    ) -> Self {
        Self { id, surface: Some(surface), user_enabled }
    }

    /// Only tests read it; production code goes through the surface.
    #[cfg(test)]
    pub(crate) fn surface(&self) -> Option<Resolved<WlSurfaceId>> {
        self.surface
    }

    pub(crate) fn user_enabled(&self) -> bool {
        self.user_enabled
    }

    pub(crate) fn set_user_enabled(&mut self, enabled: bool) {
        self.user_enabled = enabled;
    }

    pub(crate) fn send_state(&self, active: bool, dispatcher: &mut ClientDispatcher) {
        let event = if active {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active
        } else {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive
        };
        self.send_event(event, dispatcher);
    }

    fn detach(&mut self, client: &Client, state: &mut State) {
        let Some(surface) = self.surface.take() else { return };
        let Some(me) = client.resolve(self.id, state) else { return };
        if let Some(surface) = surface.try_ref_mut(state) {
            surface.clear_shortcuts_inhibitor(me);
        }
    }
}

/// The surface is going away: drop the association, keep the client-owned object.
pub(crate) fn detach_destroyed_surface(
    inhibitor: Option<Resolved<ZwpKeyboardShortcutsInhibitorV1Id>>,
    state: &mut State,
) {
    if let Some(inhibitor) = inhibitor.and_then(|i| i.try_ref_mut(state)) {
        inhibitor.surface = None;
    }
}

/// Inhibitor attached to the current keyboard-focus surface, enabled or not.
/// The focus must be the root of a live window: after `xdg_toplevel.destroy`,
/// focus can still sit on the role-less surface until the successor search runs.
pub(crate) fn focused_inhibitor(
    state: &State,
) -> Option<Resolved<ZwpKeyboardShortcutsInhibitorV1Id>> {
    let focus = state.seat.keyboard_focus?;
    state.window_for_root(focus.surface)?;
    focus.surface.try_ref(state)?.shortcuts_inhibitor()
}

/// True when compositor bindings must be skipped for the focused surface.
pub(crate) fn shortcuts_inhibited(state: &State) -> bool {
    focused_inhibitor(state)
        .and_then(|inhibitor| inhibitor.try_ref(state))
        .is_some_and(|inhibitor| inhibitor.user_enabled())
}

/// `toggle-shortcuts-inhibit`: flip the focused surface's inhibitor. The state
/// flips now, so the next key already uses it; the client gets the event when
/// it processes the queued `ClientAction`.
pub(crate) fn toggle_focused(state: &mut State, dispatcher: &mut crate::runner::Dispatcher) {
    let Some(resolved) = focused_inhibitor(state) else {
        log::debug!("toggle-shortcuts-inhibit ignored; focused surface has no inhibitor");
        return;
    };
    let Some(inhibitor) = resolved.try_ref_mut(state) else { return };
    let active = !inhibitor.user_enabled();
    inhibitor.set_user_enabled(active);
    // `focused_inhibitor` returned Some, so the focus surface exists.
    if let Some(focus) = state.seat.keyboard_focus {
        if let Some(surface) = focus.surface.try_ref_mut(state) {
            surface.set_shortcuts_inhibit_denied(!active);
        }
    }
    log::info!("keyboard shortcuts inhibitor {:?} active={}", resolved, active);
    dispatcher.dispatch_to(
        resolved.client_id(),
        crate::clients::ClientAction::ShortcutsInhibitorState { inhibitor: resolved, active },
    );
}

impl Object for ZwpKeyboardShortcutsInhibitorV1 {
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
            zwp_keyboard_shortcuts_inhibitor_v1::Request::Destroy => {
                self.detach(client, state);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
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
        self.detach(client, state);
    }
}
