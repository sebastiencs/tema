use std::collections::BTreeSet;

use crate::{
    clients::{ClientAction, ClientId, ClientManagerAction, Resolved},
    cursor::CursorSourceRef,
    drm::{request_active_output, request_outputs_for_points, request_outputs_for_rects},
    protocol_generated::{xdg_toplevel::ResizeEdge, WlSurfaceId},
    region::RectI,
    root_geometry::{CommittedRootGeometry, WindowTargetGeometry},
    runner::Dispatcher,
    seat::{dispatch_client_action, dispatch_client_event, refresh_pointer_focus, SeatClientEvent},
    state::State,
    tiling::ExtractedTiledLeaf,
    wayland_objects::xdg_surface::xdg_toplevel::{
        InteractiveSizeConstraints, ToplevelDecorationMode,
    },
    window::WindowId,
};

use super::{
    set_keyboard_focus, ActiveGrabState, InteractionIntent, InteractionStartOptions, SurfaceFocus,
    WindowRoleSnapshot, WindowTarget,
};

const POINTER_MOVE_CURSOR_ICON_NAMES: &[&str] = &["grab", "grabbing", "move"];
const POINTER_RESIZE_NW_CURSOR_ICON_NAMES: &[&str] = &["nw-resize", "size_fdiag"];
const POINTER_RESIZE_NE_CURSOR_ICON_NAMES: &[&str] = &["ne-resize", "size_bdiag"];
const POINTER_RESIZE_SW_CURSOR_ICON_NAMES: &[&str] = &["sw-resize", "size_bdiag"];
const POINTER_RESIZE_SE_CURSOR_ICON_NAMES: &[&str] = &["se-resize", "size_fdiag"];
const POINTER_RESIZE_N_CURSOR_ICON_NAMES: &[&str] = &["n-resize", "size_ver"];
const POINTER_RESIZE_S_CURSOR_ICON_NAMES: &[&str] = &["s-resize", "size_ver"];
const POINTER_RESIZE_W_CURSOR_ICON_NAMES: &[&str] = &["w-resize", "size_hor"];
const POINTER_RESIZE_E_CURSOR_ICON_NAMES: &[&str] = &["e-resize", "size_hor"];

