use {
    crate::{
        clients::{Client, ClientAction, ClientDispatcher, ClientId, Resolved},
        drm::{kick_visual_animation_pump, request_outputs_for_rects},
        object::PostAction,
        protocol_generated::{WlSeatId, WlSurfaceId},
        runner::Dispatcher,
        scene::{compose_output, CompositionTier},
        seat::{dispatch_client_event, SeatClientEvent},
        state::{DragPhase, DragTargetState, State},
        wayland_objects::zwlr_layer_surface_v1::{
            layer_surface_keyboard_focus_with_snapshot, LayerKeyboardFocus,
            LayerLayoutObjectSnapshot, LayerShellLayer,
        },
    },
    std::collections::BTreeSet,
};

use super::{scene_pick_for_pointer, surface_focus_pick_at_excluding, SurfaceFocus, SurfaceRect};

pub(crate) fn set_keyboard_focus(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    new_focus: Option<SurfaceFocus>,
) {
    set_keyboard_focus_with_snapshot(state, dispatcher, new_focus, None);
}

fn set_keyboard_focus_with_snapshot(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    new_focus: Option<SurfaceFocus>,
    snapshot: Option<LayerLayoutObjectSnapshot<'_>>,
) {
    let new_focus = normalize_keyboard_focus_with_snapshot(state, new_focus, snapshot);
    if state.seat.keyboard_focus == new_focus {
        // A remap can publish a new toplevel record while keyboard focus stays put.
        crate::capture::sync_activated_from_keyboard_focus(state, dispatcher);
        return;
    }
    crate::input::pointer_lock::release_pointer_lock_unless_surface(
        state,
        dispatcher,
        new_focus.map(|focus| focus.surface),
    );
    crate::input::pointer_confine::release_pointer_confine_unless_surface(
        state,
        dispatcher,
        new_focus.map(|focus| focus.surface),
    );

    log::debug!(
        "seat keyboard focus change old={:?} new={:?}",
        state.seat.keyboard_focus,
        new_focus
    );

    let serial = state.seat.next_serial();

    if let Some(old_focus) = state.seat.keyboard_focus {
        dispatch_client_event(
            dispatcher,
            old_focus.client_id(),
            SeatClientEvent::KeyboardLeave { serial, surface: old_focus.surface },
        );
    }

    if let Some(new_focus) = new_focus {
        let keymap = state.xkb.keymap_blob();
        let modifiers = state.keyboard.modifiers();
        crate::seat::ensure_client_keymap(
            state,
            dispatcher,
            new_focus,
            keymap,
            crate::seat::empty_keys(),
            modifiers,
        );
        let keys = state.keyboard.pressed_keys();
        dispatch_client_event(
            dispatcher,
            new_focus.client_id(),
            SeatClientEvent::KeyboardEnter { serial, surface: new_focus.surface, keys },
        );
        dispatch_modifiers(dispatcher, new_focus.client_id(), serial, state.keyboard.modifiers());
    }

    let old_focus = state.seat.keyboard_focus;
    state.seat.keyboard_focus = new_focus;
    reconfigure_activation_roots(state, dispatcher, old_focus, new_focus);
    dispatcher.dispatch(crate::xwayland::XwaylandAction::KeyboardFocus);
    super::ime::keyboard_focus_changed(state, dispatcher, old_focus, new_focus);
    if let Some(new_focus) = new_focus.filter(|_| !state.session_lock.input_gated()) {
        let root_focus = state
            .root_surface_for_surface(new_focus.surface)
            .map(SurfaceFocus::new)
            .unwrap_or(new_focus);
        state.seat.remember_keyboard_focus(root_focus);
    }
    let affected_rects = [old_focus, new_focus].into_iter().filter_map(|focus| {
        let focus = focus?;
        let root_surface = state
            .root_surface_for_surface(focus.surface)
            .unwrap_or(focus.surface);
        state.root_surface_visual_damage_rect(root_surface)
    });
    request_outputs_for_rects(state, affected_rects, dispatcher);
    kick_visual_animation_pump(dispatcher);
    refresh_selection_focus_clients(dispatcher, old_focus, new_focus);
    crate::capture::sync_activated_from_keyboard_focus(state, dispatcher);
    crate::input::pointer_lock::maybe_activate_pointer_lock(state, dispatcher);
    crate::input::pointer_confine::maybe_activate_pointer_confine(state, dispatcher);
    if state.clear_urgency_for_focused_window().is_some() {
        dispatcher.dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
    }
}

/// Ask each affected client to reconfigure the toplevel whose `activated` state changed.
///
/// A root that has no xdg toplevel (layer surface, X window, destroyed surface)
/// matches nothing in `ReconfigureToplevelsForRoots`, so it is harmless.
fn reconfigure_activation_roots(
    state: &State,
    dispatcher: &mut Dispatcher,
    old_focus: Option<SurfaceFocus>,
    new_focus: Option<SurfaceFocus>,
) {
    let mut by_client: std::collections::BTreeMap<ClientId, Vec<Resolved<WlSurfaceId>>> =
        std::collections::BTreeMap::new();
    for focus in [old_focus, new_focus].into_iter().flatten() {
        let root = state
            .root_surface_for_surface(focus.surface)
            .unwrap_or(focus.surface);
        if state.window_for_root(root).is_none() {
            continue;
        }
        let roots = by_client.entry(root.client_id()).or_default();
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    for (client_id, roots) in by_client {
        crate::seat::dispatch_client_action(
            dispatcher,
            client_id,
            ClientAction::ReconfigureToplevelsForRoots { roots },
        );
    }
}

pub(crate) fn set_keyboard_focus_from_surface(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    new_focus: Option<SurfaceFocus>,
) {
    set_keyboard_focus(state, dispatcher, new_focus);
}

pub(crate) fn resolve_activation_target(
    state: &State,
    mut surface: Resolved<WlSurfaceId>,
) -> Option<SurfaceFocus> {
    loop {
        let surface_ref = surface.try_ref(state)?;
        match surface_ref.parent_surface() {
            Some(parent) => surface = parent.clone(),
            None => {
                if !surface_ref.has_committed_buffer() {
                    return None;
                }
                return Some(SurfaceFocus::new(surface));
            }
        }
    }
}

pub(crate) fn validate_activation_serial(
    state: &State,
    client: &Client,
    seat: Resolved<WlSeatId>,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
) -> bool {
    client.object_is(seat.wayland_id(), state)
        && seat.client_id() == client.client_id()
        && surface.client_id() == client.client_id()
        && state
            .seat
            .validate_implicit_grab_serial(client.client_id(), surface, serial)
}

pub(crate) fn refresh_keyboard_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    refresh_keyboard_focus_with_snapshot(state, dispatcher, None);
}

