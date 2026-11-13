use {
    crate::{
        clients::Resolved,
        config::DecorationInsets,
        protocol_generated::WlSurfaceId,
        region::RectI,
        state::State,
        tiling::LayoutAllocation,
        transform::SizeI,
        wayland_objects::{
            wl_surface::WlSurface,
            xdg_surface::{xdg_toplevel::ToplevelDecorationMode, XdgSurface},
        },
    },
    std::cell::Cell,
};

/// Compositor-owned decoration layout for a root surface.
/// This exists only when the compositor is responsible for SSD chrome.
/// It separates shell frame semantics from generic client window geometry.
/// Input code should treat this as compositor chrome, not client content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecorationFrameLayout {
    /// Full compositor-owned frame rectangle in global coordinates.
    pub frame_rect: RectI,
    /// Client-declared window rectangle inside the compositor frame.
    pub client_window_rect: RectI,
}

/// Final committed geometry snapshot for one root surface.
///
/// This struct stores only canonical facts:
/// compositor layout in global space, the committed root-surface origin, the
/// committed surface/logical bounds in root-local space, and effective
/// decoration metadata. Consumers should derive subsystem-specific rectangles
/// such as input targets, visual extents, and decoration layout through the
/// projection methods below instead of caching separate copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedRootGeometry {
    /// Global compositor layout slot for the root surface.
    ///
    /// This is the outer frame. The client content rect sits inside it by
    /// `decoration_insets` on every edge, for both decoration modes.
    pub layout_rect_global: RectI,
    /// Current root `wl_surface` origin in global compositor coordinates.
    pub root_pos: (i32, i32),
    /// Full committed root surface-tree bounds in root-local coordinates.
    ///
    /// Translating this by `root_pos` yields the actual visual extent used for
    /// rendering, output selection, and damage decisions.
    pub surface_bounds_local: Option<RectI>,
    /// Effective client-declared window geometry in root-local coordinates.
    ///
    /// This preserves the protocol/logical window box independently from the
    /// compositor's chosen layout/content rect.
    pub logical_window_local: Option<RectI>,
    /// Effective decoration mode used when this snapshot was built.
    pub decoration_mode: Option<ToplevelDecorationMode>,
    /// Effective per-edge decoration insets used by final geometry resolution.
    pub decoration_insets: DecorationInsets,
    /// Tiled edge mask exported to xdg_toplevel configure state.
    pub tiled_edges: u32,
    /// Whether this snapshot represents fullscreen layout semantics.
    pub fullscreen: bool,
}

impl CommittedRootGeometry {
    /// Derive the compositor content rect in global space.
    ///
    /// This is the rectangle used for configure/content sizing and corresponds
    /// to the old stored `client_window_rect`.
    pub fn content_rect_global(self) -> RectI {
        project_content_rect_global(
            self.layout_rect_global,
            self.fullscreen,
            self.decoration_insets,
        )
    }

    /// Derive the client-declared logical window rectangle in global space.
    pub fn logical_window_rect_global(self) -> Option<RectI> {
        self.logical_window_local
            .and_then(|rect| translate_local_rect_to_global(rect, self.root_pos))
    }

    /// Derive the committed visual surface-tree bounds in global space.
    pub fn surface_bounds_global(self) -> Option<RectI> {
        self.surface_bounds_at(self.root_pos)
    }

    /// Surface-tree bounds translated so the root origin is `root_pos`.
    pub fn surface_bounds_at(self, root_pos: (i32, i32)) -> Option<RectI> {
        translated_surface_tree_bounds(self.surface_bounds_local, root_pos)
    }

    /// Where the current buffer's window geometry is drawn, in global space.
    ///
    /// This is `logical_window_local` placed at `root_pos`. For a floating
    /// window it equals `content_rect_global()`. For a tiled window it follows
    /// the anchor and can differ from the tile content rect while the buffer
    /// size does not match the tile.
    pub fn client_content_rect_global(self) -> Option<RectI> {
        self.logical_window_rect_global()
    }

    /// Derive the whole outer frame rect in global space.
    pub fn frame_rect_global(self) -> RectI {
        if compositor_decorations_enabled(self.fullscreen) {
            self.layout_rect_global
        } else {
            self.content_rect_global()
        }
    }

    /// Derive compositor-owned SSD chrome layout, if any.
    pub fn decoration_layout(self) -> Option<DecorationFrameLayout> {
        compositor_decoration_layout(
            self.frame_rect_global(),
            self.content_rect_global(),
            self.fullscreen,
        )
    }