/// A tiled window following the pointer as a scaled preview.
///
/// The interactive grab stores one of these. A toplevel drag stores its own and never
/// starts a compositor pointer grab.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TiledDragSession {
    pub window: WindowId,
    pub root_surface: Resolved<WlSurfaceId>,
    pub size_constraints: InteractiveSizeConstraints,
    pub decoration_mode: ToplevelDecorationMode,
    pub origin_pointer: (f64, f64),
    pub threshold: i32,
    pub picked_up: bool,
    pub grab_pointer: (f64, f64),
    pub grab_geometry: WindowTargetGeometry,
    pub preview_size: (i32, i32),
    pub frame_from_root: (i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TiledDragEnd {
    Drop,
    Cancel,
}

pub(crate) fn apply_interaction_intent(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    intent: InteractionIntent,
    options: InteractionStartOptions,
) -> bool {
    match intent {
        InteractionIntent::FocusSurface(focus) => {
            set_keyboard_focus(state, dispatcher, Some(focus));
            true
        }
        InteractionIntent::StartMove(target) | InteractionIntent::StartResize(target, _) => {
            let target = if options.prepare_window {
                prepare_window_for_pointer_grab(state, dispatcher, &target)
            } else {
                target
            };
            if options.prepare_window
                && matches!(intent, InteractionIntent::StartMove(_))
                && state.is_tiled_root_surface(target.root_surface)
            {
                if let WindowRoleSnapshot::Toplevel {
                    window,
                    size_constraints,
                    decoration_mode,
                    ..
                } = target.role
                {
                    return start_tiled_drag(
                        state,
                        dispatcher,
                        window,
                        target.root_surface,
                        size_constraints,
                        decoration_mode,
                        target.geometry,
                    );
                }
            }
            let target = anchored_grab_target(state, dispatcher, target);
            start_window_grab(state, dispatcher, &target, intent)
        }
        InteractionIntent::DismissPopupGrab | InteractionIntent::None => false,
    }
}

pub(crate) fn apply_pointer_interactive_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
) -> bool {
    let Some(grab) = state.seat.pointer_interactive_grab else {
        return false;
    };
    let window = match grab {
        ActiveGrabState::Move { window, .. }
        | ActiveGrabState::Resize { window, .. }
        | ActiveGrabState::ResizeTiled { window, .. } => window,
        ActiveGrabState::MoveTiled(session) => session.window,
    };
    if state.window_root_surface(window).is_none() {
        abort_pointer_interactive_grab(state, dispatcher, 0);
        return true;
    }
    match grab {
        ActiveGrabState::Move { root_surface, initial_pointer_pos, initial_geometry, .. } => {
            move_grabbed_root(
                state,
                dispatcher,
                root_surface,
                initial_pointer_pos,
                initial_geometry.root_pos,
            );
        }
        ActiveGrabState::MoveTiled(mut session) => {
            let mut origin = state.seat.tiled_drag_origin.take();
            match tiled_drag_motion(state, dispatcher, &mut session, &mut origin) {
                Ok(()) => {
                    state.seat.tiled_drag_origin = origin;
                    state.seat.pointer_interactive_grab = Some(ActiveGrabState::MoveTiled(session));
                }
                Err(()) => {
                    state.seat.tiled_drag_origin = origin;
                    state.seat.pointer_interactive_grab = Some(ActiveGrabState::MoveTiled(session));
                    abort_pointer_interactive_grab(state, dispatcher, 0);
                }
            }
        }
        ActiveGrabState::Resize {
            window,
            root_surface,
            size_constraints,
            edges,
            initial_pointer_pos,
            initial_geometry,
            mut last_size,
        } => {
            let dx = (state.seat.pointer_pos.0 - initial_pointer_pos.0).round() as i32;
            let dy = (state.seat.pointer_pos.1 - initial_pointer_pos.1).round() as i32;
            let (mut width, mut height) = initial_geometry.resize_size;
            let (mut x, mut y) = initial_geometry.root_pos;

            if edges & 4 != 0 {
                width = initial_geometry.resize_size.0 - dx;
            }
            if edges & 8 != 0 {
                width = initial_geometry.resize_size.0 + dx;
            }
            if edges & 1 != 0 {
                height = initial_geometry.resize_size.1 - dy;
            }
            if edges & 2 != 0 {
                height = initial_geometry.resize_size.1 + dy;
            }
            (width, height) = size_constraints.clamp(width, height);
            if edges & 4 != 0 {
                x = initial_geometry.root_pos.0 + (initial_geometry.resize_size.0 - width);
            }
            if edges & 1 != 0 {
                y = initial_geometry.root_pos.1 + (initial_geometry.resize_size.1 - height);
            }

            let old_bounds = interactive_damage_rect(state, root_surface);
            let Some(mut surface) = root_surface.try_grab(state) else {
                abort_pointer_interactive_grab(state, dispatcher, 0);
                return true;
            };
            surface.set_root_position(x, y);
            drop(surface);
            state.mark_root_geometry_dirty(root_surface);
            state.sync_window_geometry_goal(
                window,
                crate::visual_animation::WindowGeometryMotion::Warp,
            );
            let new_bounds = interactive_damage_rect(state, root_surface);
            dispatch_client_action(
                dispatcher,
                root_surface.client_id(),
                ClientAction::RefreshSurfaceTreeOutputs { root: root_surface },
            );
            if let Some(new_bounds) = new_bounds {
                let rects = old_bounds.into_iter().chain(std::iter::once(new_bounds));
                request_outputs_for_rects(state, rects, dispatcher);
            } else {
                request_active_output(state, dispatcher);
            }

            if last_size != (width, height) {
                if let Some(toplevel) = state.toplevel(window) {
                    toplevel.interactive_resize(dispatcher, (width, height), true);
                }
                last_size = (width, height);
            }

            state.seat.pointer_interactive_grab = Some(ActiveGrabState::Resize {
                window,
                root_surface,
                size_constraints,
                edges,
                initial_pointer_pos,
                initial_geometry,
                last_size,
            });
        }
        ActiveGrabState::ResizeTiled {
            window,
            root_surface,
            edges,
            initial_pointer_pos,
            resize_state,
        } => {
            let dx = (state.seat.pointer_pos.0 - initial_pointer_pos.0).round() as i32;
            let dy = (state.seat.pointer_pos.1 - initial_pointer_pos.1).round() as i32;
            let changed = state.resize_tiled_window_from_state(window, resize_state, dx, dy);
            state.seat.pointer_interactive_grab = Some(ActiveGrabState::ResizeTiled {
                window,
                root_surface,
                edges,
                initial_pointer_pos,
                resize_state,
            });
            if changed {
                let roots = tiled_resize_active_root_surfaces(state)
                    .into_iter()
                    .collect::<Vec<_>>();
                dispatcher.dispatch(ClientManagerAction::TiledResizeStep { roots });
                request_active_output(state, dispatcher);
            }
        }
    }
    true
}

pub(crate) fn finish_pointer_interactive_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
) {
    let _ = settle_pointer_interactive_grab(state, dispatcher, time, false);
}

pub(crate) fn abort_pointer_interactive_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
) -> bool {
    settle_pointer_interactive_grab(state, dispatcher, time, true)
}