pub(crate) fn refresh_keyboard_focus_with_snapshot(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    snapshot: Option<LayerLayoutObjectSnapshot<'_>>,
) {
    let preferred_focus =
        popup_keyboard_focus(state).or_else(|| state.seat.pointer_focus.map(|focus| focus.target));
    let new_focus = normalize_keyboard_focus_with_snapshot(state, preferred_focus, snapshot);
    set_keyboard_focus_with_snapshot(state, dispatcher, new_focus, snapshot);
}

pub(crate) fn refresh_scene_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
    refresh_keyboard_focus(state, dispatcher);
}

/// Pointer refresh after a fullscreen layout change. Keyboard focus is not touched.
pub(crate) fn refresh_pointer_focus_only(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::seat::refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
}

/// A window that just became fullscreen takes keyboard focus.
///
/// Does nothing when: the session lock gates input; the window has no root
/// surface; the window is no longer fullscreen; the window's workspace is not
/// active; a popup grab is active; or the window's root already holds keyboard
/// focus (including through one of its popups).
pub(crate) fn focus_fullscreen_window(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    window: crate::window::WindowId,
) {
    if state.session_lock.input_gated() {
        return;
    }
    let Some(root) = state.window_root_surface(window) else {
        return;
    };
    if !state.window_is_fullscreen(window) {
        return;
    }
    let Some(workspace_id) = state.root_surface_workspace(root) else {
        return;
    };
    if !state.workspace_is_active(workspace_id) {
        return;
    }
    if state.seat.popup_grab_client().is_some() {
        return;
    }
    if state.focused_root_surface() == Some(root) {
        return;
    }
    set_keyboard_focus(state, dispatcher, Some(SurfaceFocus::new(root)));
}

/// Runs `focus_fullscreen_window` after the request handler releases its object.
pub(crate) struct FocusFullscreenWindow {
    pub window: crate::window::WindowId,
}

impl PostAction for FocusFullscreenWindow {
    fn execute(
        self: Box<Self>,
        _client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        focus_fullscreen_window(state, dispatcher, self.window);
    }
}

/// Runs after the committing surface is released. Picking it while it is grabbed panics.
pub(crate) struct RefreshLockFocus;

impl PostAction for RefreshLockFocus {
    fn execute(
        self: Box<Self>,
        _client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        refresh_scene_focus(state, dispatcher);
    }
}

pub(crate) fn restore_keyboard_focus_after_unlock(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    saved: Option<SurfaceFocus>,
) {
    refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
    if let Some(focus) = saved.filter(|focus| keyboard_focus_is_restorable(state, *focus)) {
        set_keyboard_focus(state, dispatcher, Some(focus));
        return;
    }
    focus_after_client_disconnect(state, dispatcher);
}

/// Keep keyboard focus when it is still usable, otherwise restore it on a visible workspace.
///
/// "Usable" means: the focus surface still exists and is mapped
/// (`keyboard_focus_is_restorable`), and
/// - for a managed window root: its workspace is active;
/// - for a layer-shell root: its keyboard interactivity is not `none`.
/// Any other root kind (popup child, unmanaged X root) only needs to be restorable.
///
/// Usable focus is re-applied through `set_keyboard_focus`, so normalization still lets an
/// exclusive layer take over. Otherwise the target workspace is the active workspace of the
/// output that showed the old focused window, or of `State::active_output_id()` when the old
/// focus was not a window. On it: history (MRU), then topmost, then the window under the
/// pointer. With no candidate, `refresh_keyboard_focus` keeps today's behaviour.
pub(crate) fn keep_or_restore_keyboard_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    if state.session_lock.input_gated() {
        refresh_keyboard_focus(state, dispatcher);
        return;
    }
    let current = state.seat.keyboard_focus();
    if let Some(focus) = current.filter(|focus| keyboard_focus_is_usable(state, *focus)) {
        set_keyboard_focus(state, dispatcher, Some(focus));
        return;
    }
    let old_window_workspace = current.and_then(|focus| {
        let root = state
            .root_surface_for_surface(focus.surface)
            .unwrap_or(focus.surface);
        state.window_for_root(root)?;
        state.root_surface_workspace(root)
    });
    let output_id = old_window_workspace
        .and_then(|workspace_id| state.workspace_output_ids(workspace_id).into_iter().next())
        .or_else(|| state.active_output_id());
    let Some(workspace_id) =
        output_id.and_then(|output_id| state.active_workspace_for_output(output_id))
    else {
        refresh_keyboard_focus(state, dispatcher);
        return;
    };
    if let Some(candidate) = mru_focus_candidate_for_workspace(state, workspace_id, None)
        .or_else(|| topmost_focus_candidate_for_workspace(state, workspace_id, None))
        .or_else(|| pointer_focus_candidate_for_workspace(state, workspace_id, None))
    {
        set_keyboard_focus(state, dispatcher, Some(candidate));
        return;
    }
    refresh_keyboard_focus(state, dispatcher);
}

/// Unlike `keyboard_focus_is_restorable`, an unmanaged X root (an override-redirect menu) is
/// usable: workspace and layout publications must not pull focus off an open X menu.
fn keyboard_focus_is_usable(state: &State, focus: SurfaceFocus) -> bool {
    let Some(surface) = focus.surface.try_ref(state) else {
        return false;
    };
    if surface.has_lock_surface_role()
        || surface.has_cursor_role()
        || surface.has_dnd_icon_role()
        || surface.has_input_popup_role()
        || surface.logical_size().is_none()
    {
        return false;
    }
    let root = state
        .root_surface_for_surface(focus.surface)
        .unwrap_or(focus.surface);
    if state.window_for_root(root).is_some() {
        return state
            .root_surface_workspace(root)
            .is_some_and(|workspace_id| state.workspace_is_active(workspace_id));
    }
    !matches!(
        layer_surface_keyboard_focus_with_snapshot(state, root, None),
        Some(LayerKeyboardFocus::None)
    )
}

