use std::{
    collections::{BTreeSet, VecDeque},
    rc::Rc,
    time::Instant,
};

use crate::{
    clients::{
        Client, ClientDispatcher, ClientId, ClientManagerAction, EventWriter, RawRequest, Resolved,
    },
    color::{
        chroma_matches_format, coefficients_match_format, ColorRepresentation, ImageDescription,
        RenderIntent, RepresentationUpdate,
    },
    drm::{
        begin_managed_toplevel_close_surface, kick_visual_animation_pump, request_active_output,
        request_outputs, request_outputs_for_rect, request_outputs_for_rects,
    },
    input::{set_keyboard_focus_from_surface, SurfaceFocus},
    object::{Object, Parser, PostAction, RequestResult},
    protocol_generated::{
        wl_pointer, wl_surface, wp_fractional_scale_v1, wp_linux_drm_syncobj_surface_v1,
        wp_viewport, xdg_wm_base, ExtBackgroundEffectSurfaceV1Id, ExtSessionLockSurfaceV1Id,
        WaylandId, WlBufferId, WlCallbackId, WlOutputId, WlRegionId, WlSubsurfaceId, WlSurfaceId,
        WpAlphaModifierSurfaceV1Id, WpColorManagementSurfaceFeedbackV1Id,
        WpColorManagementSurfaceV1Id, WpColorRepresentationSurfaceV1Id, WpCommitTimerV1Id,
        WpFifoV1Id, WpFractionalScaleV1Id, WpLinuxDrmSyncobjSurfaceV1Id, WpPresentationFeedbackId,
        WpTearingControlV1Id, WpViewportId, XdgSurfaceId, XwaylandSurfaceV1Id,
        ZwlrLayerSurfaceV1Id, ZwpIdleInhibitorV1Id, ZwpInputPopupSurfaceV2Id,
    },
    region::{DamageRegion, RectI, RegionValue},
    scene::visibility::root_participates_in_input,
    state::{FractionalScale, GrabbedObject, IndexMapKey, ObjectId, OutputId, State},
    sync::{self, RetiredBufferCandidate, SurfaceSyncState},
    transform::{self, BufferTransform, SizeI, SurfaceViewport},
    wayland_objects::{
        pointer_constraint::SurfacePointerConstraint,
        wl_callback::WlCallback,
        wl_region::WlRegion,
        wp_fractional_scale_v1::WpFractionalScaleV1,
        xdg_surface::XdgSurface,
        zwlr_layer_surface_v1::{
            output_usable_bounds, LayerLayoutObjectSnapshot, ZwlrLayerSurfaceV1,
        },
        zwp_idle_inhibitor_v1::detach_destroyed_surface,
        zwp_pointer_constraints_v1::{apply_pointer_constraint_commit, defunct_pointer_constraint},
    },
    window::Toplevel,
    wire_scale::WireScale,
    xwayland::{SurfaceEvent, XwaylandAction},
    ENABLE_DAMAGE,
};

use {super::wl_buffer::WlBuffer, crate::protocol_generated::wl_display};

mod commit_queue;
mod content_update;
use content_update::PointerConstraintUpdate;
pub(crate) use {
    commit_queue::{flush_client, CommitQueues},
    content_update::{ContentUpdate, RoleUpdate, TreeUpdate},
};

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

pub(crate) fn should_focus_new_toplevel_on_map(
    state: &State,
    root_surface: Resolved<WlSurfaceId>,
) -> bool {
    if !root_surface.exists(state) {
        return false;
    }
    if !root_participates_in_input(state, root_surface) {
        return false;
    }
    if state.seat.popup_grab_client().is_some() {
        return false;
    }
    let Some(new_workspace_id) = state.root_surface_workspace(root_surface) else {
        return false;
    };
    let focused_workspace_id = state
        .focused_root_surface()
        .and_then(|focused_root| state.root_surface_workspace(focused_root));
    focused_workspace_id
        .map(|workspace_id| workspace_id == new_workspace_id)
        .unwrap_or(true)
}

enum PostWlSurface {
    FocusNewToplevel {
        root_surface: Resolved<WlSurfaceId>,
    },
    ClearFocusAfterDestroy {
        closed_window: Option<crate::input::focus::ClosedWindowFocus>,
        released_keyboard: Option<crate::input::SurfaceFocus>,
    },
    FocusSuccessorAfterClose(crate::input::focus::ClosedWindowFocus),
    CancelTiledDrag {
        root: Resolved<WlSurfaceId>,
    },
    DetachToplevelDrag {
        root: Resolved<WlSurfaceId>,
    },
    ToplevelDragMapped {
        root: Resolved<WlSurfaceId>,
    },
    PlaceDialog {
        root: Resolved<WlSurfaceId>,
    },
}