fn settle_pointer_interactive_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    cancel: bool,
) -> bool {
    let Some(grab) = state.seat.pointer_interactive_grab else {
        return false;
    };
    if !cancel {
        if let ActiveGrabState::Resize { window, last_size, .. } = grab {
            if let Some(toplevel) = state.toplevel(window) {
                toplevel.interactive_resize(dispatcher, last_size, false);
            }
        }
    }
    let origin = state.seat.tiled_drag_origin.take();
    let was_tiled_resize = matches!(grab, ActiveGrabState::ResizeTiled { .. });
    state.seat.clear_pointer_interactive_grab();
    crate::session::note_window_changed(state, dispatcher);
    if let ActiveGrabState::MoveTiled(session) = grab {
        let end = if cancel { TiledDragEnd::Cancel } else { TiledDragEnd::Drop };
        tiled_drag_settle(state, dispatcher, session, origin, end);
    }
    if was_tiled_resize {
        dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
    }
    end_compositor_pointer_grab(state, dispatcher, time);
    true
}

/// Insert the preview or put the window back. Does not touch the compositor pointer grab.
pub(crate) fn tiled_drag_settle(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    session: TiledDragSession,
    origin: Option<ExtractedTiledLeaf>,
    end: TiledDragEnd,
) {
    if !session.picked_up {
        return;
    }
    let root_surface = session.root_surface;
    let mapped = root_surface
        .try_ref(state)
        .is_some_and(|surface| surface.has_committed_buffer());
    let cancel = matches!(end, TiledDragEnd::Cancel);
    if mapped && !cancel {
        let placed = place_dragged_window(state, root_surface);
        if !placed {
            restore_tiled_drag_origin(state, root_surface, origin);
        }
    } else if mapped {
        restore_tiled_drag_origin(state, root_surface, origin);
    }
    let retiled = state.retile_root_surface_after_drag(root_surface);
    if retiled {
        dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
    }
    if cancel && mapped && retiled {
        // Cancel, session lock, and session pause put the window back on its tile immediately.
        // A normal drop leaves the windows animation that retile just recorded.
        if let Some(window) = state.window_for_root(root_surface) {
            state.sync_window_geometry_goal(
                window,
                crate::visual_animation::WindowGeometryMotion::Warp,
            );
        }
    }
}

fn place_dragged_window(state: &mut State, surface: Resolved<WlSurfaceId>) -> bool {
    let (pointer_x, pointer_y) = state.seat.pointer_pos;
    let precise_mouse_move = state.config.tiling.drag.precise_mouse_move;
    let split_ratio = state.config.tiling.drag.split_ratio.get();
    let Some(window) = state.window_for_root(surface) else {
        return false;
    };
    let Some(workspace_id) = state.root_surface_workspace(surface) else {
        return false;
    };
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return false;
    };
    workspace.layout.place_dragged_tiled_leaf(
        window,
        pointer_x,
        pointer_y,
        precise_mouse_move,
        split_ratio,
    )
}

fn restore_tiled_drag_origin(
    state: &mut State,
    surface: Resolved<WlSurfaceId>,
    origin: Option<ExtractedTiledLeaf>,
) {
    let Some(origin) = origin else {
        return;
    };
    let Some(workspace_id) = state.root_surface_workspace(surface) else {
        return;
    };
    if let Some(workspace) = state.workspaces.try_get_mut(workspace_id) {
        workspace.layout.restore_extracted_tiled_leaf(origin);
    }
}

pub(crate) fn tiled_drag_tracks_surface(state: &State, surface: Resolved<WlSurfaceId>) -> bool {
    let interactive = matches!(
        state.seat.pointer_interactive_grab,
        Some(ActiveGrabState::MoveTiled(session)) if session.root_surface == surface
    );
    if interactive {
        return true;
    }
    state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|drag| drag.attached.as_ref())
        .is_some_and(|attached| {
            attached
                .tiled
                .is_some_and(|session| session.root_surface == surface)
        })
}

pub(crate) fn tiled_resize_active_root_surfaces(state: &State) -> BTreeSet<Resolved<WlSurfaceId>> {
    let Some(ActiveGrabState::ResizeTiled { root_surface, resize_state, .. }) =
        state.seat.pointer_interactive_grab
    else {
        return BTreeSet::new();
    };
    let Some(window) = state.window_for_root(root_surface) else {
        return BTreeSet::new();
    };
    let Some(workspace_id) = state.root_surface_workspace(root_surface) else {
        return BTreeSet::new();
    };
    let Some(workspace) = state.workspaces.try_get(workspace_id) else {
        return BTreeSet::new();
    };
    workspace
        .layout
        .resize_affected_windows(window, resize_state)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|window| state.window_root_surface(window))
        .collect()
}

pub(crate) fn recover_pointer_after_interactive_grab_loss(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
) {
    end_compositor_pointer_grab(state, dispatcher, time);
}