fn keyboard_focus_is_restorable(state: &State, focus: SurfaceFocus) -> bool {
    let Some(surface) = focus.surface.try_ref(state) else {
        return false;
    };
    if surface.has_lock_surface_role()
        || surface.has_cursor_role()
        || surface.has_dnd_icon_role()
        || (surface.has_xwayland_role() && state.window_for_root(focus.surface).is_none())
        || surface.has_input_popup_role()
    {
        return false;
    }
    surface.logical_size().is_some()
}

pub(crate) fn focus_after_client_disconnect(state: &mut State, dispatcher: &mut Dispatcher) {
    if state.session_lock.input_gated() {
        refresh_keyboard_focus(state, dispatcher);
        return;
    }
    if state.seat.keyboard_focus().is_some() {
        return;
    }
    let Some(workspace_id) = state
        .active_output_id()
        .and_then(|output_id| state.active_workspace_for_output(output_id))
    else {
        refresh_keyboard_focus(state, dispatcher);
        return;
    };
    if let Some(candidate) = mru_focus_candidate_for_workspace(state, workspace_id, None)
        .or_else(|| topmost_focus_candidate_for_workspace(state, workspace_id, None))
        .or_else(|| pointer_focus_candidate_for_workspace(state, workspace_id, None))
    {
        set_keyboard_focus(state, dispatcher, Some(candidate));
        return;
    }
    refresh_keyboard_focus(state, dispatcher);
}

/// The workspace window that currently owns the keyboard, captured before it is unmapped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClosedWindowFocus {
    pub closed: Resolved<WlSurfaceId>,
    pub workspace_id: crate::state::WorkspaceId,
}

/// `Some` when `surface` is the focused workspace window. Safe while that surface is grabbed:
/// a managed toplevel is its own root and is not `try_ref`'d.
pub(crate) fn focused_window_close(
    state: &State,
    surface: Resolved<WlSurfaceId>,
) -> Option<ClosedWindowFocus> {
    if state.session_lock.input_gated() {
        return None;
    }
    if !state.is_managed_window(surface) {
        return None;
    }
    let focus = state.seat.keyboard_focus()?;
    let root = state.root_surface_for_surface(focus.surface)?;
    if root != surface {
        return None;
    }
    let workspace_id = state.root_surface_workspace(surface)?;
    Some(ClosedWindowFocus { closed: surface, workspace_id })
}

/// Pointer refresh, then a same-workspace successor when the closed surface owned the keyboard.
/// A destroy that leaves the keyboard on some other live surface does not move it.
///
/// `released_keyboard` is the focus whose `wl_keyboard.leave` was already written, before
/// `delete_id`. The stored focus is clear, so this function enters the next window only.
pub(crate) fn refocus_after_surface_destroy(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    closed_window: Option<ClosedWindowFocus>,
    released_keyboard: Option<SurfaceFocus>,
) {
    refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
    if let Some(close) = closed_window {
        focus_after_focused_window_close(state, dispatcher, close);
    } else {
        let keyboard_surface_gone = state
            .seat
            .keyboard_focus()
            .is_some_and(|focus| !focus.surface.exists(state));
        if released_keyboard.is_some() || keyboard_surface_gone {
            refresh_keyboard_focus(state, dispatcher);
        }
    }
    finish_released_keyboard(state, dispatcher, released_keyboard);
}

/// Write pointer and keyboard leave for `surface` while its id is still live, then clear that focus.
///
/// Returns the keyboard focus that was cleared. The caller destroys the surface after this returns.
pub(crate) fn leave_destroyed_surface_focus(
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    surface: Resolved<WlSurfaceId>,
) -> Option<SurfaceFocus> {
    if state
        .seat
        .pointer_focus
        .is_some_and(|focus| focus.target.surface == surface)
    {
        let serial = state.seat.next_serial();
        let client_id = surface.client_id();
        for pointer in state.seat.pointer_resources(client_id) {
            if !pointer.exists(state) {
                continue;
            }
            state.seat.record_client_serial(client_id, serial);
            let pointer = pointer.ref_mut(state);
            pointer.send_leave(serial, surface, dispatcher);
            pointer.send_frame(dispatcher);
        }
        state.seat.pointer_focus = None;
        state.seat.clear_pointer_enter();
    }

    let old_keyboard = state
        .seat
        .keyboard_focus
        .filter(|focus| focus.surface == surface);
    if let Some(old_keyboard) = old_keyboard {
        let serial = state.seat.next_serial();
        let client_id = surface.client_id();
        for keyboard in state.seat.keyboard_resources(client_id) {
            if !keyboard.exists(state) {
                continue;
            }
            state.seat.record_client_serial(client_id, serial);
            keyboard
                .ref_mut(state)
                .send_leave(serial, surface, dispatcher);
        }
        state.seat.keyboard_focus = None;
        return Some(old_keyboard);
    }
    None
}

/// Side effects of clearing the keyboard that `set_keyboard_focus` no longer sees,
/// because the leave was sent and the stored focus cleared before `delete_id`.
fn finish_released_keyboard(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    released: Option<SurfaceFocus>,
) {
    let Some(old) = released else {
        return;
    };
    let new_focus = state.seat.keyboard_focus();
    if new_focus.is_none() {
        crate::input::pointer_lock::release_pointer_lock_unless_surface(state, dispatcher, None);
        crate::input::pointer_confine::release_pointer_confine_unless_surface(
            state, dispatcher, None,
        );
        dispatcher.dispatch(crate::xwayland::XwaylandAction::KeyboardFocus);
        refresh_selection_focus_clients(dispatcher, Some(old), None);
        crate::capture::sync_activated_from_keyboard_focus(state, dispatcher);
        crate::input::pointer_lock::maybe_activate_pointer_lock(state, dispatcher);
        crate::input::pointer_confine::maybe_activate_pointer_confine(state, dispatcher);
        return;
    }
    if new_focus.is_some_and(|focus| focus.client_id() != old.client_id()) {
        refresh_selection_focus_clients(dispatcher, Some(old), new_focus);
    }
}

