//! An `xdg_toplevel_drag_v1` riding on the seat's drag-and-drop.
//!
//! A mapped floating window follows the pointer at the client's offset. A mapped tiled
//! window uses the tiled-drag preview, centered on the pointer. A window that was not
//! mapped yet becomes floating and is placed when it maps. Dropping that new window puts
//! it on the workspace under the pointer: floating when the source was floating, tiled
//! otherwise. Cancelling puts a floating window back where it started, and a tiled window
//! back in its old split. A new window is left as it is.

use crate::{
    clients::{ClientId, Resolved},
    input::grab::{
        tiled_drag_motion, tiled_drag_pickup, tiled_drag_session_new, tiled_drag_settle,
        TiledDragEnd, TiledDragSession,
    },
    protocol_generated::{WlDataSourceId, WlSurfaceId, XdgToplevelDragV1Id, XdgToplevelId},
    root_geometry::project_window_target_geometry,
    runner::Dispatcher,
    state::State,
    tiling::ExtractedTiledLeaf,
    window::WindowId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToplevelDragEnd {
    Dropped,
    ReleasedWithoutTarget,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct AttachedToplevel {
    pub toplevel: Resolved<XdgToplevelId>,
    pub window: WindowId,
    pub offset: (i32, i32),
    pub was_mapped_at_attach: bool,
    /// Root position captured for a mapped floating window. Restored on cancel.
    pub start_root_pos: Option<(i32, i32)>,
    /// Set when a mapped tiled window was picked up. The preview ignores `offset`.
    pub tiled: Option<TiledDragSession>,
    pub tiled_origin: Option<ExtractedTiledLeaf>,
}

#[derive(Debug)]
pub(crate) struct ToplevelDragState {
    pub drag: Resolved<XdgToplevelDragV1Id>,
    pub source: Resolved<WlDataSourceId>,
    pub source_window_floating: bool,
    pub attached: Option<AttachedToplevel>,
}

/// The data source has a toplevel drag and `start_drag` just became the active drag.
pub(crate) fn begin(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    source: Resolved<WlDataSourceId>,
    origin: Resolved<WlSurfaceId>,
) {
    let Some(drag) = ({
        let Some(source) = source.try_ref(state) else {
            return;
        };
        source.toplevel_drag()
    }) else {
        return;
    };
    let root = state.root_surface_for_surface(origin).unwrap_or(origin);
    let source_window_floating = state.window_for_root(root).is_some_and(|window| {
        state
            .managed_window_state(window)
            .is_some_and(|managed| managed.mode == crate::state::ManagedWindowMode::Floating)
    });
    let pending = drag.try_ref_mut(state).and_then(|drag| drag.activate());
    state.seat.toplevel_drag =
        Some(ToplevelDragState { drag, source, source_window_floating, attached: None });
    if let Some((toplevel, offset)) = pending {
        attach(state, dispatcher, toplevel, offset);
    }
}

/// Remember `toplevel` on the seat's drag and place it under the pointer.
pub(crate) fn attach(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    toplevel: Resolved<XdgToplevelId>,
    offset: (i32, i32),
) {
    if state.seat.toplevel_drag.is_none() {
        return;
    }
    let Some(window) = toplevel.try_ref(state).map(|toplevel| toplevel.window()) else {
        return;
    };
    let root = state.window_root_surface(window);
    let was_mapped_at_attach = root.is_some_and(|root| {
        root.try_ref(state)
            .is_some_and(|surface| surface.has_committed_buffer())
    });
    let floating = state
        .managed_window_state(window)
        .is_some_and(|managed| managed.mode == crate::state::ManagedWindowMode::Floating);
    let start_root_pos = (was_mapped_at_attach && floating)
        .then(|| root.and_then(|root| root.try_ref(state).map(|surface| surface.root_position())))
        .flatten();
    let Some(session) = state.seat.toplevel_drag.as_mut() else {
        return;
    };
    session.attached = Some(AttachedToplevel {
        toplevel,
        window,
        offset,
        was_mapped_at_attach,
        start_root_pos,
        tiled: None,
        tiled_origin: None,
    });
    if was_mapped_at_attach && state.window_is_tiled(window) {
        if !pickup_tiled(state, dispatcher, window, root) {
            log::debug!("xdg toplevel drag could not pick up a tiled window");
            forget_attached(state);
        }
    } else if was_mapped_at_attach {
        state.raise_window(window);
        let Some(root) = root else {
            return;
        };
        let Some(pos) = root_for_pointer(state, root, state.seat.pointer_pos, offset) else {
            return;
        };
        let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
    } else {
        state.set_window_floating(window, true);
    }
}

/// Pick up a mapped tiled window immediately. The pointer grab and the cursor stay untouched.
fn pickup_tiled(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    window: WindowId,
    root: Option<Resolved<WlSurfaceId>>,
) -> bool {
    let Some(root) = root else {
        return false;
    };
    let Some(committed) = state.committed_root_geometry(root) else {
        return false;
    };
    let geometry = project_window_target_geometry(committed, None);
    let Some((size_constraints, decoration_mode)) = state
        .toplevel(window)
        .map(|toplevel| (toplevel.size_constraints(), toplevel.decoration_mode()))
    else {
        return false;
    };
    let mut session =
        tiled_drag_session_new(state, window, root, size_constraints, decoration_mode, geometry, 0);
    match tiled_drag_pickup(state, dispatcher, &mut session) {
        Ok(origin) => {
            if let Some(attached) = state
                .seat
                .toplevel_drag
                .as_mut()
                .and_then(|drag| drag.attached.as_mut())
            {
                attached.tiled = Some(session);
                attached.tiled_origin = origin;
            }
            true
        }
        Err(()) => false,
    }
}

fn window_is_mapped(state: &State, window: WindowId) -> bool {
    state.window_root_surface(window).is_some_and(|root| {
        root.try_ref(state)
            .is_some_and(|surface| surface.has_committed_buffer())
    })
}

/// Move a window that mapped during the drag onto the workspace under the pointer.
///
/// A floating source leaves it floating. Anything else tiles it at the pointer, or as the
/// only tile when that workspace has none. No output leaves the window where it is.
fn follow_source_window(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    window: WindowId,
    source_floating: bool,
) {
    let (px, py) = state.seat.pointer_pos;
    let Some(output) = state
        .output_at_point(px.round() as i32, py.round() as i32)
        .or(state.active_output_id())
    else {
        return;
    };
    let Some(workspace_id) = state.active_workspace_for_output(output) else {
        return;
    };
    let current = state
        .managed_window_state(window)
        .map(|managed| managed.workspace_id);
    if current != Some(workspace_id) {
        state.assign_window_to_workspace(window, workspace_id);
    }
    if source_floating {
        // Assigning only updates the workspace id. Relayout moves the floating window
        // into that workspace's stack without changing the drop position.
        state.relayout_dirty_workspaces();
        dispatcher.dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
        return;
    }
    let precise = state.config.tiling.drag.precise_mouse_move;
    let ratio = state.config.tiling.drag.split_ratio.get();
    let _placed = state
        .workspaces
        .try_get_mut(workspace_id)
        .is_some_and(|workspace| {
            workspace
                .layout
                .place_dragged_tiled_leaf(window, px, py, precise, ratio)
        });
    state.retile_window_after_drag(window);
    dispatcher.dispatch(crate::clients::ClientManagerAction::WorkspaceStateChanged);
}

fn forget_attached(state: &mut State) {
    let Some(attached) = state
        .seat
        .toplevel_drag
        .as_mut()
        .and_then(|drag| drag.attached.take())
    else {
        return;
    };
    if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
        toplevel.set_toplevel_drag(None);
    }
}

/// The drag-and-drop ended.
///
/// A drop or a release leaves a window that was already mapped where it is, and places a
/// window that mapped during the drag. Cancelling puts a mapped floating window back.
/// Does not clear `active_drag`. `cancel_active_drag` and `finish_active_drag` call this,
/// so it must not call them back.
pub(crate) fn end(state: &mut State, dispatcher: &mut Dispatcher, how: ToplevelDragEnd) {
    let Some(session) = state.seat.toplevel_drag.take() else {
        return;
    };
    if let Some(mut attached) = session.attached {
        if let Some(tiled) = attached.tiled.take() {
            let origin = attached.tiled_origin.take();
            let end = match how {
                ToplevelDragEnd::Dropped | ToplevelDragEnd::ReleasedWithoutTarget => {
                    TiledDragEnd::Drop
                }
                ToplevelDragEnd::Cancelled => TiledDragEnd::Cancel,
            };
            tiled_drag_settle(state, dispatcher, tiled, origin, end);
        } else if let (ToplevelDragEnd::Cancelled, Some(pos)) = (how, attached.start_root_pos) {
            if let Some(root) = state.window_root_surface(attached.window) {
                let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
            }
        } else if matches!(how, ToplevelDragEnd::Dropped | ToplevelDragEnd::ReleasedWithoutTarget)
            && !attached.was_mapped_at_attach
            && window_is_mapped(state, attached.window)
        {
            follow_source_window(
                state,
                dispatcher,
                attached.window,
                session.source_window_floating,
            );
        }
        if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
            if toplevel.toplevel_drag() == Some(session.drag) {
                toplevel.set_toplevel_drag(None);
            }
        }
    }
    if let Some(drag) = session.drag.try_ref_mut(state) {
        drag.mark_ended();
    }
}