pub(crate) fn resize_cursor_icon_names(edges: u32) -> &'static [&'static str] {
    match edges {
        e if e == ResizeEdge::Top as u32 => POINTER_RESIZE_N_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::Bottom as u32 => POINTER_RESIZE_S_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::Left as u32 => POINTER_RESIZE_W_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::Right as u32 => POINTER_RESIZE_E_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::TopLeft as u32 => POINTER_RESIZE_NW_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::TopRight as u32 => POINTER_RESIZE_NE_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::BottomLeft as u32 => POINTER_RESIZE_SW_CURSOR_ICON_NAMES,
        e if e == ResizeEdge::BottomRight as u32 => POINTER_RESIZE_SE_CURSOR_ICON_NAMES,
        _ => POINTER_RESIZE_SE_CURSOR_ICON_NAMES,
    }
}

/// Place the logical window so the point under the cursor stays put, then snap the animation.
fn anchored_grab_target(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    mut target: WindowTarget,
) -> WindowTarget {
    if state.is_tiled_root_surface(target.root_surface) {
        return target;
    }
    let pointer = state.seat.pointer_position();
    let logical = state
        .root_geometry_cache
        .get(&target.root_surface)
        .and_then(|entry| entry.get().geometry)
        .map(|geometry| geometry.layout_rect_global)
        .unwrap_or(target.geometry.target_rect);
    let Some(visual) = state
        .window_for_root(target.root_surface)
        .and_then(|window| {
            state
                .managed_window_state(window)
                .and_then(|managed| managed.geometry_animation.visual_rect())
        })
    else {
        return target;
    };
    if visual == logical {
        return target;
    }
    let logical_point = crate::render::unmap_layout_point(logical, visual, pointer);
    let dx = (pointer.0 - logical_point.0).round() as i32;
    let dy = (pointer.1 - logical_point.1).round() as i32;
    if dx != 0 || dy != 0 {
        let Some(mut surface) = target.root_surface.try_grab(state) else {
            return target;
        };
        let position = surface.root_position();
        surface.set_root_position(position.0 + dx, position.1 + dy);
        drop(surface);
        note_root_position(state, dispatcher, target.root_surface);
        state.mark_root_geometry_dirty(target.root_surface);
        target.geometry.root_pos.0 += dx;
        target.geometry.root_pos.1 += dy;
        target.geometry.target_rect.x += dx;
        target.geometry.target_rect.y += dy;
        target.geometry.binding_rect.x += dx;
        target.geometry.binding_rect.y += dy;
    }
    let Some(goal) = RectI::new(logical.x + dx, logical.y + dy, logical.width, logical.height)
    else {
        return target;
    };
    if let Some(managed) = state.managed_window_mut(target.root_surface) {
        managed
            .geometry_animation
            .set_goal(goal, crate::visual_animation::WindowGeometryMotion::Warp);
    }
    target
}

fn prepare_window_for_pointer_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    target: &WindowTarget,
) -> WindowTarget {
    let target = *target;
    state.raise_root_surface(target.root_surface);
    request_outputs_for_rects(
        state,
        [interactive_damage_rect(state, target.root_surface)
            .unwrap_or(target.geometry.target_rect)],
        dispatcher,
    );
    set_keyboard_focus(state, dispatcher, Some(SurfaceFocus::new(target.root_surface)));
    target
}

fn start_window_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    target: &WindowTarget,
    intent: InteractionIntent,
) -> bool {
    match (intent, target.role) {
        (InteractionIntent::StartMove(_), _) => start_interactive_move(
            state,
            dispatcher,
            target.client_id(),
            target.root_surface,
            target.geometry,
        ),
        (
            InteractionIntent::StartResize(_, edges),
            WindowRoleSnapshot::Toplevel { window, size_constraints, .. },
        ) => {
            if state.window_is_tiled(window) {
                start_interactive_tiled_resize(
                    state,
                    dispatcher,
                    target.client_id(),
                    window,
                    target.root_surface,
                    edges,
                )
            } else {
                start_interactive_resize(
                    state,
                    dispatcher,
                    target.client_id(),
                    window,
                    target.root_surface,
                    size_constraints,
                    edges,
                    target.geometry,
                )
            }
        }
        _ => false,
    }
}

const TILED_DRAG_PREVIEW_SCALE: f64 = 0.8489;

pub(crate) fn tiled_drag_session_new(
    state: &State,
    window: WindowId,
    root_surface: Resolved<WlSurfaceId>,
    size_constraints: InteractiveSizeConstraints,
    decoration_mode: ToplevelDecorationMode,
    grab_geometry: WindowTargetGeometry,
    threshold: i32,
) -> TiledDragSession {
    let pointer = state.seat.pointer_pos;
    TiledDragSession {
        window,
        root_surface,
        size_constraints,
        decoration_mode,
        origin_pointer: pointer,
        threshold,
        picked_up: false,
        grab_pointer: pointer,
        grab_geometry,
        preview_size: (0, 0),
        frame_from_root: (0, 0),
    }
}