pub(crate) fn refocus_after_focused_window_unmap(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    close: ClosedWindowFocus,
) {
    refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
    focus_after_focused_window_close(state, dispatcher, close);
}

/// Window under the pointer on the closed window's workspace, then focus history, then the
/// topmost remaining window. The closed window is skipped. Hyprland's `input:focus_on_close`
/// tries the cursor first; the history step is the same list sway keeps per workspace.
fn focus_after_focused_window_close(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    close: ClosedWindowFocus,
) {
    if state.session_lock.input_gated() {
        refresh_keyboard_focus(state, dispatcher);
        return;
    }
    let excluding = Some(close.closed);
    if let Some(candidate) =
        pointer_focus_candidate_for_workspace(state, close.workspace_id, excluding)
            .or_else(|| mru_focus_candidate_for_workspace(state, close.workspace_id, excluding))
            .or_else(|| topmost_focus_candidate_for_workspace(state, close.workspace_id, excluding))
    {
        set_keyboard_focus(state, dispatcher, Some(candidate));
        return;
    }
    set_keyboard_focus(state, dispatcher, None);
}

fn mru_focus_candidate_for_workspace(
    state: &State,
    workspace_id: crate::state::WorkspaceId,
    excluding: Option<Resolved<WlSurfaceId>>,
) -> Option<SurfaceFocus> {
    let mut seen = BTreeSet::new();
    for focus in state.seat.keyboard_focus_history() {
        let root_surface = state
            .root_surface_for_surface(focus.surface)
            .unwrap_or(focus.surface);
        if !seen.insert(root_surface) {
            continue;
        }
        if Some(root_surface) == excluding {
            continue;
        }
        if !focus_candidate_matches_workspace(state, root_surface, workspace_id) {
            continue;
        }
        return Some(SurfaceFocus::new(root_surface));
    }
    None
}

fn topmost_focus_candidate_for_workspace(
    state: &State,
    workspace_id: crate::state::WorkspaceId,
    excluding: Option<Resolved<WlSurfaceId>>,
) -> Option<SurfaceFocus> {
    let scene = state.build_scene_model();
    let order = state.build_workspace_focus_order(&scene, workspace_id, None);
    order
        .roots_front_to_back
        .into_iter()
        .rev()
        .find(|surface| {
            Some(*surface) != excluding
                && focus_candidate_matches_workspace(state, *surface, workspace_id)
        })
        .map(SurfaceFocus::new)
}

fn pointer_focus_candidate_for_workspace(
    state: &State,
    workspace_id: crate::state::WorkspaceId,
    excluding: Option<Resolved<WlSurfaceId>>,
) -> Option<SurfaceFocus> {
    let pointer_focus = state.seat.pointer_focus.map(|focus| focus.target)?;
    let root_surface = state
        .root_surface_for_surface(pointer_focus.surface)
        .unwrap_or(pointer_focus.surface);
    if Some(root_surface) == excluding {
        return None;
    }
    focus_candidate_matches_workspace(state, root_surface, workspace_id)
        .then_some(SurfaceFocus::new(root_surface))
}

fn focus_candidate_matches_workspace(
    state: &State,
    root_surface: Resolved<WlSurfaceId>,
    workspace_id: crate::state::WorkspaceId,
) -> bool {
    root_surface.exists(state)
        && state.root_surface_workspace(root_surface) == Some(workspace_id)
        && state.root_surface_is_mapped(root_surface)
        && workspace_root_participates_in_input(state, root_surface, workspace_id)
}

fn workspace_root_participates_in_input(
    state: &State,
    root_surface: Resolved<WlSurfaceId>,
    workspace_id: crate::state::WorkspaceId,
) -> bool {
    let scene = state.build_scene_model();
    for output_id in state.workspace_output_ids(workspace_id) {
        if state.output_bounds(output_id).is_none() {
            continue;
        }
        let view = compose_output(state, &scene, output_id, None);
        if view
            .entries
            .iter()
            .any(|entry| entry.root_surface == root_surface && entry.participates_in_input)
        {
            return true;
        }
    }
    false
}

pub(crate) fn refresh_active_drag_focus(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    let _ = refresh_drag_focus(state, dispatcher, time, true);
}

pub(crate) fn maybe_dismiss_popup_grab_on_button(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    button_state: u32,
    time: u32,
) -> bool {
    if button_state != 1 {
        return false;
    }
    let Some(_grab_client_id) = state.seat.popup_grab_client() else {
        return false;
    };
    let inside_popup_stack = state
        .seat
        .pointer_focus
        .is_some_and(|focus| state.seat.popup_grab_contains_surface(&focus.target));
    if inside_popup_stack {
        return false;
    }
    let Some((client_id, popups)) = state.seat.dismiss_popup_grab() else {
        return false;
    };
    dispatcher.dispatch_to(client_id, ClientAction::DismissPopupGrab { popups });
    refresh_pointer_focus(state, dispatcher, time, false);
    set_keyboard_focus(state, dispatcher, state.seat.pointer_focus.map(|focus| focus.target));
    true
}