impl PostAction for PostWlSurface {
    fn execute(
        self: Box<Self>,
        _client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        match *self {
            PostWlSurface::FocusNewToplevel { root_surface } => {
                if should_focus_new_toplevel_on_map(state, root_surface) {
                    set_keyboard_focus_from_surface(
                        state,
                        dispatcher,
                        Some(SurfaceFocus::new(root_surface)),
                    );
                }
            }
            PostWlSurface::ClearFocusAfterDestroy { closed_window, released_keyboard } => {
                crate::input::focus::refocus_after_surface_destroy(
                    state,
                    dispatcher,
                    closed_window,
                    released_keyboard,
                );
            }
            PostWlSurface::FocusSuccessorAfterClose(close) => {
                crate::input::focus::refocus_after_focused_window_unmap(state, dispatcher, close);
            }
            PostWlSurface::CancelTiledDrag { root } => {
                let interactive = matches!(
                    state.seat.pointer_interactive_grab,
                    Some(crate::input::ActiveGrabState::MoveTiled(session))
                        if session.root_surface == root
                );
                if interactive {
                    crate::input::abort_pointer_interactive_grab(state, dispatcher, 0);
                }
            }
            PostWlSurface::DetachToplevelDrag { root } => {
                crate::input::toplevel_drag::detach(state, dispatcher, root);
            }
            PostWlSurface::ToplevelDragMapped { root } => {
                crate::input::toplevel_drag::on_mapped(state, dispatcher, root);
            }
            PostWlSurface::PlaceDialog { root } => {
                crate::wayland_objects::xdg_dialog_v1::place_on_map(state, dispatcher, root);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfacePresentationFeedback {
    pub feedback: Resolved<WpPresentationFeedbackId>,
    pub version: u32,
    pub output_id: Option<OutputId>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Role {
    None,
    Cursor,
    DndIcon,
    XdgSurface,
    LayerSurface,
    Subsurface,
    LockSurface,
    XwaylandSurface,
    InputPopup,
}

/// Double-buffered `wp_tearing_control_v1` presentation hint. `Vsync` is the protocol default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PresentationHint {
    #[default]
    Vsync,
    Async,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellSurfaceObject {
    XdgSurface(Resolved<XdgSurfaceId>),
    LayerSurface(Resolved<ZwlrLayerSurfaceV1Id>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubsurfaceSyncMode {
    Sync,
    Desync,
}

#[derive(Clone, Debug)]
struct SubsurfaceState {
    object: Resolved<WlSubsurfaceId>,
    parent_surface: Resolved<WlSurfaceId>,
    pending_position: (i32, i32),
    current_position: (i32, i32),
    sync_mode: SubsurfaceSyncMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackEntry {
    Parent,
    Child(Resolved<WlSurfaceId>),
}

#[derive(Clone, Debug)]
struct SurfaceTreeState {
    pending_stack: Vec<StackEntry>,
    current_stack: Vec<StackEntry>,
}

impl Default for SurfaceTreeState {
    fn default() -> Self {
        let stack = vec![StackEntry::Parent];
        Self { pending_stack: stack.clone(), current_stack: stack }
    }
}

/// What the client has committed so far, updated when a commit is captured.
/// Request-time checks and payload fallbacks read this, never `on_screen`.
#[derive(Clone, Debug)]
struct LatestCommitted {
    /// Newest committed buffer. `None` after a null attach, before any, and
    /// after an unmap that did not come from a commit.
    buffer: Option<crate::buffer_content::AttachedBuffer>,
    representation: ColorRepresentation,
    /// Regions in the client's own (wire) units, before normalization.
    input_region: RegionValue,
    opaque_region: RegionValue,
    blur_region: Option<RegionValue>,
}

impl Default for LatestCommitted {
    fn default() -> Self {
        Self {
            buffer: None,
            representation: ColorRepresentation::default(),
            input_region: RegionValue::infinite(),
            opaque_region: RegionValue::empty(),
            blur_region: None,
        }
    }
}

#[derive(Debug)]
pub struct WlSurface {
    id: WlSurfaceId,
    client_id: ClientId,
    role: Role,
    shell_surface: Option<ShellSurfaceObject>,
    popup_parent_layer_surface: Option<Resolved<ZwlrLayerSurfaceV1Id>>,
    version: u32,
    /// back buffer
    pending: Pending,
    /// front buffer
    on_screen: OnScreen,
    latest: LatestCommitted,
    /// Waiter for the acquire point of the queue head, while one is set up.
    acquire_waiter: Option<crate::sync::AcquireReadyWaiterId>,
    /// Makes the next waiter setup fail, to test the forced-apply path.
    #[cfg(test)]
    test_force_waiter_error: bool,
    syncobj_surface_object: Option<Resolved<WpLinuxDrmSyncobjSurfaceV1Id>>,
    fractional_scale_object: Option<Resolved<WpFractionalScaleV1Id>>,
    tearing_control_object: Option<Resolved<WpTearingControlV1Id>>,
    fifo_object: Option<Resolved<WpFifoV1Id>>,
    commit_timer_object: Option<Resolved<WpCommitTimerV1Id>>,
    alpha_modifier_object: Option<Resolved<WpAlphaModifierSurfaceV1Id>>,
    background_effect_object: Option<Resolved<ExtBackgroundEffectSurfaceV1Id>>,
    color_management_surface: Option<Resolved<WpColorManagementSurfaceV1Id>>,
    color_representation_object: Option<Resolved<WpColorRepresentationSurfaceV1Id>>,
    color_feedback: Vec<Resolved<WpColorManagementSurfaceFeedbackV1Id>>,
    /// Description id last sent on this surface's feedback objects.
    /// `None` until the first feedback object, which records the current
    /// preferred id without sending an event.
    preferred_description_sent: Option<u64>,
    /// Used when the committed description is absent. Real surfaces use the
    /// compositor's `ColorManager`; tests use the thread-local default.
    untagged_description: Rc<ImageDescription>,
    viewport_object: Option<Resolved<WpViewportId>>,
    retired_buffers: Vec<RetiredBufferCandidate>,
    /// `wl_surface.frame` callbacks to complete on the next `commit`.
    pending_frame_callbacks: Vec<Resolved<WlCallbackId>>,
    pending_release_callbacks: Vec<Resolved<WlCallbackId>>,
    pending_presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    /// Captured commit of a synchronized subsurface, merged with later ones until
    /// its parent commits (or it is set to desynchronized).
    cached_update: Option<ContentUpdate>,
    /// Number of commits captured so far. The first commit is 1.
    next_update_seq: u64,
    /// Highest `last_seq` applied or discarded for this surface.
    applied_update_seq: u64,
    /// Every update with `last_seq` up to this one was dropped (destroy, parent
    /// loss, role dismissal). Copies carried by a parent are never applied.
    dropped_through_seq: u64,
    /// Captured updates waiting to be applied, oldest first.
    queue: VecDeque<ContentUpdate>,
    /// Set by `test_hold_next_commit`; copied into the next captured update.
    #[cfg(test)]
    pending_test_hold: bool,
    /// Changes whenever this surface's role object changes.
    role_epoch: u64,
    /// Serial of the newest captured commit. `xwayland_serial` is the applied one.
    latest_xwayland_serial: Option<u64>,
    subsurface: Option<SubsurfaceState>,
    tree: SurfaceTreeState,
    output_membership: BTreeSet<OutputId>,
    active_output: Option<OutputId>,
    preferred_fractional_scale_sent: Option<u32>,
    preferred_buffer_scale_sent: Option<i32>,
    preferred_buffer_transform_sent: Option<u32>,
    pointer_constraint: Option<SurfacePointerConstraint>,
    idle_inhibitors: Vec<Resolved<ZwpIdleInhibitorV1Id>>,
    lock_surface: Option<Resolved<ExtSessionLockSurfaceV1Id>>,
    xwayland_surface_object: Option<Resolved<XwaylandSurfaceV1Id>>,
    /// Serial applied by a successful commit. `None` until then.
    xwayland_serial: Option<u64>,
    /// Set once the X window manager has placed this surface.
    xwayland_shown: bool,
    /// Map-time order of an override-redirect window. Later is higher.
    xwayland_shown_order: u64,
    input_popup: Option<Resolved<ZwpInputPopupSurfaceV2Id>>,
    /// Captured when the toplevel role is destroyed while this surface still
    /// owns the keyboard. `destroy_window` drops the managed window before
    /// `wl_surface.destroy`, so the successor search would otherwise see no close.
    closed_window_focus: Option<crate::input::focus::ClosedWindowFocus>,
}

/// Committed image description. `None` in the outer commit slot means unchanged.
pub(crate) type SurfaceColor = (Rc<ImageDescription>, RenderIntent);

/// Applies `split` to the feedbacks of an update and of every child update it
/// carries, returning the discarded ones.
fn split_update_feedbacks(
    update: &mut ContentUpdate,
    split: &impl Fn(&mut Vec<SurfacePresentationFeedback>, &State) -> Vec<SurfacePresentationFeedback>,
    state: &State,
) -> Vec<SurfacePresentationFeedback> {
    let mut discarded = split(&mut update.presentation_feedbacks, state);
    for child in &mut update.children {
        discarded.extend(split_update_feedbacks(&mut child.update, split, state));
    }
    discarded
}

fn surface_color_changed(
    current: &Option<SurfaceColor>,
    pending: &Option<Option<SurfaceColor>>,
) -> bool {
    let Some(next) = pending else {
        return false;
    };
    color_identity(current) != color_identity(next)
}

fn color_identity(color: &Option<SurfaceColor>) -> Option<(u64, RenderIntent)> {
    color
        .as_ref()
        .map(|(description, intent)| (description.id(), *intent))
}

fn committed_surface_color(
    current: &Option<SurfaceColor>,
    pending: &Option<Option<SurfaceColor>>,
) -> Option<SurfaceColor> {
    match pending {
        None => current.clone(),
        Some(next) => next.clone(),
    }
}

#[derive(Clone, Debug)]
struct Pending {
    /// `None` = no attach this commit; `Some(None)` = detach; `Some(Some(_))` = attach.
    buffer: Option<Option<crate::buffer_content::AttachedBuffer>>,
    offset: (i32, i32),
    sync: SurfaceSyncState,
    buffer_transform: BufferTransform,
    buffer_scale: i32,
    viewport: SurfaceViewport,
    presentation_hint: PresentationHint,
    /// `u32::MAX` is opaque. Kept as the raw protocol value.
    alpha_multiplier: u32,
    /// `None` means this commit does not change the description.
    /// `Some(None)` returns the surface to untagged. `Some(Some(_))` sets it.
    color: Option<Option<SurfaceColor>>,
    representation: RepresentationUpdate,
    damage_surface: Vec<DamageRect>,
    damage_buffer: Vec<DamageRect>,
    input_region: Option<RegionValue>,
    opaque_region: Option<RegionValue>,
    /// `None` means `set_region` was not called. `Some(None)` is a null region.
    pointer_constraint_region: Option<Option<RegionValue>>,
    /// `None` means this commit does not change the blur region.
    /// `Some(None)` removes it. `Some(Some(_))` sets the copied region.
    blur_region: Option<Option<RegionValue>>,
    pointer_constraint_hint: Option<(f32, f32)>,
    xwayland_serial: Option<u64>,
    /// `wp_fifo_v1.set_barrier` of this commit.
    fifo_set_barrier: bool,
    /// `wp_fifo_v1.wait_barrier` of this commit.
    fifo_wait_barrier: bool,
    /// `wp_commit_timer_v1.set_timestamp` of this commit.
    commit_target: Option<Instant>,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            buffer: None,
            offset: (0, 0),
            sync: SurfaceSyncState::default(),
            buffer_transform: BufferTransform::Normal,
            buffer_scale: 1,
            viewport: SurfaceViewport::default(),
            presentation_hint: PresentationHint::Vsync,
            alpha_multiplier: u32::MAX,
            color: None,
            representation: RepresentationUpdate::default(),
            damage_surface: Vec::new(),
            damage_buffer: Vec::new(),
            input_region: None,
            opaque_region: None,
            pointer_constraint_region: None,
            pointer_constraint_hint: None,
            blur_region: None,
            xwayland_serial: None,
            fifo_set_barrier: false,
            fifo_wait_barrier: false,
            commit_target: None,
        }
    }
}

#[derive(Clone, Debug)]
struct SurfaceCommitPayload {
    buffer: Option<Option<crate::buffer_content::AttachedBuffer>>,
    offset: (i32, i32),
    sync: SurfaceSyncState,
    buffer_transform: BufferTransform,
    buffer_scale: i32,
    viewport: SurfaceViewport,
    presentation_hint: PresentationHint,
    alpha_multiplier: u32,
    /// Resolved blur region for this commit. `None` removes the effect.
    blur_region: Option<RegionValue>,
    /// `None` means this commit does not change the description.
    /// `Some(None)` returns the surface to untagged. `Some(Some(_))` sets it.
    color: Option<Option<SurfaceColor>>,
    representation: RepresentationUpdate,
    damage_surface: Vec<DamageRect>,
    damage_buffer: Vec<DamageRect>,
    input_region: RegionValue,
    opaque_region: RegionValue,
    release_callbacks: Vec<Resolved<WlCallbackId>>,
    /// The raw wire inputs behind the normalized values above, filled only for
    /// wire-scaled clients. Travels with the payload so synchronized (cached)
    /// and queued commits store the same raw inputs they were normalized from.
    wire_inputs: Option<WireInputs>,
}

/// The raw (wire-unit) inputs of a wire-scaled client's commit: its own
/// `buffer_scale`, the viewport and the regions exactly as the client sent
/// them, plus the `WireScale` they were normalized with. Kept on the on-screen
/// state so a later phase can recompute the logical size, viewport and regions
/// when S changes, and so a commit without a new region request falls back to
/// the raw regions instead of re-normalizing the on-screen ones (M11).
#[derive(Clone, Debug)]
struct WireInputs {
    scale: WireScale,
    buffer_scale: i32,
    viewport: SurfaceViewport,
    input_region: RegionValue,
    opaque_region: RegionValue,
}

#[derive(Clone, Debug)]
struct OnScreen {
    buffer: Option<crate::buffer_content::AttachedBuffer>,
    content_revision: u64,
    x: i32,
    y: i32,
    sync: SurfaceSyncState,
    buffer_transform: BufferTransform,
    buffer_scale: i32,
    viewport: SurfaceViewport,
    presentation_hint: PresentationHint,
    alpha_multiplier: u32,
    /// `None` means the client has not asked for blur.
    blur_region: Option<RegionValue>,
    /// `None` is an untagged surface.
    color: Option<SurfaceColor>,
    representation: ColorRepresentation,
    logical_size: Option<SizeI>,
    sample_transform: transform::Mat3,
    damage: DamageRegion,
    frame_callbacks: Vec<Resolved<WlCallbackId>>,
    presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    input_region: RegionValue,
    opaque_region: RegionValue,
    release_callbacks: Vec<Resolved<WlCallbackId>>,
    /// Raw wire inputs of the last wire-scaled commit; `None` for normal clients.
    wire_inputs: Option<WireInputs>,
}

impl Default for OnScreen {
    fn default() -> Self {
        Self {
            buffer: None,
            content_revision: 0,
            x: 0,
            y: 0,
            sync: SurfaceSyncState::default(),
            buffer_transform: BufferTransform::Normal,
            buffer_scale: 1,
            viewport: SurfaceViewport::default(),
            presentation_hint: PresentationHint::Vsync,
            alpha_multiplier: u32::MAX,
            blur_region: None,
            color: None,
            representation: ColorRepresentation::default(),
            logical_size: None,
            sample_transform: transform::Mat3::identity(),
            damage: DamageRegion::new(),
            frame_callbacks: Vec::new(),
            presentation_feedbacks: Vec::new(),
            input_region: RegionValue::infinite(),
            opaque_region: RegionValue::empty(),
            release_callbacks: Vec::new(),
            wire_inputs: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SurfaceRenderState {
    pub surface: Option<Resolved<WlSurfaceId>>,
    pub buffer: crate::buffer_content::AttachedBuffer,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub tex_transform: transform::Mat3,
    pub damage: DamageRegion,
    pub opaque_region: DamageRegion,
    pub sync: SurfaceSyncState,
    pub presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    pub inherit_root_clip: bool,
    /// Raw `wp_alpha_modifier_surface_v1` factor. `u32::MAX` is opaque.
    pub alpha_multiplier: u32,
    /// Image description of the committed surface. Untagged surfaces use sRGB gamma 2.2.
    pub color_description: Rc<ImageDescription>,
    /// Perceptual when the surface is untagged.
    pub render_intent: RenderIntent,
    /// Unset fields use the phase 15 defaults.
    pub representation: ColorRepresentation,
}

#[derive(Clone, Debug)]
struct MappedBufferState {
    buffer: crate::buffer_content::AttachedBuffer,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    tex_transform: transform::Mat3,
    sync: SurfaceSyncState,
    frame_callbacks: Vec<Resolved<WlCallbackId>>,
    presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    buffer_transform: BufferTransform,
    buffer_scale: i32,
    viewport: SurfaceViewport,
    presentation_hint: PresentationHint,
    alpha_multiplier: u32,
    blur_region: Option<RegionValue>,
    color: Option<SurfaceColor>,
    representation: ColorRepresentation,
    input_region: RegionValue,
    opaque_region: RegionValue,
    release_callbacks: Vec<Resolved<WlCallbackId>>,
    /// Travels from the payload so activating a mapped state stores the raw
    /// wire inputs of the commit that produced this state.
    wire_inputs: Option<WireInputs>,
}

struct SurfaceChanges {
    buffer_changed: bool,
    mapping_changed: bool,
}

#[derive(Clone, Copy, Debug)]
struct DamageRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// Convert one `wl_surface.damage` rectangle from wire units to logical units,
/// rounding outward so no damaged pixel is missed. Invalid rects (negative
/// sizes) are kept as-is; the damage collection drops them anyway.
/// Viewport source and destination in logical pixels. The same conversion a
/// wire-scaled commit applies.
fn logical_viewport(mut viewport: SurfaceViewport, scale: WireScale) -> SurfaceViewport {
    if scale.is_one() {
        return viewport;
    }
    if let Some(src) = viewport.src.as_mut() {
        src.x = scale.to_logical_f64(src.x as f64) as f32;
        src.y = scale.to_logical_f64(src.y as f64) as f32;
        src.width = scale.to_logical_f64(src.width as f64) as f32;
        src.height = scale.to_logical_f64(src.height as f64) as f32;
    }
    if let Some(dst) = viewport.dst.as_mut() {
        dst.width = scale.size_to_logical(dst.width);
        dst.height = scale.size_to_logical(dst.height);
    }
    viewport
}

/// Input regions cover everything the wire region covers. Opaque regions stay
/// inside it. Empty rects are dropped so they do not grow or linger.
fn logical_regions(
    input: RegionValue,
    opaque: RegionValue,
    scale: WireScale,
) -> (RegionValue, RegionValue) {
    if scale.is_one() {
        return (input, opaque);
    }
    let input = scale.region_to_logical_outward(input);
    let opaque =
        opaque.map_rects(
            |rect| if rect.is_empty() { None } else { scale.rect_to_logical_inward(rect) },
        );
    (input, opaque)
}

fn damage_rect_to_logical(rect: DamageRect, scale: WireScale) -> DamageRect {
    match RectI::new(rect.x, rect.y, rect.width, rect.height) {
        None => rect,
        Some(valid) => {
            let mapped = scale.rect_to_logical_outward(valid);
            DamageRect {
                x: mapped.x,
                y: mapped.y,
                width: mapped.width,
                height: mapped.height,
            }
        }
    }
}

#[derive(Debug)]
struct DamageScaffold {
    surface_to_buffer: Vec<transform::RectF>,
    buffer_to_surface: Vec<transform::RectF>,
}

#[derive(Debug, Default)]
pub struct SurfaceOutputDelta {
    pub entered: Vec<OutputId>,
    pub left: Vec<OutputId>,
    pub active_output_changed: bool,
}

impl WlSurface {
    fn union_local_bounds(a: RectI, b: RectI) -> RectI {
        RectI::from_extents(a.x.min(b.x), a.y.min(b.y), a.x2().max(b.x2()), a.y2().max(b.y2()))
            .expect("union of non-empty rects should stay valid")
    }

    pub(crate) fn local_surface_tree_bounds_from_ref(
        state: &State,
        surface: &Self,
    ) -> Option<RectI> {
        let mut bounds = surface.logical_size().map(|size| RectI {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        });
        let child_entries = surface
            .current_stack()
            .iter()
            .filter_map(|entry| {
                let StackEntry::Child(child_surface_id) = *entry else {
                    return None;
                };
                let child_surface = child_surface_id.try_ref(state)?;
                Some((child_surface_id, child_surface.current_subsurface_position()))
            })
            .collect::<Vec<_>>();

        for (child_surface_id, (dx, dy)) in child_entries {
            let Some(child_bounds) = Self::local_surface_tree_bounds(state, child_surface_id)
            else {
                continue;
            };
            let translated = child_bounds.translate(dx, dy);
            bounds = Some(match bounds {
                Some(bounds) => Self::union_local_bounds(bounds, translated),
                None => translated,
            });
        }

        bounds
    }

    pub fn local_surface_tree_bounds(
        state: &State,
        surface: Resolved<WlSurfaceId>,
    ) -> Option<RectI> {
        let surface = surface.try_ref(state)?;
        Self::local_surface_tree_bounds_from_ref(state, surface)
    }

    pub fn new(id: WlSurfaceId, client_id: ClientId, version: u32) -> Self {
        Self {
            id,
            client_id,
            role: Role::None,
            shell_surface: None,
            popup_parent_layer_surface: None,
            version,
            pending: Pending::default(),
            on_screen: OnScreen::default(),
            latest: LatestCommitted::default(),
            acquire_waiter: None,
            #[cfg(test)]
            test_force_waiter_error: false,
            syncobj_surface_object: None,
            fractional_scale_object: None,
            tearing_control_object: None,
            fifo_object: None,
            commit_timer_object: None,
            alpha_modifier_object: None,
            background_effect_object: None,
            color_management_surface: None,
            color_representation_object: None,
            color_feedback: Vec::new(),
            preferred_description_sent: None,
            untagged_description: crate::color::ColorManager::untagged_srgb_gamma22(),
            viewport_object: None,
            retired_buffers: Vec::new(),
            pending_frame_callbacks: Vec::new(),
            pending_release_callbacks: Vec::new(),
            pending_presentation_feedbacks: Vec::new(),
            cached_update: None,
            next_update_seq: 0,
            applied_update_seq: 0,
            dropped_through_seq: 0,
            queue: VecDeque::new(),
            #[cfg(test)]
            pending_test_hold: false,
            role_epoch: 0,
            latest_xwayland_serial: None,
            subsurface: None,
            tree: SurfaceTreeState::default(),
            output_membership: BTreeSet::new(),
            active_output: None,
            preferred_fractional_scale_sent: None,
            preferred_buffer_scale_sent: None,
            preferred_buffer_transform_sent: None,
            pointer_constraint: None,
            idle_inhibitors: Vec::new(),
            lock_surface: None,
            xwayland_surface_object: None,
            xwayland_serial: None,
            xwayland_shown: false,
            xwayland_shown_order: 0,
            input_popup: None,
            closed_window_focus: None,
        }
    }

    /// Remember that this surface was the focused window, before its window is dropped.
    pub(crate) fn remember_closed_window_focus(
        &mut self,
        close: crate::input::focus::ClosedWindowFocus,
    ) {
        self.closed_window_focus = Some(close);
    }

    pub(crate) fn add_idle_inhibitor(&mut self, inhibitor: Resolved<ZwpIdleInhibitorV1Id>) {
        self.idle_inhibitors.push(inhibitor);
    }

    pub(crate) fn remove_idle_inhibitor(&mut self, inhibitor: Resolved<ZwpIdleInhibitorV1Id>) {
        self.idle_inhibitors
            .retain(|existing| *existing != inhibitor);
    }

    pub(crate) fn idle_inhibitors(&self) -> &[Resolved<ZwpIdleInhibitorV1Id>] {
        &self.idle_inhibitors
    }

    fn release_idle_inhibitors(&mut self, state: &mut State) {
        detach_destroyed_surface(std::mem::take(&mut self.idle_inhibitors), state);
    }

    pub(crate) fn pointer_constraint(&self) -> Option<SurfacePointerConstraint> {
        self.pointer_constraint
    }

    pub(crate) fn set_pointer_constraint(&mut self, constraint: SurfacePointerConstraint) {
        self.pointer_constraint = Some(constraint);
    }

    pub(crate) fn clear_pointer_constraint(&mut self, expected: SurfacePointerConstraint) {
        if self.pointer_constraint == Some(expected) {
            self.pointer_constraint = None;
            self.pending.pointer_constraint_region = None;
            self.pending.pointer_constraint_hint = None;
        }
    }

    pub(crate) fn set_pending_pointer_constraint_region(&mut self, region: Option<RegionValue>) {
        self.pending.pointer_constraint_region = Some(region);
    }

    pub(crate) fn set_pending_pointer_constraint_hint(&mut self, hint: (f32, f32)) {
        self.pending.pointer_constraint_hint = Some(hint);
    }

    pub fn set_syncobj_surface_object(
        &mut self,
        object: Resolved<WpLinuxDrmSyncobjSurfaceV1Id>,
    ) -> Result<(), ()> {
        if self.syncobj_surface_object.is_some() {
            return Err(());
        }
        self.syncobj_surface_object = Some(object);
        Ok(())
    }

    pub fn clear_syncobj_surface_object(&mut self, object: Resolved<WpLinuxDrmSyncobjSurfaceV1Id>) {
        if self.syncobj_surface_object == Some(object) {
            self.syncobj_surface_object = None;
        }
    }

    pub fn set_fractional_scale_object(
        &mut self,
        object: Resolved<WpFractionalScaleV1Id>,
    ) -> Result<(), ()> {
        if self.fractional_scale_object.is_some() {
            return Err(());
        }
        self.fractional_scale_object = Some(object);
        Ok(())
    }

    pub fn clear_fractional_scale_object(&mut self, object: Resolved<WpFractionalScaleV1Id>) {
        if self.fractional_scale_object == Some(object) {
            self.fractional_scale_object = None;
            self.preferred_fractional_scale_sent = None;
        }
    }

    pub fn has_fractional_scale_object(&self) -> bool {
        self.fractional_scale_object.is_some()
    }

    /// Detaches the `wp_fifo_v1` and `wp_commit_timer_v1` objects from this surface,
    /// which is being deleted. They stay alive and report `surface_destroyed` on their
    /// requests.
    fn detach_timing_objects(&mut self, state: &mut State) {
        // Only this surface is grabbed here: the fifo and timer handlers never
        // delete a surface.
        if let Some(fifo) = self.fifo_object.take() {
            if let Some(fifo) = fifo.try_ref_mut(state) {
                fifo.mark_surface_destroyed();
            }
        }
        if let Some(timer) = self.commit_timer_object.take() {
            if let Some(timer) = timer.try_ref_mut(state) {
                timer.mark_surface_destroyed();
            }
        }
    }

    fn cleanup_extension_objects(&mut self) {
        if self.fractional_scale_object.take().is_some() {
            self.preferred_fractional_scale_sent = None;
        }

        if self.viewport_object.take().is_some() {
            self.pending.viewport = SurfaceViewport::default();
        }

        if self.tearing_control_object.take().is_some() {
            self.pending.presentation_hint = PresentationHint::Vsync;
        }

        if self.alpha_modifier_object.take().is_some() {
            self.pending.alpha_multiplier = u32::MAX;
        }

        if self.background_effect_object.take().is_some() {
            self.pending.blur_region = Some(None);
        }

        self.color_management_surface = None;
        self.color_representation_object = None;
        self.color_feedback.clear();
        self.preferred_description_sent = None;
        self.pending.color = Some(None);

        if self.syncobj_surface_object.take().is_some() {
            self.pending.sync = SurfaceSyncState::default();
        }
    }

    pub fn set_viewport_object(&mut self, object: Resolved<WpViewportId>) -> Result<(), ()> {
        if self.viewport_object.is_some() {
            return Err(());
        }
        self.viewport_object = Some(object);
        Ok(())
    }

    pub fn clear_viewport_object(&mut self, object: Resolved<WpViewportId>) {
        if self.viewport_object == Some(object) {
            self.viewport_object = None;
            self.pending.viewport = SurfaceViewport::default();
        }
    }

    pub(crate) fn bump_role_epoch(&mut self) {
        self.role_epoch = self.role_epoch.wrapping_add(1);
    }

    #[cfg(test)]
    pub(crate) fn test_role_epoch(&self) -> u64 {
        self.role_epoch
    }

    #[cfg(test)]
    pub(crate) fn test_applied_update_seq(&self) -> u64 {
        self.applied_update_seq
    }

    /// The next captured commit is held until `test_release_head`.
    #[cfg(test)]
    pub(crate) fn test_hold_next_commit(&mut self) {
        self.pending_test_hold = true;
    }

    /// Lets the held head of the queue become ready (flush the client to apply it).
    #[cfg(test)]
    pub(crate) fn test_release_head(&mut self) {
        if let Some(head) = self.queue.front_mut() {
            head.test_hold = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn test_queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Queues `update` without flushing the client.
    #[cfg(test)]
    pub(crate) fn test_enqueue(
        this: &mut GrabbedObject<Self>,
        state: &mut State,
        surface: Resolved<WlSurfaceId>,
        update: ContentUpdate,
    ) {
        this.queue.push_back(update);
        state.commit_queues.note_enqueued(surface);
    }

    #[cfg(test)]
    fn test_payload(
        buffer: Option<Option<crate::buffer_content::AttachedBuffer>>,
    ) -> SurfaceCommitPayload {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6);
        surface.pending.buffer = buffer;
        surface.build_commit_payload(WireScale::ONE)
    }

    pub fn has_role(&self) -> bool {
        self.role != Role::None
    }

    pub fn has_cursor_role(&self) -> bool {
        self.role == Role::Cursor
    }

    pub fn has_dnd_icon_role(&self) -> bool {
        self.role == Role::DndIcon
    }

    pub fn has_lock_surface_role(&self) -> bool {
        self.role == Role::LockSurface
    }

    pub fn has_xwayland_role(&self) -> bool {
        self.role == Role::XwaylandSurface
    }

    pub fn is_hidden_xwayland_surface(&self) -> bool {
        self.has_xwayland_role() && !self.xwayland_shown
    }

    pub(crate) fn set_xwayland_shown(&mut self, shown: bool) {
        self.xwayland_shown = shown;
    }

    pub(crate) fn set_xwayland_shown_order(&mut self, order: u64) {
        self.xwayland_shown_order = order;
    }

    pub(crate) fn xwayland_shown_order(&self) -> u64 {
        self.xwayland_shown_order
    }

    pub fn has_input_popup_role(&self) -> bool {
        self.role == Role::InputPopup
    }

    /// Assign the input-popup role. Fails when the surface already has a role or a buffer.
    pub fn set_input_popup_role(&mut self) -> Result<(), ()> {
        if self.role != Role::None || self.has_attached_or_committed_buffer() {
            return Err(());
        }
        self.role = Role::InputPopup;
        self.bump_role_epoch();
        Ok(())
    }

    pub fn assign_input_popup(&mut self, popup: Resolved<ZwpInputPopupSurfaceV2Id>) {
        self.input_popup = Some(popup);
    }

    pub fn input_popup(&self) -> Option<Resolved<ZwpInputPopupSurfaceV2Id>> {
        self.input_popup
    }

    pub fn clear_input_popup_role(&mut self) {
        if self.role == Role::InputPopup {
            self.role = Role::None;
            self.input_popup = None;
            self.bump_role_epoch();
        }
    }

    pub fn lock_surface(&self) -> Option<Resolved<ExtSessionLockSurfaceV1Id>> {
        self.lock_surface
    }

    pub fn set_lock_surface(&mut self, lock_surface: Resolved<ExtSessionLockSurfaceV1Id>) {
        self.lock_surface = Some(lock_surface);
        self.bump_role_epoch();
    }

    pub fn has_pending_buffer_attach(&self) -> bool {
        matches!(self.pending.buffer, Some(Some(_)))
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn has_committed_buffer(&self) -> bool {
        self.on_screen.buffer.is_some()
    }

    pub(crate) fn assign_lock_surface_role(&mut self) -> bool {
        if self.role != Role::None {
            return false;
        }
        self.role = Role::LockSurface;
        self.bump_role_epoch();
        true
    }

    pub(crate) fn assign_xwayland_role(&mut self) -> bool {
        if self.role != Role::None && self.role != Role::XwaylandSurface {
            return false;
        }
        self.role = Role::XwaylandSurface;
        self.bump_role_epoch();
        true
    }

    pub fn xwayland_surface_object(&self) -> Option<Resolved<XwaylandSurfaceV1Id>> {
        self.xwayland_surface_object
    }

    pub(crate) fn set_xwayland_surface_object(&mut self, object: Resolved<XwaylandSurfaceV1Id>) {
        self.xwayland_surface_object = Some(object);
    }

    pub(crate) fn clear_xwayland_surface_object(&mut self, object: Resolved<XwaylandSurfaceV1Id>) {
        if self.xwayland_surface_object == Some(object) {
            self.xwayland_surface_object = None;
        }
    }

    pub fn xwayland_serial(&self) -> Option<u64> {
        self.xwayland_serial
    }

    pub(crate) fn latest_xwayland_serial(&self) -> Option<u64> {
        self.latest_xwayland_serial
    }

    #[cfg(test)]
    pub(crate) fn test_set_xwayland_serial(&mut self, serial: u64) {
        self.xwayland_serial = Some(serial);
    }

    pub(crate) fn set_pending_xwayland_serial(&mut self, serial: u64) {
        self.pending.xwayland_serial = Some(serial);
    }

    pub(crate) fn set_pending_fifo_barrier(&mut self) {
        self.pending.fifo_set_barrier = true;
    }

    pub(crate) fn set_pending_fifo_wait(&mut self) {
        self.pending.fifo_wait_barrier = true;
    }

    pub(crate) fn has_pending_commit_target(&self) -> bool {
        self.pending.commit_target.is_some()
    }

    pub(crate) fn set_pending_commit_target(&mut self, at: Instant) {
        self.pending.commit_target = Some(at);
    }

    pub(crate) fn pending_xwayland_serial(&self) -> Option<u64> {
        self.pending.xwayland_serial
    }

    fn apply_xwayland_serial(
        &mut self,
        client: &Client,
        surface: Resolved<WlSurfaceId>,
        serial: Option<u64>,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(serial) = serial else {
            return;
        };
        self.xwayland_serial = Some(serial);
        dispatcher.dispatch(XwaylandAction::Surface {
            client: client.client_id(),
            event: SurfaceEvent::SerialAssigned { serial, surface },
        });
    }

    pub fn has_attached_or_committed_buffer(&self) -> bool {
        self.has_pending_buffer_attach() || self.latest.buffer.is_some()
    }

    pub fn xdg_surface(&self) -> Option<Resolved<XdgSurfaceId>> {
        match self.shell_surface {
            Some(ShellSurfaceObject::XdgSurface(id)) => Some(id),
            _ => None,
        }
    }

    pub fn layer_surface(&self) -> Option<Resolved<ZwlrLayerSurfaceV1Id>> {
        match self.shell_surface {
            Some(ShellSurfaceObject::LayerSurface(id)) => Some(id),
            _ => None,
        }
    }

    pub fn clear_xdg_surface_role(&mut self, xdg_surface: Resolved<XdgSurfaceId>) {
        if self.shell_surface == Some(ShellSurfaceObject::XdgSurface(xdg_surface)) {
            self.shell_surface = None;
            self.popup_parent_layer_surface = None;
            self.bump_role_epoch();
            if self.role == Role::XdgSurface {
                self.role = Role::None;
            }
        }
    }

    pub fn clear_layer_surface_role(&mut self, layer_surface: Resolved<ZwlrLayerSurfaceV1Id>) {
        if self.shell_surface == Some(ShellSurfaceObject::LayerSurface(layer_surface)) {
            self.shell_surface = None;
            self.bump_role_epoch();
            if self.role == Role::LayerSurface {
                self.role = Role::None;
            }
        }
    }

    pub fn set_popup_parent_layer_surface(
        &mut self,
        layer_surface: Option<Resolved<ZwlrLayerSurfaceV1Id>>,
    ) {
        self.popup_parent_layer_surface = layer_surface;
    }

    pub fn popup_parent_layer_surface(&self) -> Option<Resolved<ZwlrLayerSurfaceV1Id>> {
        self.popup_parent_layer_surface
    }

    pub(crate) fn assign_subsurface(
        &mut self,
        object: Resolved<WlSubsurfaceId>,
        parent_surface: Resolved<WlSurfaceId>,
    ) {
        self.role = Role::Subsurface;
        self.bump_role_epoch();
        self.subsurface = Some(SubsurfaceState {
            object,
            parent_surface,
            pending_position: (0, 0),
            current_position: (0, 0),
            sync_mode: SubsurfaceSyncMode::Sync,
        });
    }

    pub fn subsurface_object(&self) -> Option<Resolved<WlSubsurfaceId>> {
        self.subsurface.as_ref().map(|subsurface| subsurface.object)
    }

    pub fn parent_surface(&self) -> Option<&Resolved<WlSurfaceId>> {
        self.subsurface
            .as_ref()
            .map(|subsurface| &subsurface.parent_surface)
    }

    pub fn is_root(&self) -> bool {
        self.subsurface.is_none()
    }

    pub(crate) fn current_stack(&self) -> &[StackEntry] {
        &self.tree.current_stack
    }

    pub fn current_subsurface_position(&self) -> (i32, i32) {
        self.subsurface
            .as_ref()
            .map(|subsurface| subsurface.current_position)
            .unwrap_or((0, 0))
    }

    pub fn root_position(&self) -> (i32, i32) {
        (self.on_screen.x, self.on_screen.y)
    }

    pub fn content_revision(&self) -> u64 {
        self.on_screen.content_revision
    }

    pub fn active_output_id(&self) -> Option<OutputId> {
        self.active_output
    }

    pub fn contains_output(&self, output_id: OutputId) -> bool {
        self.output_membership.contains(&output_id)
    }

    pub fn output_membership_ids(&self) -> Vec<OutputId> {
        self.output_membership.iter().copied().collect()
    }

    pub fn update_output_membership(
        &mut self,
        outputs_in_priority_order: Vec<OutputId>,
    ) -> SurfaceOutputDelta {
        let mut next_membership = BTreeSet::new();
        let mut next_active_output = None;
        for output_id in outputs_in_priority_order {
            if next_membership.insert(output_id) && next_active_output.is_none() {
                next_active_output = Some(output_id);
            }
        }

        let left = self
            .output_membership
            .difference(&next_membership)
            .copied()
            .collect();
        let entered = next_membership
            .difference(&self.output_membership)
            .copied()
            .collect();
        let active_output_changed = self.active_output != next_active_output;

        self.output_membership = next_membership;
        self.active_output = next_active_output;

        SurfaceOutputDelta { entered, left, active_output_changed }
    }

    pub fn send_output_enter(
        &self,
        output: Resolved<WlOutputId>,
        dispatcher: &mut ClientDispatcher,
    ) {
        log::debug!("sending wl_surface.enter surface={:?} output={:?}", self.id, output);
        self.send_event(wl_surface::Event::Enter { output: output.wayland_id() }, dispatcher);
    }

    pub fn send_output_leave(
        &self,
        output: Resolved<WlOutputId>,
        dispatcher: &mut ClientDispatcher,
    ) {
        log::debug!("sending wl_surface.leave surface={:?} output={:?}", self.id, output);
        self.send_event(wl_surface::Event::Leave { output: output.wayland_id() }, dispatcher);
    }

    pub fn preferred_output_id_for_object(
        &self,
        state: &State,
        surface: Resolved<WlSurfaceId>,
    ) -> Option<OutputId> {
        state.output_id_for_surface_object(surface, self)
    }

    pub fn send_preferred_buffer_state_for_object(
        &mut self,
        state: &State,
        preferred_output_id: Option<OutputId>,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_preferred_buffer_state_with_output(preferred_output_id, state, dispatcher);
    }

    fn send_preferred_buffer_state_with_output(
        &mut self,
        preferred_output_id: Option<OutputId>,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(output) =
            preferred_output_id.and_then(|output_id| state.outputs.try_get(output_id))
        else {
            return;
        };
        self.send_preferred_buffer_state_for_output(
            output.legacy_scale().max(1),
            output.transform as u32,
            dispatcher,
        );
    }

    pub fn send_preferred_fractional_scale_for_object(
        &mut self,
        state: &State,
        preferred_output_id: Option<OutputId>,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_preferred_fractional_scale_with_output(preferred_output_id, state, dispatcher);
    }

    fn send_preferred_fractional_scale_with_output(
        &mut self,
        preferred_output_id: Option<OutputId>,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(output) =
            preferred_output_id.and_then(|output_id| state.outputs.try_get(output_id))
        else {
            return;
        };
        self.send_preferred_fractional_scale_for_output(output.scale, state, dispatcher);
    }

    pub fn send_preferred_fractional_scale_for_output(
        &mut self,
        scale: FractionalScale,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(object_id) = self.fractional_scale_object else {
            return;
        };
        // A wire-scaled client renders its buffers at scale 1, so advertise the
        // 1.0 scale (120/120) instead of the output's fractional scale.
        let scale = if dispatcher.wire_scale().is_one() {
            scale.as_v120()
        } else {
            FractionalScale::ONE.as_v120()
        };
        if self.preferred_fractional_scale_sent == Some(scale) {
            return;
        }
        let Some(object) = object_id.try_ref(state) else {
            return;
        };
        object.send_preferred_scale(scale, dispatcher);
        self.preferred_fractional_scale_sent = Some(scale);
    }

    pub fn send_preferred_buffer_state_for_output(
        &mut self,
        scale: i32,
        transform: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        // A wire-scaled client renders its buffers at scale 1 (tema applies S
        // when reading them), so advertise 1 regardless of the output scale.
        let scale = if dispatcher.wire_scale().is_one() { scale } else { 1 };
        if self.version >= wl_surface::PREFERRED_BUFFER_SCALE_SINCE {
            if self.preferred_buffer_scale_sent != Some(scale) {
                self.send_event(
                    wl_surface::Event::PreferredBufferScale { factor: scale },
                    dispatcher,
                );
                self.preferred_buffer_scale_sent = Some(scale);
            }
        }

        if self.version >= wl_surface::PREFERRED_BUFFER_TRANSFORM_SINCE {
            if self.preferred_buffer_transform_sent != Some(transform) {
                self.send_event(
                    wl_surface::Event::PreferredBufferTransform { transform },
                    dispatcher,
                );
                self.preferred_buffer_transform_sent = Some(transform);
            }
        }
    }

    pub fn set_root_position(&mut self, x: i32, y: i32) {
        self.on_screen.x = x;
        self.on_screen.y = y;
    }

    fn pending_subsurface_position(&self) -> (i32, i32) {
        self.subsurface
            .as_ref()
            .map(|subsurface| subsurface.pending_position)
            .unwrap_or((0, 0))
    }

    fn sync_mode(&self) -> Option<SubsurfaceSyncMode> {
        self.subsurface
            .as_ref()
            .map(|subsurface| subsurface.sync_mode)
    }

    pub fn set_subsurface_position(&mut self, x: i32, y: i32) {
        if let Some(subsurface) = self.subsurface.as_mut() {
            subsurface.pending_position = (x, y);
        }
    }

    pub fn set_subsurface_sync_mode(&mut self, mode: bool) {
        if let Some(subsurface) = self.subsurface.as_mut() {
            subsurface.sync_mode =
                if mode { SubsurfaceSyncMode::Sync } else { SubsurfaceSyncMode::Desync };
        }
    }

    pub fn destroy_subsurface_role(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        if let Some(update) = self.cached_update.take() {
            content_update::discard_update(
                client,
                state,
                dispatcher,
                &mut self.applied_update_seq,
                update,
            );
        }
        self.subsurface = None;
        if self.role == Role::Subsurface {
            self.role = Role::None;
            self.bump_role_epoch();
        }
    }

    pub fn add_pending_child(&mut self, child_surface: Resolved<WlSurfaceId>) {
        self.ensure_pending_stack_parent();
        Self::remove_child_from_stack(&mut self.tree.pending_stack, child_surface);
        let parent_index = self
            .tree
            .pending_stack
            .iter()
            .position(|entry| *entry == StackEntry::Parent)
            .unwrap_or(self.tree.pending_stack.len());
        self.tree
            .pending_stack
            .insert(parent_index, StackEntry::Child(child_surface));
    }

    pub fn remove_child_from_tree(&mut self, child_surface: Resolved<WlSurfaceId>) {
        Self::remove_child_from_stack(&mut self.tree.pending_stack, child_surface);
        Self::remove_child_from_stack(&mut self.tree.current_stack, child_surface);
        self.ensure_pending_stack_parent();
        self.ensure_current_stack_parent();
    }

    pub fn restack_child_relative(
        &mut self,
        child_surface: Resolved<WlSurfaceId>,
        sibling_surface: Option<Resolved<WlSurfaceId>>,
        place_above: bool,
    ) -> bool {
        self.ensure_pending_stack_parent();
        let target = match sibling_surface {
            Some(surface) => StackEntry::Child(surface),
            None => StackEntry::Parent,
        };
        if !self.tree.pending_stack.contains(&target) {
            return false;
        }
        Self::remove_child_from_stack(&mut self.tree.pending_stack, child_surface);
        let Some(index) = self
            .tree
            .pending_stack
            .iter()
            .position(|entry| *entry == target)
        else {
            return false;
        };
        let insert_at = if place_above { index + 1 } else { index };
        self.tree
            .pending_stack
            .insert(insert_at, StackEntry::Child(child_surface));
        true
    }

    pub fn is_ancestor_of(
        &self,
        ancestor: Resolved<WlSurfaceId>,
        state: &State,
        descendant: Resolved<WlSurfaceId>,
    ) -> bool {
        let mut current = Some(descendant);
        while let Some(surface) = current {
            if surface.object_id() == ancestor.object_id() {
                return true;
            }
            let Some(surface) = surface.try_ref(state) else {
                return false;
            };
            current = surface.parent_surface().copied();
        }
        false
    }

    fn send_viewport_error(
        &self,
        client: &Client,
        dispatcher: &mut ClientDispatcher,
        code: u32,
        message: impl AsRef<str>,
    ) {
        let Some(viewport) = self.viewport_object else {
            log::warn!(
                "viewport protocol error without viewport object on wl_surface {:?}: {}",
                self.id,
                message.as_ref()
            );
            return;
        };
        client.send_protocol_error(dispatcher, viewport.wayland_id().into(), code, message);
    }

    pub fn pending_sync_mut(&mut self) -> &mut SurfaceSyncState {
        &mut self.pending.sync
    }

    pub fn pending_viewport_mut(&mut self) -> &mut SurfaceViewport {
        &mut self.pending.viewport
    }

    pub fn has_syncobj_surface_object(&self) -> bool {
        self.syncobj_surface_object.is_some()
    }

    pub fn has_viewport_object(&self) -> bool {
        self.viewport_object.is_some()
    }

    pub fn set_tearing_control_object(
        &mut self,
        object: Resolved<WpTearingControlV1Id>,
    ) -> Result<(), ()> {
        if self.tearing_control_object.is_some() {
            return Err(());
        }
        self.tearing_control_object = Some(object);
        Ok(())
    }

    pub fn clear_tearing_control_object(&mut self, object: Resolved<WpTearingControlV1Id>) {
        if self.tearing_control_object == Some(object) {
            self.tearing_control_object = None;
            self.pending.presentation_hint = PresentationHint::Vsync;
        }
    }

    pub fn has_tearing_control_object(&self) -> bool {
        self.tearing_control_object.is_some()
    }

    pub(crate) fn set_fifo_object(&mut self, object: Resolved<WpFifoV1Id>) {
        self.fifo_object = Some(object);
    }

    pub(crate) fn clear_fifo_object(&mut self, object: Resolved<WpFifoV1Id>) {
        if self.fifo_object == Some(object) {
            self.fifo_object = None;
        }
    }

    pub(crate) fn has_fifo_object(&self) -> bool {
        self.fifo_object.is_some()
    }

    pub(crate) fn set_commit_timer_object(&mut self, object: Resolved<WpCommitTimerV1Id>) {
        self.commit_timer_object = Some(object);
    }

    pub(crate) fn clear_commit_timer_object(&mut self, object: Resolved<WpCommitTimerV1Id>) {
        if self.commit_timer_object == Some(object) {
            self.commit_timer_object = None;
        }
    }

    pub(crate) fn has_commit_timer_object(&self) -> bool {
        self.commit_timer_object.is_some()
    }

    pub fn set_alpha_modifier_object(
        &mut self,
        object: Resolved<WpAlphaModifierSurfaceV1Id>,
    ) -> Result<(), ()> {
        if self.alpha_modifier_object.is_some() {
            return Err(());
        }
        self.alpha_modifier_object = Some(object);
        Ok(())
    }

    pub fn clear_alpha_modifier_object(&mut self, object: Resolved<WpAlphaModifierSurfaceV1Id>) {
        if self.alpha_modifier_object == Some(object) {
            self.alpha_modifier_object = None;
            self.pending.alpha_multiplier = u32::MAX;
        }
    }

    pub fn has_alpha_modifier_object(&self) -> bool {
        self.alpha_modifier_object.is_some()
    }

    pub fn set_background_effect_object(
        &mut self,
        object: Resolved<ExtBackgroundEffectSurfaceV1Id>,
    ) -> Result<(), ()> {
        if self.background_effect_object.is_some() {
            return Err(());
        }
        self.background_effect_object = Some(object);
        Ok(())
    }

    pub fn clear_background_effect_object(
        &mut self,
        object: Resolved<ExtBackgroundEffectSurfaceV1Id>,
    ) {
        if self.background_effect_object == Some(object) {
            self.background_effect_object = None;
            self.pending.blur_region = Some(None);
        }
    }

    pub fn has_background_effect_object(&self) -> bool {
        self.background_effect_object.is_some()
    }

    pub(crate) fn set_pending_blur_region(&mut self, region: Option<RegionValue>) {
        self.pending.blur_region = Some(region);
    }

    pub(crate) fn committed_blur_region(&self) -> Option<RegionValue> {
        self.on_screen.blur_region.clone()
    }

    #[cfg(test)]
    pub(crate) fn test_set_committed_blur_region(&mut self, region: Option<RegionValue>) {
        self.on_screen.blur_region = region;
    }

    pub(crate) fn set_pending_alpha_multiplier(&mut self, factor: u32) {
        self.pending.alpha_multiplier = factor;
    }

    pub(crate) fn set_color_management_surface(
        &mut self,
        object: Resolved<WpColorManagementSurfaceV1Id>,
    ) {
        self.color_management_surface = Some(object);
    }

    pub(crate) fn clear_color_management_surface(
        &mut self,
        object: Resolved<WpColorManagementSurfaceV1Id>,
    ) {
        if self.color_management_surface == Some(object) {
            self.color_management_surface = None;
            self.pending.color = Some(None);
        }
    }

    pub(crate) fn has_color_management_surface(&self) -> bool {
        self.color_management_surface.is_some()
    }

    pub(crate) fn set_color_representation_object(
        &mut self,
        object: Resolved<WpColorRepresentationSurfaceV1Id>,
    ) -> Result<(), ()> {
        if self.color_representation_object.is_some() {
            return Err(());
        }
        self.color_representation_object = Some(object);
        Ok(())
    }

    pub(crate) fn clear_color_representation_object(
        &mut self,
        object: Resolved<WpColorRepresentationSurfaceV1Id>,
    ) {
        if self.color_representation_object == Some(object) {
            self.color_representation_object = None;
        }
    }

    pub(crate) fn has_color_representation_object(&self) -> bool {
        self.color_representation_object.is_some()
    }

    pub(crate) fn set_pending_alpha_mode(&mut self, mode: Option<crate::color::AlphaMode>) {
        self.pending.representation.alpha_mode = Some(mode);
    }

    pub(crate) fn set_pending_coefficients(
        &mut self,
        coefficients: Option<crate::color::Coefficients>,
    ) {
        self.pending.representation.coefficients = Some(coefficients);
    }

    pub(crate) fn set_pending_chroma(&mut self, site: Option<crate::color::ChromaSite>) {
        self.pending.representation.chroma = Some(site);
    }

    pub(crate) fn unset_pending_representation(&mut self) {
        self.pending.representation = RepresentationUpdate::unset();
    }

    pub(crate) fn committed_representation(&self) -> ColorRepresentation {
        self.on_screen.representation
    }

    pub(crate) fn set_pending_color(&mut self, color: Option<SurfaceColor>) {
        self.pending.color = Some(color);
    }

    pub(crate) fn committed_color(&self) -> Option<(u64, RenderIntent)> {
        color_identity(&self.on_screen.color)
    }

    pub(crate) fn add_color_feedback(
        &mut self,
        feedback: Resolved<WpColorManagementSurfaceFeedbackV1Id>,
        preferred_id: u64,
    ) {
        self.color_feedback.push(feedback);
        self.preferred_description_sent = Some(preferred_id);
    }

    pub(crate) fn remove_color_feedback(
        &mut self,
        feedback: Resolved<WpColorManagementSurfaceFeedbackV1Id>,
    ) {
        self.color_feedback.retain(|existing| *existing != feedback);
        if self.color_feedback.is_empty() {
            self.preferred_description_sent = None;
        }
    }

    pub(crate) fn preferred_image_description(&self, state: &State) -> Rc<ImageDescription> {
        self.active_output
            .and_then(|output_id| state.outputs.try_get(output_id))
            .map(|output| Rc::clone(&output.color_description))
            .unwrap_or_else(|| state.color_manager.srgb_gamma22())
    }

    pub(crate) fn send_preferred_color_description(
        &mut self,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) {
        if self.color_feedback.is_empty() {
            return;
        }
        let identity = self.preferred_image_description(state).id();
        if self.preferred_description_sent == Some(identity) {
            return;
        }
        self.preferred_description_sent = Some(identity);
        for feedback in self.color_feedback.clone() {
            if let Some(feedback) = feedback.try_ref(state) {
                feedback.send_preferred_changed(identity, dispatcher);
            }
        }
    }

    pub(crate) fn set_untagged_description(&mut self, description: Rc<ImageDescription>) {
        self.untagged_description = description;
    }

    pub(crate) fn committed_alpha_multiplier(&self) -> u32 {
        self.on_screen.alpha_multiplier
    }

    pub(crate) fn set_pending_presentation_hint(&mut self, hint: PresentationHint) {
        self.pending.presentation_hint = hint;
    }

    pub(crate) fn committed_presentation_hint(&self) -> PresentationHint {
        self.on_screen.presentation_hint
    }

    pub(crate) fn test_set_committed_presentation_hint(&mut self, hint: PresentationHint) {
        self.on_screen.presentation_hint = hint;
    }

    fn send_syncobj_surface_error(
        &self,
        client: &Client,
        dispatcher: &mut ClientDispatcher,
        code: u32,
        message: impl AsRef<str>,
    ) {
        let Some(syncobj_surface) = self.syncobj_surface_object else {
            log::warn!(
                "syncobj protocol error without syncobj surface object on wl_surface {:?}: {}",
                self.id,
                message.as_ref()
            );
            return;
        };
        client.send_protocol_error(dispatcher, syncobj_surface.wayland_id().into(), code, message);
    }

    pub fn set_role(
        &mut self,
        new_role: Role,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<(), xdg_wm_base::Error> {
        if self.role != Role::None && self.role != new_role {
            self.send_error(
                dispatcher,
                xdg_wm_base::Error::Role as u32,
                "invalid xdg_surface role",
            );
            return Err(xdg_wm_base::Error::Role);
        }
        self.role = new_role;
        self.bump_role_epoch();
        Ok(())
    }

    pub fn set_cursor_role(
        &mut self,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<(), wl_pointer::Error> {
        if self.role != Role::None && self.role != Role::Cursor {
            self.send_error(dispatcher, wl_pointer::Error::Role as u32, "invalid cursor role");
            return Err(wl_pointer::Error::Role);
        }
        self.role = Role::Cursor;
        self.bump_role_epoch();
        Ok(())
    }

    pub fn set_dnd_icon_role(&mut self) -> Result<(), ()> {
        if self.role != Role::None && self.role != Role::DndIcon {
            return Err(());
        }
        self.role = Role::DndIcon;
        self.bump_role_epoch();
        Ok(())
    }

    pub fn set_xdg_surface_impl(&mut self, xdg_surface: Resolved<XdgSurfaceId>) {
        self.shell_surface = Some(ShellSurfaceObject::XdgSurface(xdg_surface));
        self.bump_role_epoch();
    }

    pub fn set_layer_surface_impl(&mut self, layer_surface: Resolved<ZwlrLayerSurfaceV1Id>) {
        self.shell_surface = Some(ShellSurfaceObject::LayerSurface(layer_surface));
        self.bump_role_epoch();
    }

    fn on_attach(
        &mut self,
        client: &mut Client,
        state: &mut State,
        buffer_id: WlBufferId,
        x: i32,
        y: i32,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<(), wl_surface::Error> {
        if self.version >= wl_surface::OFFSET_SINCE {
            if x != 0 || y != 0 {
                self.send_error(
                    dispatcher,
                    wl_surface::Error::InvalidOffset as u32,
                    "non-zero attach offset not allowed",
                );
                return Err(wl_surface::Error::InvalidOffset);
            }
        } else {
            self.pending.offset = (x, y);
        }

        let Some(buffer_id) = buffer_id.to_opt() else {
            self.pending.buffer = Some(None);
            return Ok(());
        };

        let buffer = match client.resolve_or_disconnect(buffer_id, state, dispatcher) {
            Ok(buffer) => buffer,
            Err(_) => return Ok(()),
        };
        let Some(attached) = crate::buffer_content::AttachedBuffer::resolve(state, buffer) else {
            return Ok(());
        };
        self.pending.buffer = Some(Some(attached));

        Ok(())
    }

    pub(crate) fn snapshot_region_request(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        region_id: WlRegionId,
        null_region: RegionValue,
    ) -> Option<RegionValue> {
        let Some(region_id) = region_id.to_opt() else {
            return Some(null_region);
        };
        let region = match client.resolve_or_disconnect(region_id, state, dispatcher) {
            Ok(region) => region,
            Err(_) => return None,
        };
        Some(region.rref(state).snapshot())
    }

    fn on_set_opaque_region(
        &mut self,
        client: &mut Client,
        state: &mut State,
        region_id: WlRegionId,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(region) = Self::snapshot_region_request(
            client,
            state,
            dispatcher,
            region_id,
            RegionValue::empty(),
        ) else {
            return;
        };
        self.pending.opaque_region = Some(region);
    }

    fn on_set_input_region(
        &mut self,
        client: &mut Client,
        state: &mut State,
        region_id: WlRegionId,
        dispatcher: &mut ClientDispatcher,
    ) {
        let Some(region) = Self::snapshot_region_request(
            client,
            state,
            dispatcher,
            region_id,
            RegionValue::infinite(),
        ) else {
            return;
        };
        self.pending.input_region = Some(region);
    }

    fn ensure_pending_stack_parent(&mut self) {
        if !self.tree.pending_stack.contains(&StackEntry::Parent) {
            self.tree.pending_stack.insert(0, StackEntry::Parent);
        }
    }

    fn ensure_current_stack_parent(&mut self) {
        if !self.tree.current_stack.contains(&StackEntry::Parent) {
            self.tree.current_stack.insert(0, StackEntry::Parent);
        }
    }

    fn remove_child_from_stack(stack: &mut Vec<StackEntry>, child_surface: Resolved<WlSurfaceId>) {
        stack.retain(|entry| *entry != StackEntry::Child(child_surface));
    }

    fn child_surface_ids(&self) -> Vec<Resolved<WlSurfaceId>> {
        let mut children = Vec::new();
        for stack in [&self.tree.pending_stack, &self.tree.current_stack] {
            for entry in stack {
                let StackEntry::Child(child_surface_id) = *entry else {
                    continue;
                };
                if !children.contains(&child_surface_id) {
                    children.push(child_surface_id);
                }
            }
        }
        children
    }

    pub(crate) fn unmap_due_to_parent_loss(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.discard_all_presentation_feedbacks(client, state, dispatcher);
        self.drop_queued_updates(client, state, dispatcher);
        self.retire_current();
        self.on_screen = OnScreen {
            buffer: None,
            content_revision: self.next_content_revision(),
            x: self.on_screen.x,
            y: self.on_screen.y,
            sync: SurfaceSyncState::default(),
            buffer_transform: self.on_screen.buffer_transform,
            buffer_scale: self.on_screen.buffer_scale,
            viewport: self.on_screen.viewport,
            presentation_hint: self.on_screen.presentation_hint,
            alpha_multiplier: self.on_screen.alpha_multiplier,
            blur_region: self.on_screen.blur_region.clone(),
            color: self.on_screen.color.clone(),
            representation: self.on_screen.representation,
            logical_size: None,
            sample_transform: transform::Mat3::identity(),
            damage: DamageRegion::new(),
            frame_callbacks: std::mem::take(&mut self.on_screen.frame_callbacks),
            presentation_feedbacks: Vec::new(),
            input_region: self.on_screen.input_region.clone(),
            opaque_region: self.on_screen.opaque_region.clone(),
            release_callbacks: Vec::new(),
            wire_inputs: self.on_screen.wire_inputs.clone(),
        };
        self.latest.buffer = None;
    }

    /// Build the payload from the pending state. A region this commit does not
    /// set falls back to the latest committed region, in the client's own
    /// (wire) units: the payload is built before normalization, so a
    /// wire-scaled client's fallback is never scaled a second time.
    fn build_commit_payload(&mut self, _wire_scale: WireScale) -> SurfaceCommitPayload {
        let input_region = self
            .pending
            .input_region
            .take()
            .unwrap_or_else(|| self.latest.input_region.clone());
        let opaque_region = self
            .pending
            .opaque_region
            .take()
            .unwrap_or_else(|| self.latest.opaque_region.clone());
        let blur_region = match self.pending.blur_region.take() {
            None => self.latest.blur_region.clone(),
            Some(next) => next,
        };
        let payload = SurfaceCommitPayload {
            buffer: self.pending.buffer.take(),
            offset: std::mem::take(&mut self.pending.offset),
            sync: std::mem::take(&mut self.pending.sync),
            buffer_transform: self.pending.buffer_transform,
            buffer_scale: self.pending.buffer_scale,
            viewport: self.pending.viewport,
            presentation_hint: self.pending.presentation_hint,
            alpha_multiplier: self.pending.alpha_multiplier,
            blur_region,
            color: self.pending.color.take(),
            representation: std::mem::take(&mut self.pending.representation),
            damage_surface: std::mem::take(&mut self.pending.damage_surface),
            damage_buffer: std::mem::take(&mut self.pending.damage_buffer),
            input_region,
            opaque_region,
            release_callbacks: std::mem::take(&mut self.pending_release_callbacks),
            wire_inputs: None,
        };
        self.latest.input_region = payload.input_region.clone();
        self.latest.opaque_region = payload.opaque_region.clone();
        self.latest.blur_region = payload.blur_region.clone();
        match &payload.buffer {
            Some(Some(buffer)) => self.latest.buffer = Some(buffer.clone()),
            Some(None) => self.latest.buffer = None,
            None => {}
        }
        self.latest.representation = payload.representation.apply(self.latest.representation);
        payload
    }

    /// Normalize a payload built for a wire-scaled client from wire units to
    /// logical units, once, right after it is built. Everything downstream -
    /// the effective buffer scale and logical size, damage, hit-testing and the
    /// renderer - then works in logical units with no further changes. Buffer
    /// damage needs no conversion: it maps through the effective buffer scale.
    /// The raw values are stored first so a later phase can recompute with a
    /// new S. Returns the payload unchanged for a normal client.
    fn normalize_commit_payload(
        mut payload: SurfaceCommitPayload,
        scale: WireScale,
    ) -> SurfaceCommitPayload {
        if scale.is_one() {
            return payload;
        }
        let raw_buffer_scale = payload.buffer_scale;
        let raw_viewport = payload.viewport;
        let raw_input = payload.input_region.clone();
        let raw_opaque = payload.opaque_region.clone();
        payload.wire_inputs = Some(WireInputs {
            scale,
            buffer_scale: raw_buffer_scale,
            viewport: raw_viewport,
            input_region: raw_input.clone(),
            opaque_region: raw_opaque.clone(),
        });
        // Effective buffer scale: the client renders at scale 1 (or its own
        // scale), tema reads the buffer as if it were S times denser.
        payload.buffer_scale = raw_buffer_scale.saturating_mul(scale.get() as i32);
        payload.viewport = logical_viewport(raw_viewport, scale);
        payload.damage_surface = payload
            .damage_surface
            .iter()
            .map(|rect| damage_rect_to_logical(*rect, scale))
            .collect();
        payload.offset =
            (scale.pos_to_logical(payload.offset.0), scale.pos_to_logical(payload.offset.1));
        (payload.input_region, payload.opaque_region) =
            logical_regions(raw_input, raw_opaque, scale);
        payload
    }

    /// Recompute the on-screen logical size, viewport and regions for a new
    /// wire scale, without a new commit. A commit made at scale 1 has no stored
    /// raw inputs: the on-screen values are those inputs. Returns whether the
    /// on-screen state changed.
    pub(crate) fn recompute_for_wire_scale(&mut self, _state: &State, scale: WireScale) -> bool {
        if self
            .on_screen
            .wire_inputs
            .as_ref()
            .is_some_and(|inputs| inputs.scale == scale)
        {
            return false;
        }
        if self.on_screen.wire_inputs.is_none() && scale.is_one() {
            return false;
        }
        let raw = self.on_screen.wire_inputs.clone().unwrap_or(WireInputs {
            scale: WireScale::ONE,
            buffer_scale: self.on_screen.buffer_scale,
            viewport: self.on_screen.viewport,
            input_region: self.on_screen.input_region.clone(),
            opaque_region: self.on_screen.opaque_region.clone(),
        });
        let buffer_scale = raw.buffer_scale.saturating_mul(scale.get() as i32);
        let viewport = logical_viewport(raw.viewport, scale);
        if let Some(attached) = self.on_screen.buffer.clone() {
            let (width, height) = attached.content.dimensions();
            let buffer_size = SizeI { width, height };
            self.on_screen.logical_size = Some(transform::derive_logical_size(
                buffer_size,
                self.on_screen.buffer_transform,
                buffer_scale,
                viewport,
            ));
            self.on_screen.sample_transform = transform::surface_sample_transform(
                buffer_size,
                self.on_screen.buffer_transform,
                buffer_scale,
                viewport,
            );
        }
        let (input_region, opaque_region) =
            logical_regions(raw.input_region.clone(), raw.opaque_region.clone(), scale);
        self.on_screen.buffer_scale = buffer_scale;
        self.on_screen.viewport = viewport;
        self.on_screen.input_region = input_region;
        self.on_screen.opaque_region = opaque_region;
        if scale.is_one() {
            self.on_screen.wire_inputs = None;
        } else {
            self.on_screen.wire_inputs = Some(WireInputs { scale, ..raw });
        }
        true
    }

    /// Captures the commit into the synchronized cache. A cached update takes
    /// the newer commit's state on top of its own; the returned feedback is the
    /// superseded one, which the caller discards.
    fn merge_cached_update(&mut self, update: ContentUpdate) -> Vec<SurfacePresentationFeedback> {
        match self.cached_update.as_mut() {
            Some(cached) => cached.merge(update, &mut self.retired_buffers),
            None => {
                self.cached_update = Some(update);
                Vec::new()
            }
        }
    }

    fn clear_failed_surface_commit(&mut self) {
        self.pending = Pending::default();
    }

    fn reject_release_callbacks_without_buffer(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) -> bool {
        if self.pending_release_callbacks.is_empty() || matches!(self.pending.buffer, Some(Some(_)))
        {
            return false;
        }
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wl_surface::Error::NoBuffer as u32,
            "wl_surface.get_release requires a non-null buffer in the same commit",
        );
        let callbacks = std::mem::take(&mut self.pending_release_callbacks);
        self.clear_failed_surface_commit();
        use crate::wayland_objects::wl_display::WlDisplay;
        let display = WlDisplay::default();
        for callback in callbacks {
            client.delete_id(&display, state, dispatcher, callback.wayland_id().into());
        }
        true
    }

    fn complete_release_callbacks_now(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let mut callbacks = std::mem::take(&mut self.pending_release_callbacks);
        callbacks.extend(std::mem::take(&mut self.on_screen.release_callbacks));
        if let Some(cached) = self.cached_update.as_mut() {
            callbacks.extend(std::mem::take(&mut cached.payload.release_callbacks));
        }
        for update in self.queue.iter_mut() {
            callbacks.extend(std::mem::take(&mut update.payload.release_callbacks));
        }
        use crate::wayland_objects::wl_display::WlDisplay;
        let display = WlDisplay::default();
        for callback in callbacks {
            if !callback.exists(state) {
                continue;
            }
            callback.rref(state).done(0, dispatcher);
            client.delete_id(&display, state, dispatcher, callback.wayland_id().into());
        }
    }

    fn get_changes_for_payload(&self, payload: &SurfaceCommitPayload) -> SurfaceChanges {
        SurfaceChanges {
            buffer_changed: payload.buffer.is_some(),
            mapping_changed: payload.buffer_transform != self.on_screen.buffer_transform
                || payload.buffer_scale != self.on_screen.buffer_scale
                || payload.viewport != self.on_screen.viewport,
        }
    }

    fn lock_commit_logical_size(&self, _state: &State) -> Option<SizeI> {
        let attached = match self.pending.buffer.as_ref() {
            Some(None) => return None,
            Some(Some(attached)) => attached,
            None => self.latest.buffer.as_ref()?,
        };
        let (width, height) = attached.content.dimensions();
        Some(transform::derive_logical_size(
            SizeI { width, height },
            self.pending.buffer_transform,
            self.pending.buffer_scale,
            self.pending.viewport,
        ))
    }

    fn reject_lock_surface_commit(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) -> bool {
        if self.role != Role::LockSurface {
            return false;
        }
        let Some(lock_surface_id) = self.lock_surface else {
            return false;
        };
        let size = self.lock_commit_logical_size(state);
        let Some(mut lock_surface) = lock_surface_id.try_ref_mut(state) else {
            return false;
        };
        let Err((code, message)) = lock_surface.check_commit(size) else {
            return false;
        };
        client.protocol_violation_disconnect(dispatcher, lock_surface.id().into(), code, message);
        true
    }

    fn note_lock_surface_mapped(&self, state: &mut State) -> bool {
        let Some(lock_surface_id) = self.lock_surface else {
            return false;
        };
        let Some(mut lock_surface) = lock_surface_id.try_ref_mut(state) else {
            return false;
        };
        let Some(output_id) = lock_surface.mark_mapped_if_ready() else {
            return false;
        };
        state.session_lock.note_mapped(output_id)
    }

    fn validate_pending_representation(
        &self,
        client: &Client,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) -> bool {
        let next = self
            .pending
            .representation
            .apply(self.latest.representation);
        if next.coefficients.is_none() && next.chroma.is_none() {
            return true;
        }
        let attached = match &self.pending.buffer {
            Some(None) => return true,
            Some(Some(buffer)) => buffer,
            None => match &self.latest.buffer {
                Some(buffer) => buffer,
                None => return true,
            },
        };
        let yuv = matches!(attached.content.format().kind, crate::drm::FormatKind::Yuv { .. });
        let Some(object) = self.color_representation_object else {
            return true;
        };
        if !coefficients_match_format(next.coefficients, yuv) {
            client.send_protocol_error(
                dispatcher,
                object.wayland_id().into(),
                crate::protocol_generated::wp_color_representation_surface_v1::Error::PixelFormat
                    as u32,
                "pixel format is incompatible with the coefficients and range",
            );
            return false;
        }
        if !chroma_matches_format(next.chroma, yuv) {
            client.send_protocol_error(
                dispatcher,
                object.wayland_id().into(),
                crate::protocol_generated::wp_color_representation_surface_v1::Error::PixelFormat
                    as u32,
                "pixel format is incompatible with the chroma location",
            );
            return false;
        }
        true
    }

    fn validate_pending_mapping(
        &self,
        client: &Client,
        state: &State,
        dispatcher: &mut ClientDispatcher,
    ) -> bool {
        self.validate_payload_mapping(
            client,
            state,
            dispatcher,
            self.pending.buffer.clone(),
            self.pending.buffer_transform,
            self.pending.buffer_scale,
            self.pending.viewport,
        )
    }

    fn validate_payload_mapping(
        &self,
        client: &Client,
        state: &State,
        dispatcher: &mut ClientDispatcher,
        buffer: Option<Option<crate::buffer_content::AttachedBuffer>>,
        buffer_transform: BufferTransform,
        buffer_scale: i32,
        viewport: SurfaceViewport,
    ) -> bool {
        let Some(attached) = buffer.flatten().or(self.latest.buffer.clone()) else {
            return true;
        };
        let buffer_size = attached.content.dimensions();
        let buffer_size = SizeI { width: buffer_size.0, height: buffer_size.1 };
        let base =
            transform::pre_viewport_logical_size(buffer_size, buffer_transform, buffer_scale);
        if let Some(src) = viewport.src {
            if src.width <= 0.0 || src.height <= 0.0 || src.x < 0.0 || src.y < 0.0 {
                self.send_viewport_error(
                    client,
                    dispatcher,
                    wp_viewport::Error::BadValue as u32,
                    "viewport source must have positive size and non-negative origin",
                );
                return false;
            }
            if viewport.dst.is_none() && (src.width.fract() != 0.0 || src.height.fract() != 0.0) {
                self.send_viewport_error(
                    client,
                    dispatcher,
                    wp_viewport::Error::BadSize as u32,
                    "viewport source must have integer size when destination is unset",
                );
                return false;
            }
            let content =
                transform::RectF { x: 0.0, y: 0.0, width: base.width, height: base.height };
            if !transform::rect_contains(content, src) {
                self.send_viewport_error(
                    client,
                    dispatcher,
                    wp_viewport::Error::OutOfBuffer as u32,
                    "viewport source rectangle extends outside the transformed/scaled buffer",
                );
                return false;
            }
        }
        true
    }

    fn mapped_state_for_buffer(
        &self,
        _state: &State,
        attached: crate::buffer_content::AttachedBuffer,
        x: i32,
        y: i32,
        payload: &SurfaceCommitPayload,
    ) -> Option<MappedBufferState> {
        let buffer_size = attached.content.dimensions();
        let buffer_size = SizeI { width: buffer_size.0, height: buffer_size.1 };
        let logical_size = transform::derive_logical_size(
            buffer_size,
            payload.buffer_transform,
            payload.buffer_scale,
            payload.viewport,
        );
        let tex_transform = transform::surface_sample_transform(
            buffer_size,
            payload.buffer_transform,
            payload.buffer_scale,
            payload.viewport,
        );
        Some(MappedBufferState {
            buffer: attached,
            x,
            y,
            width: logical_size.width,
            height: logical_size.height,
            tex_transform,
            sync: payload.sync.clone(),
            frame_callbacks: Vec::new(),
            presentation_feedbacks: Vec::new(),
            buffer_transform: payload.buffer_transform,
            buffer_scale: payload.buffer_scale,
            viewport: payload.viewport,
            presentation_hint: payload.presentation_hint,
            alpha_multiplier: payload.alpha_multiplier,
            blur_region: payload.blur_region.clone(),
            color: committed_surface_color(&self.on_screen.color, &payload.color),
            representation: payload.representation.apply(self.on_screen.representation),
            input_region: payload.input_region.clone(),
            opaque_region: payload.opaque_region.clone(),
            release_callbacks: payload.release_callbacks.clone(),
            wire_inputs: payload.wire_inputs.clone(),
        })
    }

    fn pending_damage_scaffold(
        &self,
        _state: &State,
        attached: &crate::buffer_content::AttachedBuffer,
        logical_size: SizeI,
        payload: &SurfaceCommitPayload,
    ) -> Option<DamageScaffold> {
        let raw_size = attached.content.dimensions();
        let raw_size = SizeI { width: raw_size.0, height: raw_size.1 };
        let surface_to_buffer = payload
            .damage_surface
            .iter()
            .map(|rect| {
                transform::surface_rect_to_buffer_rect(
                    transform::RectF {
                        x: rect.x as f32,
                        y: rect.y as f32,
                        width: rect.width as f32,
                        height: rect.height as f32,
                    },
                    logical_size,
                    raw_size,
                    payload.buffer_transform,
                    payload.buffer_scale,
                    payload.viewport,
                )
            })
            .collect();
        let buffer_to_surface = payload
            .damage_buffer
            .iter()
            .filter_map(|rect| {
                transform::buffer_rect_to_surface_rect(
                    transform::RectF {
                        x: rect.x as f32,
                        y: rect.y as f32,
                        width: rect.width as f32,
                        height: rect.height as f32,
                    },
                    logical_size,
                    raw_size,
                    payload.buffer_transform,
                    payload.buffer_scale,
                    payload.viewport,
                )
            })
            .collect();
        Some(DamageScaffold { surface_to_buffer, buffer_to_surface })
    }

    fn full_damage_for_logical_size(logical_size: SizeI) -> DamageRegion {
        let mut damage = DamageRegion::new();
        if let Some(rect) = RectI::new(0, 0, logical_size.width, logical_size.height) {
            damage.add_rect(rect);
        }
        damage
    }

    fn explicit_damage_region(
        &self,
        state: &State,
        payload: &SurfaceCommitPayload,
        logical_size: SizeI,
        attached: &crate::buffer_content::AttachedBuffer,
    ) -> DamageRegion {
        let mut damage = DamageRegion::new();
        for rect in &payload.damage_surface {
            if let Some(rect) = RectI::new(rect.x, rect.y, rect.width, rect.height) {
                damage.add_rect(rect);
            }
        }
        if let Some(scaffold) = self.pending_damage_scaffold(state, attached, logical_size, payload)
        {
            for rect in scaffold.buffer_to_surface {
                if let Some(rect) = RectI::from_f32_bounds(rect) {
                    damage.add_rect(rect);
                }
            }
        }
        if let Some(bounds) = RectI::new(0, 0, logical_size.width, logical_size.height) {
            damage.intersect_rect(bounds);
        }
        damage
    }

    fn committed_damage_region(
        &self,
        state: &State,
        payload: &SurfaceCommitPayload,
        buffer_changed: bool,
        mapping_changed: bool,
        multiplier_changed: bool,
        color_changed: bool,
        representation_changed: bool,
    ) -> DamageRegion {
        let Some(attached) = self.on_screen.buffer.as_ref() else {
            return DamageRegion::new();
        };
        let Some(logical_size) = self.on_screen.logical_size else {
            return DamageRegion::new();
        };
        if !ENABLE_DAMAGE {
            if buffer_changed
                || mapping_changed
                || multiplier_changed
                || color_changed
                || representation_changed
                || !payload.damage_surface.is_empty()
                || !payload.damage_buffer.is_empty()
            {
                return Self::full_damage_for_logical_size(logical_size);
            }
            return DamageRegion::new();
        }
        if buffer_changed
            || mapping_changed
            || multiplier_changed
            || color_changed
            || representation_changed
        {
            return Self::full_damage_for_logical_size(logical_size);
        }
        self.explicit_damage_region(state, payload, logical_size, attached)
    }

    fn parent_behaves_synchronized(&self, state: &State) -> bool {
        let Some(parent) = self.parent_surface() else {
            return false;
        };
        let Some(parent) = parent.try_ref(state) else {
            return false;
        };
        parent.behaves_synchronized(state)
    }

    fn behaves_synchronized(&self, state: &State) -> bool {
        match self.sync_mode() {
            None => false,
            Some(SubsurfaceSyncMode::Sync) => true,
            Some(SubsurfaceSyncMode::Desync) => self.parent_behaves_synchronized(state),
        }
    }

    /// Request-time capture of the subsurface tree: the pending stack and the
    /// pending position of every child in it that has a `wl_subsurface`.
    fn capture_tree(&self, state: &State) -> TreeUpdate {
        let mut positions = Vec::new();
        for entry in &self.tree.pending_stack {
            let StackEntry::Child(child_id) = *entry else {
                continue;
            };
            let Some(child) = child_id.try_ref(state) else {
                continue;
            };
            let Some(subsurface) = child.subsurface_object() else {
                continue;
            };
            positions.push((child_id, subsurface, child.pending_subsurface_position()));
        }
        TreeUpdate { stack: self.tree.pending_stack.clone(), positions }
    }

    /// Applies a captured subsurface tree. Children no longer in the pending
    /// stack, and children whose `wl_subsurface` changed since the capture, are
    /// skipped. Returns whether the order or a position changed.
    fn apply_tree(&mut self, state: &mut State, tree: &TreeUpdate) -> bool {
        let current_stack: Vec<StackEntry> = tree
            .stack
            .iter()
            .copied()
            .filter(|entry| match entry {
                StackEntry::Parent => true,
                StackEntry::Child(_) => self.tree.pending_stack.contains(entry),
            })
            .collect();
        let order_changed = current_stack != self.tree.current_stack;
        self.tree.current_stack = current_stack;
        self.ensure_current_stack_parent();

        let mut position_changed = false;
        for (child_id, subsurface, position) in &tree.positions {
            if !self
                .tree
                .pending_stack
                .contains(&StackEntry::Child(*child_id))
            {
                continue;
            }
            let Some(mut child) = child_id.try_grab(state) else {
                continue;
            };
            if child.subsurface_object() != Some(*subsurface) {
                continue;
            }
            if child.current_subsurface_position() != *position {
                position_changed = true;
            }
            if let Some(sub) = child.subsurface.as_mut() {
                sub.current_position = *position;
            }
        }

        order_changed || position_changed
    }

    #[cfg(test)]
    pub(crate) fn apply_pending_tree_state(&mut self, state: &mut State) -> bool {
        let tree = self.capture_tree(state);
        self.apply_tree(state, &tree)
    }

    pub(super) fn complete_frame_callbacks(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        callbacks: Vec<Resolved<WlCallbackId>>,
    ) {
        use crate::wayland_objects::wl_display::WlDisplay;
        let display = WlDisplay::default();
        let time_msec = monotonic_time_msec();
        for cb in callbacks {
            if !cb.exists(state) {
                continue;
            }
            {
                let callback = cb.rref(state);
                callback.done(time_msec, dispatcher);
            }
            client.delete_id(&display, state, dispatcher, cb.wayland_id().into());
        }
    }

    fn apply_surface_payload(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        payload: &SurfaceCommitPayload,
    ) -> bool {
        let changes = self.get_changes_for_payload(payload);
        let multiplier_changed = payload.alpha_multiplier != self.on_screen.alpha_multiplier;
        let color_changed = surface_color_changed(&self.on_screen.color, &payload.color);
        let next_representation = payload.representation.apply(self.on_screen.representation);
        let representation_changed = next_representation != self.on_screen.representation;
        let mut committed_damage = DamageRegion::new();
        let previous_feedbacks = std::mem::take(&mut self.on_screen.presentation_feedbacks);

        match payload.buffer.clone() {
            None => {}
            Some(None) => {
                let warp = self.pointer_lock_warp_geometry();
                let resolved = client.resolve(self.id, state);
                if let Some(resolved) = resolved {
                    if let Some(window) = state.window_for_root(resolved) {
                        begin_managed_toplevel_close_surface(
                            state, window, resolved, self, dispatcher,
                        );
                    }
                }
                self.retire_current();
                let color = committed_surface_color(&self.on_screen.color, &payload.color);
                let representation = next_representation;
                self.on_screen = OnScreen {
                    buffer: None,
                    content_revision: self.next_content_revision(),
                    x: self.on_screen.x,
                    y: self.on_screen.y,
                    sync: SurfaceSyncState::default(),
                    buffer_transform: payload.buffer_transform,
                    buffer_scale: payload.buffer_scale,
                    viewport: payload.viewport,
                    presentation_hint: payload.presentation_hint,
                    alpha_multiplier: payload.alpha_multiplier,
                    blur_region: payload.blur_region.clone(),
                    color,
                    representation,
                    logical_size: None,
                    sample_transform: transform::Mat3::identity(),
                    damage: DamageRegion::new(),
                    frame_callbacks: std::mem::take(&mut self.on_screen.frame_callbacks),
                    presentation_feedbacks: Vec::new(),
                    input_region: payload.input_region.clone(),
                    opaque_region: payload.opaque_region.clone(),
                    release_callbacks: Vec::new(),
                    wire_inputs: payload.wire_inputs.clone(),
                };
                if let Some(resolved) = resolved {
                    crate::input::pointer_lock::release_pointer_lock_for_surface(
                        state,
                        dispatcher,
                        resolved,
                        Some(warp),
                        crate::input::pointer_lock::PointerLockRelease {
                            send_unlocked: true,
                            apply_lifetime: true,
                            warp: true,
                        },
                    );
                    crate::input::pointer_confine::release_pointer_confine_for_surface(
                        state, dispatcher, resolved, true,
                    );
                }
            }
            Some(Some(buffer_object_id)) => {
                let Some(next) = self.mapped_state_for_buffer(
                    state,
                    buffer_object_id.clone(),
                    self.on_screen.x + payload.offset.0,
                    self.on_screen.y + payload.offset.1,
                    payload,
                ) else {
                    log::warn!("failed to derive mapped state for buffer {:?}", buffer_object_id);
                    self.on_screen.presentation_feedbacks = previous_feedbacks;
                    return false;
                };

                self.activate_mapped_state(next);
            }
        }

        if !changes.buffer_changed && changes.mapping_changed && self.on_screen.buffer.is_some() {
            let Some(next) = self.mapped_state_for_buffer(
                state,
                self.on_screen.buffer.clone().unwrap(),
                self.on_screen.x,
                self.on_screen.y,
                &SurfaceCommitPayload {
                    buffer: None,
                    offset: (0, 0),
                    sync: self.on_screen.sync.clone(),
                    buffer_transform: payload.buffer_transform,
                    buffer_scale: payload.buffer_scale,
                    viewport: payload.viewport,
                    presentation_hint: payload.presentation_hint,
                    alpha_multiplier: payload.alpha_multiplier,
                    blur_region: payload.blur_region.clone(),
                    color: None,
                    representation: RepresentationUpdate::default(),
                    damage_surface: Vec::new(),
                    damage_buffer: Vec::new(),
                    input_region: self.on_screen.input_region.clone(),
                    opaque_region: self.on_screen.opaque_region.clone(),
                    release_callbacks: Vec::new(),
                    wire_inputs: self.on_screen.wire_inputs.clone(),
                },
            ) else {
                log::warn!("failed to recompute mapped state for current surface {:?}", self.id);
                self.on_screen.presentation_feedbacks = previous_feedbacks;
                return false;
            };
            self.on_screen.buffer_transform = next.buffer_transform;
            self.on_screen.buffer_scale = next.buffer_scale;
            self.on_screen.viewport = next.viewport;
            self.on_screen.logical_size = Some(SizeI { width: next.width, height: next.height });
            self.on_screen.sample_transform = next.tex_transform;
        }

        let blur_changed = payload.blur_region != self.on_screen.blur_region;
        self.on_screen.presentation_hint = payload.presentation_hint;
        self.on_screen.alpha_multiplier = payload.alpha_multiplier;
        self.on_screen.blur_region = payload.blur_region.clone();
        if let Some(color) = payload.color.clone() {
            self.on_screen.color = color;
        }
        self.on_screen.representation = next_representation;
        self.on_screen.input_region = payload.input_region.clone();
        self.on_screen.opaque_region = payload.opaque_region.clone();
        self.on_screen.wire_inputs = payload.wire_inputs.clone();
        committed_damage = self.committed_damage_region(
            state,
            payload,
            changes.buffer_changed,
            changes.mapping_changed,
            multiplier_changed,
            color_changed,
            representation_changed,
        );
        self.on_screen.damage.add_region(&committed_damage);
        if changes.buffer_changed || changes.mapping_changed || !committed_damage.is_empty() {
            self.on_screen.content_revision = self.next_content_revision();
        }

        if !committed_damage.is_empty() {
            log::debug!(
                "wl_surface commit damage applied surface={:?} local_damage={:?}",
                self.id,
                committed_damage.rects()
            );
        }

        Self::discard_feedbacks(client, state, dispatcher, previous_feedbacks);

        if self.pointer_constraint().is_some() {
            dispatcher.dispatch_post_action(crate::input::pointer_lock::RecheckPointerLock);
        }

        changes.buffer_changed
            || changes.mapping_changed
            || !committed_damage.is_empty()
            || blur_changed
    }

    /// Applies the carried updates of synchronized children, in stack order.
    /// A child destroyed since the capture has its update discarded.
    fn apply_children(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        children: Vec<content_update::ChildUpdate>,
    ) -> bool {
        let mut changed = false;
        for child in children {
            let Some(mut child_surface) = child.surface.try_grab(state) else {
                if child.surface.exists(state) {
                    unreachable!("carried child grabbed during parent apply");
                }
                let mut seq = 0;
                content_update::discard_update(client, state, dispatcher, &mut seq, *child.update);
                continue;
            };
            if child.update.last_seq <= child_surface.dropped_through_seq {
                // The child was dropped after this update was captured.
                content_update::discard_update(
                    client,
                    state,
                    dispatcher,
                    &mut child_surface.applied_update_seq,
                    *child.update,
                );
                continue;
            }
            let (child_changed, child_needs_frame) =
                child_surface.apply_content(client, state, dispatcher, *child.update);
            changed |= child_changed || child_needs_frame;
        }
        changed
    }

    fn apply_content(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        update: ContentUpdate,
    ) -> (bool, bool) {
        let this = client.resolve(self.id, state).unwrap();
        let set_barrier = update.fifo_set_barrier;
        self.apply_xwayland_serial(client, this, update.xwayland_serial, dispatcher);
        if let Some(pointer_constraint) = &update.pointer_constraint {
            apply_pointer_constraint_commit(
                self,
                pointer_constraint.constraint,
                state,
                pointer_constraint.region.clone(),
                pointer_constraint.hint,
            );
        }
        if self.pointer_constraint().is_some() {
            dispatcher.dispatch_post_action(crate::input::pointer_lock::RecheckPointerLock);
        }
        let last_seq = update.last_seq;
        let payload = update.payload;
        let mut changed = self.apply_surface_payload(client, state, dispatcher, &payload);
        changed |= state
            .seat
            .apply_cursor_surface_offset(this, payload.offset.0, payload.offset.1);
        changed |= self.apply_tree(state, &update.tree);
        changed |= self.apply_children(client, state, dispatcher, update.children);
        let (discarded_feedbacks, needs_presentation_frame) =
            self.route_presentation_feedbacks(update.presentation_feedbacks);
        Self::discard_feedbacks(client, state, dispatcher, discarded_feedbacks);
        let (immediate_callbacks, needs_frame) = self.route_frame_callbacks(update.frame_callbacks);
        Self::complete_frame_callbacks(client, state, dispatcher, immediate_callbacks);
        self.applied_update_seq = self.applied_update_seq.max(last_seq);
        if set_barrier {
            crate::commit_gate::set_barrier(state, dispatcher, this, self.active_output_id());
        }
        (changed, needs_frame || needs_presentation_frame)
    }

    fn on_commit(
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        if self.reject_release_callbacks_without_buffer(client, state, dispatcher) {
            return;
        }
        crate::wayland_objects::zxdg_toplevel_decoration_v1::queue_deferred_decoration_revert(
            self.xdg_surface(),
            state,
            dispatcher,
        );
        let resolved_surface = client.resolve_this(&self, state);
        let mut pending_xdg_check = None;
        let mut pending_layer_check = None;
        let mut this = if let Some(shell_surface) = self.shell_surface {
            let (mut this, preferred_output_id) = client.with_ungrabbed(self, state, |state| {
                let this = resolved_surface.rref(state);
                this.preferred_output_id_for_object(state, resolved_surface)
            });
            this.send_preferred_fractional_scale_for_object(state, preferred_output_id, dispatcher);
            this.send_preferred_buffer_state_for_object(state, preferred_output_id, dispatcher);
            match shell_surface {
                ShellSurfaceObject::XdgSurface(xdg_surface_id) => {
                    let (has_pending_buffer_attach, is_null_buffer_commit) = (
                        this.has_pending_buffer_attach(),
                        matches!(this.pending.buffer, Some(None)),
                    );
                    drop(this);
                    let Some(kind) = xdg_surface_id.rref(state).check_commit(
                        client,
                        dispatcher,
                        has_pending_buffer_attach,
                        is_null_buffer_commit,
                    ) else {
                        let mut this = resolved_surface.ref_mut(state);
                        this.clear_failed_surface_commit();
                        return;
                    };
                    pending_xdg_check = Some((xdg_surface_id, kind));
                    resolved_surface.grab(state)
                }
                ShellSurfaceObject::LayerSurface(layer_surface_id) => {
                    let has_pending_buffer_attach = this.has_pending_buffer_attach();
                    let is_null_buffer_commit = matches!(this.pending.buffer, Some(None));
                    let (this, kind) = client.with_ungrabbed_mut(this, state, |state, client| {
                        layer_surface_id.rref(state).check_commit(
                            client,
                            dispatcher,
                            has_pending_buffer_attach,
                            is_null_buffer_commit,
                        )
                    });
                    let Some(kind) = kind else {
                        let mut this = this;
                        this.clear_failed_surface_commit();
                        return;
                    };
                    pending_layer_check = Some((layer_surface_id, kind));
                    this
                }
            }
        } else {
            self
        };

        if this.reject_lock_surface_commit(client, state, dispatcher) {
            this.clear_failed_surface_commit();
            return;
        }
        if !this.validate_pending_mapping(client, state, dispatcher) {
            this.clear_failed_surface_commit();
            return;
        }
        if !this.validate_pending_sync(client, state, dispatcher) {
            this.clear_failed_surface_commit();
            return;
        }
        if !this.validate_pending_representation(client, state, dispatcher) {
            this.clear_failed_surface_commit();
            return;
        }

        let role = match (pending_xdg_check, pending_layer_check) {
            (Some((xdg_surface_id, kind)), _) => {
                let capture = {
                    let mut xdg_surface = xdg_surface_id.grab(state);
                    xdg_surface.take_commit_capture(state, kind)
                };
                RoleUpdate::Xdg { xdg_surface: xdg_surface_id, capture }
            }
            (None, Some((layer_surface_id, kind))) => {
                let capture = {
                    let mut layer_surface = layer_surface_id.grab(state);
                    layer_surface.take_commit_capture(kind)
                };
                RoleUpdate::Layer { layer_surface: layer_surface_id, capture }
            }
            (None, None) => RoleUpdate::None,
        };

        let mut update = this.capture_commit(state, dispatcher);
        update.role = role;

        if this.behaves_synchronized(state) {
            // The constraint is ignored while the commit is synchronized; it
            // stays cleared if the child later applies the cache on its own.
            update.fifo_wait_barrier = false;
            let discarded = this.merge_cached_update(update);
            Self::discard_feedbacks(client, state, dispatcher, discarded);
            return;
        }
        let update = match this.cached_update.take() {
            Some(mut cached) => {
                let discarded = cached.merge(update, &mut this.retired_buffers);
                Self::discard_feedbacks(client, state, dispatcher, discarded);
                cached
            }
            None => update,
        };
        commit_queue::submit(this, client, state, dispatcher, update);
    }

    /// Request-time half of a commit, after every check passed: moves the
    /// pending state into one `ContentUpdate`.
    /// Request-time capture of the cached update of every synchronized child in
    /// the pending stack. The parent's update carries them.
    fn capture_children(&self, state: &mut State) -> Vec<content_update::ChildUpdate> {
        let mut children = Vec::new();
        for entry in &self.tree.pending_stack {
            let StackEntry::Child(child_id) = *entry else {
                continue;
            };
            let Some(child) = child_id.try_ref_mut(state) else {
                continue;
            };
            let Some(update) = child.cached_update.take() else {
                continue;
            };
            children
                .push(content_update::ChildUpdate { surface: child_id, update: Box::new(update) });
        }
        children
    }

    fn capture_commit(
        &mut self,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) -> ContentUpdate {
        self.next_update_seq += 1;
        let seq = self.next_update_seq;
        // The payload of a wire-scaled client is normalized to logical units
        // here, once, before it is cached or applied, so synchronized
        // subsurfaces and the on-screen state both work in logical units.
        // The pending state above was validated in the client's own wire
        // units, exactly as the client sent it.
        let wire_scale = dispatcher.wire_scale();
        let payload =
            WlSurface::normalize_commit_payload(self.build_commit_payload(wire_scale), wire_scale);
        let mut update = ContentUpdate::new(
            seq,
            self.role_epoch,
            payload,
            std::mem::take(&mut self.pending_frame_callbacks),
            std::mem::take(&mut self.pending_presentation_feedbacks),
        );
        let region = self.pending.pointer_constraint_region.take();
        let hint = self.pending.pointer_constraint_hint.take();
        update.pointer_constraint = self
            .pointer_constraint()
            .map(|constraint| PointerConstraintUpdate { constraint, region, hint });
        update.xwayland_serial = self.pending.xwayland_serial.take();
        update.fifo_set_barrier = std::mem::take(&mut self.pending.fifo_set_barrier);
        update.fifo_wait_barrier = std::mem::take(&mut self.pending.fifo_wait_barrier);
        update.commit_target = self.pending.commit_target.take();
        if update.xwayland_serial.is_some() {
            self.latest_xwayland_serial = update.xwayland_serial;
        }
        update.tree = self.capture_tree(state);
        update.children = self.capture_children(state);
        #[cfg(test)]
        {
            update.test_hold = std::mem::take(&mut self.pending_test_hold);
        }
        update
    }

    fn apply_update(
        mut this: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        mut update: ContentUpdate,
    ) {
        // Measured before any part of the commit applies, as the commit prologue did.
        let was_mapped = this.global_logical_bounds().is_some();
        let resolved_surface = client.resolve_this(&this, state);
        let old_effective_insets = (this.is_root()
            && this.role == Role::XdgSurface
            && !this.has_cursor_role()
            && !this.has_dnd_icon_role())
        .then(|| {
            crate::root_geometry::effective_decoration_insets_for_surface(state, resolved_surface)
        });
        let role_current = update.role_epoch == this.role_epoch;
        if let RoleUpdate::Layer { layer_surface, capture } = &update.role {
            // A role object destroyed since the capture leaves its part unapplied.
            if role_current {
                let layer_surface = *layer_surface;
                let (ungrabbed, ()) = client.with_ungrabbed_mut(this, state, |state, client| {
                    let mut layer = layer_surface.grab(state);
                    layer.apply_commit(client, state, dispatcher, capture);
                });
                this = ungrabbed;
            }
        }
        let mut pending_xdg_role_changes = None;
        if let RoleUpdate::Xdg { xdg_surface: xdg_surface_id, capture } =
            std::mem::take(&mut update.role)
        {
            // A role object destroyed since the capture leaves its part unapplied.
            if role_current {
                let (ungrabbed, configure_bounds) = client.with_ungrabbed(this, state, |state| {
                    let this = resolved_surface.rref(state);
                    let layout_snapshot = LayerLayoutObjectSnapshot::for_surface(resolved_surface);
                    state.configure_bounds_for_surface_with_ref(
                        resolved_surface,
                        this,
                        Some(layout_snapshot),
                    )
                });
                this = ungrabbed;
                let (ungrabbed, role_changes) =
                    client.with_ungrabbed_mut(this, state, |state, client| {
                        let mut xdg_surface = xdg_surface_id.grab(state);
                        xdg_surface.apply_commit(
                            client,
                            state,
                            dispatcher,
                            &capture,
                            configure_bounds,
                        )
                    });
                this = ungrabbed;
                pending_xdg_role_changes = Some((xdg_surface_id, role_changes, capture));
            }
        }
        let lock_output = if role_current && this.role == Role::LockSurface {
            this.lock_surface
                .and_then(|lock_surface| lock_surface.try_ref(state))
                .map(|lock_surface| lock_surface.output_id())
        } else {
            None
        };
        let mapped = role_current && this.note_lock_surface_mapped(state);
        if let Some(output_id) = lock_output {
            crate::drm::request_output(state, output_id, dispatcher);
        }

        let (mut this, old_blur_bounds) = client
            .with_ungrabbed(this, state, |state| state.client_blur_padded_bounds(resolved_surface));
        let (changed, needs_frame) = this.apply_content(client, state, dispatcher, update);
        let (mut this, new_blur_bounds) = client
            .with_ungrabbed(this, state, |state| state.client_blur_padded_bounds(resolved_surface));
        if old_blur_bounds != new_blur_bounds {
            let rects = old_blur_bounds
                .into_iter()
                .chain(new_blur_bounds)
                .collect::<Vec<_>>();
            crate::drm::queue_global_damage(state, &rects);
            request_outputs_for_rects(state, rects.iter().copied(), dispatcher);
        }
        if mapped {
            dispatcher.dispatch_post_action(crate::input::focus::RefreshLockFocus);
        }

        if let Some((xdg_surface_id, role_changes, capture)) = pending_xdg_role_changes {
            let mut xdg_surface = xdg_surface_id.grab(state);
            (this, _) = client.with_ungrabbed_mut(this, state, |state, client| {
                xdg_surface.apply_commit_role_changes(
                    client,
                    state,
                    dispatcher,
                    role_changes,
                    &capture,
                );
            });
        }

        if changed {
            let (ungrabbed, root) = client.with_ungrabbed(this, state, |state| {
                state.root_surface_for_surface(resolved_surface)
            });
            this = ungrabbed;
            match root {
                Some(root) => {
                    log::debug!(
                        "wl_surface commit scheduling RefreshSurfaceTreeOutputs client={:?} surface={:?} root={:?} changed={}",
                        client.client_id(),
                        this.id,
                        root,
                        changed
                    );
                    dispatcher.dispatch_to(
                        client.client_id(),
                        crate::clients::ClientAction::RefreshSurfaceTreeOutputs { root },
                    );
                }
                None => {
                    log::debug!(
                        "wl_surface commit scheduling RefreshSurfaceOutputs client={:?} surface={:?} changed={}",
                        client.client_id(),
                        this.id,
                        changed
                    );
                    dispatcher.dispatch_to(
                        client.client_id(),
                        crate::clients::ClientAction::RefreshSurfaceOutputs,
                    );
                }
            }
        }
        if let Some(old_effective_insets) = old_effective_insets {
            let new_effective_insets =
                crate::root_geometry::effective_decoration_insets_for_surface(
                    state,
                    resolved_surface,
                );
            if old_effective_insets != new_effective_insets
                && (state.is_tiled_root_surface(resolved_surface)
                    || state.root_surface_is_fullscreen(resolved_surface))
            {
                if let Some(workspace_id) = state.root_surface_workspace(resolved_surface) {
                    crate::tiling::mark_workspace_dirty(state, workspace_id);
                    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
                }
            }
        }
        if !this.has_cursor_role() && !this.has_dnd_icon_role() && !this.has_input_popup_role() {
            (this, _) = client.with_ungrabbed(this, state, |state| {
                state.mark_root_geometry_dirty_for_surface(resolved_surface);
            });
            // A commit that only changes the window geometry still has to re-derive a
            // floating window's root from its frame origin.
            let floating_frame = state
                .managed_window(resolved_surface)
                .is_some_and(|managed| managed.floating_frame_origin.is_some());
            if changed || floating_frame {
                let (regrabbed, (moved, old_bounds, new_bounds, snapped, old_blur, new_blur)) =
                    client.with_ungrabbed(this, state, |state| {
                        // Tiled root_pos is computed, so a rebuild already sits at
                        // the anchored origin. Damage the new buffer at the
                        // surface position from before that rebuild.
                        let old_blur = state.client_blur_padded_bounds(resolved_surface);
                        let previous_root_pos = resolved_surface
                            .try_ref(state)
                            .map(|surface| surface.root_position());
                        state.refresh_tiled_content_basis(resolved_surface);
                        let anchored = (state.is_tiled_root_surface(resolved_surface)
                            && !state.root_surface_is_fullscreen(resolved_surface))
                            || state
                                .managed_window(resolved_surface)
                                .is_some_and(|managed| managed.floating_frame_origin.is_some());
                        let old_bounds = if anchored {
                            previous_root_pos.and_then(|root_pos| {
                                state
                                    .committed_root_geometry(resolved_surface)
                                    .and_then(|geometry| geometry.surface_bounds_at(root_pos))
                            })
                        } else {
                            state.root_surface_surface_bounds_global(resolved_surface)
                        }
                        .or_else(|| state.root_surface_content_rect_global(resolved_surface));
                        let moved = state.apply_resize_anchor(resolved_surface)
                            || state.sync_anchored_root_position(resolved_surface)
                            || state.sync_floating_root_position(resolved_surface);
                        let new_bounds = state
                            .root_surface_surface_bounds_global(resolved_surface)
                            .or_else(|| state.root_surface_content_rect_global(resolved_surface));
                        let snapped = state.snap_settled_floating_geometry(resolved_surface);
                        state.clear_resize_anchor_if_idle(resolved_surface);
                        let new_blur = state.client_blur_padded_bounds(resolved_surface);
                        (moved, old_bounds, new_bounds, snapped, old_blur, new_blur)
                    });
                this = regrabbed;
                if moved {
                    if let Some(toplevel) = state
                        .window_for_root(resolved_surface)
                        .and_then(|window| state.toplevel(window))
                    {
                        toplevel.position_changed(dispatcher);
                    }
                    dispatcher.dispatch_to(
                        client.client_id(),
                        crate::clients::ClientAction::RefreshSurfaceTreeOutputs {
                            root: resolved_surface,
                        },
                    );
                    let blur_rects = old_blur.into_iter().chain(new_blur).collect::<Vec<_>>();
                    crate::drm::queue_global_damage(state, &blur_rects);
                    request_outputs_for_rects(
                        state,
                        old_bounds.into_iter().chain(new_bounds).chain(blur_rects),
                        dispatcher,
                    );
                }
                if snapped {
                    kick_visual_animation_pump(dispatcher);
                }
            }
        }
        let is_mapped = this.global_logical_bounds().is_some();
        let mut bounds_after_first_map_relayout = None;
        if was_mapped != is_mapped {
            if let Some(ShellSurfaceObject::XdgSurface(xdg_surface_id)) = this.shell_surface {
                if xdg_surface_id
                    .try_ref(state)
                    .is_some_and(XdgSurface::is_toplevel_parent)
                {
                    if is_mapped {
                        let toplevel = xdg_surface_id
                            .try_ref(state)
                            .and_then(XdgSurface::toplevel_role);
                        if let Some(toplevel) = toplevel {
                            (this, _) = client.with_ungrabbed(this, state, |state| {
                                if let Some(mut toplevel) = toplevel.try_ref_mut(state) {
                                    toplevel.note_mapped();
                                }
                            });
                        }
                        let relayout_first_map = state
                            .managed_root_surface_state(resolved_surface)
                            .is_some_and(|managed| {
                                managed.awaiting_initial_layout
                                    && (managed.fullscreen
                                        || managed.mode == crate::state::ManagedWindowMode::Tiled)
                            });
                        state.confirm_root_surface_mapped(resolved_surface);
                        if relayout_first_map {
                            let (regrabbed, (relayout_bounds, opened)) =
                                client.with_ungrabbed(this, state, |state| {
                                    let opened = state.relayout_dirty_workspaces();
                                    let bounds = resolved_surface
                                        .try_ref(state)
                                        .and_then(|surface| surface.global_logical_bounds());
                                    (bounds, opened)
                                });
                            this = regrabbed;
                            bounds_after_first_map_relayout = relayout_bounds;
                            if !opened.is_empty() {
                                kick_visual_animation_pump(dispatcher);
                            }
                        }
                        dispatcher.dispatch_post_action(PostWlSurface::PlaceDialog {
                            root: resolved_surface,
                        });
                        dispatcher.dispatch_post_action(PostWlSurface::FocusNewToplevel {
                            root_surface: resolved_surface,
                        });
                        if crate::input::toplevel_drag::tracks_root(state, resolved_surface) {
                            dispatcher.dispatch_post_action(PostWlSurface::ToplevelDragMapped {
                                root: resolved_surface,
                            });
                        }
                    }
                    if let Some(window) = state.window_for_root(resolved_surface) {
                        if is_mapped {
                            crate::capture::map_window(state, dispatcher, window);
                        } else {
                            crate::capture::unmap_window(state, dispatcher, window);
                        }
                    }
                    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
                }
            } else if this.has_xwayland_role() && state.window_for_root(resolved_surface).is_some()
            {
                if is_mapped {
                    state.confirm_root_surface_mapped(resolved_surface);
                    let (regrabbed, opened) = client
                        .with_ungrabbed(this, state, |state| state.relayout_dirty_workspaces());
                    this = regrabbed;
                    if !opened.is_empty() {
                        kick_visual_animation_pump(dispatcher);
                    }
                    dispatcher.dispatch_post_action(PostWlSurface::FocusNewToplevel {
                        root_surface: resolved_surface,
                    });
                }
                if let Some(window) = state.window_for_root(resolved_surface) {
                    if is_mapped {
                        crate::capture::map_window(state, dispatcher, window);
                        crate::capture::sync_layout_shell_state(state, dispatcher, window);
                        crate::capture::sync_activated_from_keyboard_focus(state, dispatcher);
                    } else {
                        crate::capture::unmap_window(state, dispatcher, window);
                    }
                }
                if let Some(workspace_id) = state.root_surface_workspace(resolved_surface) {
                    crate::tiling::mark_workspace_dirty(state, workspace_id);
                }
                dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
            }
            if !is_mapped {
                if let Some(close) =
                    crate::input::focus::focused_window_close(state, resolved_surface)
                {
                    dispatcher.dispatch_post_action(PostWlSurface::FocusSuccessorAfterClose(close));
                }
                if crate::input::tiled_drag_tracks_surface(state, resolved_surface) {
                    dispatcher.dispatch_post_action(PostWlSurface::CancelTiledDrag {
                        root: resolved_surface,
                    });
                }
                if crate::input::toplevel_drag::tracks_root(state, resolved_surface) {
                    dispatcher.dispatch_post_action(PostWlSurface::DetachToplevelDrag {
                        root: resolved_surface,
                    });
                }
            }
        }

        if changed || needs_frame {
            if let Some(bounds) = match bounds_after_first_map_relayout {
                Some(bounds) => Some(bounds),
                None => {
                    let bounds;
                    (this, bounds) = client.with_ungrabbed(this, state, |state| {
                        let surface = resolved_surface.rref(state);
                        state.surface_scene_bounds_with_ref(resolved_surface, surface)
                    });
                    bounds
                }
            } {
                request_outputs(state, this.output_membership_ids(), dispatcher);
                request_outputs_for_rect(state, bounds, dispatcher);
            } else {
                request_outputs(state, this.output_membership_ids(), dispatcher);
                request_active_output(state, dispatcher);
            }
        }

        if this.role == Role::InputPopup {
            dispatcher.dispatch_post_action(crate::input::ime::RepositionInputPopups);
        }

        if matches!(this.shell_surface, Some(ShellSurfaceObject::LayerSurface(_))) {
            dispatcher.dispatch(ClientManagerAction::LayerShellLayoutChanged);
        }
    }

    fn validate_pending_sync(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) -> bool {
        if self.syncobj_surface_object.is_none() {
            return true;
        }

        let pending_sync = &self.pending.sync;
        let sync_present = !pending_sync.acquire.is_none() || !pending_sync.release.is_none();
        let Some(pending_buffer) = self.pending.buffer.as_ref() else {
            if sync_present {
                self.send_syncobj_surface_error(
                    client,
                    dispatcher,
                    wp_linux_drm_syncobj_surface_v1::Error::NoBuffer as u32,
                    "explicit sync points require a pending non-null buffer",
                );
                return false;
            }
            return true;
        };

        let Some(buffer_object_id) = pending_buffer else {
            if sync_present {
                self.send_syncobj_surface_error(
                    client,
                    dispatcher,
                    wp_linux_drm_syncobj_surface_v1::Error::NoBuffer as u32,
                    "explicit sync points require a pending non-null buffer",
                );
                return false;
            }
            return true;
        };

        let Some(_) = buffer_object_id.object.try_ref(state) else {
            let msg = if state.objects.contains(buffer_object_id.object.object_id()) {
                "pending buffer object is not a wl_buffer"
            } else {
                "pending wl_buffer disappeared before commit"
            };
            client.protocol_violation_disconnect(
                dispatcher,
                self.id.into(),
                wl_display::Error::InvalidObject as u32,
                msg,
            );
            return false;
        };
        if !buffer_object_id.content.supports_explicit_sync() {
            self.send_syncobj_surface_error(
                client,
                dispatcher,
                wp_linux_drm_syncobj_surface_v1::Error::UnsupportedBuffer as u32,
                "explicit sync currently only supports dma-buf wl_buffers",
            );
            return false;
        }
        if pending_sync.acquire.is_none() {
            self.send_syncobj_surface_error(
                client,
                dispatcher,
                wp_linux_drm_syncobj_surface_v1::Error::NoAcquirePoint as u32,
                "missing acquire point for explicit-sync surface commit",
            );
            return false;
        }
        if pending_sync.release.is_none() {
            self.send_syncobj_surface_error(
                client,
                dispatcher,
                wp_linux_drm_syncobj_surface_v1::Error::NoReleasePoint as u32,
                "missing release point for explicit-sync surface commit",
            );
            return false;
        }
        if let SurfaceSyncState {
            acquire:
                sync::AcquireSync::TimelinePoint { timeline: acquire_timeline, point: acquire_point },
            release:
                sync::ReleasePoint::TimelinePoint { timeline: release_timeline, point: release_point },
        } = pending_sync
        {
            if acquire_timeline.handle() == release_timeline.handle()
                && acquire_point >= release_point
            {
                self.send_syncobj_surface_error(
                    client,
                    dispatcher,
                    wp_linux_drm_syncobj_surface_v1::Error::ConflictingPoints as u32,
                    "acquire timeline point must be strictly less than release point",
                );
                return false;
            }
        }

        true
    }

    fn flush_frame_callbacks(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let callbacks = std::mem::take(&mut self.pending_frame_callbacks);
        Self::complete_frame_callbacks(client, state, dispatcher, callbacks);
    }

    fn route_frame_callbacks(
        &mut self,
        callbacks: Vec<Resolved<WlCallbackId>>,
    ) -> (Vec<Resolved<WlCallbackId>>, bool) {
        if callbacks.is_empty() {
            return (Vec::new(), false);
        }
        if self.on_screen.logical_size.is_some() {
            self.on_screen.frame_callbacks.extend(callbacks);
            return (Vec::new(), true);
        }
        (callbacks, false)
    }

    fn route_presentation_feedbacks(
        &mut self,
        feedbacks: Vec<SurfacePresentationFeedback>,
    ) -> (Vec<SurfacePresentationFeedback>, bool) {
        if feedbacks.is_empty() {
            return (Vec::new(), false);
        }
        if self.on_screen.logical_size.is_some() {
            let superseded =
                std::mem::replace(&mut self.on_screen.presentation_feedbacks, feedbacks);
            return (superseded, true);
        }
        (feedbacks, false)
    }

    pub(super) fn discard_feedbacks(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        feedbacks: Vec<SurfacePresentationFeedback>,
    ) {
        use crate::wayland_objects::wl_display::WlDisplay;

        for feedback in feedbacks {
            if !feedback.feedback.exists(state) {
                continue;
            }
            {
                let feedback_object = feedback.feedback.rref(state);
                feedback_object.send_discarded(dispatcher);
            }
            client.delete_id(
                &WlDisplay::default(),
                state,
                dispatcher,
                feedback.feedback.wayland_id().into(),
            );
        }
    }

    pub fn discard_all_presentation_feedbacks(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let pending_feedbacks = std::mem::take(&mut self.pending_presentation_feedbacks);
        Self::discard_feedbacks(client, state, dispatcher, pending_feedbacks);
        let cached_feedbacks = self
            .cached_update
            .as_mut()
            .map(|cached| std::mem::take(&mut cached.presentation_feedbacks))
            .unwrap_or_default();
        Self::discard_feedbacks(client, state, dispatcher, cached_feedbacks);
        for update in self.queue.iter_mut() {
            let queued_feedbacks = std::mem::take(&mut update.presentation_feedbacks);
            Self::discard_feedbacks(client, state, dispatcher, queued_feedbacks);
        }
        let on_screen_feedbacks = std::mem::take(&mut self.on_screen.presentation_feedbacks);
        Self::discard_feedbacks(client, state, dispatcher, on_screen_feedbacks);
    }

    pub fn discard_presentation_feedbacks_for_backend(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        backend: crate::state::BackendOutputRef,
    ) {
        let split_feedbacks = |feedbacks: &mut Vec<SurfacePresentationFeedback>, state: &State| {
            let mut retained = Vec::new();
            let mut discarded = Vec::new();
            for feedback in feedbacks.drain(..) {
                let matches_backend = match feedback.output_id {
                    Some(output_id) => state.backend_for_output(output_id) == Some(backend),
                    None => true,
                };
                if matches_backend {
                    discarded.push(feedback);
                } else {
                    retained.push(feedback);
                }
            }
            *feedbacks = retained;
            discarded
        };

        let pending_feedbacks = split_feedbacks(&mut self.pending_presentation_feedbacks, state);
        Self::discard_feedbacks(client, state, dispatcher, pending_feedbacks);
        let cached_feedbacks = self
            .cached_update
            .as_mut()
            .map(|cached| split_update_feedbacks(cached, &split_feedbacks, state))
            .unwrap_or_default();
        Self::discard_feedbacks(client, state, dispatcher, cached_feedbacks);
        let queued_feedbacks: Vec<SurfacePresentationFeedback> = self
            .queue
            .iter_mut()
            .flat_map(|update| split_update_feedbacks(update, &split_feedbacks, state))
            .collect();
        Self::discard_feedbacks(client, state, dispatcher, queued_feedbacks);
        let on_screen_feedbacks =
            split_feedbacks(&mut self.on_screen.presentation_feedbacks, state);
        Self::discard_feedbacks(client, state, dispatcher, on_screen_feedbacks);
    }

    pub fn render_state(&self) -> Option<SurfaceRenderState> {
        self.render_state_at(0, 0)
    }

    pub fn take_frame_callbacks(&mut self) -> Vec<Resolved<WlCallbackId>> {
        std::mem::take(&mut self.on_screen.frame_callbacks)
    }

    pub fn render_state_at(&self, base_x: i32, base_y: i32) -> Option<SurfaceRenderState> {
        let logical_size = self.on_screen.logical_size?;
        let x = base_x + self.on_screen.x;
        let y = base_y + self.on_screen.y;
        let local_bounds = RectI::new(0, 0, logical_size.width, logical_size.height)?;
        Some(SurfaceRenderState {
            surface: None,
            buffer: self.on_screen.buffer.clone()?,
            x,
            y,
            width: logical_size.width,
            height: logical_size.height,
            tex_transform: self.on_screen.sample_transform,
            damage: self.on_screen.damage.translated(x, y),
            opaque_region: self
                .on_screen
                .opaque_region
                .to_region_clipped(local_bounds)
                .translated(x, y),
            sync: self.on_screen.sync.clone(),
            presentation_feedbacks: Vec::new(),
            inherit_root_clip: true,
            alpha_multiplier: self.on_screen.alpha_multiplier,
            color_description: self
                .on_screen
                .color
                .as_ref()
                .map(|(description, _)| Rc::clone(description))
                .unwrap_or_else(|| Rc::clone(&self.untagged_description)),
            render_intent: self
                .on_screen
                .color
                .as_ref()
                .map(|(_, intent)| *intent)
                .unwrap_or(RenderIntent::Perceptual),
            representation: self.on_screen.representation,
        })
    }

    pub fn take_render_state_at(
        &mut self,
        surface: Resolved<WlSurfaceId>,
        base_x: i32,
        base_y: i32,
    ) -> Option<SurfaceRenderState> {
        let mut state = self.render_state_at(base_x, base_y)?;
        state.surface = Some(surface);
        state.presentation_feedbacks = std::mem::take(&mut self.on_screen.presentation_feedbacks);
        self.on_screen.damage.clear();
        Some(state)
    }

    pub fn accepts_input_at(&self, x: f64, y: f64) -> bool {
        self.accepts_input_at_with_origin(x, y, 0, 0)
    }

    pub fn accepts_input_at_with_origin(&self, x: f64, y: f64, base_x: i32, base_y: i32) -> bool {
        let Some(surface_state) = self.render_state_at(base_x, base_y) else {
            return false;
        };
        if x < surface_state.x as f64
            || y < surface_state.y as f64
            || x >= (surface_state.x + surface_state.width) as f64
            || y >= (surface_state.y + surface_state.height) as f64
        {
            return false;
        }
        let local_x = (x - surface_state.x as f64).floor() as i32;
        let local_y = (y - surface_state.y as f64).floor() as i32;
        self.on_screen.input_region.contains(local_x, local_y)
    }

    pub fn logical_size(&self) -> Option<SizeI> {
        self.on_screen.logical_size
    }

    pub(crate) fn committed_input_region(&self) -> &RegionValue {
        &self.on_screen.input_region
    }

    pub(crate) fn pointer_lock_warp_geometry(
        &self,
    ) -> crate::input::pointer_lock::PointerLockWarpGeometry {
        crate::input::pointer_lock::PointerLockWarpGeometry {
            origin: self.root_position(),
            input_region: self.on_screen.input_region.clone(),
        }
    }

    pub fn global_logical_bounds(&self) -> Option<RectI> {
        let logical_size = self.logical_size()?;
        let (x, y) = self.root_position();
        RectI::new(x, y, logical_size.width, logical_size.height)
    }

    pub fn force_unmap_for_role_dismissal(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let warp = self.pointer_lock_warp_geometry();
        let resolved = client.resolve(self.id, state);
        self.discard_all_presentation_feedbacks(client, state, dispatcher);
        self.drop_queued_updates(client, state, dispatcher);
        self.retire_current();
        self.on_screen.logical_size = None;
        self.on_screen.damage.clear();
        if let Some(resolved) = resolved {
            crate::input::pointer_lock::release_pointer_lock_for_surface(
                state,
                dispatcher,
                resolved,
                Some(warp),
                crate::input::pointer_lock::PointerLockRelease {
                    send_unlocked: true,
                    apply_lifetime: true,
                    warp: true,
                },
            );
            crate::input::pointer_confine::release_pointer_confine_for_surface(
                state, dispatcher, resolved, true,
            );
        }
        dispatcher
            .dispatch_to(client.client_id(), crate::clients::ClientAction::RefreshSurfaceOutputs);
        self.latest.buffer = None;
    }

    pub fn take_retired_buffers(&mut self) -> Vec<RetiredBufferCandidate> {
        std::mem::take(&mut self.retired_buffers)
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(crate) fn test_frame_callback_count(&self) -> usize {
        self.on_screen.frame_callbacks.len()
    }

    #[cfg(test)]
    pub(crate) fn test_presentation_feedback_count(&self) -> usize {
        self.on_screen.presentation_feedbacks.len()
    }

    #[cfg(test)]
    pub(crate) fn test_push_frame_callback_and_feedback(&mut self) {
        self.on_screen.frame_callbacks.push(Resolved::new(
            self.client_id,
            WlCallbackId::from(1),
            ObjectId::from(1usize),
        ));
        self.on_screen
            .presentation_feedbacks
            .push(SurfacePresentationFeedback {
                feedback: Resolved::new(
                    self.client_id,
                    WpPresentationFeedbackId::from(1),
                    ObjectId::from(2usize),
                ),
                version: 1,
                output_id: None,
            });
    }

    #[cfg(test)]
    pub(crate) fn test_push_current_child(&mut self, child: Resolved<WlSurfaceId>) {
        self.tree.current_stack.push(StackEntry::Child(child));
    }

    #[cfg(test)]
    pub(crate) fn test_mark_subsurface_at(
        &mut self,
        parent: Resolved<WlSurfaceId>,
        position: (i32, i32),
    ) {
        self.role = Role::Subsurface;
        self.subsurface = Some(SubsurfaceState {
            object: Resolved::new(self.client_id, WlSubsurfaceId::from(1), ObjectId::from(1usize)),
            parent_surface: parent,
            pending_position: position,
            current_position: position,
            sync_mode: SubsurfaceSyncMode::Desync,
        });
    }

    #[cfg(test)]
    pub(crate) fn test_map(
        &mut self,
        buffer_object_id: ObjectId,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        let object = Resolved::new(
            self.client_id,
            WlBufferId::from(buffer_object_id.index()),
            buffer_object_id,
        );
        // Most tests only need a mapped surface. The id is remembered for identity;
        // pixels are a blank buffer of the mapped size. A test that must show a real
        // wl_buffer replaces this with `test_show_buffer`.
        let attached = crate::buffer_content::AttachedBuffer::placeholder(object, width, height);
        self.latest.buffer = Some(attached.clone());
        self.on_screen = OnScreen {
            buffer: Some(attached),
            content_revision: 1,
            x,
            y,
            sync: SurfaceSyncState::default(),
            buffer_transform: BufferTransform::Normal,
            buffer_scale: 1,
            viewport: SurfaceViewport::default(),
            presentation_hint: PresentationHint::Vsync,
            alpha_multiplier: u32::MAX,
            blur_region: None,
            color: None,
            representation: ColorRepresentation::default(),
            logical_size: Some(SizeI { width, height }),
            sample_transform: transform::Mat3::identity(),
            damage: Self::full_damage_for_logical_size(SizeI { width, height }),
            frame_callbacks: Vec::new(),
            presentation_feedbacks: Vec::new(),
            input_region: RegionValue::infinite(),
            opaque_region: RegionValue::empty(),
            release_callbacks: Vec::new(),
            wire_inputs: None,
        };
    }

    /// Replace the buffer `test_map` installed with one that carries real pixels.
    #[cfg(test)]
    pub(crate) fn test_show_buffer(&mut self, attached: crate::buffer_content::AttachedBuffer) {
        self.latest.buffer = Some(attached.clone());
        self.on_screen.buffer = Some(attached);
    }

    #[cfg(test)]
    pub(crate) fn test_clear_committed_buffer(&mut self) {
        self.latest.buffer = None;
        self.on_screen.buffer = None;
        self.on_screen.logical_size = None;
    }

    #[cfg(test)]
    pub(crate) fn test_latest_buffer_is_some(&self) -> bool {
        self.latest.buffer.is_some()
    }

    #[cfg(test)]
    pub(crate) fn test_set_input_region(&mut self, region: RegionValue) {
        self.on_screen.input_region = region;
    }

    /// The acquire wait of the queue head ended. `failed` means it could not be
    /// watched: the head is then applied without its acquire point.
    pub(crate) fn acquire_wait_done(
        &mut self,
        waiter_id: crate::sync::AcquireReadyWaiterId,
        failed: bool,
    ) {
        if self.acquire_waiter != Some(waiter_id) {
            return;
        }
        self.acquire_waiter = None;
        if failed {
            if let Some(head) = self.queue.front_mut() {
                head.acquire_forced = true;
            }
            log::warn!("surface {:?}: acquire wait failed; applying without it", self.id);
        }
    }

    fn activate_mapped_state(&mut self, next: MappedBufferState) {
        let mut frame_callbacks = std::mem::take(&mut self.on_screen.frame_callbacks);
        frame_callbacks.extend(next.frame_callbacks.iter().copied());
        self.retire_current();
        self.on_screen = OnScreen {
            buffer: Some(next.buffer),
            content_revision: self.next_content_revision(),
            x: next.x,
            y: next.y,
            sync: next.sync,
            buffer_transform: next.buffer_transform,
            buffer_scale: next.buffer_scale,
            viewport: next.viewport,
            presentation_hint: next.presentation_hint,
            alpha_multiplier: next.alpha_multiplier,
            blur_region: next.blur_region,
            color: next.color,
            representation: next.representation,
            logical_size: Some(SizeI { width: next.width, height: next.height }),
            sample_transform: next.tex_transform,
            damage: Self::full_damage_for_logical_size(SizeI {
                width: next.width,
                height: next.height,
            }),
            frame_callbacks,
            presentation_feedbacks: next.presentation_feedbacks,
            input_region: next.input_region,
            opaque_region: next.opaque_region,
            release_callbacks: next.release_callbacks,
            wire_inputs: next.wire_inputs,
        };
    }

    fn retire_current(&mut self) {
        if let Some(attached) = self.on_screen.buffer.take() {
            self.retired_buffers.push(RetiredBufferCandidate {
                buffer: attached.object,
                sync: self.on_screen.sync.clone(),
                release_callbacks: std::mem::take(&mut self.on_screen.release_callbacks),
            });
        }
        self.on_screen.sync = SurfaceSyncState::default();
        self.on_screen.logical_size = None;
        self.on_screen.sample_transform = transform::Mat3::identity();
        self.on_screen.damage.clear();
    }

    fn next_content_revision(&self) -> u64 {
        self.on_screen.content_revision.saturating_add(1).max(1)
    }

    /// Drops every queued update and the cached update of this surface: they are
    /// discarded (buffers released, callbacks completed), and the surface is no
    /// longer re-mapped by a copy carried in a parent's update.
    fn drop_queued_updates(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.dropped_through_seq = self.next_update_seq;
        if let Some(waiter) = self.acquire_waiter.take() {
            dispatcher.stop_model(waiter);
        }
        let surface = client.resolve(self.id, state).unwrap();
        state.commit_gate.forget_surface(surface);
        let count = self.queue.len();
        for update in std::mem::take(&mut self.queue) {
            content_update::discard_update(
                client,
                state,
                dispatcher,
                &mut self.applied_update_seq,
                update,
            );
        }
        if let Some(update) = self.cached_update.take() {
            content_update::discard_update(
                client,
                state,
                dispatcher,
                &mut self.applied_update_seq,
                update,
            );
        }
        state.commit_queues.forget_surface(surface, count);
        self.latest.buffer = None;
        // The discarded updates may have been the ones another queue waited for.
        if state.commit_queues.has_queued(client.client_id()) {
            dispatcher
                .dispatch_to(client.client_id(), crate::clients::ClientAction::FlushCommitQueues);
        }
    }

    /// Called when a synchronized subsurface turns desynchronized: its cached
    /// update is applied as a normal update, with the full commit tail.
    pub fn submit_cached_if_parent_desynchronized(
        mut this: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        if this.sync_mode() != Some(SubsurfaceSyncMode::Desync)
            || this.parent_behaves_synchronized(state)
        {
            return;
        }
        let Some(update) = this.cached_update.take() else {
            return;
        };
        commit_queue::submit(this, client, state, dispatcher, update);
    }

    fn detach_subsurface_tree(
        child_surface: Resolved<WlSurfaceId>,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let grandchildren = {
            let Some(child_surface_ref) = child_surface.try_ref(state) else {
                return;
            };
            child_surface_ref.child_surface_ids()
        };

        for grandchild_surface in grandchildren {
            Self::detach_subsurface_tree(grandchild_surface, client, state, dispatcher);
        }

        {
            let Some(mut child_surface_ref) = child_surface.try_grab(state) else {
                return;
            };
            child_surface_ref.unmap_due_to_parent_loss(client, state, dispatcher);
            // The wl_subsurface protocol object stays. The client destroys it
            // afterward; deleting it here makes that request invalid_object.
            child_surface_ref.destroy_subsurface_role(client, state, dispatcher);
        }

        state.register_root_surface(child_surface);
    }

    pub fn delete_self(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.complete_release_callbacks_now(client, state, dispatcher);
        let object = client.resolve(self.id, state).unwrap();
        if let Some(serial) = self.xwayland_serial {
            dispatcher.dispatch(XwaylandAction::Surface {
                client: client.client_id(),
                event: SurfaceEvent::Destroyed { serial, surface: object },
            });
        }
        if let Some(window) = state.window_for_root(object) {
            begin_managed_toplevel_close_surface(state, window, object, self, dispatcher);
        }
        let scene_bounds = state.surface_scene_bounds_with_ref(object, self);
        state.seat.clear_cursor_surface(object);
        let geometry = self.pointer_lock_warp_geometry();
        let constraint = self.pointer_constraint.take();
        defunct_pointer_constraint(constraint, state, dispatcher, true, Some(geometry));
        self.release_idle_inhibitors(state);
        crate::wayland_objects::zwp_linux_dmabuf_feedback_v1::retire_surface(state, object);
        self.discard_all_presentation_feedbacks(client, state, dispatcher);
        for child_surface in self.child_surface_ids() {
            Self::detach_subsurface_tree(child_surface, client, state, dispatcher);
        }
        if let Some(parent_surface) = self.parent_surface() {
            if let Some(parent_surface) = parent_surface.try_ref_mut(state) {
                parent_surface.remove_child_from_tree(object);
            }
        }
        self.drop_queued_updates(client, state, dispatcher);
        state.commit_gate.forget_surface(object);
        self.cleanup_extension_objects();
        self.detach_timing_objects(state);
        // A role destroy records the close before `destroy_window`. By the time
        // this surface goes away the window is no longer managed, so that record
        // is the only one that still names the focused close. It is used only
        // while this surface still owns the keyboard.
        let remembered = self.closed_window_focus.take();
        let closed_window = if let Some(close) = remembered {
            // This surface is grabbed, and `destroy_window` already dropped the
            // managed window, so resolving its root would try_ref a grabbed id.
            let still_focused = state
                .seat
                .keyboard_focus()
                .is_some_and(|focus| focus.surface == object);
            still_focused.then_some(close)
        } else {
            crate::input::focus::focused_window_close(state, object)
        };
        // Leave goes out while this id is still live. delete_id below retires it,
        // and the post-action enters the next window without naming this surface.
        let released_keyboard =
            crate::input::focus::leave_destroyed_surface_focus(state, dispatcher, object);
        state.unregister_root_surface(object);
        dispatcher.dispatch_post_action(PostWlSurface::ClearFocusAfterDestroy {
            closed_window,
            released_keyboard,
        });
        crate::idle_inhibit::queue_refresh(dispatcher);
        use crate::wayland_objects::wl_display::WlDisplay;
        client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
        if let Some(bounds) = scene_bounds {
            request_outputs(state, self.output_membership_ids(), dispatcher);
            request_outputs_for_rect(state, bounds, dispatcher);
        } else {
            request_outputs(state, self.output_membership_ids(), dispatcher);
            request_active_output(state, dispatcher);
        }
    }

    pub fn client_id(&self) -> ClientId {
        self.client_id
    }

    pub fn surface_id(&self) -> WlSurfaceId {
        self.id
    }

    pub fn queue_presentation_feedback(
        &mut self,
        feedback: Resolved<WpPresentationFeedbackId>,
        version: u32,
        output_id: Option<OutputId>,
    ) {
        self.pending_presentation_feedbacks
            .push(SurfacePresentationFeedback { feedback, version, output_id });
    }
}

impl Object for WlSurface {
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
        use wl_surface::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                if self.xwayland_surface_object().is_some() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_surface::Error::DefunctRoleObject as u32,
                        "wl_surface destroyed before its xwayland_surface_v1",
                    );
                    return Ok(());
                }
                self.delete_self(client, state, dispatcher);
            }
            Attach { buffer, x, y } => {
                self.on_attach(client, state, buffer, x, y, dispatcher);
            }
            Damage { x, y, width, height } => {
                self.pending
                    .damage_surface
                    .push(DamageRect { x, y, width, height });
            }
            Frame { callback } => {
                client.insert_object(state, WlCallback::new(callback));
                self.pending_frame_callbacks
                    .push(client.resolve(callback, state).unwrap());
            }
            SetOpaqueRegion { region } => {
                self.on_set_opaque_region(client, state, region, dispatcher);
            }
            SetInputRegion { region } => {
                self.on_set_input_region(client, state, region, dispatcher);
            }
            Commit => {
                self.on_commit(client, state, dispatcher);
            }
            SetBufferTransform { transform } => {
                if self.version < wl_surface::SET_BUFFER_TRANSFORM_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "wl_surface.set_buffer_transform requires wl_surface version >= 2",
                    );
                    return Ok(());
                }
                let Some(transform) = BufferTransform::from_wayland(transform) else {
                    self.send_error(
                        dispatcher,
                        wl_surface::Error::InvalidTransform as u32,
                        "invalid wl_surface buffer transform",
                    );
                    return Ok(());
                };
                self.pending.buffer_transform = transform;
            }
            SetBufferScale { scale } => {
                if self.version < wl_surface::SET_BUFFER_SCALE_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "wl_surface.set_buffer_scale requires wl_surface version >= 3",
                    );
                    return Ok(());
                }
                if scale < 1 {
                    self.send_error(
                        dispatcher,
                        wl_surface::Error::InvalidScale as u32,
                        "wl_surface buffer scale must be at least 1",
                    );
                    return Ok(());
                }
                self.pending.buffer_scale = scale;
            }
            DamageBuffer { x, y, width, height } => {
                if self.version < wl_surface::DAMAGE_BUFFER_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "wl_surface.damage_buffer requires wl_surface version >= 4",
                    );
                    return Ok(());
                }
                self.pending
                    .damage_buffer
                    .push(DamageRect { x, y, width, height });
            }
            GetRelease { callback } => {
                if self.version < wl_surface::GET_RELEASE_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "wl_surface.get_release requires wl_surface version >= 7",
                    );
                    return Ok(());
                }
                client.insert_object(state, WlCallback::new(callback));
                self.pending_release_callbacks
                    .push(client.resolve(callback, state).unwrap());
            }
            Offset { x, y } => {
                if self.version < wl_surface::OFFSET_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "wl_surface.offset requires wl_surface version >= 5",
                    );
                    return Ok(());
                }
                if self.role != Role::Subsurface {
                    self.pending.offset = (x, y);
                }
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        if let Some(resolved) = client.resolve(self.id, state) {
            crate::wayland_objects::zwp_linux_dmabuf_feedback_v1::retire_surface(state, resolved);
            if let Some(window) = state.window_for_root(resolved) {
                begin_managed_toplevel_close_surface(state, window, resolved, self, dispatcher);
            }
        }
        let constraint = self.pointer_constraint.take();
        defunct_pointer_constraint(constraint, state, dispatcher, false, None);
        self.release_idle_inhibitors(state);
        if let Some(waiter) = self.acquire_waiter.take() {
            dispatcher.stop_model(waiter);
        }
    }
}

/// Commit a null buffer on a root surface: it unmaps. Test-only, like `test_commit_new_buffer`.
#[cfg(test)]
pub(crate) fn test_commit_null_buffer(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut crate::runner::Dispatcher,
    surface: Resolved<WlSurfaceId>,
) {
    {
        let mut wl_surface = surface.grab(state);
        wl_surface.pending.buffer = Some(None);
    }
    let mut client_dispatcher = client.test_dispatcher(dispatcher);
    {
        let mut wl_surface = surface.grab(state);
        wl_surface.on_commit(client, state, &mut client_dispatcher);
    }
    client_dispatcher.drain_post_actions(client, state);
}

/// Commit one new buffer on an X root and run the map-time focus action.
#[cfg(test)]
pub(crate) fn test_commit_new_buffer(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut crate::runner::Dispatcher,
    surface: Resolved<WlSurfaceId>,
    width: i32,
    height: i32,
) {
    let buffer_id = WlBufferId::from(9100);
    let object_id = crate::test_support::insert_test_shm_buffer(
        state,
        client.client_id(),
        buffer_id,
        width,
        height,
    );
    let buffer = crate::buffer_content::AttachedBuffer::resolve(
        state,
        Resolved::new(client.client_id(), buffer_id, object_id),
    )
    .expect("test buffer");
    {
        let mut wl_surface = surface.grab(state);
        wl_surface.pending.buffer = Some(Some(buffer));
    }
    let mut client_dispatcher = client.test_dispatcher(dispatcher);
    {
        let mut wl_surface = surface.grab(state);
        wl_surface.on_commit(client, state, &mut client_dispatcher);
    }
    client_dispatcher.drain_post_actions(client, state);
}

#[cfg(test)]
mod tests {
    use {
        super::{
            ContentUpdate, DamageRect, OnScreen, PresentationHint, Role, RoleUpdate,
            SurfaceCommitPayload, SurfacePresentationFeedback, WlSurface,
        },
        crate::{
            capture::{test_sent_arg, test_sent_opcodes},
            clients::{
                Client, ClientAction, ClientId, ClientManagerAction, RawEvent, RawRequest, Resolved,
            },
            cursor::CursorSourceRef,
            drm::VisualAnimationPumpAction,
            input::{focus::set_keyboard_focus, SurfaceFocus, SurfaceFocusPick, SurfaceRect},
            io_uring::model::IoUringAction,
            object::{Object, RequestResult},
            protocol_generated::{
                xdg_wm_base, ExtSessionLockSurfaceV1Id, ExtSessionLockV1Id, WaylandId, WlBufferId,
                WlCallbackId, WlKeyboardId, WlPointerId, WlSubsurfaceId, WlSurfaceId,
                WpFractionalScaleV1Id, WpLinuxDrmSyncobjSurfaceV1Id, WpPresentationFeedbackId,
                WpViewportId, XdgSurfaceId, XdgToplevelId, XdgWmBaseId, XwaylandSurfaceV1Id,
                ZwlrLayerShellV1Id, ZwlrLayerSurfaceV1Id, ZwpLockedPointerV1Id,
            },
            region::{DamageRegion, RectI, RegionSpec, RegionValue},
            runner::{Dispatcher, Model},
            state::{
                BackendOutputRef, DrmId, FractionalScale, ObjectId, Output, OutputId, OutputMode,
                State,
            },
            sync::SurfaceSyncState,
            test_support::{
                focus_for_surface, get_toplevel_request, insert_test_shm_buffer,
                raw_request_from_event, resolved_wl_surface as root, state_for_tests,
                test_client_and_dispatcher, test_output_with_crtc_name,
            },
            transform::{self, BufferTransform, SizeI, SurfaceViewport},
            visual_animation::WindowLifecycleAnim,
            wayland_objects::{
                ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
                pointer_constraint::{ConstraintLifetime, SurfacePointerConstraint},
                wl_buffer::WlBuffer,
                wl_callback::WlCallback,
                wl_keyboard::WlKeyboard,
                wl_pointer::WlPointer,
                wl_subsurface::WlSubsurface,
                wp_fractional_scale_v1::WpFractionalScaleV1,
                wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1,
                wp_presentation_feedback::WpPresentationFeedback,
                wp_viewport::WpViewport,
                xdg_surface::{
                    xdg_toplevel::{ToplevelDecorationMode, XdgToplevel},
                    XdgSurface,
                },
                zwlr_layer_surface_v1::{LayerShellLayer, ZwlrLayerSurfaceV1},
                zwp_locked_pointer_v1::ZwpLockedPointerV1,
            },
            wire_scale::WireScale,
            xwayland::{SurfaceEvent, XwaylandAction},
        },
    };