fn start_tiled_drag(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    window: WindowId,
    root_surface: Resolved<WlSurfaceId>,
    size_constraints: InteractiveSizeConstraints,
    decoration_mode: ToplevelDecorationMode,
    initial_geometry: WindowTargetGeometry,
) -> bool {
    begin_compositor_pointer_grab(state, dispatcher, POINTER_MOVE_CURSOR_ICON_NAMES);
    let threshold = state.config.tiling.drag.threshold;
    let mut session = tiled_drag_session_new(
        state,
        window,
        root_surface,
        size_constraints,
        decoration_mode,
        initial_geometry,
        threshold,
    );
    state.seat.pointer_interactive_grab = Some(ActiveGrabState::MoveTiled(session));
    if threshold <= 0 {
        match tiled_drag_pickup(state, dispatcher, &mut session) {
            Ok(origin) => {
                state.seat.tiled_drag_origin = origin;
                state.seat.pointer_interactive_grab = Some(ActiveGrabState::MoveTiled(session));
            }
            Err(()) => {
                state.seat.pointer_interactive_grab = Some(ActiveGrabState::MoveTiled(session));
                abort_pointer_interactive_grab(state, dispatcher, 0);
            }
        }
    }
    true
}

fn tiled_drag_threshold_reached(origin: (f64, f64), pointer: (f64, f64), threshold: i32) -> bool {
    let threshold = threshold.max(0) as f64;
    let dx = pointer.0 - origin.0;
    let dy = pointer.1 - origin.1;
    dx * dx + dy * dy > threshold * threshold
}

/// Ok(origin) after the window is a preview. `Err(())` means the caller must abort.
///
/// A failure after the window has left the tree puts it back here, so a toplevel drag can
/// drop the attempt without aborting a pointer grab. The session is then not picked up.
pub(crate) fn tiled_drag_pickup(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    session: &mut TiledDragSession,
) -> Result<Option<ExtractedTiledLeaf>, ()> {
    if session.picked_up {
        return Ok(None);
    }
    let root_surface = session.root_surface;
    let window = session.window;
    let Some(geometry) = state.committed_root_geometry(root_surface) else {
        return Err(());
    };
    let pointer = state.seat.pointer_pos;
    let mut frame = scaled_tiled_preview_frame(geometry.layout_rect_global, pointer);
    let Some(mut built) = root_surface
        .try_ref(state)
        .and_then(|surface| preview_geometry(state, root_surface, &surface, frame))
    else {
        return Err(());
    };
    let configure = crate::root_geometry::project_configure_geometry(built).size;
    let clamped = session.size_constraints.clamp(configure.0, configure.1);
    if clamped != configure {
        frame.width = frame.width.saturating_add(clamped.0 - configure.0).max(1);
        frame.height = frame.height.saturating_add(clamped.1 - configure.1).max(1);
        frame.x = (pointer.0 - frame.width as f64 / 2.0).round() as i32;
        frame.y = (pointer.1 - frame.height as f64 / 2.0).round() as i32;
        let Some(rebuilt) = root_surface
            .try_ref(state)
            .and_then(|surface| preview_geometry(state, root_surface, &surface, frame))
        else {
            return Err(());
        };
        built = rebuilt;
    }
    let frame_from_root = (frame.x - built.root_pos.0, frame.y - built.root_pos.1);
    let preview_size = (frame.width, frame.height);
    let root_pos = preview_root_for_pointer(pointer, preview_size, frame_from_root);
    let origin = state
        .root_surface_workspace(root_surface)
        .and_then(|workspace_id| state.workspaces.try_get(workspace_id))
        .and_then(|workspace| workspace.layout.capture_tiled_drag_origin(window));
    if !state.float_window_for_drag(window) {
        return Err(());
    }
    session.grab_geometry.root_pos = root_pos;
    session.origin_pointer = pointer;
    session.threshold = state.config.tiling.drag.threshold;
    session.picked_up = true;
    session.grab_pointer = pointer;
    session.preview_size = preview_size;
    session.frame_from_root = frame_from_root;
    let old_bounds = interactive_damage_rect(state, root_surface);
    let Some(mut surface) = root_surface.try_grab(state) else {
        tiled_drag_settle(state, dispatcher, *session, origin, TiledDragEnd::Cancel);
        session.picked_up = false;
        return Err(());
    };
    surface.set_root_position(root_pos.0, root_pos.1);
    drop(surface);
    note_root_position(state, dispatcher, root_surface);
    state.mark_root_geometry_dirty(root_surface);
    // Tile to preview uses the windows animation. Hyprland does the same by assigning
    // m_realPosition and m_realSize when the tiled window becomes floating.
    state.sync_window_geometry_goal(window, crate::visual_animation::WindowGeometryMotion::Layout);
    if let Some(toplevel) = state.toplevel(window) {
        toplevel.interactive_resize(dispatcher, clamped, false);
    }
    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
    let new_bounds = interactive_damage_rect(state, root_surface);
    if let Some(new_bounds) = new_bounds {
        request_outputs_for_rects(
            state,
            old_bounds.into_iter().chain(std::iter::once(new_bounds)),
            dispatcher,
        );
    } else {
        request_active_output(state, dispatcher);
    }
    Ok(origin)
}