pub(crate) fn refresh_pointer_focus(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    send_motion: bool,
) {
    let old_focus = state.seat.pointer_focus;
    let scene_pick = scene_pick_for_pointer(state);
    let mut new_focus = scene_pick.surface_focus;
    if let Some(grab_client_id) = state.seat.popup_grab_client() {
        if new_focus.is_some_and(|focus| focus.target.client_id() != grab_client_id) {
            new_focus = None;
        }
    }

    crate::input::pointer_lock::release_pointer_lock_unless_surface(
        state,
        dispatcher,
        new_focus.map(|focus| focus.target.surface),
    );
    crate::input::pointer_confine::release_pointer_confine_unless_surface(
        state,
        dispatcher,
        new_focus.map(|focus| focus.target.surface),
    );
    let send_motion = send_motion && state.seat.active_pointer_lock.is_none();

    if old_focus.map(|focus| focus.target) != new_focus.map(|focus| focus.target) {
        log::debug!(
            "seat pointer focus change pos=({:.1}, {:.1}) old={:?} new={:?}",
            state.seat.pointer_pos.0,
            state.seat.pointer_pos.1,
            old_focus.map(|focus| focus.target),
            new_focus.map(|focus| focus.target)
        );
    }

    if old_focus.map(|focus| focus.target) != new_focus.map(|focus| focus.target) {
        let serial = state.seat.next_serial();

        if let Some(old_focus) = old_focus {
            dispatch_client_event(
                dispatcher,
                old_focus.target.client_id(),
                SeatClientEvent::PointerLeave { serial, surface: old_focus.target.surface },
            );
            dispatch_client_event(
                dispatcher,
                old_focus.target.client_id(),
                SeatClientEvent::PointerFrame,
            );
        }

        if let Some(new_focus) = new_focus {
            let (x, y) = local_coords(
                state,
                new_focus.target.surface,
                new_focus.rect,
                state.seat.pointer_pos,
            );
            state
                .seat
                .record_pointer_enter(new_focus.target.client_id(), serial);
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerEnter { serial, surface: new_focus.target.surface, x, y },
            );
            if send_motion {
                dispatch_client_event(
                    dispatcher,
                    new_focus.target.client_id(),
                    SeatClientEvent::PointerMotion { time, x, y },
                );
            }
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerFrame,
            );
        } else {
            state.seat.clear_pointer_enter();
        }
    } else if send_motion {
        if let Some(new_focus) = new_focus {
            let (x, y) = local_coords(
                state,
                new_focus.target.surface,
                new_focus.rect,
                state.seat.pointer_pos,
            );
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerMotion { time, x, y },
            );
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerFrame,
            );
        }
    } else if let (Some(old_focus), Some(new_focus)) = (old_focus, new_focus) {
        let old_local =
            local_coords(state, old_focus.target.surface, old_focus.rect, state.seat.pointer_pos);
        let new_local =
            local_coords(state, new_focus.target.surface, new_focus.rect, state.seat.pointer_pos);
        if old_local != new_local {
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerWarp { time, x: new_local.0, y: new_local.1 },
            );
            dispatch_client_event(
                dispatcher,
                new_focus.target.client_id(),
                SeatClientEvent::PointerFrame,
            );
        }
    }

    state.seat.pointer_focus = new_focus;
    crate::input::pointer_lock::maybe_activate_pointer_lock(state, dispatcher);
    crate::input::pointer_confine::maybe_activate_pointer_confine(state, dispatcher);
}

pub(crate) fn refresh_drag_focus(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    send_motion: bool,
) -> bool {
    let Some(active_drag) = state.seat.active_drag() else {
        return false;
    };
    if active_drag.phase != DragPhase::Dragging {
        return true;
    }

    let hit = surface_focus_pick_at_excluding(
        state,
        state.seat.pointer_position(),
        crate::input::toplevel_drag::dragged_root(state),
    );
    let mut new_target = hit.map(|focus| DragTargetState { surface: focus.target.surface });
    if active_drag.source.object_id().is_none()
        && new_target
            .is_some_and(|target| target.client_id() != active_drag.source.initiator_client_id())
    {
        new_target = None;
    }

    let old_target = active_drag.target;
    if old_target != new_target {
        if let Some(old_target) = old_target {
            dispatcher.dispatch_to(old_target.client_id(), ClientAction::DataDeviceDragLeave);
        }
        if let Some(source) = active_drag.source.resolved() {
            dispatcher.dispatch_to(
                source.client_id(),
                ClientAction::DataSourceEvent {
                    source,
                    event: crate::clients::DataSourceClientEvent::Target(None),
                },
            );
            dispatcher.dispatch_to(
                source.client_id(),
                ClientAction::DataSourceEvent {
                    source,
                    event: crate::clients::DataSourceClientEvent::Action { dnd_action: 0 },
                },
            );
        }

        if let (Some(hit), Some(new_target)) = (hit, new_target) {
            let serial = state.seat.next_serial();
            let (x, y) = local_coords(state, hit.target.surface, hit.rect, state.seat.pointer_pos);
            dispatcher.dispatch_to(
                new_target.client_id(),
                ClientAction::DataDeviceDragEnter {
                    serial,
                    surface: new_target.surface,
                    x,
                    y,
                    source: active_drag.source.resolved(),
                },
            );
            if send_motion {
                dispatcher.dispatch_to(
                    new_target.client_id(),
                    ClientAction::DataDeviceDragMotion { time, x, y },
                );
            }
        }

        state.seat.set_active_drag_target(new_target);
        return true;
    }

    if send_motion {
        if let (Some(hit), Some(target)) = (hit, new_target) {
            let (x, y) = local_coords(state, hit.target.surface, hit.rect, state.seat.pointer_pos);
            dispatcher
                .dispatch_to(target.client_id(), ClientAction::DataDeviceDragMotion { time, x, y });
        }
    }

    true
}

pub(crate) fn popup_keyboard_focus(state: &State) -> Option<SurfaceFocus> {
    Some(SurfaceFocus::new(state.seat.popup_keyboard_surface()?))
}

fn local_coords(
    state: &State,
    surface: crate::clients::Resolved<crate::protocol_generated::WlSurfaceId>,
    rect: SurfaceRect,
    pointer_pos: (f64, f64),
) -> (f32, f32) {
    let pointer_pos = state.logical_pointer_for_surface(surface, pointer_pos);
    ((pointer_pos.0 - rect.x as f64) as f32, (pointer_pos.1 - rect.y as f64) as f32)
}

fn refresh_selection_focus_clients(
    dispatcher: &mut Dispatcher,
    old_focus: Option<SurfaceFocus>,
    new_focus: Option<SurfaceFocus>,
) {
    let old_client = old_focus.map(|surface| surface.client_id());
    let new_client = new_focus.map(|surface| surface.client_id());
    if old_client == new_client {
        return;
    }
    if let Some(client_id) = old_client {
        dispatcher.dispatch_to(client_id, ClientAction::RefreshDataDeviceSelection);
        dispatcher.dispatch_to(client_id, ClientAction::RefreshPrimarySelection);
    }
    if let Some(client_id) = new_client {
        dispatcher.dispatch_to(client_id, ClientAction::RefreshDataDeviceSelection);
        dispatcher.dispatch_to(client_id, ClientAction::RefreshPrimarySelection);
    }
}