    fn make_surface() -> WlSurface {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(7), 6);
        surface.on_screen = OnScreen {
            buffer: Some(fake_buffer(ObjectId::from(1usize))),
            content_revision: 1,
            x: 10,
            y: 20,
            sync: SurfaceSyncState::default(),
            buffer_transform: BufferTransform::Normal,
            buffer_scale: 1,
            viewport: SurfaceViewport::default(),
            presentation_hint: PresentationHint::Vsync,
            alpha_multiplier: u32::MAX,
            blur_region: None,
            color: None,
            representation: crate::color::ColorRepresentation::default(),
            logical_size: Some(SizeI { width: 100, height: 80 }),
            sample_transform: transform::Mat3::identity(),
            damage: DamageRegion::new(),
            frame_callbacks: Vec::new(),
            presentation_feedbacks: Vec::new(),
            input_region: RegionValue::infinite(),
            opaque_region: RegionValue::empty(),
            release_callbacks: Vec::new(),
            wire_inputs: None,
        };
        surface.latest.buffer = surface.on_screen.buffer.clone();
        surface
    }

    fn insert_subsurface_object(
        state: &mut State,
        id: WlSubsurfaceId,
        surface: Resolved<WlSurfaceId>,
        parent: Resolved<WlSurfaceId>,
    ) -> Resolved<WlSubsurfaceId> {
        let object_id = state
            .insert_object(WlSubsurface::new(id, surface, parent))
            .1;
        Resolved::new(surface.client_id(), id, object_id)
    }