/// Keep an attached mapped window under the pointer. A failed grab does not abort anything else.
pub(crate) fn motion(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(mut attached) = state
        .seat
        .toplevel_drag
        .as_mut()
        .and_then(|drag| drag.attached.take())
    else {
        return;
    };
    if let Some(mut session) = attached.tiled.take() {
        let mut origin = attached.tiled_origin.take();
        match tiled_drag_motion(state, dispatcher, &mut session, &mut origin) {
            Ok(()) => {
                attached.tiled = Some(session);
                attached.tiled_origin = origin;
            }
            Err(()) => {
                tiled_drag_settle(state, dispatcher, session, origin, TiledDragEnd::Cancel);
            }
        }
        if let Some(drag) = state.seat.toplevel_drag.as_mut() {
            drag.attached = Some(attached);
        }
        return;
    }
    let window = attached.window;
    let offset = attached.offset;
    if let Some(drag) = state.seat.toplevel_drag.as_mut() {
        drag.attached = Some(attached);
    }
    let Some(root) = state.window_root_surface(window) else {
        return;
    };
    let mapped = root
        .try_ref(state)
        .is_some_and(|surface| surface.has_committed_buffer());
    if !mapped {
        return;
    }
    let Some(pos) = root_for_pointer(state, root, state.seat.pointer_pos, offset) else {
        return;
    };
    let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
}

