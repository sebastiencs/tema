//! An `xdg_toplevel_drag_v1` riding on the seat's drag-and-drop.
//!
//! A mapped window, tiled or floating, follows the pointer. A window that was not
//! mapped yet becomes floating and is placed when it maps. Cancelling puts a
//! floating window back where it started. A tiled drag session is not picked up here.

use crate::{
    clients::{ClientId, Resolved},
    protocol_generated::{WlDataSourceId, WlSurfaceId, XdgToplevelDragV1Id, XdgToplevelId},
    runner::Dispatcher,
    state::State,
    window::WindowId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToplevelDragEnd {
    Dropped,
    ReleasedWithoutTarget,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttachedToplevel {
    pub toplevel: Resolved<XdgToplevelId>,
    pub window: WindowId,
    pub offset: (i32, i32),
    pub was_mapped_at_attach: bool,
    /// Root position captured for a mapped floating window. Restored on cancel.
    pub start_root_pos: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    });
    if was_mapped_at_attach {
        // A mapped tiled window is placed like a floating one until tiled drags exist.
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

/// The drag-and-drop ended.
///
/// A drop or a release leaves the window where it is. Cancelling puts a mapped floating
/// window back. Does not clear `active_drag`. `cancel_active_drag` and `finish_active_drag`
/// call this, so it must not call them back.
pub(crate) fn end(state: &mut State, dispatcher: &mut Dispatcher, how: ToplevelDragEnd) {
    let Some(session) = state.seat.toplevel_drag.take() else {
        return;
    };
    if let Some(attached) = session.attached {
        if let (ToplevelDragEnd::Cancelled, Some(pos)) = (how, attached.start_root_pos) {
            if let Some(root) = state.window_root_surface(attached.window) {
                let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
            }
        }
    }
    if let Some(drag) = session.drag.try_ref_mut(state) {
        drag.mark_ended();
    }
    let Some(attached) = session.attached else {
        return;
    };
    if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
        if toplevel.toplevel_drag() == Some(session.drag) {
            toplevel.set_toplevel_drag(None);
        }
    }
}

/// Keep an attached mapped window under the pointer. A failed grab does not abort anything else.
pub(crate) fn motion(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(attached) = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached)
    else {
        return;
    };
    let Some(root) = state.window_root_surface(attached.window) else {
        return;
    };
    let mapped = root
        .try_ref(state)
        .is_some_and(|surface| surface.has_committed_buffer());
    if !mapped {
        return;
    }
    let Some(pos) = root_for_pointer(state, root, state.seat.pointer_pos, attached.offset) else {
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
    let Some(attached) = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached)
    else {
        return;
    };
    state.raise_window(attached.window);
    let Some(pos) = root_for_pointer(state, root, state.seat.pointer_pos, attached.offset) else {
        return;
    };
    let _ = crate::input::grab::place_grabbed_root(state, dispatcher, root, pos);
}

/// Root of the window currently attached to the seat's toplevel drag.
pub(crate) fn dragged_root(state: &State) -> Option<Resolved<WlSurfaceId>> {
    let attached = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached)?;
    state.window_root_surface(attached.window)
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
    let Some(attached) = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached)
    else {
        return false;
    };
    state.window_root_surface(attached.window) == Some(root)
}

/// The attached toplevel unmapped. The drag-and-drop keeps running.
///
/// A later phase settles a tiled session as a cancel before this clear.
pub(crate) fn detach(state: &mut State, root: Resolved<WlSurfaceId>) {
    let Some(attached) = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| session.attached)
    else {
        return;
    };
    if state.window_root_surface(attached.window) != Some(root) {
        return;
    }
    if let Some(session) = state.seat.toplevel_drag.as_mut() {
        session.attached = None;
    }
    if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
        toplevel.set_toplevel_drag(None);
    }
}

/// The drag object is going away. Drop its attachment without ending the drag-and-drop.
pub(crate) fn clear_attached_for_drag(state: &mut State, drag: Resolved<XdgToplevelDragV1Id>) {
    let attached = state
        .seat
        .toplevel_drag
        .as_ref()
        .and_then(|session| (session.drag == drag).then_some(session.attached).flatten());
    let Some(attached) = attached else {
        return;
    };
    if let Some(session) = state.seat.toplevel_drag.as_mut() {
        session.attached = None;
    }
    if let Some(toplevel) = attached.toplevel.try_ref_mut(state) {
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
        session.attached.is_some_and(|attached| {
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