    fn insert_client_subsurface_object(
        client: &mut Client,
        state: &mut State,
        id: WlSubsurfaceId,
        surface: Resolved<WlSurfaceId>,
        parent: Resolved<WlSurfaceId>,
    ) -> Resolved<WlSubsurfaceId> {
        let (subsurface, _) = client.insert_object(state, WlSubsurface::new(id, surface, parent));
        subsurface
    }

    fn insert_surface(state: &mut State, surface: WlSurface) -> ObjectId {
        let client_id = surface.client_id();
        let wl_id = surface.surface_id();
        let object_id = state.insert_object(surface).1;
        state.register_root_surface(Resolved::new(client_id, wl_id, object_id));
        object_id
    }

    fn attach_fractional_scale_object(state: &mut State, surface_object_id: ObjectId) -> ObjectId {
        let fractional_id = WpFractionalScaleV1Id::from(77);
        let object_id = state
            .insert_object(WpFractionalScaleV1::new(fractional_id, root(state, surface_object_id)))
            .1;
        let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
        surface
            .set_fractional_scale_object(Resolved::new(
                ClientId::from_raw(1),
                fractional_id,
                object_id,
            ))
            .unwrap();
        object_id
    }

    fn region_value(x: i32, y: i32, width: i32, height: i32) -> RegionValue {
        let mut spec = RegionSpec::default();
        spec.add_rect(RectI::new(x, y, width, height).unwrap());
        RegionValue::from_spec(spec)
    }