/// Threshold check, pickup, or preview placement. `Err(())` means the caller must abort.
pub(crate) fn tiled_drag_motion(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    session: &mut TiledDragSession,
    origin: &mut Option<ExtractedTiledLeaf>,
) -> Result<(), ()> {
    if !session.picked_up {
        if !tiled_drag_threshold_reached(
            session.origin_pointer,
            state.seat.pointer_pos,
            session.threshold,
        ) {
            return Ok(());
        }
        return match tiled_drag_pickup(state, dispatcher, session) {
            Ok(picked) => {
                *origin = picked;
                Ok(())
            }
            Err(()) => Err(()),
        };
    }
    // The pickup animation plays only while the pointer stays put. Motion samples the
    // pointer-centered preview and warps to it, matching Hyprland's warpPositionSize.
    let root_pos = preview_root_for_pointer(
        state.seat.pointer_pos,
        session.preview_size,
        session.frame_from_root,
    );
    if place_grabbed_root(state, dispatcher, session.root_surface, root_pos) {
        Ok(())
    } else {
        Err(())
    }
}

/// Places the preview so its center is the pointer. The frame may extend past the
/// source output. Pointer motion is already limited to the output layout.
fn preview_root_for_pointer(
    pointer: (f64, f64),
    preview_size: (i32, i32),
    frame_from_root: (i32, i32),
) -> (i32, i32) {
    let frame_x = (pointer.0 - preview_size.0 as f64 / 2.0).round() as i32;
    let frame_y = (pointer.1 - preview_size.1 as f64 / 2.0).round() as i32;
    (frame_x.saturating_sub(frame_from_root.0), frame_y.saturating_sub(frame_from_root.1))
}

fn scaled_tiled_preview_frame(
    tile: crate::region::RectI,
    pointer: (f64, f64),
) -> crate::region::RectI {
    let width = ((tile.width.max(0) as f64) * TILED_DRAG_PREVIEW_SCALE)
        .floor()
        .max(1.0) as i32;
    let height = ((tile.height.max(0) as f64) * TILED_DRAG_PREVIEW_SCALE)
        .floor()
        .max(1.0) as i32;
    crate::region::RectI {
        x: (pointer.0 - width as f64 / 2.0).round() as i32,
        y: (pointer.1 - height as f64 / 2.0).round() as i32,
        width,
        height,
    }
}

fn preview_geometry(
    state: &State,
    root_surface: Resolved<WlSurfaceId>,
    surface: &crate::wayland_objects::wl_surface::WlSurface,
    frame: crate::region::RectI,
) -> Option<CommittedRootGeometry> {
    crate::root_geometry::build_committed_root_geometry(
        state,
        root_surface,
        surface,
        Some(crate::tiling::LayoutAllocation {
            frame_rect: frame,
            tiled_edges: 0,
            fullscreen: false,
        }),
        crate::root_geometry::GeometryBuildOverrides::default(),
    )
}

fn move_grabbed_root(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    root_surface: Resolved<WlSurfaceId>,
    initial_pointer_pos: (f64, f64),
    initial_root_pos: (i32, i32),
) {
    let dx = (state.seat.pointer_pos.0 - initial_pointer_pos.0).round() as i32;
    let dy = (state.seat.pointer_pos.1 - initial_pointer_pos.1).round() as i32;
    if !place_grabbed_root(
        state,
        dispatcher,
        root_surface,
        (initial_root_pos.0 + dx, initial_root_pos.1 + dy),
    ) {
        abort_pointer_interactive_grab(state, dispatcher, 0);
    }
}