/// The attached window just mapped. Raise it and put it under the pointer.
pub(crate) fn on_mapped(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    root: Resolved<WlSurfaceId>,
) {
    if !tracks_root(state, root) {
        return;
    }
    let Some((window, offset)) = state.seat.toplevel_drag.as_ref().and_then(|session| {
        session
            .attached
            .as_ref()
            .map(|attached| (attached.window, attached.offset))
    }) else {
        return;
    };
    state.raise_window(window);
    let Some(pos) = root_for_pointer(state, root, state.seat.pointer_pos, offset) else {
        return;
    };
    let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
}

/// Root of the window currently attached to the seat's toplevel drag.
pub(crate) fn dragged_root(state: &State) -> Option<Resolved<WlSurfaceId>> {
    let window = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached.as_ref().map(|attached| attached.window))?;
    state.window_root_surface(window)
}

/// Root position that puts the window-geometry point `offset` on `pointer`.
///
/// Frame insets sit outside the window geometry and do not move this origin.
fn root_for_pointer(
    state: &State,
    root: Resolved<WlSurfaceId>,
    pointer: (f64, f64),
    offset: (i32, i32),
) -> Option<(i32, i32)> {
    let geometry = state.committed_root_geometry(root)?;
    let (wx, wy) = geometry
        .logical_window_local
        .map(|rect| (rect.x, rect.y))
        .unwrap_or((0, 0));
    Some((
        (pointer.0 - offset.0 as f64).round() as i32 - wx,
        (pointer.1 - offset.1 as f64).round() as i32 - wy,
    ))
}

pub(crate) fn tracks_root(state: &State, root: Resolved<WlSurfaceId>) -> bool {
    let Some(window) = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached.as_ref().map(|attached| attached.window))
    else {
        return false;
    };
    state.window_root_surface(window) == Some(root)
}

/// The attached toplevel unmapped. The drag-and-drop keeps running.
///
/// A tiled preview is cancelled first. That restore only runs while the window is still mapped.
pub(crate) fn detach(state: &mut State, dispatcher: &mut Dispatcher, root: Resolved<WlSurfaceId>) {
    let Some(mut attached) = state
        .seat
        .toplevel_drag
        .as_mut()
        .and_then(|session| session.attached.take())
    else {
        return;
    };
    if state.window_root_surface(attached.window) != Some(root) {
        if let Some(session) = state.seat.toplevel_drag.as_mut() {
            session.attached = Some(attached);
        }
        return;
    }
    if let Some(tiled) = attached.tiled.take() {
        let origin = attached.tiled_origin.take();
        tiled_drag_settle(state, dispatcher, tiled, origin, TiledDragEnd::Cancel);
    }
    if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
        toplevel.set_toplevel_drag(None);
    }
}

/// The drag object is going away. Drop its attachment without ending the drag-and-drop.
pub(crate) fn clear_attached_for_drag(state: &mut State, drag: Resolved<XdgToplevelDragV1Id>) {
    let attached = state.seat.toplevel_drag.as_ref().and_then(|session| {
        (session.drag == drag).then(|| session.attached.as_ref().map(|attached| attached.toplevel))
    });
    let Some(Some(toplevel)) = attached else {
        return;
    };
    if let Some(session) = state.seat.toplevel_drag.as_mut() {
        session.attached = None;
    }
    if let Some(toplevel) = toplevel.try_ref_mut(state) {
        if toplevel.toplevel_drag() == Some(drag) {
            toplevel.set_toplevel_drag(None);
        }
    }
}

/// `toplevel` is going away. Drop it from the seat drag without ending the drag.
pub(crate) fn clear_attached_toplevel(
    state: &mut State,
    toplevel: XdgToplevelId,
    client_id: ClientId,
) {
    let matches = state.seat.toplevel_drag.as_ref().is_some_and(|session| {
        session.attached.as_ref().is_some_and(|attached| {
            attached.toplevel.wayland_id() == toplevel && attached.toplevel.client_id() == client_id
        })
    });
    if !matches {
        return;
    }
    if let Some(session) = state.seat.toplevel_drag.as_mut() {
        session.attached = None;
    }
}