fn normalize_keyboard_focus(
    state: &State,
    requested_focus: Option<SurfaceFocus>,
) -> Option<SurfaceFocus> {
    normalize_keyboard_focus_with_snapshot(state, requested_focus, None)
}

fn normalize_keyboard_focus_with_snapshot(
    state: &State,
    requested_focus: Option<SurfaceFocus>,
    snapshot: Option<LayerLayoutObjectSnapshot<'_>>,
) -> Option<SurfaceFocus> {
    if state.session_lock.input_gated() {
        return lock_keyboard_focus(state, requested_focus);
    }
    let requested_focus = requested_focus.and_then(|focus| {
        let root_surface = root_surface_for_focus(state, focus)?;
        match layer_surface_keyboard_focus_with_snapshot(state, root_surface, snapshot) {
            Some(LayerKeyboardFocus::None) => None,
            _ => Some(focus),
        }
    });

    if let Some(popup_focus) = popup_keyboard_focus(state) {
        if requested_focus == Some(popup_focus) {
            return requested_focus;
        }
    }

    topmost_exclusive_layer_focus_with_snapshot(state, snapshot).or(requested_focus)
}

fn topmost_exclusive_layer_focus_with_snapshot(
    state: &State,
    snapshot: Option<LayerLayoutObjectSnapshot<'_>>,
) -> Option<SurfaceFocus> {
    let scene = state.build_scene_model();
    let mut top = Vec::new();
    let mut overlay = Vec::new();
    for output_id in state.outputs.iter().map(|(output_id, _)| output_id) {
        if state.output_bounds(output_id).is_none() {
            continue;
        }
        let view = compose_output(state, &scene, output_id, snapshot);
        for entry in view.entries {
            let expected_layer = match entry.tier {
                CompositionTier::Top => LayerShellLayer::Top,
                CompositionTier::Overlay => LayerShellLayer::Overlay,
                CompositionTier::Background
                | CompositionTier::Bottom
                | CompositionTier::Content => {
                    continue;
                }
            };
            let Some(LayerKeyboardFocus::Exclusive { layer }) =
                layer_surface_keyboard_focus_with_snapshot(state, entry.root_surface, snapshot)
            else {
                continue;
            };
            if layer != expected_layer {
                continue;
            }
            match layer {
                LayerShellLayer::Top => top.push(entry.root_surface),
                LayerShellLayer::Overlay => overlay.push(entry.root_surface),
                LayerShellLayer::Background | LayerShellLayer::Bottom => {}
            }
        }
    }

    top.into_iter()
        .chain(overlay)
        .rev()
        .next()
        .map(SurfaceFocus::new)
}

fn lock_keyboard_focus(
    state: &State,
    requested_focus: Option<SurfaceFocus>,
) -> Option<SurfaceFocus> {
    if let Some(focus) = requested_focus.filter(|focus| mapped_lock_content(state, focus.surface)) {
        return Some(focus);
    }
    state
        .seat
        .keyboard_focus()
        .filter(|focus| mapped_lock_content(state, focus.surface))
        .or_else(|| first_mapped_lock_focus(state))
}

fn mapped_lock_content(state: &State, mut surface: Resolved<WlSurfaceId>) -> bool {
    loop {
        let parent = {
            let Some(wl_surface) = surface.try_ref(state) else {
                return false;
            };
            if wl_surface.has_lock_surface_role() {
                let Some(lock_surface) = wl_surface.lock_surface() else {
                    return false;
                };
                let Some(output_id) = lock_surface
                    .try_ref(state)
                    .map(|lock_surface| lock_surface.output_id())
                else {
                    return false;
                };
                return state.session_lock.shows_lock_surface(output_id);
            }
            wl_surface.parent_surface().copied()
        };
        match parent {
            Some(parent) => surface = parent,
            None => return false,
        }
    }
}

fn first_mapped_lock_focus(state: &State) -> Option<SurfaceFocus> {
    state
        .session_lock
        .covered_outputs()
        .into_iter()
        .find_map(|output_id| {
            if !state.session_lock.shows_lock_surface(output_id) {
                return None;
            }
            let lock_surface = state.session_lock.surface_for_output(output_id)?;
            Some(SurfaceFocus::new(lock_surface.try_ref(state)?.wl_surface()))
        })
}

fn root_surface_for_focus(state: &State, focus: SurfaceFocus) -> Option<Resolved<WlSurfaceId>> {
    focus.exists(state).then_some(focus.surface)
}