/// Move `root_surface` to `root_pos`.
///
/// Returns false when the root is already grabbed. Callers that own an interactive grab abort
/// it. A toplevel drag does not: the drag-and-drop keeps running.
pub(crate) fn place_grabbed_root(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    root_surface: Resolved<WlSurfaceId>,
    root_pos: (i32, i32),
) -> bool {
    let old_bounds = interactive_damage_rect(state, root_surface);
    let Some(mut surface) = root_surface.try_grab(state) else {
        return false;
    };
    surface.set_root_position(root_pos.0, root_pos.1);
    drop(surface);
    note_root_position(state, dispatcher, root_surface);
    state.mark_root_geometry_dirty(root_surface);
    if let Some(window) = state.window_for_root(root_surface) {
        state
            .sync_window_geometry_goal(window, crate::visual_animation::WindowGeometryMotion::Warp);
    }
    let new_bounds = interactive_damage_rect(state, root_surface);
    dispatch_client_action(
        dispatcher,
        root_surface.client_id(),
        ClientAction::RefreshSurfaceTreeOutputs { root: root_surface },
    );
    if let Some(new_bounds) = new_bounds {
        let rects = old_bounds.into_iter().chain(std::iter::once(new_bounds));
        request_outputs_for_rects(state, rects, dispatcher);
    } else {
        request_active_output(state, dispatcher);
    }
    true
}

fn note_root_position(state: &State, dispatcher: &mut Dispatcher, root: Resolved<WlSurfaceId>) {
    if let Some(toplevel) = state
        .window_for_root(root)
        .and_then(|window| state.toplevel(window))
    {
        toplevel.position_changed(dispatcher);
    }
}

fn start_interactive_move(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    root_surface: Resolved<WlSurfaceId>,
    initial_geometry: WindowTargetGeometry,
) -> bool {
    let Some(window) = state.window_for_root(root_surface) else {
        return false;
    };
    begin_compositor_pointer_grab(state, dispatcher, POINTER_MOVE_CURSOR_ICON_NAMES);
    state
        .seat
        .start_toplevel_move(client_id, window, root_surface, initial_geometry);
    true
}

fn start_interactive_resize(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    window: WindowId,
    root_surface: Resolved<WlSurfaceId>,
    size_constraints: InteractiveSizeConstraints,
    edges: u32,
    initial_geometry: WindowTargetGeometry,
) -> bool {
    begin_compositor_pointer_grab(state, dispatcher, resize_cursor_icon_names(edges));
    state.seat.start_toplevel_resize(
        client_id,
        window,
        root_surface,
        size_constraints,
        edges,
        initial_geometry,
    );
    true
}

fn start_interactive_tiled_resize(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    window: WindowId,
    root_surface: Resolved<WlSurfaceId>,
    edges: u32,
) -> bool {
    let Some(resize_state) = state.begin_tiled_window_resize(window, edges) else {
        return false;
    };
    begin_compositor_pointer_grab(state, dispatcher, resize_cursor_icon_names(edges));
    state
        .seat
        .start_toplevel_tiled_resize(client_id, window, root_surface, edges, resize_state);
    true
}

fn clear_pointer_focus_for_compositor_grab(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    let Some(old_focus) = state.seat.pointer_focus.take() else {
        state.seat.clear_pointer_enter();
        return;
    };
    let serial = state.seat.next_serial();
    dispatch_client_event(
        dispatcher,
        old_focus.target.client_id(),
        SeatClientEvent::PointerLeave { serial, surface: old_focus.target.surface },
    );
    dispatch_client_event(dispatcher, old_focus.target.client_id(), SeatClientEvent::PointerFrame);
    state.seat.clear_pointer_enter();
}

fn update_visible_cursor_for_grab_change(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    update: impl FnOnce(&mut crate::seat::SeatState) -> bool,
) {
    let previous_cursor_owner = visible_cursor_owner_client(state);
    if !update(&mut state.seat) {
        return;
    }
    if let Some(client_id) = previous_cursor_owner {
        dispatch_client_action(dispatcher, client_id, ClientAction::RefreshSurfaceOutputs);
    }
    refresh_visible_cursor_surface_outputs(state, dispatcher);
    request_outputs_for_points(state, [state.seat.pointer_position()], dispatcher);
}

fn begin_compositor_pointer_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    cursor_icons: &[&'static str],
) {
    clear_pointer_focus_for_compositor_grab(state, dispatcher);
    update_visible_cursor_for_grab_change(state, dispatcher, |seat| {
        seat.set_compositor_named_cursor(cursor_icons)
    });
}

fn end_compositor_pointer_grab(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    update_visible_cursor_for_grab_change(state, dispatcher, crate::seat::SeatState::clear_cursor);
    refresh_pointer_focus(state, dispatcher, time, false);
    update_visible_cursor_for_grab_change(
        state,
        dispatcher,
        crate::seat::SeatState::set_default_cursor,
    );
}

fn visible_cursor_owner_client(state: &State) -> Option<ClientId> {
    match state.seat.visible_cursor_source()? {
        CursorSourceRef::Named { owner_client_id, .. } => owner_client_id,
        CursorSourceRef::Surface { cursor } => Some(cursor.surface.client_id()),
    }
}