    /// Derive the whole-window target rect for focus and pick semantics.
    pub fn window_target_rect(self) -> RectI {
        self.decoration_layout()
            .map(|layout| layout.frame_rect)
            .or_else(|| self.surface_bounds_global())
            .unwrap_or_else(|| self.content_rect_global())
    }

    /// Derive the compositor binding rect used for resize-edge inference.
    pub fn binding_rect(self, logical_size: Option<SizeI>) -> RectI {
        self.decoration_layout()
            .map(|layout| layout.frame_rect)
            .or_else(|| {
                non_ssd_binding_rect(logical_size, self.logical_window_local, self.root_pos)
            })
            .or_else(|| self.surface_bounds_global())
            .unwrap_or_else(|| self.content_rect_global())
    }
}

/// Geometry projected for xdg configure events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigureGeometry {
    /// Global configure window rect implied by the requested decoration mode.
    pub window_rect: RectI,
    /// Width and height sent to the client for the configure.
    pub size: (i32, i32),
}

/// Geometry projected for interactive move/resize targeting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowTargetGeometry {
    /// Global root `wl_surface` origin to apply before hit/input transforms.
    pub root_pos: (i32, i32),
    /// Width and height that interactive resize should treat as the window size.
    pub resize_size: (i32, i32),
    /// Whole-window target rect used for focus and move hits.
    pub target_rect: RectI,
    /// Rect used for compositor binding and resize-edge inference.
    pub binding_rect: RectI,
}

/// Project the content size sent in `xdg_toplevel.configure`.
///
/// The frame is outside that size for both decoration modes. Fullscreen sends
/// the whole slot.
pub fn project_configure_geometry(geometry: CommittedRootGeometry) -> ConfigureGeometry {
    let window_rect = geometry.content_rect_global();
    ConfigureGeometry { window_rect, size: (window_rect.width, window_rect.height) }
}

/// Project interaction target geometry from a committed snapshot.
pub fn project_window_target_geometry(
    geometry: CommittedRootGeometry,
    logical_size: Option<SizeI>,
) -> WindowTargetGeometry {
    WindowTargetGeometry {
        root_pos: geometry.root_pos,
        resize_size: {
            let content_rect = geometry.content_rect_global();
            (content_rect.width, content_rect.height)
        },
        target_rect: geometry.window_target_rect(),
        binding_rect: geometry.binding_rect(logical_size),
    }
}

/// Resolve a committed geometry snapshot, reusing cache entries when the requested
/// overrides do not change the canonical stored facts.
pub fn resolve_committed_root_geometry_with_overrides(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
    surface: &WlSurface,
    overrides: GeometryBuildOverrides,
) -> Option<CommittedRootGeometry> {
    cached_geometry_reusable_for_overrides(state, surface_object_id, overrides).or_else(|| {
        build_committed_root_geometry(
            state,
            surface_object_id,
            surface,
            crate::tiling::layout_allocation_for_surface(state, surface_object_id),
            overrides,
        )
    })
}

fn cached_geometry_reusable_for_overrides(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
    overrides: GeometryBuildOverrides,
) -> Option<CommittedRootGeometry> {
    state
        .root_geometry_cache
        .get(&surface_object_id)
        .map(Cell::get)
        .filter(|entry| entry.is_fresh())
        .and_then(|entry| entry.geometry)
        .filter(|geometry| {
            (overrides.decoration_mode.is_none()
                || geometry.decoration_mode == overrides.decoration_mode)
                && overrides.explicit_window_geometry.is_none()
                && overrides.decoration_insets.is_none()
                && overrides
                    .root_pos
                    .is_none_or(|root_pos| geometry.root_pos == root_pos)
        })
}

/// Protocol move targets use the committed interaction projections for the
/// decoration mode currently negotiated with the client.
pub fn resolve_protocol_move_target_geometry(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
    surface: &WlSurface,
    decoration_mode: ToplevelDecorationMode,
) -> Option<WindowTargetGeometry> {
    let geometry = resolve_committed_root_geometry_with_overrides(
        state,
        surface_object_id,
        surface,
        GeometryBuildOverrides {
            decoration_mode: Some(decoration_mode),
            explicit_window_geometry: None,
            decoration_insets: None,
            root_pos: Some(surface.root_position()),
        },
    )?;
    Some(project_window_target_geometry(geometry, surface.logical_size()))
}

