use crate::{
    clients::Resolved,
    config,
    protocol_generated::{WlSurfaceId, XdgToplevelId},
    runner::Dispatcher,
    state::State,
    wayland_objects::xdg_surface::xdg_toplevel::InteractiveSizeConstraints,
};

use super::{
    apply_interaction_intent, build_protocol_move_target, build_protocol_resize_target,
    popup_keyboard_focus, rect_contains_point, resize_cursor_icon_names, resize_edges_for_pointer,
    scene_pick_at, set_keyboard_focus, CompositorZone, InteractionIntent, SurfaceFocus,
    SurfaceFocusPick, WindowHit, WindowRoleSnapshot, WindowTarget,
    POINTER_INTERACTION_START_OPTIONS, PROTOCOL_INTERACTION_START_OPTIONS,
};

pub(crate) fn window_allows_compositor_interaction(target: &WindowTarget) -> bool {
    !matches!(
        target.role,
        // Comment so we can move gtk windows
        // WindowRoleSnapshot::Toplevel { is_maximized: true, .. }
        WindowRoleSnapshot::Toplevel { is_fullscreen: true, .. } | WindowRoleSnapshot::Unsupported
    )
}

pub(crate) fn begin_pointer_binding(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    action: config::MouseBindingAction,
) -> bool {
    let Some(hit) = window_hit_at(state, state.seat.pointer_position()) else {
        return false;
    };
    let Some(intent) = interaction_intent_for_pointer_binding(action, &hit) else {
        return false;
    };
    apply_interaction_intent(state, dispatcher, intent, POINTER_INTERACTION_START_OPTIONS)
}

/// Keyboard focus for a pointer press.
///
/// A layer surface with `keyboard_interactivity = none`, an unmanaged X window, or a
/// window that refuses keyboard focus produces [`InteractionIntent::None`]: the press
/// is delivered, and the current keyboard focus stays. Other hits produce
/// [`InteractionIntent::FocusSurface`]. A press with no target clears keyboard focus.
pub(crate) fn apply_pointer_press_keyboard_focus(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    pointer_focus: Option<SurfaceFocusPick>,
) {
    if !state.session_lock.input_gated() {
        // Focusing the popup sends `wl_keyboard.leave` to the parent. Firefox closes its menu on that leave.
        if let Some(focus) = popup_keyboard_focus(state) {
            apply_interaction_intent(
                state,
                dispatcher,
                InteractionIntent::FocusSurface(focus),
                POINTER_INTERACTION_START_OPTIONS,
            );
            return;
        }
    }

    let Some(pointer_focus) = pointer_focus else {
        set_keyboard_focus(state, dispatcher, None);
        return;
    };
    let focus = state
        .root_surface_for_surface(pointer_focus.target.surface)
        .map(SurfaceFocus::new)
        .unwrap_or(pointer_focus.target);
    let layer_keeps_focus =
        crate::wayland_objects::zwlr_layer_surface_v1::layer_surface_keyboard_focus(
            state,
            focus.surface,
        ) == Some(crate::wayland_objects::zwlr_layer_surface_v1::LayerKeyboardFocus::None);
    let intent = if layer_keeps_focus || press_keeps_keyboard_focus(state, focus) {
        InteractionIntent::None
    } else {
        InteractionIntent::FocusSurface(focus)
    };
    apply_interaction_intent(state, dispatcher, intent, POINTER_INTERACTION_START_OPTIONS);
}

/// An unmanaged X root keeps the current focus when that focus already belongs to an X
/// surface. A window that refuses keyboard focus keeps it too.
fn press_keeps_keyboard_focus(state: &State, root: SurfaceFocus) -> bool {
    let Some(surface) = root.surface.try_ref(state) else {
        return false;
    };
    if surface.has_xwayland_role() && state.window_for_root(root.surface).is_none() {
        return keyboard_focus_root_has_xwayland_role(state);
    }
    let Some(window) = state.window_for_root(root.surface) else {
        return false;
    };
    state
        .toplevel(window)
        .is_some_and(|toplevel| !toplevel.accepts_keyboard_focus())
}

fn keyboard_focus_root_has_xwayland_role(state: &State) -> bool {
    let Some(focus) = state.seat.keyboard_focus else {
        return false;
    };
    let root = state
        .root_surface_for_surface(focus.surface)
        .unwrap_or(focus.surface);
    root.try_ref(state)
        .is_some_and(|surface| surface.has_xwayland_role())
}

pub(crate) fn begin_decoration_pointer_action(
    state: &mut State,
    dispatcher: &mut Dispatcher,
) -> bool {
    let pointer_pos = state.seat.pointer_position();
    let Some(intent) = window_interaction_intent_at(state, pointer_pos) else {
        return false;
    };
    apply_interaction_intent(state, dispatcher, intent, POINTER_INTERACTION_START_OPTIONS)
}