fn refresh_visible_cursor_surface_outputs(state: &State, dispatcher: &mut Dispatcher) {
    if let Some(CursorSourceRef::Surface { cursor }) = state.seat.visible_cursor_source() {
        dispatch_client_action(
            dispatcher,
            cursor.surface.client_id(),
            ClientAction::RefreshSurfaceTreeOutputs { root: cursor.surface },
        );
    }
}

fn interactive_damage_rect(
    state: &State,
    root_surface: Resolved<WlSurfaceId>,
) -> Option<crate::region::RectI> {
    state
        .root_surface_surface_bounds_global(root_surface)
        .or_else(|| state.root_surface_content_rect_global(root_surface))
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{ClientId, Resolved},
        config::DecorationInsets,
        protocol_generated::WlSurfaceId,
        region::RectI,
        root_geometry::CommittedRootGeometry,
        test_support::state_for_tests,
        wayland_objects::wl_surface::WlSurface,
    };

    use super::interactive_damage_rect;

    #[test]
    fn interactive_damage_rect_prefers_visual_outer_rect() {
        let mut state = state_for_tests();
        let root_surface_id = WlSurfaceId::from(71u32);
        let root_object_id = state
            .insert_object(WlSurface::new(root_surface_id, ClientId::from_raw(7), 6))
            .1;
        let root_surface = Resolved::new(ClientId::from_raw(7), root_surface_id, root_object_id);
        state.register_root_surface(root_surface);
        state.root_geometry_cache.insert(
            root_surface,
            std::cell::Cell::new(crate::state::RootGeometryCacheEntry {
                geometry: Some(CommittedRootGeometry {
                    layout_rect_global: RectI { x: 20, y: 30, width: 120, height: 90 },
                    root_pos: (20, 30),
                    surface_bounds_local: Some(RectI { x: -5, y: -5, width: 130, height: 100 }),
                    logical_window_local: Some(RectI { x: 10, y: 10, width: 100, height: 70 }),
                    decoration_mode: None,
                    decoration_insets: DecorationInsets::default(),
                    tiled_edges: 0,
                    fullscreen: false,
                }),
                dirty: false,
                revision: 0,
                built_from_revision: 0,
            }),
        );

        assert_eq!(
            interactive_damage_rect(&state, root_surface),
            Some(RectI { x: 15, y: 25, width: 130, height: 100 })
        );
    }

    #[test]
    fn tiled_drag_preview_is_hyprland_scale_centered_on_the_pointer() {
        let frame = super::scaled_tiled_preview_frame(
            RectI { x: 10, y: 20, width: 200, height: 100 },
            (80.0, 70.0),
        );
        assert_eq!(frame.width, 169);
        assert_eq!(frame.height, 84);
        assert_eq!(frame.x, (80.0_f64 - 169.0 / 2.0).round() as i32);
        assert_eq!(frame.y, (70.0_f64 - 84.0 / 2.0).round() as i32);
    }

    #[test]
    fn tiled_drag_threshold_uses_euclidean_distance() {
        assert!(!super::tiled_drag_threshold_reached((0.0, 0.0), (3.0, 4.0), 5));
        assert!(super::tiled_drag_threshold_reached((0.0, 0.0), (3.0, 4.0), 4));
        assert!(super::tiled_drag_threshold_reached((10.0, 10.0), (10.0, 21.0), 10));
    }

    #[test]
    fn interactive_grab_ends_when_its_window_is_destroyed() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = crate::test_support::test_client_and_dispatcher(1);
        let root_object = crate::test_support::insert_root_surface(&mut state, 71);
        let root = crate::test_support::resolved_wl_surface(&state, root_object);
        let window = crate::test_support::insert_window(&mut state, root);
        let geometry = crate::root_geometry::WindowTargetGeometry {
            root_pos: (0, 0),
            resize_size: (10, 10),
            target_rect: RectI { x: 0, y: 0, width: 10, height: 10 },
            binding_rect: RectI { x: 0, y: 0, width: 10, height: 10 },
        };
        state.seat.pointer_interactive_grab = Some(super::ActiveGrabState::Move {
            window,
            root_surface: root,
            initial_pointer_pos: (0.0, 0.0),
            initial_geometry: geometry,
        });
        state.destroy_window(window);

        assert!(super::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
        assert!(state.seat.pointer_interactive_grab.is_none());
    }

    #[test]
    fn tiled_drag_preview_follows_the_pointer_past_the_output() {
        let size = (40, 20);
        assert_eq!(super::preview_root_for_pointer((1000.0, 1000.0), size, (0, 0)), (980, 990));
        assert_eq!(super::preview_root_for_pointer((0.0, 0.0), size, (0, 0)), (-20, -10));
        assert_eq!(super::preview_root_for_pointer((1000.0, 1000.0), size, (2, 3)), (978, 987));
        assert_eq!(super::preview_root_for_pointer((50.0, 50.0), (200, 200), (0, 0)), (-50, -50));
    }
}