fn monotonic_time_msec() -> u32 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    let millis = (ts.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add((ts.tv_nsec as u64) / 1_000_000);
    millis.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::ClientId,
        protocol_generated::WlSurfaceId,
        state::{BackendOutputRef, DrmId},
        test_support::{state_for_tests, test_output_with_crtc_name},
        wayland_objects::wl_surface::WlSurface,
    };

    use {
        super::{
            focus_fullscreen_window, keep_or_restore_keyboard_focus, refocus_after_surface_destroy,
            refresh_pointer_focus_only, refresh_scene_focus, set_keyboard_focus,
            topmost_focus_candidate_for_workspace, ClosedWindowFocus,
        },
        crate::{input::SurfaceFocus, runner::Dispatcher},
    };

    fn insert_root(
        state: &mut crate::state::State,
        raw: u32,
    ) -> crate::clients::Resolved<WlSurfaceId> {
        let (_, oid) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(raw), ClientId::from_raw(1), 6));
        let surface =
            crate::clients::Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(raw), oid);
        state.register_root_surface(surface);
        crate::test_support::insert_window(state, surface);
        surface
    }

    #[test]
    fn topmost_focus_candidate_uses_managed_workspace_order() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let lower = insert_root(&mut state, 81);
        let upper = insert_root(&mut state, 82);
        let _ = state.assign_root_surface_to_workspace(lower, workspace_id);
        let _ = state.assign_root_surface_to_workspace(upper, workspace_id);
        let lower_window = state.window_for_root(lower).unwrap();
        let upper_window = state.window_for_root(upper).unwrap();
        state
            .workspaces
            .try_get_mut(workspace_id)
            .unwrap()
            .layout
            .floating
            .stack = vec![lower_window, upper_window];
        state.managed_window_mut(lower).unwrap().mapped = true;
        state.managed_window_mut(upper).unwrap().mapped = true;

        let candidate = topmost_focus_candidate_for_workspace(&state, workspace_id, None)
            .expect("focus candidate");
        assert_eq!(candidate.surface, upper);
    }

    fn place_window(
        state: &mut crate::state::State,
        raw: u32,
        workspace_id: crate::state::WorkspaceId,
        x: i32,
        y: i32,
    ) -> crate::clients::Resolved<WlSurfaceId> {
        let surface = insert_root(state, raw);
        state
            .objects
            .get_mut(surface.object_id())
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(crate::state::ObjectId::from(raw as usize), x, y, 30, 30);
        let _ = state.assign_root_surface_to_workspace(surface, workspace_id);
        state.managed_window_mut(surface).unwrap().mapped = true;
        surface
    }

    struct ThreeWindows {
        workspace_id: crate::state::WorkspaceId,
        older: crate::clients::Resolved<WlSurfaceId>,
        under_pointer: crate::clients::Resolved<WlSurfaceId>,
        closing: crate::clients::Resolved<WlSurfaceId>,
    }

    fn three_windows(state: &mut crate::state::State) -> ThreeWindows {
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let older = place_window(state, 81, workspace_id, 0, 0);
        let under_pointer = place_window(state, 82, workspace_id, 40, 0);
        let closing = place_window(state, 83, workspace_id, 0, 50);
        let older_window = state.window_for_root(older).unwrap();
        let under_pointer_window = state.window_for_root(under_pointer).unwrap();
        let closing_window = state.window_for_root(closing).unwrap();
        state
            .workspaces
            .try_get_mut(workspace_id)
            .unwrap()
            .layout
            .floating
            .stack = vec![older_window, under_pointer_window, closing_window];
        ThreeWindows { workspace_id, older, under_pointer, closing }
    }

    fn close_of(windows: &ThreeWindows) -> ClosedWindowFocus {
        ClosedWindowFocus { closed: windows.closing, workspace_id: windows.workspace_id }
    }

    #[test]
    fn closing_focused_window_prefers_the_window_under_the_pointer() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.closing)));
        state.seat.pointer_pos = (50.0, 10.0);

        refocus_after_surface_destroy(&mut state, &mut dispatcher, Some(close_of(&windows)), None);

        assert_eq!(
            state.seat.keyboard_focus().map(|focus| focus.surface),
            Some(windows.under_pointer)
        );
    }

    #[test]
    fn closing_focused_window_uses_history_when_the_pointer_misses_every_window() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.closing)));
        state.seat.pointer_pos = (90.0, 90.0);

        refocus_after_surface_destroy(&mut state, &mut dispatcher, Some(close_of(&windows)), None);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(windows.older));
    }

    #[test]
    fn closing_focused_window_uses_the_topmost_window_when_history_has_no_other() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.closing)));
        state.seat.pointer_pos = (90.0, 90.0);

        refocus_after_surface_destroy(&mut state, &mut dispatcher, Some(close_of(&windows)), None);

        assert_eq!(
            state.seat.keyboard_focus().map(|focus| focus.surface),
            Some(windows.under_pointer)
        );
    }

    #[test]
    fn focusing_an_urgent_window_clears_it_and_republishes() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let surface = insert_root(&mut state, 90);
        let window = state.window_for_root(surface).unwrap();

        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(surface)));
        assert_eq!(workspace_state_changed(&dispatcher), 0);

        set_keyboard_focus(&mut state, &mut dispatcher, None);
        assert!(state.set_window_urgent(window, true));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(surface)));

        assert!(!state.managed_window_state(window).unwrap().urgent);
        assert!(!state.workspace_is_urgent(workspace_id));
        assert_eq!(workspace_state_changed(&dispatcher), 1);
    }

    #[test]
    fn focus_fullscreen_window_ignores_a_non_fullscreen_window() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        let window_b = state.window_for_root(windows.under_pointer).unwrap();
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));

        focus_fullscreen_window(&mut state, &mut dispatcher, window_b);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(windows.older));
    }

    /// The `focus.rs` fixtures have no xdg toplevel, so `set_window_fullscreen` cannot sync the
    /// layout. `focus_fullscreen_window` only reads `layout.fullscreen`, so set it directly.
    fn mark_fullscreen(
        state: &mut crate::state::State,
        workspace_id: crate::state::WorkspaceId,
        window: crate::window::WindowId,
    ) {
        state
            .workspaces
            .try_get_mut(workspace_id)
            .unwrap()
            .layout
            .set_fullscreen(Some(window));
        assert!(state.window_is_fullscreen(window));
    }

    #[test]
    fn fullscreen_window_takes_keyboard_focus() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        let window_b = state.window_for_root(windows.under_pointer).unwrap();
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        mark_fullscreen(&mut state, windows.workspace_id, window_b);

        focus_fullscreen_window(&mut state, &mut dispatcher, window_b);

        assert_eq!(
            state.seat.keyboard_focus().map(|focus| focus.surface),
            Some(windows.under_pointer)
        );
    }

    #[test]
    fn focus_fullscreen_window_keeps_focus_already_on_the_window() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        let window_b = state.window_for_root(windows.under_pointer).unwrap();
        set_keyboard_focus(
            &mut state,
            &mut dispatcher,
            Some(SurfaceFocus::new(windows.under_pointer)),
        );
        mark_fullscreen(&mut state, windows.workspace_id, window_b);

        focus_fullscreen_window(&mut state, &mut dispatcher, window_b);

        assert_eq!(
            state.seat.keyboard_focus().map(|focus| focus.surface),
            Some(windows.under_pointer)
        );
    }

    #[test]
    fn pointer_only_refresh_keeps_keyboard_focus() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        state.seat.pointer_pos = (90.0, 90.0);

        refresh_pointer_focus_only(&mut state, &mut dispatcher);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(windows.older));

        // The old behaviour: the same pointer position drops keyboard focus.
        refresh_scene_focus(&mut state, &mut dispatcher);
        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), None);
    }

    /// Like `place_window`, but the root belongs to `client_id`, so two roots can
    /// belong to different clients.
    fn place_window_for_client(
        state: &mut crate::state::State,
        raw: u32,
        client_id: ClientId,
        workspace_id: crate::state::WorkspaceId,
        x: i32,
        y: i32,
    ) -> crate::clients::Resolved<WlSurfaceId> {
        let (_, oid) = state.insert_object(WlSurface::new(WlSurfaceId::from(raw), client_id, 6));
        let surface = crate::clients::Resolved::new(client_id, WlSurfaceId::from(raw), oid);
        state.register_root_surface(surface);
        crate::test_support::insert_window(state, surface);
        state
            .objects
            .get_mut(surface.object_id())
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(crate::state::ObjectId::from(raw as usize), x, y, 30, 30);
        let _ = state.assign_root_surface_to_workspace(surface, workspace_id);
        state.managed_window_mut(surface).unwrap().mapped = true;
        surface
    }

    fn reconfigure_roots_queued_for(
        dispatcher: &Dispatcher,
        client_id: ClientId,
        root: crate::clients::Resolved<WlSurfaceId>,
    ) -> bool {
        dispatcher.test_has_queued_action_for::<crate::clients::ClientAction, ClientId>(
            client_id,
            |action| {
                matches!(
                    action,
                    crate::clients::ClientAction::ReconfigureToplevelsForRoots { roots }
                        if roots.as_slice() == [root]
                )
            },
        )
    }

    #[test]
    fn keyboard_focus_change_reconfigures_old_and_new_roots() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let client_a = ClientId::from_raw(1);
        let client_b = ClientId::from_raw(2);
        let root_a = place_window_for_client(&mut state, 91, client_a, workspace_id, 0, 0);
        let root_b = place_window_for_client(&mut state, 92, client_b, workspace_id, 40, 0);
        let mut dispatcher = Dispatcher::default();
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(root_a)));
        dispatcher = Dispatcher::default();

        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(root_b)));

        assert!(reconfigure_roots_queued_for(&dispatcher, client_a, root_a));
        assert!(reconfigure_roots_queued_for(&dispatcher, client_b, root_b));
    }

    #[test]
    fn unchanged_keyboard_focus_queues_no_reconfigure() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        dispatcher = Dispatcher::default();

        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));

        assert!(!dispatcher.test_has_queued_action::<crate::clients::ClientAction>(|action| {
            matches!(action, crate::clients::ClientAction::ReconfigureToplevelsForRoots { .. })
        }));
    }

    /// Two workspaces in one group on one output; the first one is active.
    fn two_workspaces(
        state: &mut crate::state::State,
    ) -> (crate::state::WorkspaceId, crate::state::WorkspaceId) {
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let group_id = state.workspace_group_for_output(output_id).unwrap();
        let first = state.active_workspace_for_output(output_id).unwrap();
        let second = state.create_workspace(Some(group_id), Some("2".to_string()));
        (first, second)
    }

    #[test]
    fn workspace_switch_restores_the_target_workspace_history() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (first, second) = two_workspaces(&mut state);
        let window_a = place_window(&mut state, 101, first, 0, 0);
        let window_b = place_window(&mut state, 102, second, 0, 0);
        let window_c = place_window(&mut state, 103, second, 40, 0);
        assert!(state.activate_workspace(second));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(window_b)));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(window_c)));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(window_b)));
        assert!(state.activate_workspace(first));
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(window_a)));
        assert!(state.activate_workspace(second));
        state.seat.pointer_pos = (50.0, 10.0);
        refresh_pointer_focus_only(&mut state, &mut dispatcher);

        keep_or_restore_keyboard_focus(&mut state, &mut dispatcher);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(window_b));
    }

    #[test]
    fn workspace_switch_uses_topmost_without_history() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (first, second) = two_workspaces(&mut state);
        let window_a = place_window(&mut state, 111, first, 0, 0);
        let window_b = place_window(&mut state, 112, second, 0, 0);
        let window_c = place_window(&mut state, 113, second, 40, 0);
        let window_b_handle = state.window_for_root(window_b).unwrap();
        let window_c_handle = state.window_for_root(window_c).unwrap();
        state
            .workspaces
            .try_get_mut(second)
            .unwrap()
            .layout
            .floating
            .stack = vec![window_b_handle, window_c_handle];
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(window_a)));
        assert!(state.activate_workspace(second));
        state.seat.pointer_pos = (90.0, 90.0);
        refresh_pointer_focus_only(&mut state, &mut dispatcher);

        keep_or_restore_keyboard_focus(&mut state, &mut dispatcher);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(window_c));
    }

    #[test]
    fn usable_focus_is_kept() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        state.seat.pointer_pos = (50.0, 10.0);
        refresh_pointer_focus_only(&mut state, &mut dispatcher);

        keep_or_restore_keyboard_focus(&mut state, &mut dispatcher);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(windows.older));
    }

    #[test]
    fn focus_on_an_unmanaged_x_root_is_kept() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let windows = three_windows(&mut state);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(windows.older)));
        let (_, oid) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(95), ClientId::from_raw(2), 6));
        let menu = crate::clients::Resolved::new(ClientId::from_raw(2), WlSurfaceId::from(95), oid);
        state.register_root_surface(menu);
        {
            let surface = state
                .objects
                .get_mut(oid)
                .downcast_mut::<WlSurface>()
                .unwrap();
            assert!(surface.assign_xwayland_role());
            surface.test_map(crate::state::ObjectId::from(95usize), 60, 60, 20, 20);
        }
        assert!(state.window_for_root(menu).is_none());
        state.seat.keyboard_focus = Some(SurfaceFocus::new(menu));

        keep_or_restore_keyboard_focus(&mut state, &mut dispatcher);

        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(menu));
    }

    fn workspace_state_changed(dispatcher: &Dispatcher) -> usize {
        dispatcher.test_count_queued_action::<crate::clients::ClientManagerAction>(|action| {
            matches!(action, crate::clients::ClientManagerAction::WorkspaceStateChanged)
        })
    }
}

fn dispatch_modifiers(
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    serial: u32,
    modifiers: crate::xkb::KeyboardModifiers,
) {
    dispatch_client_event(
        dispatcher,
        client_id,
        SeatClientEvent::KeyboardModifiers {
            serial,
            mods_depressed: modifiers.mods_depressed,
            mods_latched: modifiers.mods_latched,
            mods_locked: modifiers.mods_locked,
            group: modifiers.group,
        },
    );
}