/// Protocol resize targets combine committed targeting rects with configure-sized
/// resize dimensions for the pending decoration mode.
pub fn resolve_protocol_resize_target_geometry(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
    surface: &WlSurface,
    configure_decoration_mode: ToplevelDecorationMode,
) -> Option<WindowTargetGeometry> {
    let geometry = resolve_committed_root_geometry_with_overrides(
        state,
        surface_object_id,
        surface,
        GeometryBuildOverrides {
            decoration_mode: Some(configure_decoration_mode),
            explicit_window_geometry: None,
            decoration_insets: None,
            root_pos: Some(surface.root_position()),
        },
    )?;
    Some(WindowTargetGeometry {
        root_pos: geometry.root_pos,
        resize_size: project_configure_geometry(geometry).size,
        target_rect: geometry.window_target_rect(),
        binding_rect: geometry.binding_rect(surface.logical_size()),
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GeometryBuildOverrides {
    /// Force a specific decoration mode while projecting geometry.
    pub decoration_mode: Option<ToplevelDecorationMode>,
    /// Override the effective logical window geometry in root-local space.
    pub explicit_window_geometry: Option<RectI>,
    /// Override the effective decoration insets.
    pub decoration_insets: Option<DecorationInsets>,
    /// Override the committed root origin in global coordinates.
    pub root_pos: Option<(i32, i32)>,
}

pub fn effective_decoration_insets(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
) -> DecorationInsets {
    if state
        .objects
        .try_ref_object::<WlSurface>(surface_object_id.object_id())
        .is_none()
    {
        return state.frame_insets();
    }
    effective_decoration_insets_for_surface(state, surface_object_id)
}

/// Frame reserved for a root that has a toplevel. Both decoration modes use it.
pub fn effective_decoration_insets_for_surface(
    state: &State,
    surface: Resolved<WlSurfaceId>,
) -> DecorationInsets {
    match state
        .toplevel_for_root(surface)
        .map(|toplevel| toplevel.decoration_mode())
    {
        Some(_) => state.frame_insets(),
        None => DecorationInsets::default(),
    }
}

/// Derive CSD-style decoration insets from a client-declared window geometry.
pub fn decoration_insets_from_window_geometry(
    surface: &WlSurface,
    window_geometry: RectI,
) -> Option<DecorationInsets> {
    let logical_size = surface.logical_size()?;
    decoration_insets_from_window_geometry_and_logical_size(logical_size, window_geometry)
}

/// Derive CSD-style decoration insets from a logical size and window geometry.
pub fn decoration_insets_from_window_geometry_and_logical_size(
    logical_size: SizeI,
    window_geometry: RectI,
) -> Option<DecorationInsets> {
    let w = logical_size.width;
    let h = logical_size.height;
    let gx2 = window_geometry.x2();
    let gy2 = window_geometry.y2();
    Some(DecorationInsets {
        left: window_geometry
            .x
            .saturating_neg()
            .max(0)
            .saturating_add(window_geometry.x.max(0)),
        top: window_geometry
            .y
            .saturating_neg()
            .max(0)
            .saturating_add(window_geometry.y.max(0)),
        right: gx2
            .saturating_sub(w)
            .max(0)
            .saturating_add(w.saturating_sub(gx2).max(0)),
        bottom: gy2
            .saturating_sub(h)
            .max(0)
            .saturating_add(h.saturating_sub(gy2).max(0)),
    })
}

/// Global top-left of the client content inside `tile_content`.
///
/// Sway places a tiled surface at the content box's top-left
/// (`view_center_and_clip_surface`) and only centers floating windows. Pinning
/// the edge that stayed put keeps that edge still while the buffer is still
/// the previous size. Per axis: no basis, or sizes already equal, keeps the
/// tile origin. Only the start edge moved pins the end. Every other case,
/// including both edges moving, keeps the tile origin.
pub fn anchored_content_origin(
    tile_content: RectI,
    basis: Option<RectI>,
    client_size: (i32, i32),
) -> (i32, i32) {
    (
        anchored_axis(
            tile_content.x,
            tile_content.x2(),
            tile_content.width,
            basis.map(|basis| (basis.x, basis.x2())),
            client_size.0,
        ),
        anchored_axis(
            tile_content.y,
            tile_content.y2(),
            tile_content.height,
            basis.map(|basis| (basis.y, basis.y2())),
            client_size.1,
        ),
    )
}

fn anchored_axis(origin: i32, end: i32, span: i32, basis: Option<(i32, i32)>, client: i32) -> i32 {
    let Some((basis_origin, basis_end)) = basis else {
        return origin;
    };
    if client == span {
        return origin;
    }
    let start_moved = origin != basis_origin;
    let end_moved = end != basis_end;
    if start_moved && !end_moved {
        end.saturating_sub(client)
    } else {
        origin
    }
}

/// Clamp explicit window geometry to committed surface bounds, or fall back to
/// committed surface-tree bounds when the client did not provide one.
pub fn effective_window_geometry_for_surface(
    state: &State,
    surface: &WlSurface,
    explicit_window_geometry: Option<RectI>,
) -> Option<RectI> {
    let surface_bounds = WlSurface::local_surface_tree_bounds_from_ref(state, surface);
    match (surface_bounds, explicit_window_geometry) {
        (Some(surface_bounds), Some(raw)) => {
            Some(clamp_window_geometry_to_surface_bounds(raw, surface_bounds))
        }
        (Some(surface_bounds), None) => Some(surface_bounds),
        (None, Some(raw)) => Some(raw),
        (None, None) => None,
    }
}

/// Build the canonical committed geometry snapshot for a root surface.
///
/// The builder first resolves effective logical geometry and decoration policy,
/// then computes compositor layout in global space and stores only canonical
/// layout/root-local facts. Consumers should derive subsystem-specific views
/// from the returned snapshot instead of storing duplicate rectangles.
pub fn build_committed_root_geometry(
    state: &State,
    surface_id: Resolved<WlSurfaceId>,
    surface: &WlSurface,
    layout_allocation: Option<LayoutAllocation>,
    overrides: GeometryBuildOverrides,
) -> Option<CommittedRootGeometry> {
    // The initial commit holds the xdg surface out of the map.
    let xdg_surface = surface
        .xdg_surface()
        .filter(|xdg_surface| !state.objects.is_grabbed(xdg_surface.object_id()))
        .and_then(|xdg_surface| xdg_surface.try_ref(state));
    let decoration_mode = overrides.decoration_mode.or_else(|| {
        state
            .toplevel_for_root(surface_id)
            .map(|toplevel| toplevel.decoration_mode())
    });
    let explicit_window_geometry = overrides
        .explicit_window_geometry
        .or_else(|| xdg_surface.and_then(XdgSurface::current_window_geometry));
    let effective_window_geometry =
        effective_window_geometry_for_surface(state, surface, explicit_window_geometry);
    let root_pos = overrides
        .root_pos
        .unwrap_or_else(|| surface.root_position());
    let fullscreen = layout_allocation.is_some_and(|allocation| allocation.fullscreen);
    let decoration_insets = overrides
        .decoration_insets
        .unwrap_or_else(|| decoration_insets_for_surface(state, decoration_mode, fullscreen));

    let local_surface_bounds = WlSurface::local_surface_tree_bounds_from_ref(state, surface);
    let (layout_rect_global, logical_window_rect_global, computed_root_pos, tiled_edges) =
        if let Some(allocation) = layout_allocation {
            let layout_rect_global = allocation.frame_rect;
            let content_rect_global = project_content_rect_global(
                layout_rect_global,
                allocation.fullscreen,
                decoration_insets,
            );
            // The client's window-geometry origin places its content inside the root surface.
            // Align the root so that content lands on the compositor content rect. A client
            // shadow then hangs outside that rect. A tiled window that still has a previous
            // size is pinned to the tile edges that did not move since that size matched.
            let is_tiled = !allocation.fullscreen && state.is_tiled_root_surface(surface_id);
            let basis = is_tiled
                .then(|| state.managed_window(surface_id))
                .flatten()
                .and_then(|managed| managed.tiled_content_basis);
            let computed_root_pos = effective_window_geometry
                .map(|window_geometry| {
                    let origin = if is_tiled {
                        anchored_content_origin(
                            content_rect_global,
                            basis,
                            (window_geometry.width, window_geometry.height),
                        )
                    } else {
                        (content_rect_global.x, content_rect_global.y)
                    };
                    (
                        origin.0.saturating_sub(window_geometry.x),
                        origin.1.saturating_sub(window_geometry.y),
                    )
                })
                .unwrap_or((content_rect_global.x, content_rect_global.y));
            (layout_rect_global, content_rect_global, computed_root_pos, allocation.tiled_edges)
        } else if let Some((origin, window_geometry)) =
            floating_frame_origin(state, surface_id).zip(effective_window_geometry)
        {
            // A floating xdg window is positioned by its frame. The root is derived,
            // so a window-geometry offset change does not move the frame.
            let insets = if compositor_decorations_enabled(fullscreen) {
                decoration_insets
            } else {
                DecorationInsets::default()
            };
            let content_rect_global = RectI {
                x: origin.0.saturating_add(insets.left.max(0)),
                y: origin.1.saturating_add(insets.top.max(0)),
                width: window_geometry.width,
                height: window_geometry.height,
            };
            let layout_rect_global = inflate_rect_with_insets(content_rect_global, insets);
            let computed_root_pos = (
                content_rect_global.x.saturating_sub(window_geometry.x),
                content_rect_global.y.saturating_sub(window_geometry.y),
            );
            (layout_rect_global, content_rect_global, computed_root_pos, 0)
        } else {
            let translated_surface_bounds =
                translated_surface_tree_bounds(local_surface_bounds, root_pos);
            let logical_window_rect_global = effective_window_geometry
                .and_then(|window_geometry| {
                    project_window_geometry_from_surface_bounds(
                        window_geometry,
                        translated_surface_bounds,
                        local_surface_bounds,
                        root_pos,
                    )
                })
                .or(translated_surface_bounds)?;
            let layout_rect_global = if compositor_decorations_enabled(fullscreen) {
                inflate_rect_with_insets(logical_window_rect_global, decoration_insets)
            } else {
                logical_window_rect_global
            };
            (layout_rect_global, logical_window_rect_global, root_pos, 0)
        };

    let logical_window_local = effective_window_geometry
        .or_else(|| rect_in_root_local_space(logical_window_rect_global, computed_root_pos));

    let geometry = CommittedRootGeometry {
        layout_rect_global,
        root_pos: computed_root_pos,
        surface_bounds_local: local_surface_bounds,
        logical_window_local,
        decoration_mode,
        decoration_insets,
        tiled_edges,
        fullscreen,
    };

    Some(geometry)
}

/// Build a predictive committed geometry snapshot for initial tiled configure
/// paths before a real committed surface-tree snapshot is available.
pub fn predictive_tiled_geometry(
    layout_allocation: LayoutAllocation,
    decoration_mode: ToplevelDecorationMode,
    explicit_window_geometry: Option<RectI>,
    decoration_insets: DecorationInsets,
) -> CommittedRootGeometry {
    let layout_rect_global = layout_allocation.frame_rect;
    let decoration_insets =
        if layout_allocation.fullscreen { DecorationInsets::default() } else { decoration_insets };
    let content_rect_global = project_content_rect_global(
        layout_rect_global,
        layout_allocation.fullscreen,
        decoration_insets,
    );
    let root_pos = explicit_window_geometry
        .map(|window_geometry| {
            (
                content_rect_global.x.saturating_sub(window_geometry.x),
                content_rect_global.y.saturating_sub(window_geometry.y),
            )
        })
        .unwrap_or((content_rect_global.x, content_rect_global.y));
    let logical_window_local = explicit_window_geometry
        .or_else(|| rect_in_root_local_space(content_rect_global, root_pos));
    CommittedRootGeometry {
        layout_rect_global,
        root_pos,
        surface_bounds_local: None,
        logical_window_local,
        decoration_mode: Some(decoration_mode),
        decoration_insets,
        tiled_edges: layout_allocation.tiled_edges,
        fullscreen: layout_allocation.fullscreen,
    }
}

/// Frame reserved around every non-fullscreen toplevel, from `decorations.border.size`.
fn decoration_insets_for_surface(
    state: &State,
    decoration_mode: Option<ToplevelDecorationMode>,
    fullscreen: bool,
) -> DecorationInsets {
    if fullscreen || decoration_mode.is_none() {
        DecorationInsets::default()
    } else {
        state.frame_insets()
    }
}

/// Derive the resize/binding rect for non-SSD windows from logical size and the
/// effective logical window box in root-local coordinates.
fn non_ssd_binding_rect(
    logical_size: Option<SizeI>,
    logical_window_local: Option<RectI>,
    root_pos: (i32, i32),
) -> Option<RectI> {
    let Some(window_geometry) = logical_window_local else {
        return None;
    };
    let logical_size = logical_size?;
    let x1 = if window_geometry.x >= 0 && window_geometry.x2() <= logical_size.width {
        0
    } else {
        window_geometry.x.max(0)
    };
    let y1 = if window_geometry.y >= 0 && window_geometry.y2() <= logical_size.height {
        0
    } else {
        window_geometry.y.max(0)
    };
    let x2 = if window_geometry.x >= 0 && window_geometry.x2() <= logical_size.width {
        logical_size.width
    } else {
        window_geometry.x2().min(logical_size.width)
    };
    let y2 = if window_geometry.y >= 0 && window_geometry.y2() <= logical_size.height {
        logical_size.height
    } else {
        window_geometry.y2().min(logical_size.height)
    };
    RectI::new(
        root_pos.0.saturating_add(x1),
        root_pos.1.saturating_add(y1),
        x2.saturating_sub(x1).max(1),
        y2.saturating_sub(y1).max(1),
    )
}

/// Whether the compositor frame is reserved around this snapshot.
fn compositor_decorations_enabled(fullscreen: bool) -> bool {
    !fullscreen
}

/// Frame origin of a floating, non-fullscreen managed window, when it has one.
fn floating_frame_origin(state: &State, surface_id: Resolved<WlSurfaceId>) -> Option<(i32, i32)> {
    let managed = state.managed_window(surface_id)?;
    (managed.mode == crate::state::ManagedWindowMode::Floating && !managed.fullscreen)
        .then_some(managed.floating_frame_origin)
        .flatten()
}

/// Build the compositor frame when it is larger than the content rect.
fn compositor_decoration_layout(
    frame_rect: RectI,
    client_window_rect: RectI,
    fullscreen: bool,
) -> Option<DecorationFrameLayout> {
    if !compositor_decorations_enabled(fullscreen) || frame_rect == client_window_rect {
        return None;
    }
    Some(DecorationFrameLayout { frame_rect, client_window_rect })
}

/// Translate committed root-local surface bounds into global compositor space.
fn translated_surface_tree_bounds(
    local_surface_bounds: Option<RectI>,
    root_pos: (i32, i32),
) -> Option<RectI> {
    local_surface_bounds
        .and_then(|local_bounds| translate_local_rect_to_global(local_bounds, root_pos))
}

/// Translate a root-local rect into global compositor space.
fn translate_local_rect_to_global(local_rect: RectI, root_pos: (i32, i32)) -> Option<RectI> {
    RectI::new(
        root_pos.0.saturating_add(local_rect.x),
        root_pos.1.saturating_add(local_rect.y),
        local_rect.width,
        local_rect.height,
    )
}

/// Express a global rect in root-local coordinates for the current root origin.
fn rect_in_root_local_space(global_rect: RectI, root_pos: (i32, i32)) -> Option<RectI> {
    RectI::new(
        global_rect.x.saturating_sub(root_pos.0),
        global_rect.y.saturating_sub(root_pos.1),
        global_rect.width,
        global_rect.height,
    )
}

/// Project root-local window geometry into global compositor coordinates using
/// the best available committed surface bounds snapshot.
fn project_window_geometry_from_surface_bounds(
    window_geometry: RectI,
    translated_surface_bounds: Option<RectI>,
    local_surface_bounds: Option<RectI>,
    root_pos: (i32, i32),
) -> Option<RectI> {
    match (translated_surface_bounds, local_surface_bounds) {
        (Some(global_bounds), Some(local_bounds)) => RectI::new(
            global_bounds
                .x
                .saturating_add(window_geometry.x.saturating_sub(local_bounds.x)),
            global_bounds
                .y
                .saturating_add(window_geometry.y.saturating_sub(local_bounds.y)),
            window_geometry.width,
            window_geometry.height,
        ),
        _ => RectI::new(
            root_pos.0.saturating_add(window_geometry.x),
            root_pos.1.saturating_add(window_geometry.y),
            window_geometry.width,
            window_geometry.height,
        ),
    }
}

/// Inflate a global rect by positive decoration insets.
fn inflate_rect_with_insets(rect: RectI, insets: DecorationInsets) -> RectI {
    RectI::from_extents(
        rect.x.saturating_sub(insets.left.max(0)),
        rect.y.saturating_sub(insets.top.max(0)),
        rect.x2().saturating_add(insets.right.max(0)),
        rect.y2().saturating_add(insets.bottom.max(0)),
    )
    .unwrap_or(rect)
}

/// Project the compositor content rect from the global layout slot and the
/// effective decoration ownership for this snapshot.
fn project_content_rect_global(
    layout_rect_global: RectI,
    fullscreen: bool,
    decoration_insets: DecorationInsets,
) -> RectI {
    if fullscreen {
        layout_rect_global
    } else {
        shrink_rect(layout_rect_global, decoration_insets)
    }
}

/// Shrink a global rect by positive decoration insets.
fn shrink_rect(rect: RectI, insets: DecorationInsets) -> RectI {
    if rect.width <= 0 || rect.height <= 0 {
        return rect;
    }
    let x1 = rect.x.saturating_add(insets.left.max(0));
    let y1 = rect.y.saturating_add(insets.top.max(0));
    let x2 = rect.x2().saturating_sub(insets.right.max(0));
    let y2 = rect.y2().saturating_sub(insets.bottom.max(0));
    RectI::from_extents(x1, y1, x2.max(x1.saturating_add(1)), y2.max(y1.saturating_add(1)))
        .unwrap_or(rect)
}

/// Clamp explicit logical window geometry so it never escapes the committed
/// surface-tree bounds.
fn clamp_window_geometry_to_surface_bounds(window_geometry: RectI, surface_bounds: RectI) -> RectI {
    let x1 = window_geometry
        .x
        .clamp(surface_bounds.x, surface_bounds.x2());
    let y1 = window_geometry
        .y
        .clamp(surface_bounds.y, surface_bounds.y2());
    let x2 = window_geometry
        .x2()
        .clamp(surface_bounds.x, surface_bounds.x2());
    let y2 = window_geometry
        .y2()
        .clamp(surface_bounds.y, surface_bounds.y2());
    RectI::from_extents(x1, y1, x2, y2).unwrap_or(surface_bounds)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::ClientId, protocol_generated::WlSurfaceId, test_support::state_for_tests,
            wayland_objects::wl_surface::WlSurface,
        },
    };

    #[test]
    fn effective_window_geometry_keeps_explicit_geometry_without_tree_bounds() {
        let state = state_for_tests();
        let surface = WlSurface::new(WlSurfaceId::from(1u32), ClientId::from_raw(1), 6);
        let explicit = RectI::new(10, 20, 300, 200).unwrap();
        assert_eq!(
            effective_window_geometry_for_surface(&state, &surface, Some(explicit)),
            Some(explicit)
        );
    }

    fn rect(x: i32, y: i32, width: i32, height: i32) -> RectI {
        RectI { x, y, width, height }
    }

    #[test]
    fn anchored_content_origin_pins_the_edge_that_stayed() {
        let client = (100, 50);
        let basis = rect(0, 0, 100, 50);

        assert_eq!(anchored_content_origin(rect(0, 0, 140, 50), None, client), (0, 0));
        assert_eq!(
            anchored_content_origin(rect(10, 20, 100, 50), Some(basis), client),
            (10, 20),
            "matching size follows the tile"
        );

        // Left edge moved, right stayed at x=100. Pin the right edge.
        assert_eq!(anchored_content_origin(rect(-40, 0, 140, 50), Some(basis), client), (0, 0));
        // Right edge moved, left stayed. Pin the left edge.
        assert_eq!(anchored_content_origin(rect(0, 0, 140, 50), Some(basis), client), (0, 0));
        // Top moved up by 40, bottom stayed at y=50.
        assert_eq!(anchored_content_origin(rect(0, -40, 100, 90), Some(basis), client), (0, 0));
        // Bottom moved, top stayed.
        assert_eq!(anchored_content_origin(rect(0, 0, 100, 90), Some(basis), client), (0, 0));
        // Both horizontal edges moved: keep the tile's left.
        assert_eq!(anchored_content_origin(rect(30, 0, 80, 50), Some(basis), client), (30, 0));
    }

    #[test]
    fn anchored_content_origin_lets_a_larger_buffer_overflow_the_moved_edge() {
        // Tile shrank from the left. The old right edge is still at x=100, and
        // the 100px buffer starts at x=0, left of the new tile.
        let origin =
            anchored_content_origin(rect(20, 0, 80, 50), Some(rect(0, 0, 100, 50)), (100, 50));
        assert_eq!(origin, (0, 0));
        assert!(origin.0 < 20);
    }
}