    fn feedback(id: u32, version: u32) -> SurfacePresentationFeedback {
        SurfacePresentationFeedback {
            feedback: Resolved::new(
                ClientId::from_raw(1),
                WpPresentationFeedbackId::from(id),
                ObjectId::from(id as usize),
            ),
            version,
            output_id: None,
        }
    }

    fn resolved_buffer(
        state: &State,
        object_id: ObjectId,
    ) -> crate::buffer_content::AttachedBuffer {
        let object =
            Resolved::new(ClientId::from_raw(1), WlBufferId::from(object_id.index()), object_id);
        crate::buffer_content::AttachedBuffer::resolve(state, object)
            .unwrap_or_else(|| crate::buffer_content::AttachedBuffer::placeholder(object, 2, 2))
    }

    fn fake_buffer(object_id: ObjectId) -> crate::buffer_content::AttachedBuffer {
        let object =
            Resolved::new(ClientId::from_raw(1), WlBufferId::from(object_id.index()), object_id);
        crate::buffer_content::AttachedBuffer::placeholder(object, 2, 2)
    }

    fn resolved_callback(id: u32) -> Resolved<crate::protocol_generated::WlCallbackId> {
        Resolved::new(
            ClientId::from_raw(1),
            crate::protocol_generated::WlCallbackId::from(id),
            ObjectId::from(id as usize),
        )
    }