pub(crate) fn window_hit_at(state: &State, pointer_pos: (f64, f64)) -> Option<WindowHit> {
    let pick = scene_pick_at(state, pointer_pos);
    let target = pick.window_target?;
    let pointer_pos = state.logical_pointer_for_surface(target.root_surface, pointer_pos);
    let binding_resize_edges = rect_contains_point(target.geometry.binding_rect, pointer_pos)
        .then(|| resize_edges_for_pointer(target.geometry.binding_rect, pointer_pos))
        .unwrap_or(0);
    Some(WindowHit { target, zone: pick.zone, binding_resize_edges })
}

pub(crate) fn decoration_hover_cursor_icon_names(state: &State) -> Option<&'static [&'static str]> {
    // A gap has a zone and no window. Mod+drag still goes through `window_hit_at`.
    match scene_pick_at(state, state.seat.pointer_position()).zone? {
        CompositorZone::WindowResize { edges, .. } => Some(resize_cursor_icon_names(edges)),
        CompositorZone::SplitResize { cursor_edges, .. } => {
            Some(resize_cursor_icon_names(cursor_edges))
        }
    }
}

pub(crate) fn request_toplevel_move(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: crate::clients::ClientId,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
    root_surface: Resolved<WlSurfaceId>,
    xdg_toplevel: Resolved<XdgToplevelId>,
    configure_decoration_mode: crate::wayland_objects::xdg_surface::xdg_toplevel::ToplevelDecorationMode,
    is_maximized: bool,
    is_fullscreen: bool,
) -> bool {
    if state.session_lock.input_gated() {
        return false;
    }
    if !state
        .seat
        .validate_implicit_grab_serial(client_id, surface, serial)
    {
        return false;
    }
    let Some(window) = state.window_for_root(root_surface) else {
        return false;
    };
    if state.window_role_object(window) != Some(xdg_toplevel.object_id()) {
        return false;
    }
    let Some(target) = build_protocol_move_target(
        state,
        root_surface,
        WindowRoleSnapshot::Toplevel {
            window,
            size_constraints: InteractiveSizeConstraints::default(),
            is_maximized,
            is_fullscreen,
            decoration_mode: configure_decoration_mode,
        },
    ) else {
        return false;
    };
    if target.client_id() != client_id || target.surface_id() != surface.wayland_id() {
        return false;
    }
    apply_interaction_intent(
        state,
        dispatcher,
        InteractionIntent::StartMove(target),
        PROTOCOL_INTERACTION_START_OPTIONS,
    )
}

pub(crate) fn request_toplevel_resize(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: crate::clients::ClientId,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
    root_surface: Resolved<WlSurfaceId>,
    xdg_toplevel: Resolved<XdgToplevelId>,
    configure_decoration_mode: crate::wayland_objects::xdg_surface::xdg_toplevel::ToplevelDecorationMode,
    size_constraints: InteractiveSizeConstraints,
    is_maximized: bool,
    is_fullscreen: bool,
    edges: u32,
) -> bool {
    if state.session_lock.input_gated() {
        return false;
    }
    if !state
        .seat
        .validate_implicit_grab_serial(client_id, surface, serial)
    {
        return false;
    }
    let Some(window) = state.window_for_root(root_surface) else {
        return false;
    };
    if state.window_role_object(window) != Some(xdg_toplevel.object_id()) {
        return false;
    }
    let Some(target) = build_protocol_resize_target(
        state,
        root_surface,
        WindowRoleSnapshot::Toplevel {
            window,
            size_constraints,
            is_maximized,
            is_fullscreen,
            decoration_mode: configure_decoration_mode,
        },
    ) else {
        return false;
    };
    if target.client_id() != client_id || target.surface_id() != surface.wayland_id() {
        return false;
    }
    apply_interaction_intent(
        state,
        dispatcher,
        InteractionIntent::StartResize(target, edges),
        PROTOCOL_INTERACTION_START_OPTIONS,
    )
}

fn interaction_intent_for_pointer_binding(
    action: config::MouseBindingAction,
    hit: &WindowHit,
) -> Option<InteractionIntent> {
    if !window_allows_compositor_interaction(&hit.target) {
        return None;
    }
    match action {
        config::MouseBindingAction::MoveWindow => Some(InteractionIntent::StartMove(hit.target)),
        config::MouseBindingAction::ResizeWindow => (hit.binding_resize_edges != 0)
            .then_some(InteractionIntent::StartResize(hit.target, hit.binding_resize_edges)),
    }
}

fn window_interaction_intent_at(
    state: &State,
    pointer_pos: (f64, f64),
) -> Option<InteractionIntent> {
    let pick = scene_pick_at(state, pointer_pos);
    match pick.zone? {
        CompositorZone::WindowResize { target, edges } => {
            Some(InteractionIntent::StartResize(target, edges))
        }
        CompositorZone::SplitResize { workspace, splits, cursor_edges } => {
            Some(InteractionIntent::StartSplitResize {
                workspace,
                splits,
                cursor_edges,
                origin: pick.window_target,
            })
        }
    }
}
