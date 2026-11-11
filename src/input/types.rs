use crate::{
    clients::{ClientId, Resolved},
    protocol_generated::WlSurfaceId,
    root_geometry::WindowTargetGeometry,
    state::WorkspaceId,
    tiling::{SplitDragPair, SplitPair},
    wayland_objects::xdg_surface::xdg_toplevel::{
        InteractiveSizeConstraints, ToplevelDecorationMode,
    },
    window::WindowId,
};

use super::grab::TiledDragSession;

#[derive(Clone, Copy, Debug, PartialEq, Eq, derive_more::Deref)]
pub struct SurfaceFocus {
    pub surface: Resolved<WlSurfaceId>,
}

impl SurfaceFocus {
    pub fn new(surface: Resolved<WlSurfaceId>) -> Self {
        Self { surface }
    }

    pub fn surface_id(self) -> WlSurfaceId {
        self.surface.wayland_id()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceFocusPick {
    pub target: SurfaceFocus,
    pub rect: SurfaceRect,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScenePick {
    pub surface_focus: Option<SurfaceFocusPick>,
    pub window_target: Option<WindowSnapshot>,
    pub zone: Option<CompositorZone>,
}

/// A point the compositor owns: a floating resize edge, or one or two split handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompositorZone {
    /// Floating window edge or corner. `edges` uses xdg `ResizeEdge` bits.
    WindowResize { target: WindowSnapshot, edges: u32 },
    /// One split, or two perpendicular splits at a corner.
    SplitResize { workspace: crate::state::WorkspaceId, splits: SplitPair, cursor_edges: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowRoleSnapshot {
    Toplevel {
        window: WindowId,
        size_constraints: InteractiveSizeConstraints,
        is_maximized: bool,
        is_fullscreen: bool,
        decoration_mode: ToplevelDecorationMode,
    },
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WindowSnapshot {
    pub root_surface: Resolved<WlSurfaceId>,
    pub role: WindowRoleSnapshot,
    pub geometry: WindowTargetGeometry,
}

impl WindowSnapshot {
    pub fn client_id(self) -> ClientId {
        self.root_surface.client_id()
    }

    pub fn surface_id(self) -> WlSurfaceId {
        self.root_surface.wayland_id()
    }

    pub fn object_id(self) -> crate::state::ObjectId {
        self.root_surface.object_id()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InteractionIntent {
    FocusSurface(SurfaceFocus),
    StartMove(WindowSnapshot),
    StartResize(WindowSnapshot, u32),
    StartSplitResize {
        workspace: crate::state::WorkspaceId,
        splits: SplitPair,
        cursor_edges: u32,
        origin: Option<WindowSnapshot>,
    },
    DismissPopupGrab,
    None,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowHit {
    pub target: WindowSnapshot,
    pub zone: Option<CompositorZone>,
    pub binding_resize_edges: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InteractionStartOptions {
    pub prepare_window: bool,
}

pub(crate) const POINTER_INTERACTION_START_OPTIONS: InteractionStartOptions =
    InteractionStartOptions { prepare_window: true };

pub(crate) const PROTOCOL_INTERACTION_START_OPTIONS: InteractionStartOptions =
    InteractionStartOptions { prepare_window: false };

#[derive(Clone, Copy, Debug)]
pub(crate) enum ActiveGrab {
    Move {
        window: WindowId,
        root_surface: Resolved<WlSurfaceId>,
        initial_pointer_pos: (f64, f64),
        initial_geometry: WindowTargetGeometry,
    },
    Resize {
        window: WindowId,
        root_surface: Resolved<WlSurfaceId>,
        size_constraints: InteractiveSizeConstraints,
        edges: u32,
        initial_pointer_pos: (f64, f64),
        initial_geometry: WindowTargetGeometry,
        last_size: (i32, i32),
    },
    ResizeSplit {
        workspace: WorkspaceId,
        drags: SplitDragPair,
        cursor_edges: u32,
        initial_pointer_pos: (f64, f64),
        origin_window: Option<WindowId>,
    },
    /// Compositor move of a tiled window. The window stays tiled until the pointer
    /// travels past `threshold`, then it becomes a temporary floating preview.
    /// The preview follows the pointer and may extend past the source output.
    MoveTiled(TiledDragSession),
}

pub(crate) type ActiveGrabState = ActiveGrab;
pub(crate) type PointerFocus = SurfaceFocusPick;
pub(crate) type PointerInteractiveGrab = ActiveGrab;
pub(crate) type WindowTarget = WindowSnapshot;