    fn ack_configure_request(xdg_surface_id: XdgSurfaceId, serial: u32) -> RawRequest<'static> {
        let mut event = RawEvent::new(xdg_surface_id.into(), 4, 1);
        event.uint(serial);
        raw_request_from_event(event)
    }

    fn create_root_xdg_toplevel(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        surface_id: WlSurfaceId,
        xdg_surface_id: XdgSurfaceId,
        toplevel_id: XdgToplevelId,
    ) -> ObjectId {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let (surface_resolved, _) =
            client.insert_object(state, WlSurface::new(surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            assert!(surface
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .is_ok());
        }
        let (xdg_surface_resolved, _) = client.insert_object(
            state,
            XdgSurface::new(
                xdg_surface_id,
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    crate::state::ObjectId::from(1usize),
                ),
                crate::clients::Resolved::new(client.client_id(), surface_id, surface_object_id),
                7,
            ),
        );
        let xdg_surface_object_id = xdg_surface_resolved.object_id();
        state
            .objects
            .get_mut(surface_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .set_xdg_surface_impl(Resolved::new(
                client.client_id(),
                xdg_surface_id,
                xdg_surface_object_id,
            ));
        {
            let mut xdg_surface = client.grab_object(xdg_surface_id, state);
            xdg_surface.on(
                client,
                state,
                get_toplevel_request(xdg_surface_id, toplevel_id),
                &mut client_dispatcher,
            );
        }
        surface_object_id
    }

    fn commit_initial_xdg_toplevel(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        surface_object_id: ObjectId,
        xdg_surface_id: XdgSurfaceId,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(client, state, &mut client_dispatcher);
        }
        {
            let xdg_surface_object_id = client.object_id(xdg_surface_id);
            let mut xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface_object_id);
            xdg_surface.on(
                client,
                state,
                ack_configure_request(xdg_surface_id, 1),
                &mut client_dispatcher,
            );
        }
    }

    #[test]
    fn cursor_role_conflicts_with_xdg_role() {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(3), 6);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        assert!(surface.set_cursor_role(&mut client_dispatcher).is_ok());
        assert!(surface.has_cursor_role());
        assert_eq!(
            surface.set_role(Role::XdgSurface, &mut client_dispatcher),
            Err(xdg_wm_base::Error::Role)
        );
    }

    #[test]
    fn xwayland_role_is_assignable_and_conflicts_with_cursor_and_xdg() {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(3), 6);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        assert!(surface.assign_xwayland_role());
        assert!(surface.has_xwayland_role());
        assert_eq!(surface.role(), Role::XwaylandSurface);
        assert!(surface.assign_xwayland_role());
        assert_eq!(
            surface.set_role(Role::XdgSurface, &mut client_dispatcher),
            Err(xdg_wm_base::Error::Role)
        );
        assert!(surface.has_xwayland_role());
        assert!(surface.set_cursor_role(&mut client_dispatcher).is_err());
        assert!(surface.has_xwayland_role());

        let object = Resolved::new(
            ClientId::from_raw(3),
            XwaylandSurfaceV1Id::from(8),
            ObjectId::from(8usize),
        );
        let other = Resolved::new(
            ClientId::from_raw(3),
            XwaylandSurfaceV1Id::from(9),
            ObjectId::from(9usize),
        );
        assert!(surface.xwayland_surface_object().is_none());
        surface.set_xwayland_surface_object(object);
        assert_eq!(surface.xwayland_surface_object(), Some(object));
        surface.clear_xwayland_surface_object(other);
        assert_eq!(surface.xwayland_surface_object(), Some(object));
        surface.clear_xwayland_surface_object(object);
        assert!(surface.xwayland_surface_object().is_none());

        assert!(surface.xwayland_serial().is_none());
        surface.set_pending_xwayland_serial(7);
        assert_eq!(surface.pending.xwayland_serial, Some(7));
        assert!(surface.xwayland_serial().is_none());

        let mut cursor = WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(3), 6);
        assert!(cursor.set_cursor_role(&mut client_dispatcher).is_ok());
        assert!(!cursor.assign_xwayland_role());
        assert!(cursor.has_cursor_role());

        let mut xdg = WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(3), 6);
        assert!(xdg
            .set_role(Role::XdgSurface, &mut client_dispatcher)
            .is_ok());
        assert!(!xdg.assign_xwayland_role());
        assert_eq!(xdg.role(), Role::XdgSurface);
    }

    struct XwaylandCommit {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
    }

    impl XwaylandCommit {
        fn new() -> Self {
            let mut state = state_for_tests();
            let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(4));
            let surface = client
                .insert_object(
                    &mut state,
                    WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6),
                )
                .0;
            assert!(surface.grab(&mut state).assign_xwayland_role());
            let (xsurface, _) = client.insert_object(
                &mut state,
                crate::wayland_objects::xwayland_surface_v1::XwaylandSurfaceV1::new(
                    XwaylandSurfaceV1Id::from(5),
                    surface,
                ),
            );
            surface
                .grab(&mut state)
                .set_xwayland_surface_object(xsurface);
            Self { state, client, dispatcher: Dispatcher::default(), surface }
        }

        fn set_serial(&mut self, serial_lo: u32, serial_hi: u32) {
            let mut event = RawEvent::new(XwaylandSurfaceV1Id::from(5).into(), 0, 2);
            event.uint(serial_lo);
            event.uint(serial_hi);
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher
                .set_requester_object_id_for_tests(XwaylandSurfaceV1Id::from(5).into());
            self.client
                .grab_object(XwaylandSurfaceV1Id::from(5), &mut self.state)
                .on(
                    &mut self.client,
                    &mut self.state,
                    (&*event.into_bytes()).try_into().unwrap(),
                    &mut client_dispatcher,
                );
        }

        fn commit(&mut self) {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            let mut surface = self.surface.grab(&mut self.state);
            surface.on_commit(&mut self.client, &mut self.state, &mut client_dispatcher);
        }

        /// Captures the pending commit without applying it.
        fn capture(&mut self) -> ContentUpdate {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            let mut surface = self.surface.grab(&mut self.state);
            surface.capture_commit(&mut self.state, &mut client_dispatcher)
        }

        fn surface_events(&self) -> usize {
            self.dispatcher
                .test_count_queued_action(|action: &XwaylandAction| {
                    matches!(action, XwaylandAction::Surface { .. })
                })
        }

        fn request<K>(&mut self, object: K, opcode: u16)
        where
            K: crate::object::IdToObject + Into<WaylandId> + Copy,
        {
            let event = RawEvent::new(object.into(), opcode, 0);
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(object.into());
            self.client.grab_object(object, &mut self.state).on(
                &mut self.client,
                &mut self.state,
                (&*event.into_bytes()).try_into().unwrap(),
                &mut client_dispatcher,
            );
        }

        fn destroy_role(&mut self) {
            self.request(XwaylandSurfaceV1Id::from(5), 1);
        }

        fn destroy_surface(&mut self) {
            self.request(WlSurfaceId::from(4), 0);
        }
    }

    #[test]
    fn a_pending_serial_is_committed_once() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.set_serial(7, 0);
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_serial()
            .is_none());
        assert_eq!(xwayland.surface_events(), 0);

        xwayland.commit();

        let client_id = xwayland.client.client_id();
        let surface = xwayland.surface;
        assert_eq!(xwayland.surface.rref(&xwayland.state).xwayland_serial(), Some(7));
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .pending_xwayland_serial()
            .is_none());
        assert_eq!(xwayland.surface_events(), 1);
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &XwaylandAction| {
                matches!(
                    action,
                    XwaylandAction::Surface {
                        client,
                        event: SurfaceEvent::SerialAssigned { serial: 7, surface: assigned },
                    } if *client == client_id && *assigned == surface
                )
            }));

        xwayland.commit();
        assert_eq!(xwayland.surface_events(), 1);
    }

    #[test]
    fn capture_numbers_commits_in_order() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.commit();
        xwayland.commit();
        assert_eq!(
            xwayland
                .surface
                .rref(&xwayland.state)
                .test_applied_update_seq(),
            2
        );
    }

    #[test]
    fn xwayland_serial_applies_after_the_role_object_is_destroyed() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.set_serial(7, 0);
        let update = xwayland.capture();
        assert_eq!(update.xwayland_serial, Some(7));
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_serial()
            .is_none());

        let object = xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_surface_object()
            .expect("the role object is associated");
        xwayland
            .surface
            .grab(&mut xwayland.state)
            .clear_xwayland_surface_object(object);

        let client_id = xwayland.client.client_id();
        let surface = xwayland.surface;
        {
            let mut client_dispatcher = xwayland.client.test_dispatcher(&mut xwayland.dispatcher);
            let mut wl_surface = surface.grab(&mut xwayland.state);
            wl_surface.apply_content(
                &mut xwayland.client,
                &mut xwayland.state,
                &mut client_dispatcher,
                update,
            );
        }

        assert_eq!(xwayland.surface.rref(&xwayland.state).xwayland_serial(), Some(7));
        assert_eq!(xwayland.surface_events(), 1);
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &XwaylandAction| {
                matches!(
                    action,
                    XwaylandAction::Surface {
                        client,
                        event: SurfaceEvent::SerialAssigned { serial: 7, surface: assigned },
                    } if *client == client_id && *assigned == surface
                )
            }));
    }

    #[test]
    fn already_associated_uses_the_latest_committed_serial() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.set_serial(7, 0);
        let _update = xwayland.capture();
        {
            let surface = xwayland.surface.rref(&xwayland.state);
            assert_eq!(surface.latest_xwayland_serial(), Some(7));
            assert!(surface.xwayland_serial().is_none());
        }

        let client_id = xwayland.client.client_id();
        xwayland.set_serial(8, 0);
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == client_id)
            }));
    }

    #[test]
    fn pointer_constraint_region_of_a_replaced_constraint_is_dropped() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(3), 6),
        );
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        let (lock_a, _) = client.insert_object(
            &mut state,
            ZwpLockedPointerV1::new(
                ZwpLockedPointerV1Id::from(9),
                surface,
                pointer,
                ConstraintLifetime::Persistent,
                None,
            ),
        );
        let (lock_b, _) = client.insert_object(
            &mut state,
            ZwpLockedPointerV1::new(
                ZwpLockedPointerV1Id::from(10),
                surface,
                pointer,
                ConstraintLifetime::Persistent,
                None,
            ),
        );

        // Captured for lock A, applied after the surface switched to lock B.
        surface
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock_a));
        surface
            .grab(&mut state)
            .set_pending_pointer_constraint_region(Some(region_value(1, 1, 5, 5)));
        let update = capture_update(&mut client, &mut state, &mut dispatcher, surface);
        surface
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock_b));
        apply_captured(&mut client, &mut state, &mut dispatcher, surface, update);
        assert!(lock_a.try_ref(&state).unwrap().region().is_none());
        assert!(lock_b.try_ref(&state).unwrap().region().is_none());

        // The same constraint at apply time takes the region.
        surface
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock_b));
        surface
            .grab(&mut state)
            .set_pending_pointer_constraint_region(Some(region_value(2, 2, 3, 3)));
        let update = capture_update(&mut client, &mut state, &mut dispatcher, surface);
        apply_captured(&mut client, &mut state, &mut dispatcher, surface, update);
        assert!(lock_b.try_ref(&state).unwrap().region().is_some());
    }

    #[test]
    fn sync_child_pointer_region_applies_with_parent() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let (parent, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(3), 6),
        );
        let (child, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(5), ClientId::from_raw(3), 6),
        );
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(6), 9));
        let (subsurface, _) = client
            .insert_object(&mut state, WlSubsurface::new(WlSubsurfaceId::from(20), child, parent));
        let (lock, _) = client.insert_object(
            &mut state,
            ZwpLockedPointerV1::new(
                ZwpLockedPointerV1Id::from(9),
                child,
                pointer,
                ConstraintLifetime::Persistent,
                None,
            ),
        );
        state.register_root_surface(parent);
        parent.grab(&mut state).add_pending_child(child);
        child.grab(&mut state).assign_subsurface(subsurface, parent);
        child.grab(&mut state).set_subsurface_sync_mode(true);
        child
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock));
        child
            .grab(&mut state)
            .set_pending_pointer_constraint_region(Some(region_value(1, 1, 5, 5)));

        // A synchronized child's commit is cached, so the lock is untouched.
        commit_surface(&mut client, &mut state, &mut dispatcher, child);
        assert!(lock.try_ref(&state).unwrap().region().is_none());

        // The parent's commit applies the child's cached region.
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert!(lock.try_ref(&state).unwrap().region().is_some());
    }

    fn capture_update(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: Resolved<WlSurfaceId>,
    ) -> ContentUpdate {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let mut wl_surface = surface.grab(state);
        wl_surface.capture_commit(state, &mut client_dispatcher)
    }

    fn apply_captured(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: Resolved<WlSurfaceId>,
        update: ContentUpdate,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let mut wl_surface = surface.grab(state);
        wl_surface.apply_content(client, state, &mut client_dispatcher, update);
    }

    #[test]
    fn a_commit_without_a_pending_serial_queues_nothing() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.commit();
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_serial()
            .is_none());
        assert_eq!(xwayland.surface_events(), 0);
    }

    #[test]
    fn a_rejected_commit_keeps_the_serial_uncommitted() {
        let mut xwayland = XwaylandCommit::new();
        {
            let mut surface = xwayland.surface.grab(&mut xwayland.state);
            surface.syncobj_surface_object = Some(Resolved::new(
                xwayland.client.client_id(),
                WpLinuxDrmSyncobjSurfaceV1Id::from(9),
                ObjectId::from(9usize),
            ));
            surface.pending.sync.acquire = crate::sync::AcquireSync::SyncFile(std::rc::Rc::new(
                std::fs::File::open("/dev/null").unwrap().into(),
            ));
            surface.set_pending_xwayland_serial(7);
        }

        xwayland.commit();

        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_serial()
            .is_none());
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .pending_xwayland_serial()
            .is_none());
        assert_eq!(xwayland.surface_events(), 0);
    }

    #[test]
    fn set_serial_after_commit_is_already_associated() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.set_serial(7, 0);
        xwayland.commit();
        xwayland.set_serial(8, 0);

        let client_id = xwayland.client.client_id();
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == client_id)
            }));
        assert_eq!(xwayland.surface.rref(&xwayland.state).xwayland_serial(), Some(7));
        assert_eq!(xwayland.surface_events(), 1);
    }

    #[test]
    fn destroying_the_surface_before_its_xwayland_role_object_disconnects() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.destroy_surface();

        let client_id = xwayland.client.client_id();
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == client_id)
            }));
        assert!(xwayland
            .client
            .contains_object(WlSurfaceId::from(4), &xwayland.state));
        assert!(xwayland
            .client
            .contains_object(XwaylandSurfaceV1Id::from(5), &xwayland.state));
        assert_eq!(xwayland.surface_events(), 0);
    }

    #[test]
    fn destroying_a_committed_xwayland_surface_reports_the_serial() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.set_serial(7, 0);
        xwayland.commit();
        let surface = xwayland.surface;
        xwayland.destroy_role();
        xwayland.destroy_surface();

        assert!(!xwayland
            .client
            .contains_object(WlSurfaceId::from(4), &xwayland.state));
        assert!(xwayland
            .dispatcher
            .test_has_queued_action(|action: &XwaylandAction| {
                matches!(
                    action,
                    XwaylandAction::Surface {
                        event: SurfaceEvent::Destroyed { serial: 7, surface: destroyed },
                        ..
                    } if *destroyed == surface
                )
            }));
    }

    #[test]
    fn destroying_an_xwayland_surface_without_a_serial_reports_nothing() {
        let mut xwayland = XwaylandCommit::new();
        xwayland.destroy_role();
        xwayland.destroy_surface();

        assert!(!xwayland
            .client
            .contains_object(WlSurfaceId::from(4), &xwayland.state));
        assert!(!xwayland
            .dispatcher
            .test_has_queued_action(|action: &XwaylandAction| {
                matches!(
                    action,
                    XwaylandAction::Surface { event: SurfaceEvent::Destroyed { .. }, .. }
                )
            }));
    }

    #[test]
    fn deleting_active_cursor_surface_clears_seat_cursor_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_resolved, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(5), client.client_id(), 6));
        let object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);
        let surface_id = WlSurfaceId::from(5);

        assert!(state.seat.set_cursor_surface(
            crate::clients::Resolved::new(client.client_id(), surface_id, object_id),
            1,
            2,
        ));
        assert!(matches!(
            state.seat.visible_cursor_source(),
            Some(CursorSourceRef::Surface { cursor })
                if cursor.surface.client_id() == client.client_id()
                    && cursor.surface.object_id() == object_id
        ));

        let mut surface = state.objects.grab_object::<WlSurface>(object_id);
        surface.delete_self(&mut client, &mut state, &mut client_dispatcher);

        assert!(state.seat.visible_cursor_source().is_none());
    }

    #[test]
    fn deleting_surface_detaches_surface_extension_objects() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let surface_id = WlSurfaceId::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);

        let fractional_id = WpFractionalScaleV1Id::from(6);
        let (fractional_resolved, _) = client.insert_object(
            &mut state,
            WpFractionalScaleV1::new(
                fractional_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );
        let fractional_object_id = fractional_resolved.object_id();
        let viewport_id = WpViewportId::from(7);
        let (viewport_resolved, _) = client.insert_object(
            &mut state,
            WpViewport::new(
                viewport_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );
        let viewport_object_id = viewport_resolved.object_id();
        let syncobj_surface_id = WpLinuxDrmSyncobjSurfaceV1Id::from(8);
        let (syncobj_surface_resolved, _) = client.insert_object(
            &mut state,
            WpLinuxDrmSyncobjSurfaceV1::new(
                syncobj_surface_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );
        let syncobj_surface_object_id = syncobj_surface_resolved.object_id();

        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface
                .set_fractional_scale_object(Resolved::new(
                    client.client_id(),
                    fractional_id,
                    fractional_object_id,
                ))
                .unwrap();
            surface
                .set_viewport_object(Resolved::new(
                    client.client_id(),
                    viewport_id,
                    viewport_object_id,
                ))
                .unwrap();
            surface
                .set_syncobj_surface_object(Resolved::new(
                    client.client_id(),
                    syncobj_surface_id,
                    syncobj_surface_object_id,
                ))
                .unwrap();
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        drop(client_dispatcher);

        assert!(!client.contains_object(surface_id, &state));
        assert!(client.contains_object(fractional_id, &state));
        assert!(client.contains_object(viewport_id, &state));
        assert!(client.contains_object(syncobj_surface_id, &state));
        assert!(state
            .objects
            .try_ref_object::<WpFractionalScaleV1>(fractional_object_id)
            .is_some());
        assert!(state
            .objects
            .try_ref_object::<WpViewport>(viewport_object_id)
            .is_some());
        assert!(state
            .objects
            .try_ref_object::<WpLinuxDrmSyncobjSurfaceV1>(syncobj_surface_object_id)
            .is_some());
    }

    #[test]
    fn detached_surface_extension_objects_can_still_be_destroyed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_id = WlSurfaceId::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);

        let fractional_id = WpFractionalScaleV1Id::from(6);
        let (fractional_resolved, _) = client.insert_object(
            &mut state,
            WpFractionalScaleV1::new(
                fractional_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );
        let viewport_id = WpViewportId::from(7);
        let (viewport_resolved, _) = client.insert_object(
            &mut state,
            WpViewport::new(
                viewport_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );
        let syncobj_surface_id = WpLinuxDrmSyncobjSurfaceV1Id::from(8);
        let (syncobj_surface_resolved, _) = client.insert_object(
            &mut state,
            WpLinuxDrmSyncobjSurfaceV1::new(
                syncobj_surface_id,
                Resolved::new(client.client_id(), surface_id, surface_object_id),
            ),
        );

        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface
                .set_fractional_scale_object(fractional_resolved)
                .unwrap();
            surface.set_viewport_object(viewport_resolved).unwrap();
            surface
                .set_syncobj_surface_object(syncobj_surface_resolved)
                .unwrap();
        }

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut fractional = state
                .objects
                .grab_object::<WpFractionalScaleV1>(fractional_resolved.object_id());
            fractional
                .on(
                    &mut client,
                    &mut state,
                    raw_request_from_event(RawEvent::new(fractional_id.into(), 0, 1)),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut viewport = state
                .objects
                .grab_object::<WpViewport>(viewport_resolved.object_id());
            viewport
                .on(
                    &mut client,
                    &mut state,
                    raw_request_from_event(RawEvent::new(viewport_id.into(), 0, 1)),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut syncobj_surface = state
                .objects
                .grab_object::<WpLinuxDrmSyncobjSurfaceV1>(syncobj_surface_resolved.object_id());
            syncobj_surface
                .on(
                    &mut client,
                    &mut state,
                    raw_request_from_event(RawEvent::new(syncobj_surface_id.into(), 0, 1)),
                    &mut client_dispatcher,
                )
                .unwrap();
        }

        assert!(!client.contains_object(fractional_id, &state));
        assert!(!client.contains_object(viewport_id, &state));
        assert!(!client.contains_object(syncobj_surface_id, &state));
    }

    #[test]
    fn mapping_root_xdg_toplevel_schedules_workspace_relayout() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_id = WlSurfaceId::from(50);
        let xdg_surface_id = XdgSurfaceId::from(60);
        let toplevel_id = XdgToplevelId::from(70);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_id,
            xdg_surface_id,
            toplevel_id,
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            xdg_surface_id,
        );
        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(90), 2, 2);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        }));
    }

    #[test]
    fn tiled_root_initial_configure_uses_tile_size() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(2usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(80),
            XdgSurfaceId::from(81),
            XdgToplevelId::from(82),
        );

        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(81),
        );

        let toplevel = state
            .objects
            .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(82)))
            .unwrap();
        assert_eq!(toplevel.test_current_size(), (1280, 720));
        let states = toplevel.test_current_states();
        assert!(
            !states.contains(&(crate::protocol_generated::xdg_toplevel::State::Maximized as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledRight as u32))
        );
        assert!(states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledTop as u32)));
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledBottom as u32))
        );
        assert!(state
            .managed_root_surface_state(root(&state, surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));
    }

    #[test]
    fn tiled_root_initial_configure_uses_frame_size_with_decoration_insets() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 3;
        let backend = BackendOutputRef::new(DrmId::from(22usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(180),
            XdgSurfaceId::from(181),
            XdgToplevelId::from(182),
        );

        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(181),
        );

        {
            let toplevel = state
                .objects
                .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(182)))
                .unwrap();
            assert_eq!(toplevel.test_current_size(), (1274, 714));
        }
        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(183), 2, 2);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        state.config.decorations.rounding = 8;
        let surface = root(&state, surface_object_id);
        let geometry = state
            .committed_root_geometry(surface)
            .expect("client-side frame");
        let frame = geometry.frame_rect_global();
        let content = geometry.content_rect_global();
        assert_eq!(geometry.decoration_mode, Some(ToplevelDecorationMode::ClientSide));
        assert_eq!(frame.width, content.width + 6);
        assert_eq!(frame.height, content.height + 6);
        assert!(state.root_surface_decoration_layout(surface).is_some());
        assert!(state
            .root_surface_visual_state(surface)
            .unwrap()
            .border
            .is_some());
        assert_eq!(
            state.root_surface_window_clip(surface),
            Some(crate::render::WindowClip::Corners(crate::state::RootSurfaceClip::new(
                content,
                8,
                crate::config::RoundingPower::DEFAULT,
            )))
        );
    }

    #[test]
    fn floating_root_initial_configure_sends_tiled_states_at_zero_size() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(3usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(90),
            XdgSurfaceId::from(91),
            XdgToplevelId::from(92),
        );
        assert!(state.set_root_surface_floating(root(&state, surface_object_id), true));

        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(91),
        );

        let toplevel = state
            .objects
            .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(92)))
            .unwrap();
        assert_eq!(toplevel.test_current_size(), (0, 0));
        let states = toplevel.test_current_states();
        assert!(
            !states.contains(&(crate::protocol_generated::xdg_toplevel::State::Maximized as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledRight as u32))
        );
        assert!(states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledTop as u32)));
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledBottom as u32))
        );
        assert!(!state
            .managed_root_surface_state(root(&state, surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));
    }

    #[test]
    fn floating_root_first_map_adds_maximized_without_a_maximize_request() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(31usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(290),
            XdgSurfaceId::from(291),
            XdgToplevelId::from(292),
        );
        assert!(state.set_root_surface_floating(root(&state, surface_object_id), true));
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(291),
        );

        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(293), 80, 60);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }

        let toplevel = state
            .objects
            .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(292)))
            .unwrap();
        assert!(!toplevel.is_maximized());
        assert_eq!(toplevel.published_shell_state(), (false, false));
        let states = state
            .objects
            .try_ref_object::<XdgSurface>(client.object_id(XdgSurfaceId::from(291)))
            .unwrap()
            .test_latest_pending_toplevel_states()
            .expect("map configure");
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::Maximized as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledRight as u32))
        );
        assert!(states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledTop as u32)));
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledBottom as u32))
        );
    }

    #[test]
    fn fullscreen_root_initial_configure_uses_fullscreen_layout() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 3;
        state.config.decorations.rounding = 10;
        let backend = BackendOutputRef::new(DrmId::from(4usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(100),
            XdgSurfaceId::from(101),
            XdgToplevelId::from(102),
        );
        assert!(state.set_root_surface_fullscreen(root(&state, surface_object_id), true));

        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(101),
        );

        {
            let toplevel = state
                .objects
                .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(102)))
                .unwrap();
            assert_eq!(toplevel.test_current_size(), (1280, 720));
            let states = toplevel.test_current_states();
            assert!(states
                .contains(&(crate::protocol_generated::xdg_toplevel::State::Fullscreen as u32)));
            assert!(!states
                .contains(&(crate::protocol_generated::xdg_toplevel::State::Maximized as u32)));
            assert!(states
                .contains(&(crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32)));
        }
        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(103), 2, 2);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        let surface = root(&state, surface_object_id);
        let geometry = state
            .committed_root_geometry(surface)
            .expect("fullscreen geometry");
        assert_eq!(geometry.frame_rect_global(), geometry.content_rect_global());
        assert!(state.root_surface_decoration_layout(surface).is_none());
        assert_eq!(state.root_surface_window_clip(surface), None);
        assert!(state
            .root_surface_visual_state(surface)
            .unwrap()
            .border
            .is_none());
    }

    #[test]
    fn first_map_clears_awaiting_initial_layout_state() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(110),
            XdgSurfaceId::from(111),
            XdgToplevelId::from(112),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(111),
        );
        assert!(state
            .managed_root_surface_state(root(&state, surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));

        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(90), 2, 2);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        assert!(!state
            .managed_root_surface_state(root(&state, surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));
        let managed = state
            .managed_root_surface_state(root(&state, surface_object_id))
            .unwrap();
        assert!(managed.mapped);
        assert_eq!(
            managed.lifecycle_animation.sample(),
            WindowLifecycleAnim::Opening { progress: crate::visual_animation::UnitValue::ZERO }
        );
        assert_eq!(
            state
                .root_surface_visual_state(root(&state, surface_object_id))
                .unwrap()
                .effects
                .opacity_alpha,
            0
        );
        assert!(dispatcher.test_has_queued_action::<VisualAnimationPumpAction>(|action| {
            matches!(action, VisualAnimationPumpAction::Kick)
        }));
        assert!(state.seat.keyboard_focus.is_none());
    }

    fn mapped_xdg_toplevel(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::runner::Dispatcher,
        surface_id: u32,
    ) -> ObjectId {
        let surface_object_id = create_root_xdg_toplevel(
            state,
            client,
            dispatcher,
            WlSurfaceId::from(surface_id),
            XdgSurfaceId::from(surface_id + 1),
            XdgToplevelId::from(surface_id + 2),
        );
        commit_initial_xdg_toplevel(
            state,
            client,
            dispatcher,
            surface_object_id,
            XdgSurfaceId::from(surface_id + 1),
        );
        let buffer_object_id = insert_test_shm_buffer(
            state,
            client.client_id(),
            WlBufferId::from(surface_id + 3),
            80,
            60,
        );
        let attached_buffer = resolved_buffer(state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(client, state, &mut client_dispatcher);
        }
        surface_object_id
    }

    fn pending_toplevel_configures(
        state: &State,
        client: &Client,
        xdg_surface_id: XdgSurfaceId,
    ) -> usize {
        state
            .objects
            .try_ref_object::<XdgSurface>(client.object_id(xdg_surface_id))
            .unwrap()
            .test_pending_configure_count()
    }

    /// An update that carries an initial xdg commit for a toplevel, captured
    /// while the surface's role epoch was `epoch`.
    fn initial_xdg_update(
        client: &Client,
        surface_object_id: ObjectId,
        xdg_surface_id: XdgSurfaceId,
        state: &State,
    ) -> ContentUpdate {
        let epoch = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap()
            .test_role_epoch();
        let mut update =
            ContentUpdate::test_new(WlSurface::test_payload(None), Vec::new(), Vec::new());
        update.role_epoch = epoch;
        update.role = RoleUpdate::Xdg {
            xdg_surface: Resolved::new(
                client.client_id(),
                xdg_surface_id,
                client.object_id(xdg_surface_id),
            ),
            capture: crate::wayland_objects::xdg_surface::XdgCommitCapture {
                initial: true,
                ..Default::default()
            },
        };
        update
    }

    #[test]
    fn xdg_role_part_applies_while_the_toplevel_is_alive() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(50),
            XdgSurfaceId::from(60),
            XdgToplevelId::from(70),
        );
        let update = initial_xdg_update(&client, surface_object_id, XdgSurfaceId::from(60), &state);
        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), 0);

        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let surface = state.objects.grab_object::<WlSurface>(surface_object_id);
        WlSurface::apply_update(surface, &mut client, &mut state, &mut client_dispatcher, update);

        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), 1);
    }

    #[test]
    fn xdg_role_part_is_skipped_after_the_toplevel_is_destroyed() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(50),
            XdgSurfaceId::from(60),
            XdgToplevelId::from(70),
        );
        let update = initial_xdg_update(&client, surface_object_id, XdgSurfaceId::from(60), &state);
        {
            let mut xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(client.object_id(XdgSurfaceId::from(60)));
            let toplevel = Resolved::new(
                client.client_id(),
                XdgToplevelId::from(70),
                client.object_id(XdgToplevelId::from(70)),
            );
            xdg_surface.destroy_toplevel_role(&mut state, toplevel);
        }
        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), 0);

        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let surface = state.objects.grab_object::<WlSurface>(surface_object_id);
        WlSurface::apply_update(surface, &mut client, &mut state, &mut client_dispatcher, update);

        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), 0);
    }

    #[test]
    fn changed_commit_refreshes_only_that_toplevel_tree() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(48usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 500);
        let root_surface = root(&state, surface_object_id);
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::RefreshSurfaceTreeOutputs { root } if *root == root_surface
            )
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(action, ClientAction::RefreshSurfaceOutputs)
        ));
    }

    #[test]
    fn reconfigure_mapped_toplevels_skips_unchanged_configure() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(41usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 410);
        let xdg_surface_id = XdgSurfaceId::from(411);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let before = pending_toplevel_configures(&state, &client, xdg_surface_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        assert_eq!(pending_toplevel_configures(&state, &client, xdg_surface_id), before);
    }

    #[test]
    fn reconfigure_mapped_toplevels_sends_when_state_changes() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(42usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 420);
        let xdg_surface_id = XdgSurfaceId::from(421);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let before = pending_toplevel_configures(&state, &client, xdg_surface_id);
        assert!(state.set_root_surface_fullscreen(root(&state, surface_object_id), true));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        assert_eq!(pending_toplevel_configures(&state, &client, xdg_surface_id), before + 1);
    }

    /// `xdg_toplevel.state` value of `activated`.
    const XDG_TOPLEVEL_STATE_ACTIVATED: u32 = 4;

    fn latest_toplevel_configure_states(
        state: &State,
        client: &Client,
        xdg_surface_id: XdgSurfaceId,
    ) -> Vec<u32> {
        state
            .objects
            .try_ref_object::<XdgSurface>(client.object_id(xdg_surface_id))
            .unwrap()
            .test_latest_toplevel_configure_states()
            .expect("toplevel configure")
    }

    #[test]
    fn reconfigure_sends_activated_for_the_focused_toplevel() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(44usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 450);
        let xdg_surface_id = XdgSurfaceId::from(451);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let before = pending_toplevel_configures(&state, &client, xdg_surface_id);
        let root_surface = root(&state, surface_object_id);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(root_surface)));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        assert_eq!(pending_toplevel_configures(&state, &client, xdg_surface_id), before + 1);
        assert!(latest_toplevel_configure_states(&state, &client, xdg_surface_id)
            .contains(&XDG_TOPLEVEL_STATE_ACTIVATED));

        let after_focus = pending_toplevel_configures(&state, &client, xdg_surface_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        assert_eq!(pending_toplevel_configures(&state, &client, xdg_surface_id), after_focus);
    }

    #[test]
    fn reconfigure_toplevels_for_roots_configures_only_the_given_root() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(43usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let first_surface = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 430);
        let _second_surface = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 440);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let first_before = pending_toplevel_configures(&state, &client, XdgSurfaceId::from(431));
        let second_before = pending_toplevel_configures(&state, &client, XdgSurfaceId::from(441));
        let first_root = root(&state, first_surface);
        let first_window = state.window_for_root(first_root).expect("first window");
        assert!(state.resize_tiled_window(first_window, crate::tiling::EDGE_RIGHT, 40, 0));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_toplevels_for_roots(
                &mut state,
                &mut client_dispatcher,
                vec![first_root],
            );
        }
        assert_eq!(
            pending_toplevel_configures(&state, &client, XdgSurfaceId::from(431)),
            first_before + 1
        );
        assert_eq!(
            pending_toplevel_configures(&state, &client, XdgSurfaceId::from(441)),
            second_before
        );
    }

    fn arm_latest_pending_resize(
        state: &mut State,
        client: &Client,
        xdg_surface_id: XdgSurfaceId,
    ) -> u32 {
        let xdg_surface = state
            .objects
            .try_ref_object::<XdgSurface>(client.object_id(xdg_surface_id))
            .unwrap();
        let serial = xdg_surface
            .test_latest_pending_serial()
            .expect("pending configure");
        state
            .objects
            .get_mut(client.object_id(xdg_surface_id))
            .downcast_mut::<XdgSurface>()
            .unwrap()
            .test_arm_deferred_resize(serial);
        serial
    }

    fn flush_deferred_queued(
        dispatcher: &Dispatcher,
        client_id: ClientId,
        toplevel_id: XdgToplevelId,
    ) -> bool {
        dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(client_id, |action| {
            matches!(
                action,
                ClientAction::FlushDeferredResizeConfigure { xdg_toplevel }
                    if xdg_toplevel.wayland_id() == toplevel_id
            )
        })
    }

    #[test]
    fn commit_after_acked_resize_configure_flushes_the_deferred_one() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(45usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 470);
        let _sibling = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 474);
        let xdg_surface_id = XdgSurfaceId::from(471);
        let toplevel_id = XdgToplevelId::from(472);
        let root_surface = root(&state, surface_object_id);
        let window = state.window_for_root(root_surface).expect("window");
        assert!(state.resize_tiled_window(window, crate::tiling::EDGE_RIGHT, 40, 0));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_toplevels_for_roots(
                &mut state,
                &mut client_dispatcher,
                vec![root_surface],
            );
        }
        let serial = arm_latest_pending_resize(&mut state, &client, xdg_surface_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut xdg_surface = client.grab_object(xdg_surface_id, &mut state);
            xdg_surface
                .on(
                    &mut client,
                    &mut state,
                    ack_configure_request(xdg_surface_id, serial),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(flush_deferred_queued(&dispatcher, client.client_id(), toplevel_id));
        let queued = dispatcher.test_count_queued_action::<ClientAction>(|action| {
            matches!(action, ClientAction::FlushDeferredResizeConfigure { .. })
        });
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(action, ClientAction::FlushDeferredResizeConfigure { .. })
            }),
            queued
        );
    }

    #[test]
    fn commit_without_acking_the_resize_configure_queues_nothing() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(46usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 480);
        let _sibling = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 484);
        let xdg_surface_id = XdgSurfaceId::from(481);
        let toplevel_id = XdgToplevelId::from(482);
        let root_surface = root(&state, surface_object_id);
        let window = state.window_for_root(root_surface).expect("window");
        assert!(state.resize_tiled_window(window, crate::tiling::EDGE_RIGHT, 40, 0));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_toplevels_for_roots(
                &mut state,
                &mut client_dispatcher,
                vec![root_surface],
            );
        }
        let _serial = arm_latest_pending_resize(&mut state, &client, xdg_surface_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(!flush_deferred_queued(&dispatcher, client.client_id(), toplevel_id));
    }

    #[test]
    fn flush_deferred_resize_of_an_unchanged_tiled_window_sends_no_configure() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(47usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 490);
        let xdg_surface_id = XdgSurfaceId::from(491);
        let toplevel_id = XdgToplevelId::from(492);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let root_surface = root(&state, surface_object_id);
        let window = state.window_for_root(root_surface).expect("window");
        assert!(state.tiled_window_geometry(window).is_some());
        let before = pending_toplevel_configures(&state, &client, xdg_surface_id);
        let xdg_toplevel = client.resolve(toplevel_id, &state).expect("toplevel");
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_flush_deferred_resize_configure(
                &mut state,
                &mut client_dispatcher,
                xdg_toplevel,
            );
        }
        assert_eq!(pending_toplevel_configures(&state, &client, xdg_surface_id), before);
    }

    #[test]
    fn tiled_resize_step_dispatches_narrow_action() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(44usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let first_surface = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 450);
        let second_surface = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 460);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        let first_root = root(&state, first_surface);
        let second_root = root(&state, second_surface);
        let first_window = state.window_for_root(first_root).expect("first window");
        state.seat.pointer_pos = (100.0, 50.0);
        let (workspace, drags) = state
            .begin_window_edge_drags(first_window, crate::tiling::EDGE_RIGHT)
            .expect("left leaf should support right-edge resize");
        state.seat.start_toplevel_tiled_resize(
            workspace,
            drags,
            crate::tiling::EDGE_RIGHT,
            Some(first_window),
        );
        let _ = dispatcher.test_take_queued_actions::<ClientManagerAction>();

        state.seat.pointer_pos = (110.0, 50.0);
        assert!(crate::input::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
        let actions = dispatcher.test_take_queued_actions::<ClientManagerAction>();
        let mut stepped = actions.iter().filter_map(|action| match action {
            ClientManagerAction::TiledResizeStep { roots } => Some(roots.clone()),
            _ => None,
        });
        let roots = stepped.next().expect("tiled resize step");
        assert!(stepped.next().is_none());
        assert_eq!(
            roots
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [first_root, second_root].into_iter().collect()
        );
        assert!(!actions
            .iter()
            .any(|action| matches!(action, ClientManagerAction::WorkspaceStateChanged)));

        assert!(crate::input::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
        assert!(dispatcher
            .test_take_queued_actions::<ClientManagerAction>()
            .is_empty());
    }

    fn settle_floating_toplevel(state: &mut State, surface: Resolved<WlSurfaceId>) -> RectI {
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::Windows);
        state.config.animations.set_open_duration_msec(0);
        assert!(state.set_root_surface_floating(surface, true));
        let window = state.window_for_root(surface).expect("window");
        state
            .sync_window_geometry_goal(window, crate::visual_animation::WindowGeometryMotion::Warp);
        state.advance_visual_animations(std::time::Instant::now());
        let geometry = state.managed_window(surface).unwrap().geometry_animation;
        assert!(!geometry.is_active());
        geometry.visual_rect().unwrap()
    }

    #[test]
    fn floating_commit_snaps_a_settled_window_to_its_new_size() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(49usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 510);
        let root_surface = root(&state, surface_object_id);
        let before = settle_floating_toplevel(&mut state, root_surface);
        let _ = dispatcher.test_take_queued_actions::<VisualAnimationPumpAction>();

        super::test_commit_new_buffer(
            &mut client,
            &mut state,
            &mut dispatcher,
            root_surface,
            120,
            90,
        );

        assert!(dispatcher.test_has_queued_action::<VisualAnimationPumpAction>(|action| {
            matches!(action, VisualAnimationPumpAction::Kick)
        }));
        state.advance_visual_animations(std::time::Instant::now());
        let logical = state
            .committed_root_geometry(root_surface)
            .unwrap()
            .layout_rect_global;
        assert_ne!(logical, before);
        let geometry = state
            .managed_window(root_surface)
            .unwrap()
            .geometry_animation;
        assert_eq!(geometry.visual_rect(), Some(logical));
        assert!(!geometry.is_active());
    }

    #[test]
    fn floating_commit_keeps_a_held_layout_goal() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(50usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = mapped_xdg_toplevel(&mut state, &mut client, &mut dispatcher, 520);
        let root_surface = root(&state, surface_object_id);
        let before = settle_floating_toplevel(&mut state, root_surface);
        state
            .managed_window_mut(root_surface)
            .unwrap()
            .hold_floating_layout = true;
        let _ = dispatcher.test_take_queued_actions::<VisualAnimationPumpAction>();

        super::test_commit_new_buffer(
            &mut client,
            &mut state,
            &mut dispatcher,
            root_surface,
            120,
            90,
        );

        assert!(!dispatcher.test_has_queued_action::<VisualAnimationPumpAction>(|action| {
            matches!(action, VisualAnimationPumpAction::Kick)
        }));
        state.advance_visual_animations(std::time::Instant::now());
        let logical = state
            .committed_root_geometry(root_surface)
            .unwrap()
            .layout_rect_global;
        assert_ne!(logical, before);
        let geometry = state
            .managed_window(root_surface)
            .unwrap()
            .geometry_animation;
        assert_eq!(geometry.visual_rect(), Some(before));
        assert_eq!(geometry.goal_rect(), Some(before));
        assert!(!geometry.is_active());
    }

    fn map_closing_candidate(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::runner::Dispatcher,
        surface_id: u32,
    ) -> ObjectId {
        state.realize_closing_snapshots_in_tests = true;
        let surface_object_id = create_root_xdg_toplevel(
            state,
            client,
            dispatcher,
            WlSurfaceId::from(surface_id),
            XdgSurfaceId::from(surface_id + 1),
            XdgToplevelId::from(surface_id + 2),
        );
        commit_initial_xdg_toplevel(
            state,
            client,
            dispatcher,
            surface_object_id,
            XdgSurfaceId::from(surface_id + 1),
        );
        let buffer_object_id = insert_test_shm_buffer(
            state,
            client.client_id(),
            WlBufferId::from(surface_id + 3),
            8,
            8,
        );
        let attached_buffer = resolved_buffer(state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(client, state, &mut client_dispatcher);
        }
        surface_object_id
    }

    #[test]
    fn null_buffer_unmap_retains_one_snapshot_across_later_destruction() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 210);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(None);
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        assert_eq!(state.closing_snapshots[0].source(), root(&state, surface_object_id));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        assert!(state
            .managed_window(state.closing_snapshots[0].source())
            .is_none());
    }

    #[test]
    fn closing_animation_plays_after_the_toplevel_is_destroyed() {
        let mut state = state_for_tests();
        state.config.animations.set_close_duration_msec(100);
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 240);
        let window = state
            .window_for_root(root(&state, surface_object_id))
            .unwrap();
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(None);
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        assert_eq!(state.closing_snapshots[0].window(), window);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        assert!(state.managed_window_state(window).is_none());

        let now = std::time::Instant::now();
        state.advance_visual_animations(now);
        assert_eq!(state.closing_snapshots[0].alpha(), 255);
        state.advance_visual_animations(now + std::time::Duration::from_millis(50));
        let alpha = state.closing_snapshots[0].alpha();
        assert!(alpha > 0 && alpha < 255);
        assert_eq!(state.closing_snapshots[0].window(), window);
    }

    #[test]
    fn direct_surface_destruction_captures_a_mapped_toplevel() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 220);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
    }

    #[test]
    fn client_disconnect_captures_a_mapped_toplevel() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 230);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_client_disconnect(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
    }

    #[test]
    fn client_disconnect_captures_the_keyboard_focused_toplevel() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 231);
        let focused = root(&state, surface_object_id);
        state.seat.keyboard_focus =
            Some(focus_for_surface(focused.client_id(), focused.wayland_id(), surface_object_id));
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_client_disconnect(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        assert_eq!(state.closing_snapshots[0].source(), focused);
    }

    fn sent_events(dispatcher: &Dispatcher) -> Vec<(u32, u32, u32, u32)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            let arg = if buffer.len() >= 12 {
                u32::from_ne_bytes(buffer[8..12].try_into().unwrap())
            } else {
                0
            };
            let second = if buffer.len() >= 16 {
                u32::from_ne_bytes(buffer[12..16].try_into().unwrap())
            } else {
                0
            };
            sent.push((object, opcode, arg, second));
        });
        sent
    }

    fn surface_origin(state: &State, surface_object_id: ObjectId) -> (i32, i32) {
        let bounds = state
            .objects
            .get(surface_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap()
            .global_logical_bounds()
            .unwrap();
        (bounds.x, bounds.y)
    }

    #[test]
    fn destroying_the_focused_toplevel_focuses_another_window_on_its_workspace() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let closing_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 250);
        let remaining_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 260);
        let closing = root(&state, closing_id);
        let remaining = root(&state, remaining_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(remaining)),
            );
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(closing)),
            );
        }
        let (x, y) = surface_origin(&state, closing_id);
        state.seat.pointer_pos = (x as f64 + 1.0, y as f64 + 1.0);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            {
                let mut surface = state.objects.grab_object::<WlSurface>(closing_id);
                surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
            }
            client_dispatcher.drain_post_actions(&mut client, &mut state);
        }
        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(remaining));
    }

    #[test]
    fn destroying_the_focused_surface_leaves_before_delete_id() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let closing_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 250);
        let remaining_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 260);
        let closing = root(&state, closing_id);
        let remaining = root(&state, remaining_id);
        let (keyboard, _) =
            client.insert_object(&mut state, WlKeyboard::new(WlKeyboardId::from(80)));
        state.seat.register_keyboard(keyboard);
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(81), 5));
        state.seat.register_pointer(pointer);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(remaining)),
            );
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(closing)),
            );
        }
        let (x, y) = surface_origin(&state, closing_id);
        state.seat.pointer_pos = (x as f64 + 1.0, y as f64 + 1.0);
        state.seat.pointer_focus = Some(SurfaceFocusPick {
            target: SurfaceFocus::new(closing),
            rect: SurfaceRect { x, y, width: 8, height: 8 },
        });
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            {
                let mut surface = state.objects.grab_object::<WlSurface>(closing_id);
                surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
            }
            client_dispatcher.drain_post_actions(&mut client, &mut state);
        }

        let sent = sent_events(&dispatcher);
        let surface_id = u32::from(closing.wayland_id());
        let delete_at = sent
            .iter()
            .position(|(object, opcode, arg, _)| *object == 1 && *opcode == 1 && *arg == surface_id)
            .expect("delete_id");
        let keyboard_leave_at = sent
            .iter()
            .position(|(object, opcode, _, surface)| {
                *object == 80 && *opcode == 2 && *surface == surface_id
            })
            .expect("wl_keyboard.leave");
        let pointer_leave_at = sent
            .iter()
            .position(|(object, opcode, _, surface)| {
                *object == 81 && *opcode == 1 && *surface == surface_id
            })
            .expect("wl_pointer.leave");
        let pointer_frame_at = sent
            .iter()
            .position(|(object, opcode, _, _)| *object == 81 && *opcode == 5)
            .expect("wl_pointer.frame");
        assert!(keyboard_leave_at < delete_at);
        assert!(pointer_leave_at < pointer_frame_at);
        assert!(pointer_frame_at < delete_at);
        assert_eq!(
            sent.iter()
                .filter(|(object, opcode, _, surface)| {
                    *object == 80 && *opcode == 2 && *surface == surface_id
                })
                .count(),
            1
        );
        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(remaining));
        assert_ne!(state.seat.pointer_focus.map(|focus| focus.target.surface), Some(closing));
    }

    #[test]
    fn destroying_the_last_focused_window_still_drops_keyboard_focus() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let closing_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 270);
        let closing = root(&state, closing_id);
        let (keyboard, _) =
            client.insert_object(&mut state, WlKeyboard::new(WlKeyboardId::from(82)));
        state.seat.register_keyboard(keyboard);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(closing)),
            );
        }
        let before =
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::KeyboardFocus)
            });
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            {
                let mut surface = state.objects.grab_object::<WlSurface>(closing_id);
                surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
            }
            client_dispatcher.drain_post_actions(&mut client, &mut state);
        }
        let after =
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::KeyboardFocus)
            });
        assert!(after > before);
        assert!(state.seat.keyboard_focus().is_none());
    }

    #[test]
    fn destroying_an_unfocused_toplevel_keeps_keyboard_focus() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let closing_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 270);
        let focused_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 280);
        let focused = root(&state, focused_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(focused)),
            );
        }
        let (x, y) = surface_origin(&state, closing_id);
        state.seat.pointer_pos = (x as f64 + 1.0, y as f64 + 1.0);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            {
                let mut surface = state.objects.grab_object::<WlSurface>(closing_id);
                surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
            }
            client_dispatcher.drain_post_actions(&mut client, &mut state);
        }
        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(focused));
    }

    #[test]
    fn null_buffer_unmap_of_the_focused_toplevel_focuses_another_window() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let closing_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 290);
        let remaining_id = map_closing_candidate(&mut state, &mut client, &mut dispatcher, 300);
        let closing = root(&state, closing_id);
        let remaining = root(&state, remaining_id);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(remaining)),
            );
            set_keyboard_focus(
                &mut state,
                &mut client_dispatcher,
                Some(SurfaceFocus::new(closing)),
            );
        }
        {
            let surface = state
                .objects
                .get_mut(closing_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(None);
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            {
                let mut surface = state.objects.grab_object::<WlSurface>(closing_id);
                surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
            }
            client_dispatcher.drain_post_actions(&mut client, &mut state);
        }
        assert_eq!(state.seat.keyboard_focus().map(|focus| focus.surface), Some(remaining));
    }

    #[test]
    fn mapped_role_destruction_captures_once() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 240);
        let toplevel_id = XdgToplevelId::from(242);
        let destroy_request: RawRequest<'static> = (&*RawEvent::new(toplevel_id.into(), 0, 0)
            .into_bytes())
            .try_into()
            .unwrap();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut toplevel = client.grab_object(toplevel_id, &mut state);
            toplevel.on(&mut client, &mut state, destroy_request, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert_eq!(state.closing_snapshots.len(), 1);
    }

    #[test]
    fn close_capture_skips_without_duration_session_or_toplevel_role() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(5usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        state.config.animations.set_close_duration_msec(0);
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 250);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(None);
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(state.closing_snapshots.is_empty());

        state.config.animations.set_close_duration_msec(160);
        state.session_active = false;
        state.realize_closing_snapshots_in_tests = true;
        let surface_object_id =
            map_closing_candidate(&mut state, &mut client, &mut dispatcher, 260);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(state.closing_snapshots.is_empty());
    }

    #[test]
    fn floating_map_starts_opening_when_reconfigure_consumes_the_edge() {
        let mut state = state_for_tests();
        state.config.animations.set_open_duration_msec(160);
        let backend = BackendOutputRef::new(DrmId::from(8usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(130),
            XdgSurfaceId::from(131),
            XdgToplevelId::from(132),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(131),
        );
        state
            .managed_window_mut(root(&state, surface_object_id))
            .unwrap()
            .mode = crate::state::ManagedWindowMode::Floating;

        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(133), 80, 60);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        let surface = root(&state, surface_object_id);
        assert!(!state.managed_root_surface_state(surface).unwrap().mapped);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            WindowLifecycleAnim::Idle
        );
        assert!(!dispatcher.test_has_queued_action::<VisualAnimationPumpAction>(|action| {
            matches!(action, VisualAnimationPumpAction::Kick)
        }));

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            client.test_reconfigure_mapped_toplevels(&mut state, &mut client_dispatcher);
        }
        assert!(state.managed_root_surface_state(surface).unwrap().mapped);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            WindowLifecycleAnim::Opening { progress: crate::visual_animation::UnitValue::ZERO }
        );
        assert_eq!(
            state
                .root_surface_visual_state(surface)
                .unwrap()
                .effects
                .opacity_alpha,
            0
        );
        assert!(dispatcher.test_has_queued_action::<VisualAnimationPumpAction>(|action| {
            matches!(action, VisualAnimationPumpAction::Kick)
        }));
        assert!(state.seat.keyboard_focus.is_none());
    }

    #[test]
    fn first_map_tiled_server_side_updates_root_position_before_returning() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 5;
        let backend = BackendOutputRef::new(DrmId::from(24usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(200),
            XdgSurfaceId::from(201),
            XdgToplevelId::from(202),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(201),
        );

        {
            let toplevel = state
                .objects
                .get_mut(client.object_id(XdgToplevelId::from(202)))
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }

        let buffer_object_id = insert_test_shm_buffer(
            &mut state,
            client.client_id(),
            WlBufferId::from(203),
            1270,
            710,
        );
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        let surface = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap();
        assert_eq!(surface.root_position(), (5, 5));
        assert_eq!(
            surface.global_logical_bounds(),
            Some(RectI { x: 5, y: 5, width: 1270, height: 710 })
        );
        assert!(!state
            .managed_root_surface_state(root(&state, surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));
        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        }));
    }

    #[test]
    fn buffer_resize_without_new_window_geometry_keeps_the_frame_insets() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 3;
        let backend = BackendOutputRef::new(DrmId::from(7usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(140),
            XdgSurfaceId::from(141),
            XdgToplevelId::from(142),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(141),
        );

        let (child_surface_resolved, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(145), client.client_id(), 6),
        );
        let child_surface_object_id = child_surface_resolved.object_id();
        let surface_resolved = root(&state, surface_object_id);
        let child_surface_resolved = root(&state, child_surface_object_id);
        {
            let root_surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.add_pending_child(child_surface_resolved);
        }
        let child_subsurface = insert_client_subsurface_object(
            &mut client,
            &mut state,
            WlSubsurfaceId::from(146),
            child_surface_resolved,
            surface_resolved,
        );
        {
            let child_surface = state
                .objects
                .get_mut(child_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, surface_resolved);
            child_surface.set_subsurface_position(2, 0);
            child_surface.test_map(ObjectId::from(903usize), 0, 0, 2, 2);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }

        let initial_buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(143), 2, 2);
        let attached_buffer = resolved_buffer(&state, initial_buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let toplevel = state
                .objects
                .get_mut(client.object_id(XdgToplevelId::from(142)))
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ClientSide);
        }
        {
            let xdg_surface = state
                .objects
                .get_mut(client.object_id(XdgSurfaceId::from(141)))
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_pending_window_geometry(Some(RectI {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            }));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        }));
        state.relayout_dirty_workspaces();

        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .content_rect_global()
                .width,
            1274
        );
        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .layout_rect_global
                .width,
            1280
        );
        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .decoration_insets
                .right,
            3
        );

        let resized_buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(144), 4, 2);
        let attached_buffer = resolved_buffer(&state, resized_buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        }));
        state.relayout_dirty_workspaces();

        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .content_rect_global()
                .width,
            1274
        );
        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .layout_rect_global
                .width,
            1280
        );
        assert_eq!(
            state
                .committed_root_geometry(root(&state, surface_object_id))
                .unwrap()
                .decoration_insets
                .right,
            3
        );
    }

    #[test]
    fn stale_pending_root_does_not_shrink_next_initial_configure() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(6usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let first_surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(120),
            XdgSurfaceId::from(121),
            XdgToplevelId::from(122),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            first_surface_object_id,
            XdgSurfaceId::from(121),
        );
        assert!(state
            .managed_root_surface_state(root(&state, first_surface_object_id))
            .is_some_and(|managed| managed.awaiting_initial_layout));

        let second_surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(130),
            XdgSurfaceId::from(131),
            XdgToplevelId::from(132),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            second_surface_object_id,
            XdgSurfaceId::from(131),
        );

        let second_toplevel = state
            .objects
            .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(132)))
            .unwrap();
        assert_eq!(second_toplevel.test_current_size(), (1280, 720));
        let states = second_toplevel.test_current_states();
        assert!(
            !states.contains(&(crate::protocol_generated::xdg_toplevel::State::Maximized as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32))
        );
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledRight as u32))
        );
        assert!(states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledTop as u32)));
        assert!(
            states.contains(&(crate::protocol_generated::xdg_toplevel::State::TiledBottom as u32))
        );
    }

    #[test]
    fn rejected_commit_does_not_apply_pending_window_geometry() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(23usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);

        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(190),
            XdgSurfaceId::from(191),
            XdgToplevelId::from(192),
        );
        commit_initial_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            surface_object_id,
            XdgSurfaceId::from(191),
        );

        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(90), 2, 2);
        let attached_buffer = resolved_buffer(&state, buffer_object_id);
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.pending.buffer = Some(Some(attached_buffer));
            surface.pending.viewport.src =
                Some(crate::transform::RectF { x: 0.0, y: 0.0, width: 10.0, height: 10.0 });
        }
        {
            let xdg_surface = state
                .objects
                .get_mut(client.object_id(XdgSurfaceId::from(191)))
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_pending_window_geometry(Some(RectI {
                x: -4,
                y: -12,
                width: 90,
                height: 80,
            }));
        }

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        let toplevel = state
            .objects
            .try_ref_object::<XdgToplevel>(client.object_id(XdgToplevelId::from(192)))
            .unwrap();
        assert_eq!(toplevel.window_geometry(), None);
    }

    #[test]
    fn root_commit_refreshes_cached_geometry_for_non_xdg_roots() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let surface_id = WlSurfaceId::from(210);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        state.register_root_surface(surface_resolved);
        let initial_buffer =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(410), 200, 40);

        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(initial_buffer, 0, 0, 200, 40);
        }

        state.mark_root_geometry_dirty(root(&state, surface_object_id));
        let _ = state.committed_root_geometry(root(&state, surface_object_id));
        assert_eq!(
            state.root_geometry_cache[&root(&state, surface_object_id)]
                .get()
                .geometry
                .unwrap()
                .layout_rect_global,
            RectI { x: 0, y: 0, width: 200, height: 40 }
        );

        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.set_root_position(50, 60);
        }

        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        let _ = state.committed_root_geometry(root(&state, surface_object_id));
        assert_eq!(
            state.root_geometry_cache[&root(&state, surface_object_id)]
                .get()
                .geometry
                .unwrap()
                .layout_rect_global,
            RectI { x: 50, y: 60, width: 200, height: 40 }
        );
    }

    #[test]
    fn surface_defaults_match_region_protocol_defaults() {
        let surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6);
        assert_eq!(surface.on_screen.input_region, RegionValue::infinite());
        assert_eq!(surface.on_screen.opaque_region, RegionValue::empty());
    }

    #[test]
    fn hit_testing_uses_committed_input_region_not_pending_region() {
        let mut surface = make_surface();
        surface.pending.input_region = Some(region_value(0, 0, 10, 10));

        assert!(surface.accepts_input_at(50.0, 50.0));

        surface.on_screen.input_region = surface.pending.input_region.take().unwrap();

        assert!(surface.accepts_input_at(15.0, 25.0));
        assert!(!surface.accepts_input_at(50.0, 50.0));
    }

    #[test]
    fn committed_input_region_uses_surface_local_coordinates() {
        let mut surface = make_surface();
        surface.on_screen.input_region = region_value(5, 5, 10, 10);

        assert!(surface.accepts_input_at(15.0, 25.0));
        assert!(!surface.accepts_input_at(30.0, 25.0));
    }

    #[test]
    fn configure_bounds_falls_back_to_current_output_without_surface_geometry() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            200,
            120,
        ));
        let surface_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(4), 6),
        );
        let surface = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap();

        assert_eq!(
            surface.preferred_output_id_for_object(
                &state,
                Resolved::new(ClientId::from_raw(4), WlSurfaceId::from(3), surface_object_id),
            ),
            Some(output_id)
        );
        assert_eq!(
            state.configure_bounds_for_surface(
                Resolved::new(ClientId::from_raw(4), WlSurfaceId::from(3), surface_object_id),
                None,
            ),
            Some((200, 120))
        );
    }

    #[test]
    fn configure_bounds_for_object_works_while_surface_is_grabbed() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            200,
            120,
        ));
        let (_, surface_object_id) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(11u32), ClientId::from_raw(4), 6));

        let surface = state.objects.grab_object::<WlSurface>(surface_object_id);
        let resolved =
            Resolved::new(ClientId::from_raw(4), WlSurfaceId::from(11u32), surface_object_id);
        assert_eq!(surface.preferred_output_id_for_object(&state, resolved), Some(output_id));
        assert_eq!(
            state.configure_bounds_for_surface_with_ref(resolved, &surface, None),
            Some((200, 120))
        );
    }

    #[test]
    fn preferred_buffer_state_sends_v6_hints_once_per_change() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            200,
            120,
        ));
        {
            let output = state.outputs.try_get_mut(output_id).unwrap();
            output.scale = FractionalScale::from_integer(2);
            output.transform = 3;
        }
        let surface_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(9), ClientId::from_raw(6), 6),
        );
        let mut surface = WlSurface::new(WlSurfaceId::from(9), ClientId::from_raw(6), 6);
        let (client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(9), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_buffer_state_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);

        assert_eq!(surface.preferred_buffer_scale_sent, Some(2));
        assert_eq!(surface.preferred_buffer_transform_sent, Some(3));
        assert!(dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));

        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(9), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_buffer_state_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);

        assert!(!dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));
    }

    #[test]
    fn preferred_fractional_scale_sends_once_per_scale_value() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            200,
            120,
        ));
        state.outputs.try_get_mut(output_id).unwrap().scale = FractionalScale::ONE_POINT_FIVE;

        let surface_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(10), ClientId::from_raw(6), 6),
        );
        let fractional_object_id = attach_fractional_scale_object(&mut state, surface_object_id);

        let mut surface = WlSurface::new(WlSurfaceId::from(10), ClientId::from_raw(6), 6);
        surface.update_output_membership(vec![output_id]);
        surface.fractional_scale_object = Some(Resolved::new(
            ClientId::from_raw(1),
            WpFractionalScaleV1Id::from(77),
            fractional_object_id,
        ));

        let (client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(10), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_fractional_scale_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);

        assert_eq!(
            surface.preferred_fractional_scale_sent,
            Some(FractionalScale::ONE_POINT_FIVE.as_v120())
        );
        assert!(dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));

        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(10), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_fractional_scale_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);

        assert!(!dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));
    }

    #[test]
    fn preferred_fractional_scale_updates_when_surface_moves_outputs() {
        let mut state = state_for_tests();
        let left_output = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            200,
            120,
        ));
        let right_output = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            2,
            200,
            0,
            200,
            120,
        ));
        state.outputs.try_get_mut(left_output).unwrap().scale = FractionalScale::ONE;
        state.outputs.try_get_mut(right_output).unwrap().scale = FractionalScale::TWO;

        let surface_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(11), ClientId::from_raw(7), 6),
        );
        let fractional_object_id = attach_fractional_scale_object(&mut state, surface_object_id);

        let mut surface = WlSurface::new(WlSurfaceId::from(11), ClientId::from_raw(7), 6);
        surface.fractional_scale_object = Some(Resolved::new(
            ClientId::from_raw(1),
            WpFractionalScaleV1Id::from(77),
            fractional_object_id,
        ));
        surface.update_output_membership(vec![left_output]);

        let (client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(10), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_fractional_scale_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);
        assert_eq!(surface.preferred_fractional_scale_sent, Some(FractionalScale::ONE.as_v120()));

        let mut dispatcher = Dispatcher::default();
        surface.update_output_membership(vec![right_output]);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let id = Resolved::new(ClientId::from_raw(6), WlSurfaceId::from(10), surface_object_id);
        let preferred_output_id = surface.preferred_output_id_for_object(&state, id);
        surface.send_preferred_fractional_scale_for_object(
            &state,
            preferred_output_id,
            &mut client_dispatcher,
        );
        drop(client_dispatcher);

        assert_eq!(surface.preferred_fractional_scale_sent, Some(FractionalScale::TWO.as_v120()));
        assert!(dispatcher.test_has_queued_action::<IoUringAction>(|action| matches!(
            action,
            IoUringAction::Sendmsg { .. }
        )));
    }

    const SURFACE_ID: u32 = 9;
    const FRACTIONAL_ID: u32 = 77;

    /// A 2x output: mode 2560x1440 gives a logical size of 1280x720.
    fn two_x_state() -> (State, OutputId) {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            2560,
            1440,
        ));
        state.outputs.try_get_mut(output_id).unwrap().scale = FractionalScale::TWO;
        (state, output_id)
    }

    /// A surface whose fractional-scale object lives in `state`, matching the
    /// object the `get_fractional_scale` manager send uses.
    fn fractional_surface() -> (State, OutputId, WlSurface) {
        let (mut state, output_id) = two_x_state();
        let surface_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(SURFACE_ID), ClientId::from_raw(9), 6),
        );
        let fractional_object_id = attach_fractional_scale_object(&mut state, surface_object_id);
        let mut surface = WlSurface::new(WlSurfaceId::from(SURFACE_ID), ClientId::from_raw(9), 6);
        surface.fractional_scale_object = Some(Resolved::new(
            ClientId::from_raw(1),
            WpFractionalScaleV1Id::from(FRACTIONAL_ID),
            fractional_object_id,
        ));
        (state, output_id, surface)
    }

    #[test]
    fn wire_scaled_client_gets_preferred_buffer_scale_one() {
        let (state, output_id) = two_x_state();
        let mut surface = WlSurface::new(WlSurfaceId::from(SURFACE_ID), ClientId::from_raw(9), 6);
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        client.set_wire_scale_for_tests(WireScale::new(2));
        {
            let mut cd = client.test_dispatcher(&mut dispatcher);
            surface.send_preferred_buffer_state_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_arg(&dispatcher, SURFACE_ID, 2, 1));
        assert_eq!(surface.preferred_buffer_scale_sent, Some(1));

        // The cache holds the value that was sent, so a second call is quiet.
        let mut again = Dispatcher::default();
        {
            let mut cd = client.test_dispatcher(&mut again);
            surface.send_preferred_buffer_state_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_opcodes(&again, SURFACE_ID).is_empty());
    }

    #[test]
    fn normal_client_gets_preferred_buffer_scale_from_output() {
        let (state, output_id) = two_x_state();
        let mut surface = WlSurface::new(WlSurfaceId::from(SURFACE_ID), ClientId::from_raw(9), 6);
        let (client, mut dispatcher) = test_client_and_dispatcher(9);
        {
            let mut cd = client.test_dispatcher(&mut dispatcher);
            surface.send_preferred_buffer_state_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_arg(&dispatcher, SURFACE_ID, 2, 2));
        assert_eq!(surface.preferred_buffer_scale_sent, Some(2));
    }

    #[test]
    fn wire_scaled_client_gets_preferred_fractional_scale_120() {
        let (state, output_id, mut surface) = fractional_surface();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        client.set_wire_scale_for_tests(WireScale::new(2));
        {
            let mut cd = client.test_dispatcher(&mut dispatcher);
            surface.send_preferred_fractional_scale_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_arg(&dispatcher, FRACTIONAL_ID, 0, 120));
        assert_eq!(surface.preferred_fractional_scale_sent, Some(120));

        // The cache holds the value that was sent, so a second call is quiet.
        let mut again = Dispatcher::default();
        {
            let mut cd = client.test_dispatcher(&mut again);
            surface.send_preferred_fractional_scale_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_opcodes(&again, FRACTIONAL_ID).is_empty());
    }

    #[test]
    fn normal_client_gets_preferred_fractional_scale_240() {
        let (state, output_id, mut surface) = fractional_surface();
        let (client, mut dispatcher) = test_client_and_dispatcher(9);
        {
            let mut cd = client.test_dispatcher(&mut dispatcher);
            surface.send_preferred_fractional_scale_for_object(&state, Some(output_id), &mut cd);
        }
        assert!(test_sent_arg(&dispatcher, FRACTIONAL_ID, 0, 240));
        assert_eq!(surface.preferred_fractional_scale_sent, Some(240));
    }

    /// An Xwayland client committing through the full `on_commit` path, like
    /// [`XwaylandCommit`] but with a wire scale and an attachable buffer.
    struct WireScaledCommit {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
    }

    impl WireScaledCommit {
        fn new(scale: WireScale) -> Self {
            let mut state = state_for_tests();
            let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(4));
            client.set_wire_scale_for_tests(scale);
            let surface = client
                .insert_object(
                    &mut state,
                    WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6),
                )
                .0;
            assert!(surface.grab(&mut state).assign_xwayland_role());
            Self { state, client, dispatcher: Dispatcher::default(), surface }
        }

        fn with_surface(&mut self, with: impl FnOnce(&mut WlSurface)) {
            with(&mut self.surface.grab(&mut self.state));
        }

        fn attach_buffer(&mut self, id: u32, width: i32, height: i32) {
            let buffer_id = WlBufferId::from(id);
            let object_id = insert_test_shm_buffer(
                &mut self.state,
                self.client.client_id(),
                buffer_id,
                width,
                height,
            );
            let buffer = crate::buffer_content::AttachedBuffer::resolve(
                &self.state,
                Resolved::new(self.client.client_id(), buffer_id, object_id),
            )
            .expect("test buffer");
            self.with_surface(|surface| surface.pending.buffer = Some(Some(buffer)));
        }

        fn commit(&mut self) {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            let mut surface = self.surface.grab(&mut self.state);
            surface.on_commit(&mut self.client, &mut self.state, &mut client_dispatcher);
        }

        fn check(&self, check: impl FnOnce(&OnScreen)) {
            check(&self.surface.rref(&self.state).on_screen);
        }

        fn stays_connected(&self) -> bool {
            let client_id = self.client.client_id();
            !self.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == client_id)
            })
        }
    }

    #[test]
    fn wire_scaled_commit_reads_the_buffer_at_the_effective_scale() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 100, height: 50 }));
            assert_eq!(on_screen.buffer_scale, 2);
            let wire = on_screen.wire_inputs.as_ref().unwrap();
            assert_eq!(wire.scale, WireScale::new(2));
            assert_eq!(wire.buffer_scale, 1);
            assert_eq!(wire.viewport, SurfaceViewport::default());
            assert_eq!(wire.input_region, RegionValue::infinite());
            assert_eq!(wire.opaque_region, RegionValue::empty());
        });

        // An odd wire size rounds up to the next logical pixel.
        commit.attach_buffer(9101, 201, 101);
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 101, height: 51 }));
        });
    }

    #[test]
    fn changing_the_wire_scale_recomputes_an_on_screen_buffer_without_a_new_commit() {
        let mut commit = WireScaledCommit::new(WireScale::ONE);
        commit.attach_buffer(9100, 200, 100);
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 200, height: 100 }));
            assert!(on_screen.wire_inputs.is_none());
        });

        commit
            .client
            .process(
                &mut commit.state,
                ClientAction::SetWireScale { scale: WireScale::new(2) },
                &mut commit.dispatcher,
            )
            .unwrap();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 100, height: 50 }));
            assert_eq!(on_screen.buffer_scale, 2);
            assert_eq!(on_screen.wire_inputs.as_ref().unwrap().buffer_scale, 1);
        });
        assert!(commit.dispatcher.test_has_queued_action(
            |action: &crate::xwayland::XwaylandAction| {
                matches!(action, crate::xwayland::XwaylandAction::Reconfigure)
            }
        ));

        let queued = commit
            .dispatcher
            .test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::Reconfigure)
            });
        commit
            .client
            .process(
                &mut commit.state,
                ClientAction::SetWireScale { scale: WireScale::new(2) },
                &mut commit.dispatcher,
            )
            .unwrap();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 100, height: 50 }));
        });
        assert_eq!(
            commit
                .dispatcher
                .test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                    matches!(action, crate::xwayland::XwaylandAction::Reconfigure)
                }),
            queued,
        );
    }

    #[test]
    fn wire_scaled_commit_multiplies_the_clients_own_buffer_scale() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.with_surface(|surface| surface.pending.buffer_scale = 2);
        commit.attach_buffer(9100, 200, 100);
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 50, height: 25 }));
            assert_eq!(on_screen.buffer_scale, 4);
            assert_eq!(on_screen.wire_inputs.as_ref().unwrap().buffer_scale, 2);
        });
    }

    #[test]
    fn wire_scaled_surface_damage_rounds_outward() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.commit();
        commit.with_surface(|surface| {
            surface
                .pending
                .damage_surface
                .push(DamageRect { x: 1, y: 1, width: 3, height: 3 });
            // Drop the first commit's full damage so the explicit rect shows.
            surface.on_screen.damage.clear();
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.damage.rects(), &[RectI { x: 0, y: 0, width: 2, height: 2 }]);
        });
    }

    #[test]
    fn wire_scaled_input_region_rounds_outward_and_opaque_region_inward() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.with_surface(|surface| {
            surface.pending.input_region = Some(region_value(0, 0, 200, 100));
            surface.pending.opaque_region = Some(region_value(1, 0, 2, 2));
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.input_region, region_value(0, 0, 100, 50));
            assert!(on_screen.input_region.contains(49, 24));
            assert!(!on_screen.input_region.contains(100, 50));
            // The inward rect shrinks to nothing: not opaque at all.
            assert_eq!(on_screen.opaque_region, RegionValue::empty());
            let wire = on_screen.wire_inputs.as_ref().unwrap();
            assert_eq!(wire.input_region, region_value(0, 0, 200, 100));
            assert_eq!(wire.opaque_region, region_value(1, 0, 2, 2));
        });
    }

    #[test]
    fn wire_scaled_regions_fall_back_to_the_raw_wire_inputs() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.with_surface(|surface| {
            surface.pending.input_region = Some(region_value(0, 0, 100, 100));
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.input_region, region_value(0, 0, 50, 50));
        });

        // A commit with a new buffer and no region request falls back to the
        // raw region and normalizes it exactly once: still (0,0,50,50), not
        // (0,0,25,25).
        commit.attach_buffer(9101, 200, 100);
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.input_region, region_value(0, 0, 50, 50));
            assert_eq!(
                on_screen.wire_inputs.as_ref().unwrap().input_region,
                region_value(0, 0, 100, 100),
            );
        });
    }

    #[test]
    fn wire_scaled_viewport_destination_rounds_to_logical() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.with_surface(|surface| {
            surface.pending.viewport.dst = Some(SizeI { width: 400, height: 200 });
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 200, height: 100 }));
            assert_eq!(on_screen.viewport.dst, Some(SizeI { width: 200, height: 100 }));
            assert_eq!(
                on_screen.wire_inputs.as_ref().unwrap().viewport.dst,
                Some(SizeI { width: 400, height: 200 }),
            );
        });
    }

    #[test]
    fn wire_scaled_viewport_source_keeps_the_clients_integer_request() {
        // The client sends an integer source of (0, 0, 201, 101) with no
        // destination. The raw pending values pass the mapping validation;
        // the normalized source becomes fractional (100.5 x 50.5) and the
        // logical size rounds up.
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 201, 101);
        commit.with_surface(|surface| {
            surface.pending.viewport.src =
                Some(transform::RectF { x: 0.0, y: 0.0, width: 201.0, height: 101.0 });
        });
        commit.commit();
        assert!(commit.stays_connected());
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 101, height: 51 }));
            assert_eq!(
                on_screen.viewport.src,
                Some(transform::RectF { x: 0.0, y: 0.0, width: 100.5, height: 50.5 }),
            );
            assert_eq!(
                on_screen.wire_inputs.as_ref().unwrap().viewport.src,
                Some(transform::RectF { x: 0.0, y: 0.0, width: 201.0, height: 101.0 }),
            );
        });
    }

    #[test]
    fn wire_scaled_offset_moves_in_logical_units() {
        let mut commit = WireScaledCommit::new(WireScale::new(2));
        commit.attach_buffer(9100, 200, 100);
        commit.commit();
        commit.check(|on_screen| assert_eq!((on_screen.x, on_screen.y), (0, 0)));

        commit.attach_buffer(9101, 200, 100);
        commit.with_surface(|surface| surface.pending.offset = (3, 5));
        commit.commit();
        commit.check(|on_screen| assert_eq!((on_screen.x, on_screen.y), (1, 2)));
    }

    #[test]
    fn wire_scale_one_is_the_identity_on_commit() {
        let mut commit = WireScaledCommit::new(WireScale::ONE);
        commit.attach_buffer(9100, 200, 100);
        commit.with_surface(|surface| {
            surface.pending.input_region = Some(region_value(0, 0, 100, 100));
            surface.pending.opaque_region = Some(region_value(1, 0, 2, 2));
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.logical_size, Some(SizeI { width: 200, height: 100 }));
            assert_eq!(on_screen.buffer_scale, 1);
            assert_eq!(on_screen.input_region, region_value(0, 0, 100, 100));
            assert_eq!(on_screen.opaque_region, region_value(1, 0, 2, 2));
            assert!(on_screen.wire_inputs.is_none());
        });

        commit.with_surface(|surface| {
            surface
                .pending
                .damage_surface
                .push(DamageRect { x: 1, y: 1, width: 3, height: 3 });
            surface.on_screen.damage.clear();
        });
        commit.commit();
        commit.check(|on_screen| {
            assert_eq!(on_screen.damage.rects(), &[RectI { x: 1, y: 1, width: 3, height: 3 }]);
        });
    }

    #[test]
    fn a_normal_client_is_never_normalized() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(9), client.client_id(), 6))
            .0;
        let buffer_id = WlBufferId::from(9100);
        let object_id = insert_test_shm_buffer(&mut state, client.client_id(), buffer_id, 200, 100);
        let attached = crate::buffer_content::AttachedBuffer::resolve(
            &state,
            Resolved::new(client.client_id(), buffer_id, object_id),
        )
        .expect("test buffer");
        {
            let mut surface = surface.grab(&mut state);
            surface.pending.buffer = Some(Some(attached));
            surface.pending.input_region = Some(region_value(0, 0, 100, 100));
        }
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = surface.grab(&mut state);
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }

        let on_screen = &surface.rref(&state).on_screen;
        assert_eq!(on_screen.logical_size, Some(SizeI { width: 200, height: 100 }));
        assert_eq!(on_screen.buffer_scale, 1);
        assert_eq!(on_screen.input_region, region_value(0, 0, 100, 100));
        assert!(on_screen.wire_inputs.is_none());

        // A commit without a region request keeps today's on-screen fallback.
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface = surface.grab(&mut state);
            surface.pending.buffer = None;
            surface.on_commit(&mut client, &mut state, &mut client_dispatcher);
        }
        let on_screen = &surface.rref(&state).on_screen;
        assert_eq!(on_screen.input_region, region_value(0, 0, 100, 100));
        assert!(on_screen.wire_inputs.is_none());
    }

    #[test]
    fn pending_null_like_regions_can_be_committed_explicitly() {
        let mut surface = make_surface();
        surface.pending.input_region = Some(RegionValue::infinite());
        surface.pending.opaque_region = Some(RegionValue::empty());

        surface.on_screen.input_region = surface.pending.input_region.take().unwrap();
        surface.on_screen.opaque_region = surface.pending.opaque_region.take().unwrap();

        assert!(surface.accepts_input_at(50.0, 50.0));
        assert_eq!(surface.on_screen.opaque_region, RegionValue::empty());
    }

    #[test]
    fn parent_commit_applies_pending_child_positions_and_stack_order() {
        let mut state = state_for_tests();
        let parent_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6),
        );
        let child_a_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(1), 6),
        );
        let child_b_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(1), 6),
        );
        let parent_resolved = root(&state, parent_object_id);
        let child_a_resolved = root(&state, child_a_object_id);
        let child_b_resolved = root(&state, child_b_object_id);
        let child_a_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(10),
            child_a_resolved,
            parent_resolved,
        );
        let child_b_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(11),
            child_b_resolved,
            parent_resolved,
        );

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_a_resolved);
            parent.add_pending_child(child_b_resolved);
        }
        {
            let child_a = state
                .objects
                .get_mut(child_a_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_a.assign_subsurface(child_a_subsurface, parent_resolved);
            child_a.set_subsurface_position(20, 30);
        }
        {
            let child_b = state
                .objects
                .get_mut(child_b_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_b.assign_subsurface(child_b_subsurface, parent_resolved);
            child_b.set_subsurface_position(-5, 7);
        }

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            assert!(parent.restack_child_relative(child_a_resolved, None, false));
        }
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
            assert_eq!(
                parent.current_stack(),
                &[
                    super::StackEntry::Child(child_b_resolved),
                    super::StackEntry::Child(child_a_resolved),
                    super::StackEntry::Parent,
                ]
            );
        }

        let child_a = state
            .objects
            .get(child_a_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        let child_b = state
            .objects
            .get(child_b_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert_eq!(child_a.current_subsurface_position(), (20, 30));
        assert_eq!(child_b.current_subsurface_position(), (-5, 7));
    }

    /// A root with two subsurface children `a` and `b`, both in the pending
    /// stack and applied, so pending and current order agree.
    fn parent_with_two_children(
        state: &mut State,
    ) -> (ObjectId, Resolved<WlSurfaceId>, Resolved<WlSurfaceId>, Resolved<WlSurfaceId>) {
        let parent_object_id =
            insert_surface(state, WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6));
        let a_object_id =
            insert_surface(state, WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(1), 6));
        let b_object_id =
            insert_surface(state, WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(1), 6));
        let parent = root(state, parent_object_id);
        let a = root(state, a_object_id);
        let b = root(state, b_object_id);
        let a_subsurface = insert_subsurface_object(state, WlSubsurfaceId::from(10), a, parent);
        let b_subsurface = insert_subsurface_object(state, WlSubsurfaceId::from(11), b, parent);
        {
            let parent_surface = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent_surface.add_pending_child(a);
            parent_surface.add_pending_child(b);
        }
        for (child_object_id, subsurface) in
            [(a_object_id, a_subsurface), (b_object_id, b_subsurface)]
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(subsurface, parent);
        }
        {
            let mut parent_surface = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent_surface.apply_pending_tree_state(state));
        }
        (parent_object_id, parent, a, b)
    }

    #[test]
    fn stack_order_is_the_one_at_commit() {
        let mut state = state_for_tests();
        let (parent_object_id, _parent, a, _b) = parent_with_two_children(&mut state);

        let first = {
            let parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            parent.capture_tree(&state)
        };
        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            assert!(parent.restack_child_relative(a, None, true));
        }
        let second = {
            let parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            parent.capture_tree(&state)
        };
        assert_ne!(first.stack, second.stack);

        let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
        parent.apply_tree(&mut state, &first);
        assert_eq!(parent.current_stack(), first.stack.as_slice());
        assert_ne!(parent.current_stack(), second.stack.as_slice());

        parent.apply_tree(&mut state, &second);
        assert_eq!(parent.current_stack(), second.stack.as_slice());
    }

    #[test]
    fn removed_child_is_not_restacked_by_an_older_tree_update() {
        let mut state = state_for_tests();
        let (parent_object_id, _parent, a, _b) = parent_with_two_children(&mut state);

        let older = {
            let parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            parent.capture_tree(&state)
        };
        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.remove_child_from_tree(a);
        }

        let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
        parent.apply_tree(&mut state, &older);
        assert!(!parent
            .current_stack()
            .contains(&super::StackEntry::Child(a)));
    }

    #[test]
    fn synchronized_behavior_inherits_from_sync_ancestors() {
        let mut state = state_for_tests();
        let root_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6),
        );
        let child_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(1), 6),
        );
        let grandchild_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(1), 6),
        );
        let root_resolved = root(&state, root_object_id);
        let child_resolved = root(&state, child_object_id);
        let grandchild_resolved = root(&state, grandchild_object_id);
        let child_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(20),
            child_resolved,
            root_resolved,
        );
        let grandchild_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(21),
            grandchild_resolved,
            child_resolved,
        );

        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(child_subsurface, root_resolved);
            child.set_subsurface_sync_mode(true);
        }
        {
            let grandchild = state
                .objects
                .get_mut(grandchild_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            grandchild.assign_subsurface(grandchild_subsurface, child_resolved);
            grandchild.set_subsurface_sync_mode(false);
        }

        let child = state
            .objects
            .get(child_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        let grandchild = state
            .objects
            .get(grandchild_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert!(child.behaves_synchronized(&state));
        assert!(grandchild.behaves_synchronized(&state));

        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.set_subsurface_sync_mode(false);
        }

        let child = state
            .objects
            .get(child_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        let grandchild = state
            .objects
            .get(grandchild_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert!(!child.behaves_synchronized(&state));
        assert!(!grandchild.behaves_synchronized(&state));
    }

    #[test]
    fn restack_relative_to_sibling_updates_pending_order() {
        let mut state = state_for_tests();
        let parent_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6),
        );
        let child_a_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(1), 6),
        );
        let child_b_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(3), ClientId::from_raw(1), 6),
        );
        let child_c_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(1), 6),
        );
        let parent_resolved = root(&state, parent_object_id);
        let child_a_resolved = root(&state, child_a_object_id);
        let child_b_resolved = root(&state, child_b_object_id);
        let child_c_resolved = root(&state, child_c_object_id);
        let child_a_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(41),
            child_a_resolved,
            parent_resolved,
        );
        let child_b_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(42),
            child_b_resolved,
            parent_resolved,
        );
        let child_c_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(43),
            child_c_resolved,
            parent_resolved,
        );

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_a_resolved);
            parent.add_pending_child(child_b_resolved);
            parent.add_pending_child(child_c_resolved);
            assert!(parent.restack_child_relative(child_c_resolved, Some(child_a_resolved), false));
            assert!(parent.restack_child_relative(child_b_resolved, Some(child_c_resolved), true));
        }
        {
            let child_a = state
                .objects
                .get_mut(child_a_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_a.assign_subsurface(child_a_subsurface, parent_resolved);
        }
        {
            let child_b = state
                .objects
                .get_mut(child_b_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_b.assign_subsurface(child_b_subsurface, parent_resolved);
        }
        {
            let child_c = state
                .objects
                .get_mut(child_c_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_c.assign_subsurface(child_c_subsurface, parent_resolved);
        }
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
            assert_eq!(
                parent.current_stack(),
                &[
                    super::StackEntry::Child(child_c_resolved),
                    super::StackEntry::Child(child_b_resolved),
                    super::StackEntry::Child(child_a_resolved),
                    super::StackEntry::Parent,
                ]
            );
        }
    }

    #[test]
    fn applying_unchanged_tree_state_is_idempotent() {
        let mut state = state_for_tests();
        let parent_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6),
        );
        let child_object_id = insert_surface(
            &mut state,
            WlSurface::new(WlSurfaceId::from(2), ClientId::from_raw(1), 6),
        );
        let parent_resolved = root(&state, parent_object_id);
        let child_resolved = root(&state, child_object_id);
        let child_subsurface = insert_subsurface_object(
            &mut state,
            WlSubsurfaceId::from(51),
            child_resolved,
            parent_resolved,
        );

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_resolved);
        }
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(child_subsurface, parent_resolved);
            child.set_subsurface_position(12, 34);
        }
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
        }
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(!parent.apply_pending_tree_state(&mut state));
        }
    }

    #[test]
    fn repeated_synchronized_cache_updates_compose_latest_state() {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6);
        let mut first = WlSurface::test_payload(Some(Some(fake_buffer(ObjectId::from(1usize)))));
        first.offset = (10, 20);
        first.alpha_multiplier = u32::MAX;
        first.color = Some(Some((
            crate::color::ColorManager::untagged_srgb_gamma22(),
            crate::color::RenderIntent::Relative,
        )));
        first.damage_surface = vec![DamageRect { x: 0, y: 0, width: 10, height: 10 }];
        let discarded = surface.merge_cached_update(ContentUpdate::test_new(
            first,
            vec![resolved_callback(7)],
            vec![feedback(17, 2)],
        ));
        assert!(discarded.is_empty());

        let mut second = WlSurface::test_payload(Some(Some(fake_buffer(ObjectId::from(2usize)))));
        second.offset = (30, 40);
        second.buffer_transform = BufferTransform::FlipRotate180;
        second.buffer_scale = 2;
        second.presentation_hint = PresentationHint::Async;
        second.alpha_multiplier = 42;
        second.damage_surface = vec![DamageRect { x: 5, y: 6, width: 7, height: 8 }];
        second.input_region = RegionValue::empty();
        second.opaque_region = RegionValue::infinite();
        let discarded = surface.merge_cached_update(ContentUpdate::new(
            2,
            0,
            second,
            vec![resolved_callback(8)],
            vec![feedback(18, 2)],
        ));
        assert_eq!(discarded, vec![feedback(17, 2)]);

        let cached = surface.cached_update.as_ref().unwrap();
        assert_eq!(cached.payload.buffer, Some(Some(fake_buffer(ObjectId::from(2usize)))));
        assert_eq!(cached.payload.offset, (30, 40));
        assert_eq!(cached.payload.buffer_transform, BufferTransform::FlipRotate180);
        assert_eq!(cached.payload.buffer_scale, 2);
        assert_eq!(cached.payload.presentation_hint, PresentationHint::Async);
        assert_eq!(cached.payload.alpha_multiplier, 42);
        assert_eq!(cached.payload.damage_surface.len(), 2);
        assert_eq!(cached.frame_callbacks, vec![resolved_callback(7), resolved_callback(8),]);
        assert_eq!(cached.presentation_feedbacks, vec![feedback(18, 2)]);
        assert_eq!(
            cached.payload.color.as_ref().map(|color| color
                .as_ref()
                .map(|(description, intent)| (description.id(), *intent))),
            Some(Some((1, crate::color::RenderIntent::Relative)))
        );
        let (feedbacks, mut clearing) = {
            let cached = surface.cached_update.as_ref().unwrap();
            let feedbacks = cached.presentation_feedbacks.clone();
            let mut clearing = cached.payload.clone();
            clearing.color = Some(None);
            clearing.damage_surface.clear();
            clearing.damage_buffer.clear();
            (feedbacks, clearing)
        };
        surface.merge_cached_update(ContentUpdate::new(3, 0, clearing, Vec::new(), feedbacks));
        assert!(surface
            .cached_update
            .as_ref()
            .unwrap()
            .payload
            .color
            .as_ref()
            .is_some_and(|color| color.is_none()));
    }

    #[test]
    fn merged_cached_update_spans_both_commits() {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6);
        surface.merge_cached_update(ContentUpdate::test_new(
            WlSurface::test_payload(None),
            Vec::new(),
            Vec::new(),
        ));
        surface.merge_cached_update(ContentUpdate::new(
            2,
            0,
            WlSurface::test_payload(None),
            Vec::new(),
            Vec::new(),
        ));
        let cached = surface.cached_update.as_ref().unwrap();
        assert_eq!((cached.first_seq, cached.last_seq), (1, 2));
    }

    #[test]
    fn subsurface_destroy_with_cached_update_does_not_freeze_later_commits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (_, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 0);

        destroy_subsurface(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61));
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
        assert!(buffer_release_was_sent(&dispatcher, 20));

        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 2);
    }

    #[test]
    fn discarded_update_completes_its_frame_callbacks() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (_, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        let (callback, _) =
            client.insert_object(&mut state, WlCallback::new(WlCallbackId::from(30)));
        child
            .grab(&mut state)
            .pending_frame_callbacks
            .push(callback);
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);

        destroy_subsurface(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61));
        assert!(done_event_was_sent(&dispatcher, 30));
        assert!(delete_id_was_sent(&dispatcher, 30));
    }

    #[test]
    fn route_presentation_feedbacks_replaces_superseded_on_screen_commit() {
        let mut surface = make_surface();
        surface.on_screen.presentation_feedbacks = vec![feedback(21, 2)];

        let (discarded, needs_frame) = surface.route_presentation_feedbacks(vec![feedback(22, 2)]);

        assert_eq!(discarded, vec![feedback(21, 2)]);
        assert!(needs_frame);
        assert_eq!(surface.on_screen.presentation_feedbacks, vec![feedback(22, 2)]);
    }

    #[test]
    fn route_presentation_feedbacks_on_unmapped_surface_discards_immediately() {
        let mut surface = WlSurface::new(WlSurfaceId::from(1), ClientId::from_raw(1), 6);

        let (discarded, needs_frame) = surface.route_presentation_feedbacks(vec![feedback(23, 2)]);

        assert_eq!(discarded, vec![feedback(23, 2)]);
        assert!(!needs_frame);
    }

    #[test]
    fn take_render_state_drains_presentation_feedbacks() {
        let mut surface = make_surface();
        surface.on_screen.presentation_feedbacks = vec![feedback(31, 2), feedback(32, 1)];

        let render_state = surface
            .take_render_state_at(
                Resolved::new(ClientId::from_raw(7), WlSurfaceId::from(1), ObjectId::from(31usize)),
                0,
                0,
            )
            .unwrap();

        assert_eq!(render_state.presentation_feedbacks, vec![feedback(31, 2), feedback(32, 1)]);
        assert!(surface.on_screen.presentation_feedbacks.is_empty());
    }

    #[test]
    fn backend_discard_also_clears_unassigned_feedbacks() {
        let mut surface = make_surface();
        surface.pending_presentation_feedbacks = vec![feedback(41, 2)];
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut state = state_for_tests();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(0usize));

        surface.discard_presentation_feedbacks_for_backend(
            &mut client,
            &mut state,
            &mut dispatcher,
            backend,
        );

        assert!(surface.pending_presentation_feedbacks.is_empty());
    }

    #[test]
    fn deleting_parent_surface_detaches_and_unmaps_child_subtree() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut dispatcher = client.test_dispatcher(&mut dispatcher);

        let (parent_resolved_insert, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(1), client.client_id(), 6));
        let parent_object_id = parent_resolved_insert.object_id();
        state.register_root_surface(parent_resolved_insert);
        let (child_resolved_insert, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(2), client.client_id(), 6));
        let child_object_id = child_resolved_insert.object_id();
        state.register_root_surface(child_resolved_insert);
        let (grandchild_resolved_insert, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(3), client.client_id(), 6));
        let grandchild_object_id = grandchild_resolved_insert.object_id();
        state.register_root_surface(grandchild_resolved_insert);
        let parent_resolved = root(&state, parent_object_id);
        let child_resolved = root(&state, child_object_id);
        let grandchild_resolved = root(&state, grandchild_object_id);
        let child_subsurface = insert_client_subsurface_object(
            &mut client,
            &mut state,
            WlSubsurfaceId::from(61),
            child_resolved,
            parent_resolved,
        );
        let grandchild_subsurface = insert_client_subsurface_object(
            &mut client,
            &mut state,
            WlSubsurfaceId::from(62),
            grandchild_resolved,
            child_resolved,
        );

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_resolved);
            parent.test_map(ObjectId::from(300usize), 0, 0, 40, 40);
        }
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(child_subsurface, parent_resolved);
            child.set_subsurface_position(5, 6);
            child.add_pending_child(grandchild_resolved);
            child.test_map(ObjectId::from(301usize), 0, 0, 20, 20);
        }
        {
            let grandchild = state
                .objects
                .get_mut(grandchild_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            grandchild.assign_subsurface(grandchild_subsurface, child_resolved);
            grandchild.set_subsurface_position(7, 8);
            grandchild.test_map(ObjectId::from(302usize), 0, 0, 10, 10);
        }
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
        }
        {
            let mut child = state.objects.grab_object::<WlSurface>(child_object_id);
            assert!(child.apply_pending_tree_state(&mut state));
        }

        {
            let mut parent_object = client.grab_object(WlSurfaceId::from(1), &mut state);
            parent_object.delete_self(&mut client, &mut state, &mut dispatcher);
        }

        let child = state
            .objects
            .get(child_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        let grandchild = state
            .objects
            .get(grandchild_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert!(child.is_root());
        assert!(grandchild.is_root());
        assert!(child.subsurface_object().is_none());
        assert!(grandchild.subsurface_object().is_none());
        assert!(child.render_state().is_none());
        assert!(grandchild.render_state().is_none());
        assert!(client.contains_object(WlSubsurfaceId::from(61), &state));
        assert!(client.contains_object(WlSubsurfaceId::from(62), &state));

        {
            let mut subsurface = client.grab_object(WlSubsurfaceId::from(61), &mut state);
            let mut event = RawEvent::new(WlSubsurfaceId::from(61).into(), 1, 2);
            event.int(3);
            event.int(4);
            subsurface
                .on(&mut client, &mut state, raw_request_from_event(event), &mut dispatcher)
                .unwrap();
        }
        {
            let mut subsurface = client.grab_object(WlSubsurfaceId::from(61), &mut state);
            subsurface
                .on(
                    &mut client,
                    &mut state,
                    raw_request_from_event(RawEvent::new(WlSubsurfaceId::from(61).into(), 0, 0)),
                    &mut dispatcher,
                )
                .unwrap();
        }
        {
            let mut subsurface = client.grab_object(WlSubsurfaceId::from(62), &mut state);
            subsurface
                .on(
                    &mut client,
                    &mut state,
                    raw_request_from_event(RawEvent::new(WlSubsurfaceId::from(62).into(), 0, 0)),
                    &mut dispatcher,
                )
                .unwrap();
        }

        assert!(!dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::Close { .. })
        }));
        assert!(!client.contains_object(WlSubsurfaceId::from(61), &state));
        assert!(!client.contains_object(WlSubsurfaceId::from(62), &state));
    }

    #[test]
    fn mapped_state_keeps_color_alpha_and_representation() {
        let mut state = state_for_tests();
        let (client, _) = test_client_and_dispatcher(11);
        let buffer_object_id =
            insert_test_shm_buffer(&mut state, client.client_id(), WlBufferId::from(90), 2, 2);
        let buffer = Resolved::new(client.client_id(), WlBufferId::from(90), buffer_object_id);
        let buffer = crate::buffer_content::AttachedBuffer::resolve(&state, buffer)
            .expect("test buffer is live");
        let mut surface = make_surface();
        let description = state.color_manager.windows_scrgb();
        let coefficients = crate::color::Coefficients {
            kind: crate::color::yuv::CoefficientKind::Bt709,
            range: crate::color::yuv::VideoRange::Full,
        };
        let representation = crate::color::ColorRepresentation {
            alpha_mode: Some(crate::color::AlphaMode::Straight),
            coefficients: Some(coefficients),
            chroma: None,
        };
        let payload = SurfaceCommitPayload {
            buffer: Some(Some(buffer.clone())),
            offset: (0, 0),
            sync: SurfaceSyncState::default(),
            buffer_transform: BufferTransform::Normal,
            buffer_scale: 1,
            viewport: SurfaceViewport::default(),
            presentation_hint: PresentationHint::Vsync,
            alpha_multiplier: 9,
            blur_region: None,
            color: Some(Some((
                std::rc::Rc::clone(&description),
                crate::color::RenderIntent::RelativeBpc,
            ))),
            representation: crate::color::RepresentationUpdate {
                alpha_mode: Some(Some(crate::color::AlphaMode::Straight)),
                coefficients: Some(Some(coefficients)),
                chroma: None,
            },
            damage_surface: Vec::new(),
            damage_buffer: Vec::new(),
            input_region: RegionValue::infinite(),
            opaque_region: RegionValue::empty(),
            release_callbacks: Vec::new(),
            wire_inputs: None,
        };
        let mapped = surface
            .mapped_state_for_buffer(&state, buffer, 1, 2, &payload)
            .expect("buffer maps");
        assert_eq!(mapped.alpha_multiplier, 9);
        assert_eq!(mapped.color.as_ref().unwrap().0.id(), description.id());
        assert_eq!(mapped.color.as_ref().unwrap().1, crate::color::RenderIntent::RelativeBpc);
        assert_eq!(mapped.representation, representation);
        assert_eq!(surface.on_screen.alpha_multiplier, u32::MAX);
        assert!(surface.on_screen.color.is_none());
        assert_eq!(surface.on_screen.representation, crate::color::ColorRepresentation::default());

        surface.activate_mapped_state(mapped);
        assert_eq!(surface.on_screen.alpha_multiplier, 9);
        assert_eq!(surface.on_screen.color.as_ref().unwrap().0.id(), description.id());
        assert_eq!(surface.on_screen.representation, representation);
    }

    #[test]
    fn client_disconnect_stops_the_acquire_waiter() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let mut surface = make_surface();
        let waiter = crate::sync::AcquireReadyWaiterId::from(42u64);
        surface.acquire_waiter = Some(waiter);

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            crate::object::Object::on_client_disconnect(
                &mut surface,
                &mut client,
                &mut state,
                &mut client_dispatcher,
            );
        }

        assert_eq!(surface.acquire_waiter, None);
        assert!(dispatcher.test_stop_queued(waiter));
    }

    #[test]
    fn focus_on_map_follows_interactive_participation() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let (surface_resolved, _) = Client::new_for_tests(ClientId::from_raw(4)).insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(44u32), ClientId::from_raw(4), 6),
        );
        state.register_root_surface(surface_resolved);
        crate::test_support::insert_window(&mut state, surface_resolved);
        let _ = state.assign_root_surface_to_workspace(surface_resolved, workspace_id);
        let window = state.window_for_root(surface_resolved).unwrap();
        state
            .workspaces
            .try_get_mut(workspace_id)
            .unwrap()
            .layout
            .floating
            .stack = vec![window];

        assert!(!super::should_focus_new_toplevel_on_map(&state, surface_resolved));
        state.managed_window_mut(surface_resolved).unwrap().mapped = true;
        assert!(super::should_focus_new_toplevel_on_map(&state, surface_resolved));
    }

    struct BufferLifetime {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
    }

    impl BufferLifetime {
        fn new() -> Self {
            let mut state = state_for_tests();
            let mut client = Client::new_for_tests(ClientId::from_raw(1));
            let (surface, _) = client.insert_object(
                &mut state,
                WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6),
            );
            Self { state, client, dispatcher: Dispatcher::default(), surface }
        }

        fn insert_buffer(&mut self, id: u32) -> Resolved<WlBufferId> {
            let bytes = vec![0u8; 16];
            let (resolved, _) = self.client.insert_object(
                &mut self.state,
                WlBuffer::new(crate::wayland_objects::wl_buffer::NewWlBufferParams {
                    id: WlBufferId::from(id),
                    offset: 0,
                    width: 2,
                    height: 2,
                    stride: 8,
                    format: &crate::drm::ARGB8888,
                    mem: crate::wayland_objects::wl_shm_pool::shared_memory_from_bytes(&bytes),
                })
                .unwrap(),
            );
            resolved
        }

        fn commit_buffer(&mut self, buffer: Resolved<WlBufferId>) {
            let attached = crate::buffer_content::AttachedBuffer::resolve(&self.state, buffer)
                .expect("test buffer");
            self.surface.grab(&mut self.state).pending.buffer = Some(Some(attached));
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            let surface = self.surface.grab(&mut self.state);
            surface.on_commit(&mut self.client, &mut self.state, &mut client_dispatcher);
            client_dispatcher.drain_post_actions(&mut self.client, &mut self.state);
        }

        fn destroy_buffer(&mut self, buffer: Resolved<WlBufferId>) {
            let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(buffer.wayland_id().into());
            let request = raw_request_from_event(RawEvent::new(buffer.wayland_id().into(), 0, 0));
            buffer
                .grab(&mut self.state)
                .on(&mut self.client, &mut self.state, request, &mut client_dispatcher)
                .unwrap();
            client_dispatcher.drain_post_actions(&mut self.client, &mut self.state);
        }

        fn content_weak(
            &self,
            buffer: Resolved<WlBufferId>,
        ) -> std::rc::Weak<crate::buffer_content::BufferContent> {
            std::rc::Rc::downgrade(buffer.try_ref(&self.state).unwrap().content())
        }

        /// Deliver releases for buffers this surface just retired, the way a frame would.
        fn release_retired(&mut self) {
            let retired = self.surface.grab(&mut self.state).take_retired_buffers();
            for candidate in retired {
                crate::sync::DeferredRelease::from_candidate(candidate)
                    .resolve(&mut self.state, &mut self.dispatcher);
            }
            for action in self.dispatcher.test_take_queued_actions::<ClientAction>() {
                self.client
                    .process(&mut self.state, action, &mut self.dispatcher)
                    .unwrap();
            }
        }
    }

    fn delete_id_was_sent(dispatcher: &Dispatcher, id: u32) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 12 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            let arg = u32::from_ne_bytes(buffer[8..12].try_into().unwrap());
            if object == 1 && opcode == 1 && arg == id {
                found = true;
            }
        });
        found
    }

    fn buffer_release_was_sent(dispatcher: &Dispatcher, id: u32) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            if object == id && opcode == 0 {
                found = true;
            }
        });
        found
    }

    fn done_event_was_sent(dispatcher: &Dispatcher, id: u32) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            if object == id && opcode == 0 {
                found = true;
            }
        });
        found
    }

    #[test]
    fn destroying_an_unattached_buffer_sends_delete_id_and_frees_it() {
        let mut life = BufferLifetime::new();
        let buffer = life.insert_buffer(20);
        let content = life.content_weak(buffer);
        life.destroy_buffer(buffer);
        assert!(delete_id_was_sent(&life.dispatcher, 20));
        assert!(!life.state.objects.contains(buffer.object_id()));
        assert!(!life
            .client
            .contains_object(buffer.wayland_id(), &life.state));
        assert!(content.upgrade().is_none());
    }

    #[test]
    fn destroying_an_on_screen_buffer_keeps_its_pixels_until_replaced() {
        let mut life = BufferLifetime::new();
        let first = life.insert_buffer(20);
        life.commit_buffer(first);
        let content = life.content_weak(first);
        life.destroy_buffer(first);
        assert!(delete_id_was_sent(&life.dispatcher, 20));
        assert!(!life.state.objects.contains(first.object_id()));
        assert!(!life.client.contains_object(first.wayland_id(), &life.state));
        let shown = content.upgrade().expect("surface still shows the pixels");
        {
            let surface = life.surface.grab(&mut life.state);
            let attached = surface.on_screen.buffer.as_ref().unwrap();
            assert!(std::rc::Rc::ptr_eq(&attached.content, &shown));
        }
        drop(shown);

        let second = life.insert_buffer(21);
        life.commit_buffer(second);
        assert!(content.upgrade().is_none());
        assert!(life.state.objects.contains(second.object_id()));
    }

    #[test]
    fn destroying_a_surface_drops_its_destroyed_buffer_content() {
        let mut life = BufferLifetime::new();
        let buffer = life.insert_buffer(20);
        life.commit_buffer(buffer);
        let content = life.content_weak(buffer);
        life.destroy_buffer(buffer);
        assert!(content.upgrade().is_some());
        {
            let mut client_dispatcher = life.client.test_dispatcher(&mut life.dispatcher);
            let mut surface = life.surface.grab(&mut life.state);
            surface.delete_self(&mut life.client, &mut life.state, &mut client_dispatcher);
            drop(surface);
            client_dispatcher.drain_post_actions(&mut life.client, &mut life.state);
        }
        assert!(content.upgrade().is_none());
    }

    #[test]
    fn client_disconnect_drops_destroyed_buffer_content() {
        let mut life = BufferLifetime::new();
        let buffer = life.insert_buffer(20);
        life.commit_buffer(buffer);
        let content = life.content_weak(buffer);
        life.destroy_buffer(buffer);
        assert!(content.upgrade().is_some());
        life.client
            .process(&mut life.state, ClientAction::Close, &mut life.dispatcher)
            .unwrap();
        assert!(content.upgrade().is_none());
    }

    #[test]
    fn release_for_destroyed_buffer_is_not_sent() {
        let mut life = BufferLifetime::new();
        let first = life.insert_buffer(20);
        life.commit_buffer(first);
        life.destroy_buffer(first);
        let second = life.insert_buffer(21);
        life.commit_buffer(second);
        life.release_retired();
        assert!(!buffer_release_was_sent(&life.dispatcher, 20));
        assert!(life.state.objects.contains(second.object_id()));
    }

    #[test]
    fn reused_buffer_id_gets_its_own_release() {
        let mut life = BufferLifetime::new();
        let first = life.insert_buffer(20);
        let deferred =
            crate::sync::DeferredRelease::from_candidate(crate::sync::RetiredBufferCandidate {
                buffer: first,
                sync: SurfaceSyncState::default(),
                release_callbacks: Vec::new(),
            });
        life.destroy_buffer(first);
        assert!(!life.state.objects.contains(first.object_id()));
        let second = life.insert_buffer(20);
        deferred.resolve(&mut life.state, &mut life.dispatcher);
        for action in life.dispatcher.test_take_queued_actions::<ClientAction>() {
            life.client
                .process(&mut life.state, action, &mut life.dispatcher)
                .unwrap();
        }
        let mut released = false;
        life.dispatcher
            .test_for_each_queued_action::<IoUringAction>(|action| {
                let IoUringAction::Sendmsg { buffer, .. } = action else {
                    return;
                };
                if buffer.len() < 8 {
                    return;
                }
                let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
                let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
                if object == 20 && opcode == 0 {
                    released = true;
                }
            });
        assert!(!released);
        assert!(life.state.objects.contains(second.object_id()));
        assert!(life
            .client
            .contains_object(WlBufferId::from(20), &life.state));
    }

    fn insert_request_buffer(client: &mut Client, state: &mut State, id: u32) {
        let bytes = vec![0u8; 16];
        client.insert_object(
            state,
            WlBuffer::new(crate::wayland_objects::wl_buffer::NewWlBufferParams {
                id: WlBufferId::from(id),
                offset: 0,
                width: 2,
                height: 2,
                stride: 8,
                format: &crate::drm::ARGB8888,
                mem: crate::wayland_objects::wl_shm_pool::shared_memory_from_bytes(&bytes),
            })
            .unwrap(),
        );
    }

    /// Send one request to a surface through its object dispatch, as a client would.
    fn surface_request(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: WlSurfaceId,
        opcode: u16,
        args: &[u32],
    ) {
        let mut event = RawEvent::new(surface.into(), opcode, args.len());
        for arg in args {
            event.uint(*arg);
        }
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(surface.into());
        let mut object = client.grab_object(surface, state);
        object
            .on(client, state, raw_request_from_event(event), &mut client_dispatcher)
            .unwrap();
    }

    fn commit_surface(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: Resolved<WlSurfaceId>,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        surface
            .grab(state)
            .on_commit(client, state, &mut client_dispatcher);
        client_dispatcher.drain_post_actions(client, state);
    }

    /// A root parent (surface 1) and a synchronized subsurface child (surface 2,
    /// `wl_subsurface` 61) attached to it.
    fn synchronized_child_of_root_parent(
        client: &mut Client,
        state: &mut State,
    ) -> (Resolved<WlSurfaceId>, Resolved<WlSurfaceId>) {
        let (parent, _) = client
            .insert_object(state, WlSurface::new(WlSurfaceId::from(1), client.client_id(), 6));
        let (child, _) = client
            .insert_object(state, WlSurface::new(WlSurfaceId::from(2), client.client_id(), 6));
        state.register_root_surface(parent);
        state.register_root_surface(child);
        let subsurface =
            insert_client_subsurface_object(client, state, WlSubsurfaceId::from(61), child, parent);
        parent.grab(state).add_pending_child(child);
        child.grab(state).assign_subsurface(subsurface, parent);
        (parent, child)
    }

    /// Send `wl_subsurface.destroy` as a client would.
    fn destroy_subsurface(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        subsurface: WlSubsurfaceId,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(subsurface.into());
        let mut object = client.grab_object(subsurface, state);
        object
            .on(
                client,
                state,
                raw_request_from_event(RawEvent::new(subsurface.into(), 0, 0)),
                &mut client_dispatcher,
            )
            .unwrap();
    }

    #[test]
    fn latest_tracks_attach_and_null_attach() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let surface = WlSurfaceId::from(4);
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        insert_request_buffer(&mut client, &mut state, 20);

        surface_request(&mut client, &mut state, &mut dispatcher, surface, 1, &[20, 0, 0]);
        surface_request(&mut client, &mut state, &mut dispatcher, surface, 6, &[]);
        assert!(client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .test_latest_buffer_is_some());

        surface_request(&mut client, &mut state, &mut dispatcher, surface, 1, &[0, 0, 0]);
        surface_request(&mut client, &mut state, &mut dispatcher, surface, 6, &[]);
        assert!(!client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .test_latest_buffer_is_some());
    }

    #[test]
    fn latest_keeps_regions_without_a_new_region_request() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let (surface, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6));
        surface.grab(&mut state).pending.input_region = Some(region_value(0, 0, 10, 10));
        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface.rref(&state).committed_input_region(), &region_value(0, 0, 10, 10));

        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface.rref(&state).committed_input_region(), &region_value(0, 0, 10, 10));
    }

    #[test]
    fn role_dismissal_clears_latest_buffer() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (surface, _) = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6));
        surface
            .grab(&mut state)
            .test_map(ObjectId::from(20usize), 0, 0, 2, 2);
        assert!(surface.rref(&state).test_latest_buffer_is_some());

        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        surface.grab(&mut state).force_unmap_for_role_dismissal(
            &mut client,
            &mut state,
            &mut client_dispatcher,
        );
        assert!(!surface.rref(&state).test_latest_buffer_is_some());
    }

    /// Send one request to a `wl_subsurface` as a client would.
    fn subsurface_request(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        subsurface: WlSubsurfaceId,
        opcode: u16,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(subsurface.into());
        let mut object = client.grab_object(subsurface, state);
        object
            .on(
                client,
                state,
                raw_request_from_event(RawEvent::new(subsurface.into(), opcode, 0)),
                &mut client_dispatcher,
            )
            .unwrap();
    }

    /// A synchronized grandchild (surface 3, `wl_subsurface` 62) of the child
    /// of `synchronized_child_of_root_parent`.
    fn synchronized_grandchild(
        client: &mut Client,
        state: &mut State,
        child: Resolved<WlSurfaceId>,
    ) -> Resolved<WlSurfaceId> {
        let (grandchild, _) = client
            .insert_object(state, WlSurface::new(WlSurfaceId::from(3), client.client_id(), 6));
        let subsurface = insert_client_subsurface_object(
            client,
            state,
            WlSubsurfaceId::from(62),
            grandchild,
            child,
        );
        child.grab(state).add_pending_child(grandchild);
        grandchild.grab(state).assign_subsurface(subsurface, child);
        grandchild
    }

    #[test]
    fn parent_commit_carries_the_child_cached_update() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 0);
        assert!(child.rref(&state).cached_update.is_some());

        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
        assert!(child.rref(&state).cached_update.is_none());
        assert!(child.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn child_commit_after_the_parent_commit_waits_for_the_next_parent_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);

        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
        assert!(child.rref(&state).cached_update.is_some());

        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 2);
        assert!(child.rref(&state).cached_update.is_none());
    }

    #[test]
    fn set_desync_applies_the_cached_update() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (_, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 0);

        // wl_subsurface.set_desync
        subsurface_request(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61), 5);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
        assert!(child.rref(&state).cached_update.is_none());
        assert!(child.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn nested_sync_grandchild_applies_with_the_childs_captured_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        let grandchild = synchronized_grandchild(&mut client, &mut state, child);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(3),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(3), 6, &[]);
        // The child's commit captures the grandchild's cached update.
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert!(grandchild.rref(&state).cached_update.is_none());
        assert_eq!(grandchild.rref(&state).test_applied_update_seq(), 0);
        assert_eq!(
            child
                .rref(&state)
                .cached_update
                .as_ref()
                .map(|u| u.children.len()),
            Some(1)
        );

        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(grandchild.rref(&state).test_applied_update_seq(), 1);
        assert!(grandchild.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn grandchild_commit_after_the_childs_commit_waits_for_the_childs_next_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        let grandchild = synchronized_grandchild(&mut client, &mut state, child);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(3),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(3), 6, &[]);

        // The parent carries the child's first commit, which has no grandchild state.
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
        assert_eq!(grandchild.rref(&state).test_applied_update_seq(), 0);
        assert!(grandchild.rref(&state).cached_update.is_some());

        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(grandchild.rref(&state).test_applied_update_seq(), 1);
        assert!(grandchild.rref(&state).cached_update.is_none());
    }

    #[test]
    fn destroyed_child_in_a_parent_update_is_discarded_and_released() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);

        let update = capture_update(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(update.children.len(), 1);
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 0, &[]);
        assert!(!child.exists(&state));
        assert!(!buffer_release_was_sent(&dispatcher, 20));

        apply_captured(&mut client, &mut state, &mut dispatcher, parent, update);
        assert!(buffer_release_was_sent(&dispatcher, 20));
    }

    /// A root surface with no role, registered as a root.
    fn insert_plain_root(client: &mut Client, state: &mut State, id: u32) -> Resolved<WlSurfaceId> {
        let (surface, _) = client
            .insert_object(state, WlSurface::new(WlSurfaceId::from(id), client.client_id(), 6));
        state.register_root_surface(surface);
        surface
    }

    /// Send one request without arguments to a `wl_buffer` as a client would.
    fn buffer_request(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        buffer: WlBufferId,
        opcode: u16,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(buffer.into());
        let mut object = client.grab_object(buffer, state);
        object
            .on(
                client,
                state,
                raw_request_from_event(RawEvent::new(buffer.into(), opcode, 0)),
                &mut client_dispatcher,
            )
            .unwrap();
    }

    /// Runs the `FlushCommitQueues` action of `client`, as the event loop does.
    fn flush_queued_commits(client: &mut Client, state: &mut State, dispatcher: &mut Dispatcher) {
        client
            .process(state, ClientAction::FlushCommitQueues, dispatcher)
            .unwrap();
    }

    #[test]
    fn ready_update_applies_at_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert!(surface.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn held_update_waits_and_later_updates_queue_behind_it() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);

        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 2);
        assert!(surface.rref(&state).on_screen.buffer.is_none());

        surface.grab(&mut state).test_release_head();
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 2);
        assert_eq!(
            surface
                .rref(&state)
                .on_screen
                .buffer
                .as_ref()
                .map(|buffer| buffer.object.wayland_id()),
            Some(WlBufferId::from(21))
        );
    }

    #[test]
    fn other_surfaces_of_the_client_are_not_held() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let held = insert_plain_root(&mut client, &mut state, 1);
        let free = insert_plain_root(&mut client, &mut state, 5);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        held.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(5),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(5), 6, &[]);
        assert_eq!(held.rref(&state).test_queue_len(), 1);
        assert_eq!(free.rref(&state).test_queue_len(), 0);
        assert_eq!(free.rref(&state).test_applied_update_seq(), 1);
        assert!(free.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn parent_update_waits_for_the_childs_earlier_own_update() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        // Desynchronized, the child's first commit is its own queued update.
        subsurface_request(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61), 5);
        child.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_queue_len(), 1);

        // Synchronized again, the next child commit is cached for the parent.
        subsurface_request(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61), 4);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert!(child.rref(&state).cached_update.is_some());

        // The parent's update carries the child's second commit, which must wait
        // for the first one.
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(parent.rref(&state).test_queue_len(), 1);
        assert_eq!(parent.rref(&state).test_applied_update_seq(), 0);

        child.grab(&mut state).test_release_head();
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(parent.rref(&state).test_queue_len(), 0);
        assert_eq!(parent.rref(&state).test_applied_update_seq(), 1);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 2);
    }

    #[test]
    fn child_own_update_waits_for_the_parent_update_that_carries_its_earlier_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        // The child's first commit is cached; the parent's held update carries it.
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        parent.grab(&mut state).test_hold_next_commit();
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(parent.rref(&state).test_queue_len(), 1);
        assert!(child.rref(&state).cached_update.is_none());

        // Desynchronized, the child's next commit is its own update, which waits
        // for the parent's update to apply the earlier one.
        subsurface_request(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61), 5);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_queue_len(), 1);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 0);

        parent.grab(&mut state).test_release_head();
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(parent.rref(&state).test_applied_update_seq(), 1);
        assert_eq!(child.rref(&state).test_queue_len(), 0);
        assert_eq!(child.rref(&state).test_applied_update_seq(), 2);
    }

    #[test]
    fn queue_limit_disconnects_with_no_memory() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let limit = super::commit_queue::MAX_QUEUED_UPDATES_PER_CLIENT;
        // Held commits without a buffer queue behind one another.
        for _ in 0..limit {
            surface.grab(&mut state).test_hold_next_commit();
            surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        }
        assert_eq!(state.commit_queues.depth(client.client_id()), limit);

        surface.grab(&mut state).test_hold_next_commit();
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(state.commit_queues.depth(client.client_id()), limit);
        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::Close { id } if *id == client.client_id())
        }));
    }

    #[test]
    fn destroying_a_surface_discards_its_queued_updates() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 2);

        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 0, &[]);
        assert!(buffer_release_was_sent(&dispatcher, 20));
        assert!(buffer_release_was_sent(&dispatcher, 21));
        assert_eq!(state.commit_queues.depth(client.client_id()), 0);
    }

    #[test]
    fn destroying_a_parent_unblocks_the_childs_own_queue() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        parent.grab(&mut state).test_hold_next_commit();
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        subsurface_request(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61), 5);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_queue_len(), 1);

        // The parent goes away: its held update and the child's queued commit are
        // discarded, and the child no longer waits for them.
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 0, &[]);
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(child.rref(&state).test_queue_len(), 0);
        assert!(!state.commit_queues.has_queued(client.client_id()));

        // As a root, the child's next commit applies at once.
        insert_request_buffer(&mut client, &mut state, 22);
        let applied_before = child.rref(&state).test_applied_update_seq();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[22, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert_eq!(child.rref(&state).test_queue_len(), 0);
        assert_eq!(child.rref(&state).test_applied_update_seq(), applied_before + 1);
        assert!(child.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn role_dismissal_drops_queued_updates() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        insert_request_buffer(&mut client, &mut state, 21);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 2);

        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        surface.grab(&mut state).force_unmap_for_role_dismissal(
            &mut client,
            &mut state,
            &mut client_dispatcher,
        );
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert!(!surface.rref(&state).test_latest_buffer_is_some());
        assert_eq!(state.commit_queues.depth(client.client_id()), 0);
        assert!(buffer_release_was_sent(&dispatcher, 20));
        assert!(buffer_release_was_sent(&dispatcher, 21));
    }

    #[test]
    fn client_close_clears_the_index() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(state.commit_queues.depth(client.client_id()), 1);

        client
            .process(&mut state, ClientAction::Close, &mut dispatcher)
            .unwrap();
        assert_eq!(state.commit_queues.depth(client.client_id()), 0);
        assert!(!state.commit_queues.has_queued(client.client_id()));
    }

    #[test]
    fn destroyed_buffer_in_a_queued_update_still_shows_its_pixels() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);

        // wl_buffer.destroy while the commit that attaches it waits.
        buffer_request(&mut client, &mut state, &mut dispatcher, WlBufferId::from(20), 0);
        surface.grab(&mut state).test_release_head();
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        assert!(surface.rref(&state).on_screen.buffer.is_some());
        assert!(!buffer_release_was_sent(&dispatcher, 20));
    }

    #[test]
    fn role_destroyed_while_queued_skips_the_xdg_part() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(50),
            XdgSurfaceId::from(60),
            XdgToplevelId::from(70),
        );
        let surface = root(&state, surface_object_id);
        let mut update =
            initial_xdg_update(&client, surface_object_id, XdgSurfaceId::from(60), &state);
        update.test_hold = true;
        {
            let mut grabbed = state.objects.grab_object::<WlSurface>(surface_object_id);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, update);
        }
        {
            let mut xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(client.object_id(XdgSurfaceId::from(60)));
            let toplevel = Resolved::new(
                client.client_id(),
                XdgToplevelId::from(70),
                client.object_id(XdgToplevelId::from(70)),
            );
            xdg_surface.destroy_toplevel_role(&mut state, toplevel);
        }
        surface.grab(&mut state).test_release_head();
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);

        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
    }

    /// A pipe whose read end is an explicit-sync acquire point: it is readable
    /// once something is written to the write end.
    fn acquire_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) }, 0);
        unsafe {
            (std::os::fd::OwnedFd::from_raw_fd(fds[0]), std::os::fd::OwnedFd::from_raw_fd(fds[1]))
        }
    }

    /// Signals the acquire point whose write end is `signal`.
    fn signal_acquire(signal: &std::os::fd::OwnedFd) {
        use std::os::fd::AsRawFd;
        let byte = 1u8;
        let written =
            unsafe { libc::write(signal.as_raw_fd(), std::ptr::addr_of!(byte).cast(), 1) };
        assert_eq!(written, 1);
    }

    /// A captured update that shows buffer `buffer_id` once `acquire` signals.
    fn acquire_gated_update(
        client: &Client,
        state: &mut State,
        buffer_id: u32,
        seq: u64,
        acquire: std::os::fd::OwnedFd,
    ) -> ContentUpdate {
        let object =
            insert_test_shm_buffer(state, client.client_id(), WlBufferId::from(buffer_id), 2, 2);
        let buffer = crate::buffer_content::AttachedBuffer::resolve(
            state,
            Resolved::new(client.client_id(), WlBufferId::from(buffer_id), object),
        )
        .expect("test buffer is live");
        let mut payload = WlSurface::test_payload(Some(Some(buffer)));
        payload.sync.acquire = crate::sync::AcquireSync::from_sync_file(acquire);
        ContentUpdate::new(seq, 0, payload, Vec::new(), Vec::new())
    }

    #[test]
    fn readiness_waits_for_an_unsignaled_acquire_point() {
        let mut state = state_for_tests();
        let (mut client, _dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (read, write) = acquire_pipe();
        let update = acquire_gated_update(&client, &mut state, 90, 1, read);
        assert!(matches!(
            super::commit_queue::readiness(&state, surface, 0, &update, false, None),
            super::commit_queue::Readiness::WaitAcquire { .. }
        ));
        signal_acquire(&write);
        assert!(matches!(
            super::commit_queue::readiness(&state, surface, 0, &update, false, None),
            super::commit_queue::Readiness::Ready
        ));
    }

    #[test]
    fn readiness_waits_for_a_carried_child_acquire_point() {
        let mut state = state_for_tests();
        let (mut client, _dispatcher) = test_client_and_dispatcher(9);
        let parent = insert_plain_root(&mut client, &mut state, 1);
        let child = insert_plain_root(&mut client, &mut state, 2);
        let (read, write) = acquire_pipe();
        let child_update = acquire_gated_update(&client, &mut state, 90, 1, read);
        let mut parent_update =
            ContentUpdate::test_new(WlSurface::test_payload(None), Vec::new(), Vec::new());
        parent_update.children.push(
            crate::wayland_objects::wl_surface::content_update::ChildUpdate {
                surface: child,
                update: Box::new(child_update),
            },
        );
        assert!(matches!(
            super::commit_queue::readiness(&state, parent, 0, &parent_update, false, None),
            super::commit_queue::Readiness::WaitAcquire { .. }
        ));
        signal_acquire(&write);
        assert!(matches!(
            super::commit_queue::readiness(&state, parent, 0, &parent_update, false, None),
            super::commit_queue::Readiness::Ready
        ));
    }

    #[test]
    fn forced_update_ignores_its_acquire_point() {
        let mut state = state_for_tests();
        let (mut client, _dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (read, _write) = acquire_pipe();
        let mut update = acquire_gated_update(&client, &mut state, 90, 1, read);
        assert!(matches!(
            super::commit_queue::readiness(&state, surface, 0, &update, true, None),
            super::commit_queue::Readiness::Ready
        ));
        update.acquire_forced = true;
        assert!(matches!(
            super::commit_queue::readiness(&state, surface, 0, &update, false, None),
            super::commit_queue::Readiness::Ready
        ));
    }

    #[test]
    fn sync_file_update_waits_without_a_waiter_and_applies_once_signaled() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (read, write) = acquire_pipe();
        let update = acquire_gated_update(&client, &mut state, 90, 1, read);
        {
            let mut grabbed = surface.grab(&mut state);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, update);
        }
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert!(surface.rref(&state).acquire_waiter.is_none());

        signal_acquire(&write);
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        assert!(surface.rref(&state).on_screen.buffer.is_some());
    }

    #[test]
    fn waiter_setup_error_forces_the_head() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        surface.grab(&mut state).test_force_waiter_error = true;
        let (read, _write) = acquire_pipe();
        let update = acquire_gated_update(&client, &mut state, 90, 1, read);
        {
            let mut grabbed = surface.grab(&mut state);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, update);
        }
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
    }

    #[test]
    fn acquire_wait_failed_forces_the_head() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (read, _write) = acquire_pipe();
        let update = acquire_gated_update(&client, &mut state, 90, 1, read);
        let waiter = crate::sync::AcquireReadyWaiterId::from(77u64);
        {
            let mut grabbed = surface.grab(&mut state);
            grabbed.acquire_waiter = Some(waiter);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, update);
        }
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);

        client
            .process(
                &mut state,
                ClientAction::AcquireWaitDone { surface, waiter_id: waiter, failed: true },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
    }

    #[test]
    fn acquire_wait_done_with_another_waiter_id_is_ignored() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (read, _write) = acquire_pipe();
        let update = acquire_gated_update(&client, &mut state, 90, 1, read);
        let waiter = crate::sync::AcquireReadyWaiterId::from(77u64);
        {
            let mut grabbed = surface.grab(&mut state);
            grabbed.acquire_waiter = Some(waiter);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, update);
        }
        client
            .process(
                &mut state,
                ClientAction::AcquireWaitDone {
                    surface,
                    waiter_id: crate::sync::AcquireReadyWaiterId::from(78u64),
                    failed: true,
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);
        assert_eq!(surface.rref(&state).acquire_waiter, Some(waiter));
    }

    #[test]
    fn queued_explicit_sync_updates_are_not_dropped() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let (first_read, first_write) = acquire_pipe();
        let (second_read, second_write) = acquire_pipe();
        let first = acquire_gated_update(&client, &mut state, 90, 1, first_read);
        let second = acquire_gated_update(&client, &mut state, 91, 2, second_read);
        {
            let mut grabbed = surface.grab(&mut state);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, first);
        }
        {
            let mut grabbed = surface.grab(&mut state);
            WlSurface::test_enqueue(&mut grabbed, &mut state, surface, second);
        }
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 2);

        // The second point signals first, but the head still waits.
        signal_acquire(&second_write);
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 2);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);

        signal_acquire(&first_write);
        flush_queued_commits(&mut client, &mut state, &mut dispatcher);
        assert_eq!(surface.rref(&state).test_queue_len(), 0);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 2);
        let retired = surface.grab(&mut state).take_retired_buffers();
        assert!(retired
            .iter()
            .any(|c| c.buffer.wayland_id() == WlBufferId::from(90)));
    }

    /// Sends one request with `u32` arguments to `object` as a client would.
    fn object_request<K>(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        object: K,
        opcode: u16,
        args: &[u32],
    ) where
        K: crate::object::IdToObject + Into<crate::protocol_generated::WaylandId> + Copy,
    {
        let mut event = RawEvent::new(object.into(), opcode, args.len());
        for arg in args {
            event.uint(*arg);
        }
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(object.into());
        let mut grabbed = client.grab_object(object, state);
        grabbed
            .on(client, state, raw_request_from_event(event), &mut client_dispatcher)
            .unwrap();
    }

    /// Lets the held head of `surface` become ready, then flushes the client.
    fn release_held_head(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: Resolved<WlSurfaceId>,
    ) {
        surface.grab(state).test_release_head();
        flush_queued_commits(client, state, dispatcher);
    }

    #[test]
    fn carried_update_of_a_destroyed_subsurface_role_is_discarded() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let (parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
        insert_request_buffer(&mut client, &mut state, 20);
        parent.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        // The child's commit is cached; the held parent commit carries it.
        commit_surface(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(parent.rref(&state).test_queue_len(), 1);
        assert!(!buffer_release_was_sent(&dispatcher, 20));

        destroy_subsurface(&mut client, &mut state, &mut dispatcher, WlSubsurfaceId::from(61));
        assert!(child.exists(&state));
        assert!(!buffer_release_was_sent(&dispatcher, 20));

        release_held_head(&mut client, &mut state, &mut dispatcher, parent);
        assert!(buffer_release_was_sent(&dispatcher, 20));
        assert!(child.rref(&state).on_screen.buffer.is_none());
        assert!(!child.rref(&state).test_latest_buffer_is_some());

        // The child is a root now: its next commit applies at once.
        insert_request_buffer(&mut client, &mut state, 21);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(2),
            1,
            &[21, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(2), 6, &[]);
        assert!(child.rref(&state).on_screen.buffer.is_some());
        assert_eq!(child.rref(&state).test_applied_update_seq(), 2);
    }

    #[test]
    fn queued_cursor_offset_applies_with_its_update() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface.grab(&mut state).pending.offset = (3, 5);
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert_eq!((surface.rref(&state).on_screen.x, surface.rref(&state).on_screen.y), (0, 0));

        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!((surface.rref(&state).on_screen.x, surface.rref(&state).on_screen.y), (3, 5));
    }

    #[test]
    fn queued_xwayland_serial_applies_once_with_its_update() {
        let mut xwayland = XwaylandCommit::new();
        xwayland
            .surface
            .grab(&mut xwayland.state)
            .test_hold_next_commit();
        xwayland.set_serial(7, 0);
        xwayland.commit();
        assert_eq!(xwayland.surface.rref(&xwayland.state).test_queue_len(), 1);
        assert!(xwayland
            .surface
            .rref(&xwayland.state)
            .xwayland_serial()
            .is_none());
        assert_eq!(xwayland.surface_events(), 0);

        xwayland
            .surface
            .grab(&mut xwayland.state)
            .test_release_head();
        xwayland
            .client
            .process(&mut xwayland.state, ClientAction::FlushCommitQueues, &mut xwayland.dispatcher)
            .unwrap();
        assert_eq!(xwayland.surface.rref(&xwayland.state).xwayland_serial(), Some(7));
        assert_eq!(xwayland.surface_events(), 1);

        // Nothing is queued any more, so a second flush does not assign it again.
        xwayland
            .client
            .process(&mut xwayland.state, ClientAction::FlushCommitQueues, &mut xwayland.dispatcher)
            .unwrap();
        assert_eq!(xwayland.surface_events(), 1);
    }

    #[test]
    fn queued_pointer_constraint_region_survives_unless_the_constraint_changes() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(3), 6),
        );
        state.register_root_surface(surface);
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        let (lock_a, _) = client.insert_object(
            &mut state,
            ZwpLockedPointerV1::new(
                ZwpLockedPointerV1Id::from(9),
                surface,
                pointer,
                ConstraintLifetime::Persistent,
                None,
            ),
        );
        let (lock_b, _) = client.insert_object(
            &mut state,
            ZwpLockedPointerV1::new(
                ZwpLockedPointerV1Id::from(10),
                surface,
                pointer,
                ConstraintLifetime::Persistent,
                None,
            ),
        );

        // Held for lock A: the region is taken at capture and applied at release.
        surface.grab(&mut state).test_hold_next_commit();
        surface
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock_a));
        surface
            .grab(&mut state)
            .set_pending_pointer_constraint_region(Some(region_value(1, 1, 5, 5)));
        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        assert!(lock_a.try_ref(&state).unwrap().region().is_none());
        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert!(lock_a.try_ref(&state).unwrap().region().is_some());

        // Held for lock A, but the surface switches to lock B before release.
        surface.grab(&mut state).test_hold_next_commit();
        surface
            .grab(&mut state)
            .set_pending_pointer_constraint_region(Some(region_value(2, 2, 3, 3)));
        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        surface
            .grab(&mut state)
            .set_pointer_constraint(SurfacePointerConstraint::Lock(lock_b));
        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert!(lock_b.try_ref(&state).unwrap().region().is_none());
    }

    #[test]
    fn queued_lock_surface_commit_marks_mapped_on_apply() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let lock =
            Resolved::new(client.client_id(), ExtSessionLockV1Id::from(3), ObjectId::from(3usize));
        let (lock_surface, _) = client.insert_object(
            &mut state,
            ExtSessionLockSurfaceV1::new(
                ExtSessionLockSurfaceV1Id::from(40),
                lock,
                surface,
                OutputId::from(0usize),
            ),
        );
        surface.grab(&mut state).set_lock_surface(lock_surface);
        assert!(surface.grab(&mut state).assign_lock_surface_role());
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            lock_surface
                .grab(&mut state)
                .configure(1, 2, 2, &mut client_dispatcher);
        }
        object_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            ExtSessionLockSurfaceV1Id::from(40),
            1,
            &[1],
        );
        assert!(!lock_surface.rref(&state).is_mapped());

        surface.grab(&mut state).test_hold_next_commit();
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert!(!lock_surface.rref(&state).is_mapped());

        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert!(lock_surface.rref(&state).is_mapped());
    }

    #[test]
    fn queued_layer_surface_commit_maps_on_apply() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        let manager = Resolved::new(
            client.client_id(),
            ZwlrLayerShellV1Id::from(31),
            ObjectId::from(31usize),
        );
        let (layer, _) = client.insert_object(
            &mut state,
            ZwlrLayerSurfaceV1::new(
                ZwlrLayerSurfaceV1Id::from(30),
                manager,
                5,
                surface,
                None,
                LayerShellLayer::Top,
                "panel".to_string(),
            ),
        );
        surface.grab(&mut state).set_layer_surface_impl(layer);
        state.register_layer_surface_object(layer);
        object_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            ZwlrLayerSurfaceV1Id::from(30),
            0,
            &[2, 2],
        );
        // Initial commit without a buffer.
        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        // Layout then sends the first configure, now that the initial commit is applied.
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            crate::wayland_objects::zwlr_layer_surface_v1::relayout_client_layer_surfaces(
                &mut client,
                &mut state,
                &mut client_dispatcher,
            );
        }
        object_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            ZwlrLayerSurfaceV1Id::from(30),
            6,
            &[1],
        );
        assert!(!layer.rref(&state).is_mapped());

        surface.grab(&mut state).test_hold_next_commit();
        insert_request_buffer(&mut client, &mut state, 20);
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert!(!layer.rref(&state).is_mapped());

        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert!(layer.rref(&state).is_mapped());
    }

    #[test]
    fn queued_xdg_initial_commit_sends_the_configure_on_apply() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let _output_id =
            state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 1280, 720));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface_object_id = create_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut dispatcher,
            WlSurfaceId::from(50),
            XdgSurfaceId::from(60),
            XdgToplevelId::from(70),
        );
        let surface = root(&state, surface_object_id);
        let before = pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60));

        surface.grab(&mut state).test_hold_next_commit();
        commit_surface(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert_eq!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)), before);

        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        assert!(pending_toplevel_configures(&state, &client, XdgSurfaceId::from(60)) > before);
        assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
    }

    #[test]
    fn frame_callbacks_of_a_queued_update_fire_after_it_is_shown() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        let (callback, _) =
            client.insert_object(&mut state, WlCallback::new(WlCallbackId::from(30)));
        surface
            .grab(&mut state)
            .pending_frame_callbacks
            .push(callback);
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert!(!done_event_was_sent(&dispatcher, 30));

        release_held_head(&mut client, &mut state, &mut dispatcher, surface);
        // Shown with its buffer: the callback waits for the next frame, it is not completed early.
        assert_eq!(surface.rref(&state).test_frame_callback_count(), 1);
        assert!(!done_event_was_sent(&dispatcher, 30));
    }

    #[test]
    fn presentation_feedback_of_a_destroyed_surface_queue_is_discarded() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = insert_plain_root(&mut client, &mut state, 1);
        insert_request_buffer(&mut client, &mut state, 20);
        let (feedback, _) = client.insert_object(
            &mut state,
            WpPresentationFeedback::new(WpPresentationFeedbackId::from(40), 1),
        );
        surface
            .grab(&mut state)
            .pending_presentation_feedbacks
            .push(SurfacePresentationFeedback { feedback, version: 1, output_id: None });
        surface.grab(&mut state).test_hold_next_commit();
        surface_request(
            &mut client,
            &mut state,
            &mut dispatcher,
            WlSurfaceId::from(1),
            1,
            &[20, 0, 0],
        );
        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 6, &[]);
        assert_eq!(surface.rref(&state).test_queue_len(), 1);
        assert!(!delete_id_was_sent(&dispatcher, 40));

        surface_request(&mut client, &mut state, &mut dispatcher, WlSurfaceId::from(1), 0, &[]);
        assert!(!surface.exists(&state));
        assert!(buffer_release_was_sent(&dispatcher, 20));
        assert!(delete_id_was_sent(&dispatcher, 40));
        assert!(!feedback.exists(&state));
    }

    mod fifo_and_timing {
        use {
            super::*,
            std::time::{Duration, Instant},
        };

        /// Queues `update` on `surface` and flushes the client, as a commit that
        /// arrives behind nothing would.
        fn queue_update_and_flush(
            client: &mut Client,
            state: &mut State,
            dispatcher: &mut Dispatcher,
            surface: Resolved<WlSurfaceId>,
            update: ContentUpdate,
        ) {
            {
                let mut grabbed = surface.grab(state);
                WlSurface::test_enqueue(&mut grabbed, state, surface, update);
            }
            flush_queued_commits(client, state, dispatcher);
        }

        fn plain_update(seq: u64) -> ContentUpdate {
            ContentUpdate::new(seq, 0, WlSurface::test_payload(None), Vec::new(), Vec::new())
        }

        #[test]
        fn set_barrier_in_an_applied_update_creates_a_barrier() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let surface = insert_plain_root(&mut client, &mut state, 1);
            let mut update = plain_update(1);
            update.fifo_set_barrier = true;
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, surface, update);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
            assert!(state.commit_gate.has_barrier(surface));
        }

        #[test]
        fn wait_barrier_without_a_barrier_applies_at_once() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let surface = insert_plain_root(&mut client, &mut state, 1);
            let mut update = plain_update(1);
            update.fifo_wait_barrier = true;
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, surface, update);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
            assert_eq!(surface.rref(&state).test_queue_len(), 0);
        }

        #[test]
        fn wait_barrier_with_a_barrier_queues_until_the_barrier_clears() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let surface = insert_plain_root(&mut client, &mut state, 1);
            let t0 = Instant::now();
            let seq = state
                .commit_gate
                .insert_barrier(surface, t0 + Duration::from_secs(1));
            let mut update = plain_update(1);
            update.fifo_wait_barrier = true;
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, surface, update);
            assert_eq!(surface.rref(&state).test_queue_len(), 1);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);

            assert!(state.commit_gate.remove_barrier(surface, seq));
            flush_queued_commits(&mut client, &mut state, &mut dispatcher);
            assert_eq!(surface.rref(&state).test_queue_len(), 0);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        }

        #[test]
        fn other_surfaces_are_not_held_by_a_fifo_wait() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let held = insert_plain_root(&mut client, &mut state, 1);
            let other = insert_plain_root(&mut client, &mut state, 2);
            let t0 = Instant::now();
            state
                .commit_gate
                .insert_barrier(held, t0 + Duration::from_secs(1));
            let mut update = plain_update(1);
            update.fifo_wait_barrier = true;
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, other, update);
            assert_eq!(other.rref(&state).test_applied_update_seq(), 1);
            assert_eq!(other.rref(&state).test_queue_len(), 0);
        }

        #[test]
        fn wait_barrier_of_a_sync_commit_is_cleared_before_caching() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let (_parent, child) = synchronized_child_of_root_parent(&mut client, &mut state);
            let t0 = Instant::now();
            state
                .commit_gate
                .insert_barrier(child, t0 + Duration::from_secs(1));
            child.grab(&mut state).set_pending_fifo_wait();
            commit_surface(&mut client, &mut state, &mut dispatcher, child);
            // Synchronized: the wait is ignored and cleared before the cache.
            let cached = child.rref(&state).cached_update.as_ref().expect("cached");
            assert!(!cached.fifo_wait_barrier);

            // Desynchronized, the cache applies on its own although the child
            // still holds its barrier.
            subsurface_request(
                &mut client,
                &mut state,
                &mut dispatcher,
                WlSubsurfaceId::from(61),
                5,
            );
            assert_eq!(child.rref(&state).test_applied_update_seq(), 1);
            assert!(child.rref(&state).cached_update.is_none());
        }

        #[test]
        fn set_barrier_on_a_carried_child_sets_the_childs_barrier_on_parent_apply() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let parent = insert_plain_root(&mut client, &mut state, 1);
            let child = insert_plain_root(&mut client, &mut state, 2);
            let mut child_update = plain_update(1);
            child_update.fifo_set_barrier = true;
            let mut parent_update = plain_update(1);
            parent_update.children.push(
                crate::wayland_objects::wl_surface::content_update::ChildUpdate {
                    surface: child,
                    update: Box::new(child_update),
                },
            );
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, parent, parent_update);
            assert_eq!(parent.rref(&state).test_applied_update_seq(), 1);
            assert!(state.commit_gate.has_barrier(child));
        }

        #[test]
        fn future_target_queues_with_a_timed_wait() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let surface = insert_plain_root(&mut client, &mut state, 1);
            let t0 = Instant::now();
            state.commit_gate.test_now = Some(t0);
            let mut update = plain_update(1);
            update.commit_target = Some(t0 + Duration::from_millis(50));
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, surface, update);
            assert_eq!(surface.rref(&state).test_queue_len(), 1);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 0);
            assert_eq!(state.commit_gate.next_deadline(), Some(t0 + Duration::from_millis(50)));

            let due = t0 + Duration::from_millis(50);
            state.commit_gate.test_now = Some(due);
            assert_eq!(state.commit_gate.take_due(due), vec![surface]);
            flush_queued_commits(&mut client, &mut state, &mut dispatcher);
            assert_eq!(surface.rref(&state).test_queue_len(), 0);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
        }

        #[test]
        fn past_target_applies_at_once() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let surface = insert_plain_root(&mut client, &mut state, 1);
            let t0 = Instant::now();
            state.commit_gate.test_now = Some(t0);
            let mut update = plain_update(1);
            update.commit_target = Some(t0.checked_sub(Duration::from_millis(1)).unwrap());
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, surface, update);
            assert_eq!(surface.rref(&state).test_applied_update_seq(), 1);
            assert_eq!(surface.rref(&state).test_queue_len(), 0);
            assert!(state.commit_gate.next_deadline().is_none());
        }

        #[test]
        fn child_target_holds_the_parent_update() {
            let mut state = state_for_tests();
            let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
            let parent = insert_plain_root(&mut client, &mut state, 1);
            let child = insert_plain_root(&mut client, &mut state, 2);
            let t0 = Instant::now();
            state.commit_gate.test_now = Some(t0);
            let mut child_update = plain_update(1);
            child_update.commit_target = Some(t0 + Duration::from_millis(50));
            let mut parent_update = plain_update(1);
            parent_update.children.push(
                crate::wayland_objects::wl_surface::content_update::ChildUpdate {
                    surface: child,
                    update: Box::new(child_update),
                },
            );
            queue_update_and_flush(&mut client, &mut state, &mut dispatcher, parent, parent_update);
            assert_eq!(parent.rref(&state).test_applied_update_seq(), 0);
            assert_eq!(state.commit_gate.next_deadline(), Some(t0 + Duration::from_millis(50)));
        }

        #[test]
        fn merged_targets_keep_the_latest() {
            let t0 = Instant::now();
            let mut update = plain_update(1);
            update.commit_target = Some(t0 + Duration::from_millis(50));
            let mut earlier = plain_update(2);
            earlier.commit_target = Some(t0 + Duration::from_millis(20));
            update.merge(earlier, &mut Vec::new());
            assert_eq!(update.commit_target, Some(t0 + Duration::from_millis(50)));

            update.merge(plain_update(3), &mut Vec::new());
            assert_eq!(update.commit_target, Some(t0 + Duration::from_millis(50)));

            let mut later = plain_update(4);
            later.commit_target = Some(t0 + Duration::from_millis(80));
            update.merge(later, &mut Vec::new());
            assert_eq!(update.commit_target, Some(t0 + Duration::from_millis(80)));
        }
    }
}
