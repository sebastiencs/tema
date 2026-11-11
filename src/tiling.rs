use std::collections::{BTreeMap, BTreeSet};

use crate::{
    clients::Resolved,
    config::{DecorationInsets, SplitRatio},
    protocol_generated::WlSurfaceId,
    region::RectI,
    root_geometry::{self, CommittedRootGeometry},
    state::{ManagedWindowMode, State, WorkspaceId},
    visual_animation::WindowGeometryMotion,
    wayland_objects::{
        wl_surface::{Role, WlSurface},
        zwlr_layer_surface_v1::output_usable_bounds,
    },
    window::{ToplevelDecorationMode, WindowId},
};

pub const EDGE_TOP: u32 = 1;
pub const EDGE_BOTTOM: u32 = 2;
pub const EDGE_LEFT: u32 = 4;
pub const EDGE_RIGHT: u32 = 8;

/// Pointer resizes of tiled windows jump to the new tile.
///
/// `false` plays the `windows` animation for that drag. Keyboard resizes and
/// every other layout change keep their own motion either way. Hyprland's
/// `warpPositionSize()` snaps a mouse resize the same way. The configure pass
/// and button release both relayout after the grab's own warp, so the snap has
/// to be applied there too or the animation replaces it.
pub const SNAP_TILED_RESIZE: bool = true;

const MIN_CHILD_WEIGHT: f64 = 0.1;
const MIN_SPLIT_HANDLE_SPAN: i32 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GapPolicy {
    pub outer_gap: i32,
    pub inner_gap: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutOperation {
    window: WindowId,
    allocation: LayoutAllocation,
}

type DecorationInsetsMap = BTreeMap<Resolved<WlSurfaceId>, DecorationInsets>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutAllocation {
    pub frame_rect: RectI,
    pub tiled_edges: u32,
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TiledEdgeConstraints {
    left: Option<i32>,
    right: Option<i32>,
    top: Option<i32>,
    bottom: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LayoutSizeHint {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutNodeId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

impl Default for SplitAxis {
    fn default() -> Self {
        Self::Horizontal
    }
}

impl SplitAxis {
    fn opposite(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LayoutChild {
    pub node_id: LayoutNodeId,
    pub weight: f64,
}

#[derive(Clone, Debug)]
pub struct ContainerNode {
    pub axis: SplitAxis,
    pub parent: Option<LayoutNodeId>,
    pub children: Vec<LayoutChild>,
    pub focused_child: Option<usize>,
    pub last_applied_rect: Option<RectI>,
}

#[derive(Clone, Debug)]
pub struct LeafNode {
    pub window: WindowId,
    pub parent: Option<LayoutNodeId>,
    pub last_applied_rect: Option<RectI>,
    pub tiled_edges: u32,
}

#[derive(Clone, Debug)]
pub enum LayoutNode {
    Container(ContainerNode),
    Leaf(LeafNode),
}

#[derive(Clone, Debug, Default)]
pub struct FloatingState {
    pub stack: Vec<WindowId>,
}

#[derive(Clone, Debug)]
pub struct FullscreenState {
    pub window: WindowId,
}

/// Boundary between `children[first]` and `children[first + 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitRef {
    pub container: LayoutNodeId,
    pub first: usize,
}

/// One split on each axis. A corner sets both; a single edge sets one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SplitPair {
    pub horizontal: Option<SplitRef>,
    pub vertical: Option<SplitRef>,
}

impl SplitPair {
    pub fn is_empty(self) -> bool {
        self.horizontal.is_none() && self.vertical.is_none()
    }
}

/// A split resize anchored to the two child node ids, not their indices.
///
/// Positive [`WorkspaceLayout::apply_split_drag`] grows `first_id`.
#[derive(Clone, Copy, Debug)]
pub struct SplitDrag {
    split: SplitRef,
    axis: SplitAxis,
    first_id: LayoutNodeId,
    second_id: LayoutNodeId,
    pair_weight: f64,
    initial_first_weight: f64,
    axis_span: i32,
}

/// At most one drag per axis.
#[derive(Clone, Copy, Debug, Default)]
pub struct SplitDragPair {
    pub horizontal: Option<SplitDrag>,
    pub vertical: Option<SplitDrag>,
}

/// Screen strip that grabs one split: both borders, the inner gap, and a
/// minimum width. `center` is the middle of the gap, on the split's main axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitHandle {
    pub split: SplitRef,
    pub axis: SplitAxis,
    pub rect: RectI,
    pub center: f64,
}

#[derive(Clone, Copy, Debug)]
struct SplitHandleParams {
    border: i32,
    grab_extent: i32,
}

impl SplitHandleParams {
    const NONE: Self = Self { border: 0, grab_extent: 0 };

    fn from_state(state: &State) -> Self {
        Self {
            border: state.frame_insets().left,
            grab_extent: state.config.decorations.resize_grab_extent,
        }
    }
}

impl SplitDragPair {
    pub fn is_empty(self) -> bool {
        self.horizontal.is_none() && self.vertical.is_none()
    }

    pub fn iter(self) -> impl Iterator<Item = SplitDrag> {
        self.horizontal.into_iter().chain(self.vertical)
    }
}

/// A tiled window removed from the tree, with the layout needed to put it back.
#[derive(Debug)]
pub struct ExtractedTiledLeaf {
    window: WindowId,
    previous: WorkspaceLayout,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceLayout {
    pub root: Option<LayoutNodeId>,
    pub nodes: BTreeMap<LayoutNodeId, LayoutNode>,
    pub window_leaves: BTreeMap<WindowId, LayoutNodeId>,
    pub floating: FloatingState,
    pub fullscreen: Option<FullscreenState>,
    pub focused_leaf: Option<LayoutNodeId>,
    pub dirty: bool,
    pub split_handles: Vec<SplitHandle>,
    next_node_id: u64,
}

impl WorkspaceLayout {
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn contains_window(&self, window: WindowId) -> bool {
        self.window_leaves.contains_key(&window)
            || self.floating.stack.contains(&window)
            || self
                .fullscreen
                .as_ref()
                .is_some_and(|fullscreen| fullscreen.window == window)
    }

    pub fn is_tiled_window(&self, window: WindowId) -> bool {
        self.window_leaves.contains_key(&window)
    }

    pub fn is_floating_window(&self, window: WindowId) -> bool {
        self.floating.stack.contains(&window)
    }

    pub fn tiled_rect(&self, window: WindowId) -> Option<RectI> {
        let leaf_id = *self.window_leaves.get(&window)?;
        let leaf = self.nodes.get(&leaf_id)?.as_leaf()?;
        leaf.last_applied_rect
    }

    pub fn tiled_edges(&self, window: WindowId) -> Option<u32> {
        let leaf_id = *self.window_leaves.get(&window)?;
        let leaf = self.nodes.get(&leaf_id)?.as_leaf()?;
        Some(leaf.tiled_edges)
    }

    pub fn tiled_windows(&self) -> Vec<WindowId> {
        let mut order = Vec::new();
        if let Some(root_id) = self.root {
            self.collect_leaf_windows(root_id, &mut order);
        }
        order
    }

    pub fn floating_windows(&self) -> Vec<WindowId> {
        self.floating.stack.clone()
    }

    pub fn focus_window(&mut self, window: WindowId) -> bool {
        if let Some(&leaf_id) = self.window_leaves.get(&window) {
            self.focused_leaf = Some(leaf_id);
            self.update_focus_chain(leaf_id);
            return true;
        }
        if self.is_floating_window(window) {
            self.raise_floating_window(window);
            return true;
        }
        false
    }

    pub fn raise_floating_window(&mut self, window: WindowId) -> bool {
        let Some(index) = self
            .floating
            .stack
            .iter()
            .position(|candidate| *candidate == window)
        else {
            return false;
        };
        let window = self.floating.stack.remove(index);
        self.floating.stack.push(window);
        true
    }

    /// Move tiled windows through the existing split slots.
    ///
    /// The tree shape stays, so a focus cycle can change which window is backmost
    /// without rebuilding containers. Rects are applied on the next layout pass.
    pub fn reorder_tiled_windows(&mut self, order: &[WindowId]) -> bool {
        let current = self.tiled_windows();
        if !same_id_set(&current, order) {
            return false;
        }
        if current == order {
            return true;
        }
        let leaf_ids = current
            .iter()
            .map(|surface| self.window_leaves[surface])
            .collect::<Vec<_>>();
        for (leaf_id, surface) in leaf_ids.iter().copied().zip(order.iter().copied()) {
            let Some(LayoutNode::Leaf(leaf)) = self.nodes.get_mut(&leaf_id) else {
                return false;
            };
            leaf.window = surface;
        }
        self.window_leaves.clear();
        for (leaf_id, surface) in leaf_ids.iter().copied().zip(order.iter().copied()) {
            self.window_leaves.insert(surface, leaf_id);
        }
        self.mark_dirty();
        true
    }

    pub fn set_floating_windows(&mut self, order: Vec<WindowId>) -> bool {
        if !same_id_set(&self.floating.stack, &order) {
            return false;
        }
        self.floating.stack = order;
        true
    }

    pub fn insert_tiled(&mut self, window: WindowId) -> bool {
        if self.contains_window(window) {
            return false;
        }

        let leaf_id = self.alloc_leaf(window, None);
        let root_id = match self
            .focused_leaf
            .filter(|leaf_id| self.nodes.contains_key(leaf_id))
        {
            Some(focused_leaf_id) => self.insert_leaf_adjacent_to_focus(focused_leaf_id, leaf_id),
            None => match self.root {
                None => leaf_id,
                Some(root_id) => {
                    let axis = self
                        .nodes
                        .get(&root_id)
                        .and_then(LayoutNode::as_container)
                        .map(|container| container.axis)
                        .unwrap_or_default();
                    self.insert_leaf_under_root(root_id, leaf_id, axis)
                }
            },
        };

        self.root = Some(root_id);
        self.focus_window(window);
        self.mark_dirty();
        true
    }

    /// Removes a tiled leaf and collapses a parent that would be left with one child.
    ///
    /// The sole tiled window is left in place. The returned value can restore the
    /// previous tree, including focus and weights.
    pub fn extract_tiled_leaf(&mut self, window: WindowId) -> Option<ExtractedTiledLeaf> {
        if !self.is_tiled_window(window) || self.tiled_windows().len() <= 1 {
            return None;
        }
        let previous = self.clone();
        if !self.remove_window(window) {
            return None;
        }
        Some(ExtractedTiledLeaf { window, previous })
    }

    pub fn restore_extracted_tiled_leaf(&mut self, extracted: ExtractedTiledLeaf) {
        *self = extracted.previous;
    }

    /// Clones the layout when `window` is one of several tiled windows.
    ///
    /// The window stays in the tree. A sole tiled window has nothing to restore.
    pub fn capture_tiled_drag_origin(&self, window: WindowId) -> Option<ExtractedTiledLeaf> {
        if !self.is_tiled_window(window) || self.tiled_windows().len() <= 1 {
            return None;
        }
        Some(ExtractedTiledLeaf { window, previous: self.clone() })
    }

    /// Splits the tiled leaf under the pointer and inserts `window` beside it.
    ///
    /// The dragged window must already be absent from the tree. A point inside a tiled
    /// rect selects that leaf. Empty space selects the nearest remaining tiled rect.
    /// Floating windows are not destinations.
    pub fn place_dragged_tiled_leaf(
        &mut self,
        window: WindowId,
        pointer_x: f64,
        pointer_y: f64,
        precise_mouse_move: bool,
        split_ratio: f32,
    ) -> bool {
        let Some(destination) = self.tiled_leaf_at_point(pointer_x, pointer_y) else {
            return false;
        };
        let Some(rect) = self.tiled_rect(destination) else {
            return false;
        };
        let (axis, dragged_is_first) =
            tiled_drop_side(rect, pointer_x, pointer_y, precise_mouse_move);
        self.split_leaf_for_drag(destination, window, axis, dragged_is_first, split_ratio)
    }

    /// Splits the tiled leaf under `pointer` and inserts the extracted window beside it.
    ///
    /// On failure the extracted window is returned so the caller can restore it.
    pub fn drop_extracted_tiled_leaf(
        &mut self,
        extracted: ExtractedTiledLeaf,
        pointer_x: f64,
        pointer_y: f64,
        precise_mouse_move: bool,
        split_ratio: f32,
    ) -> Result<(), ExtractedTiledLeaf> {
        if self.place_dragged_tiled_leaf(
            extracted.window,
            pointer_x,
            pointer_y,
            precise_mouse_move,
            split_ratio,
        ) {
            Ok(())
        } else {
            Err(extracted)
        }
    }

    fn tiled_leaf_at_point(&self, pointer_x: f64, pointer_y: f64) -> Option<WindowId> {
        let mut containing = None;
        let mut nearest: Option<(f64, WindowId)> = None;
        for window in self.tiled_windows() {
            let Some(rect) = self.tiled_rect(window) else {
                continue;
            };
            if rect.width <= 0 || rect.height <= 0 {
                continue;
            }
            if rect_contains_point(rect, pointer_x, pointer_y) {
                containing = Some(window);
                break;
            }
            let distance = distance_squared_to_rect(rect, pointer_x, pointer_y);
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, window));
            }
        }
        containing.or_else(|| nearest.map(|(_, window)| window))
    }

    fn split_leaf_for_drag(
        &mut self,
        destination: WindowId,
        dragged: WindowId,
        axis: SplitAxis,
        dragged_is_first: bool,
        split_ratio: f32,
    ) -> bool {
        if self.window_leaves.contains_key(&dragged) {
            return false;
        }
        let Some(&existing_leaf_id) = self.window_leaves.get(&destination) else {
            return false;
        };
        if !matches!(self.nodes.get(&existing_leaf_id), Some(LayoutNode::Leaf(_))) {
            return false;
        }
        let parent_id = self
            .nodes
            .get(&existing_leaf_id)
            .and_then(LayoutNode::parent);
        if parent_id.is_none() && self.root != Some(existing_leaf_id) {
            return false;
        }

        let dragged_leaf_id = self.alloc_leaf(dragged, None);
        let (first_weight, second_weight) = dwindle_child_weights(split_ratio, dragged_is_first);
        let (first_id, second_id) = if dragged_is_first {
            (dragged_leaf_id, existing_leaf_id)
        } else {
            (existing_leaf_id, dragged_leaf_id)
        };
        let container_id = self.alloc_container(
            axis,
            parent_id,
            vec![
                LayoutChild { node_id: first_id, weight: first_weight },
                LayoutChild { node_id: second_id, weight: second_weight },
            ],
        );
        for node_id in [first_id, second_id] {
            if let Some(node) = self.nodes.get_mut(&node_id) {
                node.set_parent(Some(container_id));
            }
        }
        if let Some(parent_id) = parent_id {
            let LayoutNode::Container(parent) = self
                .nodes
                .get_mut(&parent_id)
                .expect("tiled leaf parent must exist")
            else {
                panic!("tiled leaf parent must be a container");
            };
            let index = parent
                .children
                .iter()
                .position(|child| child.node_id == existing_leaf_id)
                .expect("tiled leaf must be a child of its parent");
            parent.children[index].node_id = container_id;
        } else {
            self.root = Some(container_id);
        }
        self.focus_window(dragged);
        self.mark_dirty();
        true
    }

    pub fn remove_window(&mut self, window: WindowId) -> bool {
        let mut removed = false;
        if let Some(index) = self
            .floating
            .stack
            .iter()
            .position(|candidate| *candidate == window)
        {
            self.floating.stack.remove(index);
            removed = true;
        }

        if self
            .fullscreen
            .as_ref()
            .is_some_and(|fullscreen| fullscreen.window == window)
        {
            self.fullscreen = None;
            removed = true;
        }

        if let Some(leaf_id) = self.window_leaves.remove(&window) {
            self.remove_leaf(leaf_id);
            removed = true;
        }

        if removed {
            self.mark_dirty();
        }
        removed
    }

    pub fn set_floating(&mut self, window: WindowId, floating: bool) -> bool {
        if floating {
            if self.is_floating_window(window) {
                return false;
            }
            self.remove_window(window);
            self.floating.stack.push(window);
            self.raise_floating_window(window);
            self.mark_dirty();
            return true;
        }

        let Some(index) = self
            .floating
            .stack
            .iter()
            .position(|candidate| *candidate == window)
        else {
            return false;
        };
        self.floating.stack.remove(index);
        self.insert_tiled(window)
    }

    pub fn set_fullscreen(&mut self, fullscreen_surface: Option<WindowId>) -> bool {
        let next = fullscreen_surface.map(|window| FullscreenState { window });
        let changed = self.fullscreen.as_ref().map(|fullscreen| fullscreen.window)
            != next.as_ref().map(|fullscreen| fullscreen.window);
        if changed {
            self.fullscreen = next;
            self.mark_dirty();
        }
        changed
    }

    /// Keyboard and legacy resize. `dx`/`dy` grow the window's right/bottom
    /// edge and shrink its left/top edge, including through ancestor splits.
    pub fn resize_tiled_window(&mut self, window: WindowId, edges: u32, dx: i32, dy: i32) -> bool {
        let drags = self.begin_split_drags(self.splits_for_window_edges(window, edges));
        self.apply_split_drags(&drags, dx, dy)
    }

    /// Nearest split on one `edge` of the subtree rooted at `node`.
    ///
    /// `EDGE_LEFT` and `EDGE_RIGHT` walk horizontal containers; `EDGE_TOP` and
    /// `EDGE_BOTTOM` walk vertical ones. A container of the right axis with no
    /// neighbour on that side is skipped. Drag-and-drop can nest two containers
    /// of the same axis, and the outer edge of the inner one is the next split.
    pub fn split_for_node_edge(&self, node: LayoutNodeId, edge: u32) -> Option<SplitRef> {
        let (axis, toward_second) = match edge {
            EDGE_LEFT => (SplitAxis::Horizontal, true),
            EDGE_RIGHT => (SplitAxis::Horizontal, false),
            EDGE_TOP => (SplitAxis::Vertical, true),
            EDGE_BOTTOM => (SplitAxis::Vertical, false),
            _ => return None,
        };
        let mut child = node;
        loop {
            let parent_id = self.nodes.get(&child)?.parent()?;
            let container = self.nodes.get(&parent_id)?.as_container()?;
            let index = container
                .children
                .iter()
                .position(|candidate| candidate.node_id == child)?;
            if container.axis == axis {
                if toward_second && index > 0 {
                    return Some(SplitRef { container: parent_id, first: index - 1 });
                }
                if !toward_second && index + 1 < container.children.len() {
                    return Some(SplitRef { container: parent_id, first: index });
                }
            }
            child = parent_id;
        }
    }

    /// Up to one split per axis. Left/top wins when both bits of an axis are set
    /// and that side has a neighbour; otherwise the other bit is tried.
    pub fn splits_for_window_edges(&self, window: WindowId, edges: u32) -> SplitPair {
        let Some(&leaf_id) = self.window_leaves.get(&window) else {
            return SplitPair::default();
        };
        SplitPair {
            horizontal: self.split_for_requested_edge(leaf_id, edges, EDGE_LEFT, EDGE_RIGHT),
            vertical: self.split_for_requested_edge(leaf_id, edges, EDGE_TOP, EDGE_BOTTOM),
        }
    }

    fn split_for_requested_edge(
        &self,
        node: LayoutNodeId,
        edges: u32,
        near_edge: u32,
        far_edge: u32,
    ) -> Option<SplitRef> {
        if edges & near_edge != 0 {
            if let Some(split) = self.split_for_node_edge(node, near_edge) {
                return Some(split);
            }
        }
        if edges & far_edge != 0 {
            self.split_for_node_edge(node, far_edge)
        } else {
            None
        }
    }

    pub fn begin_split_drag(&self, split: SplitRef) -> Option<SplitDrag> {
        let container = self.nodes.get(&split.container)?.as_container()?;
        let first = container.children.get(split.first)?;
        let second = container.children.get(split.first.checked_add(1)?)?;
        let first_rect = self.node_rect(first.node_id)?;
        let second_rect = self.node_rect(second.node_id)?;
        let axis_span = match container.axis {
            SplitAxis::Horizontal => first_rect.width.saturating_add(second_rect.width),
            SplitAxis::Vertical => first_rect.height.saturating_add(second_rect.height),
        };
        if axis_span <= 1 {
            return None;
        }
        Some(SplitDrag {
            split,
            axis: container.axis,
            first_id: first.node_id,
            second_id: second.node_id,
            pair_weight: first.weight + second.weight,
            initial_first_weight: first.weight,
            axis_span,
        })
    }

    pub fn begin_split_drags(&self, splits: SplitPair) -> SplitDragPair {
        SplitDragPair {
            horizontal: splits
                .horizontal
                .and_then(|split| self.begin_split_drag(split)),
            vertical: splits
                .vertical
                .and_then(|split| self.begin_split_drag(split)),
        }
    }

    /// Returns whether a weight changed. `false` is also a missing or
    /// non-adjacent pair: indices are resolved again from the stored node ids.
    pub fn apply_split_drag(&mut self, drag: &SplitDrag, delta_px: i32) -> bool {
        let Some((first_index, second_index)) = self.split_drag_indices(drag) else {
            return false;
        };
        let delta_weight = (delta_px as f64 / drag.axis_span as f64) * drag.pair_weight;
        let updated_first = (drag.initial_first_weight + delta_weight)
            .clamp(MIN_CHILD_WEIGHT, drag.pair_weight - MIN_CHILD_WEIGHT);
        let updated_second = drag.pair_weight - updated_first;
        let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&drag.split.container)
        else {
            return false;
        };
        if container.children[first_index].weight == updated_first
            && container.children[second_index].weight == updated_second
        {
            return false;
        }
        container.children[first_index].weight = updated_first;
        container.children[second_index].weight = updated_second;
        self.mark_dirty();
        true
    }

    pub fn apply_split_drags(&mut self, drags: &SplitDragPair, dx: i32, dy: i32) -> bool {
        let mut changed = false;
        if let Some(drag) = drags.horizontal {
            changed |= self.apply_split_drag(&drag, dx);
        }
        if let Some(drag) = drags.vertical {
            changed |= self.apply_split_drag(&drag, dy);
        }
        changed
    }

    pub fn split_drag_resolves(&self, drag: &SplitDrag) -> bool {
        self.split_drag_indices(drag).is_some()
    }

    pub fn split_drag_affected_windows(&self, drag: &SplitDrag) -> BTreeSet<WindowId> {
        let mut windows = Vec::new();
        self.collect_leaf_windows(drag.first_id, &mut windows);
        self.collect_leaf_windows(drag.second_id, &mut windows);
        windows.into_iter().collect()
    }

    pub fn split_drags_affected_windows(&self, drags: &SplitDragPair) -> BTreeSet<WindowId> {
        let mut windows = BTreeSet::new();
        for drag in drags.iter() {
            windows.extend(self.split_drag_affected_windows(&drag));
        }
        windows
    }

    fn split_drag_indices(&self, drag: &SplitDrag) -> Option<(usize, usize)> {
        let container = self.nodes.get(&drag.split.container)?.as_container()?;
        if container.axis != drag.axis {
            return None;
        }
        let first = container
            .children
            .iter()
            .position(|child| child.node_id == drag.first_id)?;
        let second = container
            .children
            .iter()
            .position(|child| child.node_id == drag.second_id)?;
        (second == first + 1).then_some((first, second))
    }

    fn alloc_leaf(&mut self, window: WindowId, parent: Option<LayoutNodeId>) -> LayoutNodeId {
        let id = self.alloc_node_id();
        self.nodes.insert(
            id,
            LayoutNode::Leaf(LeafNode { window, parent, last_applied_rect: None, tiled_edges: 0 }),
        );
        self.window_leaves.insert(window, id);
        id
    }

    fn alloc_container(
        &mut self,
        axis: SplitAxis,
        parent: Option<LayoutNodeId>,
        children: Vec<LayoutChild>,
    ) -> LayoutNodeId {
        let id = self.alloc_node_id();
        self.nodes.insert(
            id,
            LayoutNode::Container(ContainerNode {
                axis,
                parent,
                focused_child: children.len().checked_sub(1),
                children,
                last_applied_rect: None,
            }),
        );
        id
    }

    fn alloc_node_id(&mut self) -> LayoutNodeId {
        let id = LayoutNodeId(self.next_node_id);
        self.next_node_id = self.next_node_id.saturating_add(1);
        id
    }

    fn insert_leaf_under_root(
        &mut self,
        root_id: LayoutNodeId,
        leaf_id: LayoutNodeId,
        axis: SplitAxis,
    ) -> LayoutNodeId {
        let root_is_container = matches!(self.nodes.get(&root_id), Some(LayoutNode::Container(_)));
        if root_is_container {
            let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&root_id) else {
                unreachable!();
            };
            container
                .children
                .push(LayoutChild { node_id: leaf_id, weight: 1.0 });
            container.focused_child = Some(container.children.len() - 1);
            if let Some(node) = self.nodes.get_mut(&leaf_id) {
                node.set_parent(Some(root_id));
            }
            return root_id;
        }

        let new_root = self.alloc_container(
            axis,
            None,
            vec![
                LayoutChild { node_id: root_id, weight: 1.0 },
                LayoutChild { node_id: leaf_id, weight: 1.0 },
            ],
        );
        if let Some(node) = self.nodes.get_mut(&root_id) {
            node.set_parent(Some(new_root));
        }
        if let Some(node) = self.nodes.get_mut(&leaf_id) {
            node.set_parent(Some(new_root));
        }
        new_root
    }

    fn insert_leaf_adjacent_to_focus(
        &mut self,
        focused_leaf_id: LayoutNodeId,
        new_leaf_id: LayoutNodeId,
    ) -> LayoutNodeId {
        let desired_axis = self.preferred_insert_axis(focused_leaf_id);
        let parent_id = self
            .nodes
            .get(&focused_leaf_id)
            .and_then(LayoutNode::parent);
        if let Some(parent_id) = parent_id {
            let parent_axis = self
                .nodes
                .get(&parent_id)
                .and_then(LayoutNode::as_container)
                .map(|container| container.axis);
            if parent_axis == Some(desired_axis) {
                self.insert_leaf_into_container_after_focus(
                    parent_id,
                    focused_leaf_id,
                    new_leaf_id,
                );
                return self.root.unwrap_or(parent_id);
            }
            return self.wrap_focused_leaf_with_container(
                focused_leaf_id,
                new_leaf_id,
                Some(parent_id),
                desired_axis,
            );
        }
        self.wrap_focused_leaf_with_container(focused_leaf_id, new_leaf_id, None, desired_axis)
    }

    fn preferred_insert_axis(&self, focused_leaf_id: LayoutNodeId) -> SplitAxis {
        self.nodes
            .get(&focused_leaf_id)
            .and_then(LayoutNode::parent)
            .and_then(|parent_id| {
                self.nodes
                    .get(&parent_id)
                    .and_then(LayoutNode::as_container)
            })
            .map(|container| container.axis.opposite())
            .unwrap_or_default()
    }

    fn insert_leaf_into_container_after_focus(
        &mut self,
        container_id: LayoutNodeId,
        focused_leaf_id: LayoutNodeId,
        new_leaf_id: LayoutNodeId,
    ) {
        let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&container_id) else {
            return;
        };
        let insert_index = container
            .children
            .iter()
            .position(|child| child.node_id == focused_leaf_id)
            .map(|index| index + 1)
            .unwrap_or(container.children.len());
        container
            .children
            .insert(insert_index, LayoutChild { node_id: new_leaf_id, weight: 1.0 });
        container.focused_child = Some(insert_index);
        if let Some(node) = self.nodes.get_mut(&new_leaf_id) {
            node.set_parent(Some(container_id));
        }
    }

    fn wrap_focused_leaf_with_container(
        &mut self,
        focused_leaf_id: LayoutNodeId,
        new_leaf_id: LayoutNodeId,
        parent_id: Option<LayoutNodeId>,
        axis: SplitAxis,
    ) -> LayoutNodeId {
        let new_container_id = self.alloc_container(
            axis,
            parent_id,
            vec![
                LayoutChild { node_id: focused_leaf_id, weight: 1.0 },
                LayoutChild { node_id: new_leaf_id, weight: 1.0 },
            ],
        );
        if let Some(node) = self.nodes.get_mut(&focused_leaf_id) {
            node.set_parent(Some(new_container_id));
        }
        if let Some(node) = self.nodes.get_mut(&new_leaf_id) {
            node.set_parent(Some(new_container_id));
        }
        if let Some(parent_id) = parent_id {
            if let Some(LayoutNode::Container(parent)) = self.nodes.get_mut(&parent_id) {
                if let Some(child_index) = parent
                    .children
                    .iter()
                    .position(|child| child.node_id == focused_leaf_id)
                {
                    parent.children[child_index].node_id = new_container_id;
                    parent.focused_child = Some(child_index);
                }
            }
            self.root
                .expect("root must exist when wrapping a leaf under an existing parent")
        } else {
            new_container_id
        }
    }

    fn remove_leaf(&mut self, leaf_id: LayoutNodeId) {
        let parent_id = self.nodes.get(&leaf_id).and_then(LayoutNode::parent);
        let removed_was_focused = self.focused_leaf == Some(leaf_id);
        self.nodes.remove(&leaf_id);
        if removed_was_focused {
            self.focused_leaf = None;
        }
        let Some(parent_id) = parent_id else {
            if self.root == Some(leaf_id) {
                self.root = None;
            }
            if removed_was_focused {
                self.focused_leaf = self
                    .root
                    .and_then(|root_id| self.first_leaf_in_subtree(root_id));
            }
            return;
        };

        let parent_snapshot = self
            .nodes
            .get(&parent_id)
            .and_then(LayoutNode::as_container)
            .cloned();
        let Some(parent_snapshot) = parent_snapshot else {
            return;
        };
        let remaining_children = parent_snapshot
            .children
            .iter()
            .filter(|child| child.node_id != leaf_id)
            .cloned()
            .collect::<Vec<_>>();
        let replacement_focus = removed_was_focused
            .then(|| self.replacement_focus_child(&parent_snapshot, &remaining_children, leaf_id))
            .flatten();
        if let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&parent_id) {
            container.children = remaining_children;
            container.focused_child = (!container.children.is_empty()).then(|| {
                let focused_child = container.focused_child.unwrap_or(0);
                focused_child.min(container.children.len().saturating_sub(1))
            });
        }
        self.normalize_container(parent_id);
        if let Some(replacement_focus) = replacement_focus {
            self.focused_leaf = self.first_leaf_in_subtree(replacement_focus);
        } else if removed_was_focused {
            self.focused_leaf = self
                .root
                .and_then(|root_id| self.first_leaf_in_subtree(root_id));
        }
    }

    fn normalize_container(&mut self, container_id: LayoutNodeId) {
        let snapshot = self
            .nodes
            .get(&container_id)
            .and_then(LayoutNode::as_container)
            .cloned();
        let Some(snapshot) = snapshot else {
            return;
        };
        match snapshot.children.len() {
            0 => {
                let parent = snapshot.parent;
                self.nodes.remove(&container_id);
                if let Some(parent_id) = parent {
                    if let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&parent_id) {
                        container
                            .children
                            .retain(|child| child.node_id != container_id);
                    }
                    self.normalize_container(parent_id);
                } else if self.root == Some(container_id) {
                    self.root = None;
                }
            }
            1 => {
                let promoted_id = snapshot.children[0].node_id;
                let parent = snapshot.parent;
                self.nodes.remove(&container_id);
                if let Some(node) = self.nodes.get_mut(&promoted_id) {
                    node.set_parent(parent);
                }
                if let Some(parent_id) = parent {
                    if let Some(LayoutNode::Container(container)) = self.nodes.get_mut(&parent_id) {
                        for child in &mut container.children {
                            if child.node_id == container_id {
                                child.node_id = promoted_id;
                            }
                        }
                    }
                    self.normalize_container(parent_id);
                } else {
                    self.root = Some(promoted_id);
                }
            }
            _ => {}
        }
    }

    fn update_focus_chain(&mut self, leaf_id: LayoutNodeId) {
        let mut current = Some(leaf_id);
        while let Some(node_id) = current {
            let parent_id = self.nodes.get(&node_id).and_then(LayoutNode::parent);
            let Some(parent_id) = parent_id else {
                break;
            };
            let child_index = self
                .nodes
                .get(&parent_id)
                .and_then(LayoutNode::as_container)
                .and_then(|container| {
                    container
                        .children
                        .iter()
                        .position(|child| child.node_id == node_id)
                });
            if let (Some(LayoutNode::Container(container)), Some(child_index)) =
                (self.nodes.get_mut(&parent_id), child_index)
            {
                container.focused_child = Some(child_index);
            }
            current = Some(parent_id);
        }
    }

    fn collect_leaf_windows(&self, node_id: LayoutNodeId, output: &mut Vec<WindowId>) {
        let Some(node) = self.nodes.get(&node_id) else {
            return;
        };
        match node {
            LayoutNode::Leaf(leaf) => output.push(leaf.window),
            LayoutNode::Container(container) => {
                for child in &container.children {
                    self.collect_leaf_windows(child.node_id, output);
                }
            }
        }
    }

    fn node_rect(&self, node_id: LayoutNodeId) -> Option<RectI> {
        match self.nodes.get(&node_id)? {
            LayoutNode::Leaf(leaf) => leaf.last_applied_rect,
            LayoutNode::Container(container) => container.last_applied_rect,
        }
    }

    fn first_leaf_in_subtree(&self, node_id: LayoutNodeId) -> Option<LayoutNodeId> {
        match self.nodes.get(&node_id)? {
            LayoutNode::Leaf(_) => Some(node_id),
            LayoutNode::Container(container) => container
                .children
                .iter()
                .find_map(|child| self.first_leaf_in_subtree(child.node_id)),
        }
    }

    fn replacement_focus_child(
        &self,
        parent_snapshot: &ContainerNode,
        remaining_children: &[LayoutChild],
        removed_leaf_id: LayoutNodeId,
    ) -> Option<LayoutNodeId> {
        let removed_index = parent_snapshot
            .children
            .iter()
            .position(|child| child.node_id == removed_leaf_id)?;
        remaining_children
            .get(removed_index)
            .or_else(|| {
                removed_index
                    .checked_sub(1)
                    .and_then(|index| remaining_children.get(index))
            })
            .map(|child| child.node_id)
    }
}

impl LayoutNode {
    fn parent(&self) -> Option<LayoutNodeId> {
        match self {
            LayoutNode::Container(container) => container.parent,
            LayoutNode::Leaf(leaf) => leaf.parent,
        }
    }

    fn set_parent(&mut self, parent: Option<LayoutNodeId>) {
        match self {
            LayoutNode::Container(container) => container.parent = parent,
            LayoutNode::Leaf(leaf) => leaf.parent = parent,
        }
    }

    fn as_container(&self) -> Option<&ContainerNode> {
        match self {
            LayoutNode::Container(container) => Some(container),
            LayoutNode::Leaf(_) => None,
        }
    }

    fn as_leaf(&self) -> Option<&LeafNode> {
        match self {
            LayoutNode::Leaf(leaf) => Some(leaf),
            LayoutNode::Container(_) => None,
        }
    }
}

pub fn initial_root_geometry_for_surface(
    state: &mut State,
    surface: Resolved<WlSurfaceId>,
    surface_decoration_insets: DecorationInsets,
    decoration_mode: ToplevelDecorationMode,
    explicit_window_geometry: Option<RectI>,
    target_min_size: Option<(i32, i32)>,
    configure_bounds: Option<(i32, i32)>,
) -> Option<CommittedRootGeometry> {
    let surface_object_id = surface;
    if !state.prepare_root_surface_initial_layout(surface_object_id) {
        return None;
    }
    let workspace_id = state.root_surface_workspace(surface_object_id)?;
    let workspace_surface_ids = state
        .managed_windows()
        .filter_map(|(surface, managed)| (managed.workspace_id == workspace_id).then_some(surface))
        .collect::<Vec<_>>();
    let candidate_surfaces = workspace_surface_ids
        .iter()
        .copied()
        .filter(|object_id| {
            if *object_id == surface_object_id {
                return state
                    .managed_root_surface_state(*object_id)
                    .is_some_and(|managed| managed.awaiting_initial_layout);
            }
            is_managed_root_surface_live(state, *object_id)
        })
        .collect::<Vec<_>>();
    let candidate_set = candidate_surfaces.iter().copied().collect::<BTreeSet<_>>();
    let candidate_modes = candidate_surfaces
        .iter()
        .filter_map(|object_id| {
            state
                .managed_root_surface_state(*object_id)
                .map(|managed| (*object_id, managed.mode))
        })
        .collect::<BTreeMap<_, _>>();
    let fullscreen_surface = workspace_surface_ids.iter().find_map(|object_id| {
        state
            .managed_root_surface_state(*object_id)
            .is_some_and(|managed| managed.fullscreen && candidate_set.contains(object_id))
            .then_some(*object_id)
    });
    let fullscreen_bounds = workspace_usable_bounds(state, workspace_id).map(|bounds| {
        let (width, height) = configure_bounds.unwrap_or((bounds.width, bounds.height));
        RectI { x: bounds.x, y: bounds.y, width, height }
    })?;
    let tiling_bounds = workspace_tiling_bounds(state, workspace_id, fullscreen_bounds);
    let min_sizes = state
        .managed_windows()
        .filter_map(|(surface, managed)| {
            (managed.workspace_id == workspace_id).then_some((
                surface,
                if surface == surface_object_id {
                    target_min_size.unwrap_or((1, 1))
                } else {
                    minimum_size_for_surface(state, surface)
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let gap_policy = workspace_gap_policy(state, workspace_id);
    let mut decoration_insets = decoration_insets_for_surfaces(
        state,
        workspace_surface_ids
            .iter()
            .copied()
            .filter(|object_id| *object_id != surface_object_id),
    );
    decoration_insets.insert(surface_object_id, surface_decoration_insets);
    let mut predictive_layout = state.workspaces.try_get(workspace_id)?.layout.clone();
    let (candidate_windows, candidate_window_modes, fullscreen_window) =
        layout_candidates(state, &candidate_surfaces, &candidate_modes, fullscreen_surface);
    sync_workspace_layout_candidates(
        &mut predictive_layout,
        &candidate_windows,
        &candidate_window_modes,
        fullscreen_window,
    );
    apply_workspace_layout(
        &mut predictive_layout,
        tiling_bounds,
        fullscreen_bounds,
        gap_policy,
        &keyed_by_window(state, &decoration_insets),
        &keyed_by_window(state, &min_sizes),
        SplitHandleParams::from_state(state),
    );
    let window = state.window_for_root(surface_object_id)?;
    let allocation = predictive_layout
        .fullscreen
        .as_ref()
        .is_some_and(|fullscreen| fullscreen.window == window)
        .then_some(LayoutAllocation {
            frame_rect: fullscreen_bounds,
            tiled_edges: EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
            fullscreen: true,
        })
        .or_else(|| {
            predictive_layout
                .tiled_rect(window)
                .map(|frame_rect| LayoutAllocation {
                    frame_rect,
                    tiled_edges: predictive_layout.tiled_edges(window).unwrap_or_default(),
                    fullscreen: false,
                })
        })?;
    Some(root_geometry::predictive_tiled_geometry(
        allocation,
        decoration_mode,
        explicit_window_geometry,
        surface_decoration_insets,
    ))
}

pub fn layout_allocation_for_surface(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
) -> Option<LayoutAllocation> {
    let window = state.window_for_root(surface_object_id)?;
    let workspace_id = state.root_surface_workspace(surface_object_id)?;
    let workspace = state.workspaces.try_get(workspace_id)?;
    if let Some(frame_rect) = workspace
        .layout
        .fullscreen
        .as_ref()
        .is_some_and(|fullscreen| fullscreen.window == window)
        .then(|| workspace_usable_bounds(state, workspace_id))
        .flatten()
    {
        return Some(LayoutAllocation {
            frame_rect,
            tiled_edges: EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
            fullscreen: true,
        });
    }
    workspace
        .layout
        .tiled_rect(window)
        .map(|frame_rect| LayoutAllocation {
            frame_rect,
            tiled_edges: workspace.layout.tiled_edges(window).unwrap_or_default(),
            fullscreen: false,
        })
}

pub fn window_is_tiled(state: &State, window: WindowId) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    state
        .root_surface_workspace(root)
        .and_then(|workspace_id| state.workspaces.try_get(workspace_id))
        .is_some_and(|workspace| workspace.layout.is_tiled_window(window))
}

pub fn window_is_floating(state: &State, window: WindowId) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    state
        .root_surface_workspace(root)
        .and_then(|workspace_id| state.workspaces.try_get(workspace_id))
        .is_some_and(|workspace| workspace.layout.is_floating_window(window))
}

pub fn focus_window(state: &mut State, window: WindowId) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    let Some(workspace_id) = state.root_surface_workspace(root) else {
        return false;
    };
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return false;
    };
    workspace.layout.focus_window(window)
}

pub fn raise_window(state: &mut State, window: WindowId) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    let Some(workspace_id) = state.root_surface_workspace(root) else {
        return false;
    };
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return false;
    };
    if workspace.layout.is_tiled_window(window) {
        return workspace.layout.focus_window(window);
    }
    if workspace.layout.is_floating_window(window) {
        return workspace.layout.raise_floating_window(window);
    }
    false
}

pub fn set_window_floating(state: &mut State, window: WindowId, floating: bool) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    let Some(workspace_id) = state.root_surface_workspace(root) else {
        return false;
    };
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return false;
    };
    workspace.layout.set_floating(window, floating)
}

pub fn resize_tiled_window(
    state: &mut State,
    window: WindowId,
    edges: u32,
    dx: i32,
    dy: i32,
) -> bool {
    let Some(root) = state.window_root_surface(window) else {
        return false;
    };
    let Some(workspace_id) = state.root_surface_workspace(root) else {
        return false;
    };
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return false;
    };
    workspace.layout.resize_tiled_window(window, edges, dx, dy)
}

pub fn mark_workspace_dirty(state: &mut State, workspace_id: WorkspaceId) {
    if let Some(workspace) = state.workspaces.try_get_mut(workspace_id) {
        workspace.layout.mark_dirty();
    }
}

pub fn relayout_dirty_workspaces(state: &mut State) -> Vec<Resolved<WlSurfaceId>> {
    relayout_dirty_workspaces_with_motion(state, WindowGeometryMotion::Layout)
}

pub fn relayout_dirty_workspaces_with_motion(
    state: &mut State,
    motion: WindowGeometryMotion,
) -> Vec<Resolved<WlSurfaceId>> {
    let snap_windows = tiled_resize_snap_windows(state);
    let workspace_ids = state
        .workspace_ids()
        .into_iter()
        .filter(|workspace_id| {
            state.workspace_is_active(*workspace_id)
                || state
                    .workspaces
                    .try_get(*workspace_id)
                    .is_some_and(|workspace| workspace.layout.dirty)
        })
        .collect::<Vec<_>>();
    let mut opened = Vec::new();
    for workspace_id in workspace_ids.iter().copied() {
        opened.extend(sync_workspace_layout(state, workspace_id));
    }
    for workspace_id in workspace_ids {
        let workspace_surface_ids = state
            .managed_windows()
            .filter_map(|(surface, managed)| {
                (managed.workspace_id == workspace_id).then_some(surface)
            })
            .collect::<Vec<_>>();
        let Some(fullscreen_bounds) = workspace_usable_bounds(state, workspace_id) else {
            continue;
        };
        let tiling_bounds = workspace_tiling_bounds(state, workspace_id, fullscreen_bounds);
        let min_sizes = workspace_minimum_sizes(state, workspace_id);
        let gap_policy = workspace_gap_policy(state, workspace_id);
        let decoration_insets = decoration_insets_for_surfaces(
            state,
            state.managed_windows().filter_map(|(surface, managed)| {
                (managed.workspace_id == workspace_id).then_some(surface)
            }),
        );
        let window_insets = keyed_by_window(state, &decoration_insets);
        let window_min_sizes = keyed_by_window(state, &min_sizes);
        let handle_params = SplitHandleParams::from_state(state);
        let operations = {
            let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
                continue;
            };
            apply_workspace_layout(
                &mut workspace.layout,
                tiling_bounds,
                fullscreen_bounds,
                gap_policy,
                &window_insets,
                &window_min_sizes,
                handle_params,
            )
        };
        for operation in operations {
            let Some(root) = state.window_root_surface(operation.window) else {
                continue;
            };
            let Some(mut surface) = root.try_grab(state) else {
                continue;
            };
            let previous_root_pos = surface.root_position();
            let Some(geometry) = root_geometry::build_committed_root_geometry(
                state,
                root,
                &surface,
                Some(operation.allocation),
                root_geometry::GeometryBuildOverrides {
                    decoration_mode: None,
                    explicit_window_geometry: None,
                    decoration_insets: None,
                    root_pos: Some(previous_root_pos),
                },
            ) else {
                continue;
            };
            surface.set_root_position(geometry.root_pos.0, geometry.root_pos.1);
            drop(surface);
            state.mark_root_geometry_dirty(root);
        }
        for surface in &workspace_surface_ids {
            if !state.is_tiled_root_surface(*surface) && !state.root_surface_is_fullscreen(*surface)
            {
                state.mark_root_geometry_dirty(*surface);
            }
        }
        for surface in workspace_surface_ids {
            if let Some(window) = state.window_for_root(surface) {
                let window_motion = if snap_windows.contains(&window) {
                    WindowGeometryMotion::Warp
                } else {
                    motion
                };
                state.sync_window_geometry_goal(window, window_motion);
            }
        }
    }
    opened
}

/// Tiles whose pointer resize should jump instead of animating.
///
/// The grab covers the drag itself. The seat set covers button release, which
/// clears the grab before the workspace relayout runs.
fn tiled_resize_snap_windows(state: &State) -> BTreeSet<WindowId> {
    if !SNAP_TILED_RESIZE {
        return BTreeSet::new();
    }
    let mut windows = state.seat.tiled_resize_snap_windows.clone();
    windows.extend(active_split_resize_windows(state));
    windows
}

pub(crate) fn active_split_resize_windows(state: &State) -> BTreeSet<WindowId> {
    let Some((workspace_id, drags)) = state.seat.active_tiled_resize() else {
        return BTreeSet::new();
    };
    state
        .workspaces
        .try_get(workspace_id)
        .map(|workspace| workspace.layout.split_drags_affected_windows(&drags))
        .unwrap_or_default()
}

fn workspace_usable_bounds(state: &State, workspace_id: WorkspaceId) -> Option<RectI> {
    let output_id = state.workspace_output_id(workspace_id)?;
    output_usable_bounds(state, output_id).or_else(|| state.output_bounds(output_id))
}

fn workspace_gap_policy(state: &State, _workspace_id: WorkspaceId) -> GapPolicy {
    GapPolicy {
        outer_gap: state.config.general.outer_gap,
        inner_gap: state.config.general.inner_gap,
    }
}

fn same_id_set<T: Copy + Ord>(left: &[T], right: &[T]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

pub fn tiled_scene_order(state: &State, layout: &WorkspaceLayout) -> Vec<Resolved<WlSurfaceId>> {
    layout
        .tiled_windows()
        .into_iter()
        .filter_map(|window| state.window_root_surface(window))
        .collect()
}

pub fn floating_scene_order(state: &State, layout: &WorkspaceLayout) -> Vec<Resolved<WlSurfaceId>> {
    layout
        .floating_windows()
        .into_iter()
        .filter_map(|window| state.window_root_surface(window))
        .collect()
}

fn keyed_by_window<V: Copy>(
    state: &State,
    values: &BTreeMap<Resolved<WlSurfaceId>, V>,
) -> BTreeMap<WindowId, V> {
    values
        .iter()
        .filter_map(|(root, value)| state.window_for_root(*root).map(|window| (window, *value)))
        .collect()
}

fn decoration_insets_for_window(
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    window: WindowId,
) -> DecorationInsets {
    decoration_insets.get(&window).copied().unwrap_or_default()
}

fn decoration_insets_for_surfaces<I>(state: &State, surface_object_ids: I) -> DecorationInsetsMap
where
    I: IntoIterator<Item = Resolved<WlSurfaceId>>,
{
    surface_object_ids
        .into_iter()
        .map(|surface_object_id| {
            (
                surface_object_id,
                root_geometry::effective_decoration_insets(state, surface_object_id),
            )
        })
        .collect()
}

fn workspace_tiling_bounds(
    state: &State,
    workspace_id: WorkspaceId,
    usable_bounds: RectI,
) -> RectI {
    let gap_policy = workspace_gap_policy(state, workspace_id);
    outer_gapped_bounds(usable_bounds, gap_policy)
}

fn sync_workspace_layout(
    state: &mut State,
    workspace_id: WorkspaceId,
) -> Vec<Resolved<WlSurfaceId>> {
    let workspace_surface_ids = state
        .managed_windows()
        .filter_map(|(surface, managed)| (managed.workspace_id == workspace_id).then_some(surface))
        .collect::<Vec<_>>();
    let candidate_surfaces = workspace_surface_ids
        .iter()
        .copied()
        .filter(|object_id| is_managed_root_surface_live(state, *object_id))
        .collect::<Vec<_>>();
    let candidate_set = candidate_surfaces.iter().copied().collect::<BTreeSet<_>>();
    let candidate_modes = candidate_surfaces
        .iter()
        .filter_map(|object_id| {
            state
                .managed_root_surface_state(*object_id)
                .map(|managed| (*object_id, managed.mode))
        })
        .collect::<BTreeMap<_, _>>();
    let open_duration = state.config.animations.opening().duration();
    let fade_in_duration = state.config.animations.fade_in().duration();
    let workspace_active = state.workspace_is_active(workspace_id);
    let mut opened = Vec::new();
    for surface in &workspace_surface_ids {
        let Some(window) = state.window_for_root(*surface) else {
            continue;
        };
        let Some(managed) = state.managed_window_state_mut(window) else {
            continue;
        };
        let was_mapped = managed.mapped;
        let now_mapped = candidate_set.contains(surface);
        managed.mapped = now_mapped;
        if !was_mapped && now_mapped {
            let mut started = false;
            if workspace_active && !fade_in_duration.is_zero() {
                managed.lifecycle_animation.start_opening();
                started = true;
            } else {
                managed.lifecycle_animation.settle_idle();
            }
            if workspace_active && !open_duration.is_zero() {
                managed.geometry_animation.arm_opening_pose();
                started = true;
            }
            if started {
                opened.push(*surface);
            }
        } else if was_mapped && !now_mapped {
            managed.lifecycle_animation.settle_idle();
        }
    }

    let fullscreen_surface = workspace_surface_ids.iter().find_map(|object_id| {
        state
            .managed_root_surface_state(*object_id)
            .is_some_and(|managed| managed.fullscreen && managed.mapped)
            .then_some(*object_id)
    });

    let (candidate_windows, candidate_window_modes, fullscreen_window) =
        layout_candidates(state, &candidate_surfaces, &candidate_modes, fullscreen_surface);
    let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
        return opened;
    };

    sync_workspace_layout_candidates(
        &mut workspace.layout,
        &candidate_windows,
        &candidate_window_modes,
        fullscreen_window,
    );
    opened
}

fn layout_candidates(
    state: &State,
    candidate_surfaces: &[Resolved<WlSurfaceId>],
    candidate_modes: &BTreeMap<Resolved<WlSurfaceId>, ManagedWindowMode>,
    fullscreen_surface: Option<Resolved<WlSurfaceId>>,
) -> (Vec<WindowId>, BTreeMap<WindowId, ManagedWindowMode>, Option<WindowId>) {
    let windows = candidate_surfaces
        .iter()
        .filter_map(|root| state.window_for_root(*root))
        .collect::<Vec<_>>();
    let modes = candidate_surfaces
        .iter()
        .filter_map(|root| {
            let window = state.window_for_root(*root)?;
            Some((
                window,
                candidate_modes
                    .get(root)
                    .copied()
                    .unwrap_or(ManagedWindowMode::Tiled),
            ))
        })
        .collect();
    let fullscreen = fullscreen_surface.and_then(|root| state.window_for_root(root));
    (windows, modes, fullscreen)
}

fn sync_workspace_layout_candidates(
    layout: &mut WorkspaceLayout,
    candidates: &[WindowId],
    candidate_modes: &BTreeMap<WindowId, ManagedWindowMode>,
    fullscreen: Option<WindowId>,
) {
    let candidate_windows = candidates.iter().copied().collect::<BTreeSet<_>>();
    let current_windows = layout
        .window_leaves
        .keys()
        .copied()
        .chain(layout.floating.stack.iter().copied())
        .collect::<BTreeSet<_>>();
    for removed in current_windows
        .difference(&candidate_windows)
        .copied()
        .collect::<Vec<_>>()
    {
        layout.remove_window(removed);
    }
    for window in candidates.iter().copied() {
        let mode = candidate_modes
            .get(&window)
            .copied()
            .unwrap_or(ManagedWindowMode::Tiled);
        match mode {
            ManagedWindowMode::Tiled => {
                if layout.is_floating_window(window) {
                    layout.set_floating(window, false);
                } else if !layout.contains_window(window) {
                    layout.insert_tiled(window);
                }
            }
            ManagedWindowMode::Floating => {
                if !layout.is_floating_window(window) {
                    layout.set_floating(window, true);
                }
            }
        }
    }
    layout.set_fullscreen(fullscreen);
}

/// Hit-test split handles. One split per axis; a nearby perpendicular handle
/// joins when it is within `corner_len` (Chebyshev). The cursor edges are
/// `EDGE_RIGHT` or `EDGE_BOTTOM` for one axis, and a diagonal for two.
///
/// Containment matches [`rect_contains_point`]: the far edges are included.
pub fn split_handles_at(
    handles: &[SplitHandle],
    point: (f64, f64),
    corner_len: f64,
) -> Option<(SplitPair, u32)> {
    let horizontal = closest_containing_handle(handles, SplitAxis::Horizontal, point);
    let vertical = closest_containing_handle(handles, SplitAxis::Vertical, point);
    let (horizontal, vertical) = match (horizontal, vertical) {
        (None, None) => return None,
        (Some(horizontal), None) => (
            Some(horizontal),
            handle_within_corner(handles, SplitAxis::Vertical, point, corner_len),
        ),
        (None, Some(vertical)) => (
            handle_within_corner(handles, SplitAxis::Horizontal, point, corner_len),
            Some(vertical),
        ),
        pair => pair,
    };
    let splits = SplitPair {
        horizontal: horizontal.map(|handle| handle.split),
        vertical: vertical.map(|handle| handle.split),
    };
    if splits.is_empty() {
        return None;
    }
    Some((splits, split_handle_cursor_edges(horizontal, vertical, point)))
}

fn closest_containing_handle(
    handles: &[SplitHandle],
    axis: SplitAxis,
    point: (f64, f64),
) -> Option<&SplitHandle> {
    handles
        .iter()
        .filter(|handle| handle.axis == axis && rect_contains_point(handle.rect, point.0, point.1))
        .min_by(|left, right| {
            handle_main_distance(left, point)
                .partial_cmp(&handle_main_distance(right, point))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn handle_within_corner<'a>(
    handles: &'a [SplitHandle],
    axis: SplitAxis,
    point: (f64, f64),
    corner_len: f64,
) -> Option<&'a SplitHandle> {
    let handle = handles
        .iter()
        .filter(|handle| handle.axis == axis)
        .min_by(|left, right| {
            chebyshev_distance_to_rect(left.rect, point)
                .partial_cmp(&chebyshev_distance_to_rect(right.rect, point))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    (chebyshev_distance_to_rect(handle.rect, point) <= corner_len).then_some(handle)
}

fn handle_main_distance(handle: &SplitHandle, point: (f64, f64)) -> f64 {
    let main = match handle.axis {
        SplitAxis::Horizontal => point.0,
        SplitAxis::Vertical => point.1,
    };
    (main - handle.center).abs()
}

fn split_handle_cursor_edges(
    horizontal: Option<&SplitHandle>,
    vertical: Option<&SplitHandle>,
    point: (f64, f64),
) -> u32 {
    match (horizontal, vertical) {
        (Some(_), None) => EDGE_RIGHT,
        (None, Some(_)) => EDGE_BOTTOM,
        (Some(horizontal), Some(vertical)) => {
            let left_of_vertical_boundary = point.0 < horizontal.center;
            let above_horizontal_boundary = point.1 < vertical.center;
            if (left_of_vertical_boundary && above_horizontal_boundary)
                || (!left_of_vertical_boundary && !above_horizontal_boundary)
            {
                EDGE_BOTTOM | EDGE_RIGHT
            } else {
                EDGE_BOTTOM | EDGE_LEFT
            }
        }
        (None, None) => 0,
    }
}

fn chebyshev_distance_to_rect(rect: RectI, point: (f64, f64)) -> f64 {
    let left = rect.x as f64;
    let right = left + rect.width.max(0) as f64;
    let top = rect.y as f64;
    let bottom = top + rect.height.max(0) as f64;
    let dx = if point.0 < left {
        left - point.0
    } else if point.0 > right {
        point.0 - right
    } else {
        0.0
    };
    let dy = if point.1 < top {
        top - point.1
    } else if point.1 > bottom {
        point.1 - bottom
    } else {
        0.0
    };
    dx.max(dy)
}

fn rebuild_split_handles(layout: &mut WorkspaceLayout, params: SplitHandleParams) {
    let mut handles = Vec::new();
    if layout.fullscreen.is_none() {
        if let Some(root_id) = layout.root {
            collect_split_handles(layout, root_id, params, &mut handles);
        }
    }
    layout.split_handles = handles;
}

fn collect_split_handles(
    layout: &WorkspaceLayout,
    node_id: LayoutNodeId,
    params: SplitHandleParams,
    handles: &mut Vec<SplitHandle>,
) {
    let Some(LayoutNode::Container(container)) = layout.nodes.get(&node_id) else {
        return;
    };
    let axis = container.axis;
    let container_rect = container.last_applied_rect;
    let children = container
        .children
        .iter()
        .map(|child| child.node_id)
        .collect::<Vec<_>>();
    if let Some(container_rect) = container_rect {
        for (index, pair) in children.windows(2).enumerate() {
            let (Some(first), Some(second)) =
                (layout.node_rect(pair[0]), layout.node_rect(pair[1]))
            else {
                continue;
            };
            if let Some(handle) = split_handle_for_pair(
                SplitRef { container: node_id, first: index },
                axis,
                first,
                second,
                container_rect,
                params,
            ) {
                handles.push(handle);
            }
        }
    }
    for child in children {
        collect_split_handles(layout, child, params, handles);
    }
}

fn split_handle_for_pair(
    split: SplitRef,
    axis: SplitAxis,
    first: RectI,
    second: RectI,
    container: RectI,
    params: SplitHandleParams,
) -> Option<SplitHandle> {
    let border = params.border.max(0);
    let min_span = params.grab_extent.max(MIN_SPLIT_HANDLE_SPAN);
    let (x1, y1, x2, y2, center) = match axis {
        SplitAxis::Horizontal => {
            let center = (f64::from(first.x2()) + f64::from(second.x)) / 2.0;
            let (x1, x2) = widened_main_span(
                first.x2().saturating_sub(border),
                second.x.saturating_add(border),
                center,
                min_span,
            );
            (x1, container.y, x2, container.y2(), center)
        }
        SplitAxis::Vertical => {
            let center = (f64::from(first.y2()) + f64::from(second.y)) / 2.0;
            let (y1, y2) = widened_main_span(
                first.y2().saturating_sub(border),
                second.y.saturating_add(border),
                center,
                min_span,
            );
            (container.x, y1, container.x2(), y2, center)
        }
    };
    let rect = RectI::from_extents(x1, y1, x2, y2)?;
    if rect.is_empty() {
        return None;
    }
    Some(SplitHandle { split, axis, rect, center })
}

/// Keep a span that is already wide enough. Otherwise center `min_span` on the gap.
fn widened_main_span(start: i32, end: i32, center: f64, min_span: i32) -> (i32, i32) {
    if end >= start && end.saturating_sub(start) >= min_span {
        (start, end)
    } else {
        let start = (center - (f64::from(min_span) / 2.0)).round() as i32;
        (start, start.saturating_add(min_span))
    }
}

fn apply_workspace_layout(
    layout: &mut WorkspaceLayout,
    tiling_bounds: RectI,
    fullscreen_bounds: RectI,
    gap_policy: GapPolicy,
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    min_sizes: &BTreeMap<WindowId, (i32, i32)>,
    handle_params: SplitHandleParams,
) -> Vec<LayoutOperation> {
    let mut operations = Vec::new();
    if let Some(fullscreen) = layout.fullscreen.as_ref() {
        if let Some(&leaf_id) = layout.window_leaves.get(&fullscreen.window) {
            if let Some(LayoutNode::Leaf(leaf)) = layout.nodes.get_mut(&leaf_id) {
                leaf.last_applied_rect = Some(fullscreen_bounds);
                leaf.tiled_edges = EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT;
            }
        }
        operations.push(LayoutOperation {
            window: fullscreen.window,
            allocation: LayoutAllocation {
                frame_rect: fullscreen_bounds,
                tiled_edges: EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
                fullscreen: true,
            },
        });
        layout.split_handles.clear();
        layout.dirty = false;
        return operations;
    }

    if let Some(root_id) = layout.root {
        let edge_constraints = workspace_edge_constraints(tiling_bounds);
        apply_node_layout(
            layout,
            root_id,
            tiling_bounds,
            tiling_bounds,
            edge_constraints,
            gap_policy,
            decoration_insets,
            min_sizes,
            &mut operations,
        );
    }
    rebuild_split_handles(layout, handle_params);
    layout.dirty = false;
    operations
}

fn apply_node_layout(
    layout: &mut WorkspaceLayout,
    node_id: LayoutNodeId,
    rect: RectI,
    workspace_bounds: RectI,
    edge_constraints: TiledEdgeConstraints,
    gap_policy: GapPolicy,
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    min_sizes: &BTreeMap<WindowId, (i32, i32)>,
    operations: &mut Vec<LayoutOperation>,
) {
    let Some(node_snapshot) = layout.nodes.get(&node_id).cloned() else {
        return;
    };
    match node_snapshot {
        LayoutNode::Leaf(leaf) => {
            let tiled_edges =
                if let Some(LayoutNode::Leaf(leaf_mut)) = layout.nodes.get_mut(&node_id) {
                    leaf_mut.last_applied_rect = Some(rect);
                    leaf_mut.tiled_edges =
                        tiled_edges_for_rect(rect, workspace_bounds, edge_constraints);
                    leaf_mut.tiled_edges
                } else {
                    tiled_edges_for_rect(rect, workspace_bounds, edge_constraints)
                };
            operations.push(LayoutOperation {
                window: leaf.window,
                allocation: LayoutAllocation { frame_rect: rect, tiled_edges, fullscreen: false },
            });
        }
        LayoutNode::Container(container) => {
            if let Some(LayoutNode::Container(live)) = layout.nodes.get_mut(&node_id) {
                live.last_applied_rect = Some(rect);
            }
            let axis_span = match container.axis {
                SplitAxis::Horizontal => rect.width,
                SplitAxis::Vertical => rect.height,
            };
            let inner_gap =
                effective_inner_gap(gap_policy.inner_gap, container.children.len(), axis_span);
            let main_sizes = compute_child_main_sizes(
                layout,
                &container,
                rect,
                gap_policy,
                decoration_insets,
                min_sizes,
            );
            let mut cursor_x = rect.x;
            let mut cursor_y = rect.y;
            for (index, child) in container.children.iter().enumerate() {
                let main_size = main_sizes.get(index).copied().unwrap_or(0);
                let child_rect = match container.axis {
                    SplitAxis::Horizontal => {
                        let child_rect = RectI {
                            x: cursor_x,
                            y: rect.y,
                            width: main_size,
                            height: rect.height,
                        };
                        cursor_x += main_size;
                        if index + 1 != container.children.len() {
                            cursor_x += inner_gap;
                        }
                        child_rect
                    }
                    SplitAxis::Vertical => {
                        let child_rect =
                            RectI { x: rect.x, y: cursor_y, width: rect.width, height: main_size };
                        cursor_y += main_size;
                        if index + 1 != container.children.len() {
                            cursor_y += inner_gap;
                        }
                        child_rect
                    }
                };
                let child_edge_constraints =
                    child_edge_constraints(edge_constraints, &container, child_rect, index);
                apply_node_layout(
                    layout,
                    child.node_id,
                    child_rect,
                    workspace_bounds,
                    child_edge_constraints,
                    gap_policy,
                    decoration_insets,
                    min_sizes,
                    operations,
                );
            }
        }
    }
}

fn tiled_edges_for_rect(
    rect: RectI,
    workspace_bounds: RectI,
    edge_constraints: TiledEdgeConstraints,
) -> u32 {
    let mut edges = 0;
    if edge_constraints
        .left
        .is_some_and(|left| rect.x <= left && left >= workspace_bounds.x)
    {
        edges |= EDGE_LEFT;
    }
    if edge_constraints
        .top
        .is_some_and(|top| rect.y <= top && top >= workspace_bounds.y)
    {
        edges |= EDGE_TOP;
    }
    if edge_constraints
        .right
        .is_some_and(|right| rect.x2() >= right && right <= workspace_bounds.x2())
    {
        edges |= EDGE_RIGHT;
    }
    if edge_constraints
        .bottom
        .is_some_and(|bottom| rect.y2() >= bottom && bottom <= workspace_bounds.y2())
    {
        edges |= EDGE_BOTTOM;
    }
    edges
}

fn workspace_edge_constraints(workspace_bounds: RectI) -> TiledEdgeConstraints {
    TiledEdgeConstraints {
        left: Some(workspace_bounds.x),
        right: Some(workspace_bounds.x2()),
        top: Some(workspace_bounds.y),
        bottom: Some(workspace_bounds.y2()),
    }
}

fn child_edge_constraints(
    parent_constraints: TiledEdgeConstraints,
    container: &ContainerNode,
    child_rect: RectI,
    index: usize,
) -> TiledEdgeConstraints {
    let mut constraints = parent_constraints;
    match container.axis {
        SplitAxis::Horizontal => {
            if index > 0 {
                constraints.left = Some(child_rect.x);
            }
            if index + 1 != container.children.len() {
                constraints.right = Some(child_rect.x2());
            }
        }
        SplitAxis::Vertical => {
            if index > 0 {
                constraints.top = Some(child_rect.y);
            }
            if index + 1 != container.children.len() {
                constraints.bottom = Some(child_rect.y2());
            }
        }
    }
    constraints
}

fn compute_child_main_sizes(
    layout: &WorkspaceLayout,
    container: &ContainerNode,
    rect: RectI,
    gap_policy: GapPolicy,
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    min_sizes: &BTreeMap<WindowId, (i32, i32)>,
) -> Vec<i32> {
    let span = match container.axis {
        SplitAxis::Horizontal => rect.width,
        SplitAxis::Vertical => rect.height,
    }
    .max(0);
    if container.children.is_empty() {
        return Vec::new();
    }
    let inner_gap = effective_inner_gap(gap_policy.inner_gap, container.children.len(), span);
    let total_gap = total_inner_gap(inner_gap, container.children.len());
    let available = span.saturating_sub(total_gap).max(0);

    let preferred_sizes = weighted_child_main_sizes(container, available);
    let minimums = container
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            child_minimum_main_size(
                layout,
                child.node_id,
                container.axis,
                child_size_hint(
                    container.axis,
                    rect,
                    preferred_sizes.get(index).copied().unwrap_or(0),
                ),
                gap_policy,
                decoration_insets,
                min_sizes,
            )
        })
        .collect::<Vec<_>>();
    let effective_minimums = if minimums.iter().sum::<i32>() <= available {
        minimums
    } else {
        // If client minimums do not fit, fall back to weighted distribution with a 1px floor.
        vec![1; container.children.len()]
    };

    let mut sizes = Vec::with_capacity(container.children.len());
    let mut consumed = 0;
    for index in 0..container.children.len() {
        let remaining_available = available.saturating_sub(consumed);
        if index + 1 == container.children.len() {
            sizes.push(remaining_available);
            break;
        }
        let remaining_required = effective_minimums[index + 1..].iter().sum::<i32>();
        let max_size = remaining_available
            .saturating_sub(remaining_required)
            .max(0);
        let min_size = effective_minimums[index].min(max_size);
        let size = preferred_sizes[index].clamp(min_size, max_size);
        sizes.push(size);
        consumed += size;
    }
    sizes
}

fn child_minimum_main_size(
    layout: &WorkspaceLayout,
    node_id: LayoutNodeId,
    axis: SplitAxis,
    size_hint: LayoutSizeHint,
    gap_policy: GapPolicy,
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    min_sizes: &BTreeMap<WindowId, (i32, i32)>,
) -> i32 {
    let (min_width, min_height) = node_minimum_size(
        layout,
        node_id,
        Some(size_hint),
        gap_policy,
        decoration_insets,
        min_sizes,
    );
    match axis {
        SplitAxis::Horizontal => min_width,
        SplitAxis::Vertical => min_height,
    }
}

fn node_minimum_size(
    layout: &WorkspaceLayout,
    node_id: LayoutNodeId,
    size_hint: Option<LayoutSizeHint>,
    gap_policy: GapPolicy,
    decoration_insets: &BTreeMap<WindowId, DecorationInsets>,
    min_sizes: &BTreeMap<WindowId, (i32, i32)>,
) -> (i32, i32) {
    let Some(node) = layout.nodes.get(&node_id) else {
        return (1, 1);
    };
    match node {
        LayoutNode::Leaf(leaf) => {
            let (min_width, min_height) = min_sizes.get(&leaf.window).copied().unwrap_or((1, 1));
            let insets = decoration_insets_for_window(decoration_insets, leaf.window);
            (
                min_width
                    .saturating_add(insets.left)
                    .saturating_add(insets.right)
                    .max(1),
                min_height
                    .saturating_add(insets.top)
                    .saturating_add(insets.bottom)
                    .max(1),
            )
        }
        LayoutNode::Container(container) => {
            let effective_gap = size_hint
                .map(|hint| {
                    effective_inner_gap_for_container(container.axis, hint, gap_policy, container)
                })
                .unwrap_or_else(|| gap_policy.inner_gap.max(0));
            let child_hints = size_hint
                .map(|hint| child_size_hint_for_container(container, hint, gap_policy))
                .unwrap_or_else(|| vec![LayoutSizeHint::default(); container.children.len()]);
            let child_sizes = container
                .children
                .iter()
                .zip(child_hints)
                .map(|(child, child_hint)| {
                    node_minimum_size(
                        layout,
                        child.node_id,
                        size_hint.map(|_| child_hint),
                        gap_policy,
                        decoration_insets,
                        min_sizes,
                    )
                })
                .collect::<Vec<_>>();
            let total_gap = total_inner_gap(effective_gap, container.children.len());
            match container.axis {
                SplitAxis::Horizontal => (
                    child_sizes
                        .iter()
                        .map(|(width, _)| *width)
                        .sum::<i32>()
                        .saturating_add(total_gap)
                        .max(1),
                    child_sizes
                        .iter()
                        .map(|(_, height)| *height)
                        .max()
                        .unwrap_or(1),
                ),
                SplitAxis::Vertical => (
                    child_sizes
                        .iter()
                        .map(|(width, _)| *width)
                        .max()
                        .unwrap_or(1),
                    child_sizes
                        .iter()
                        .map(|(_, height)| *height)
                        .sum::<i32>()
                        .saturating_add(total_gap)
                        .max(1),
                ),
            }
        }
    }
}

fn effective_inner_gap_for_container(
    axis: SplitAxis,
    size_hint: LayoutSizeHint,
    gap_policy: GapPolicy,
    container: &ContainerNode,
) -> i32 {
    let axis_span = match axis {
        SplitAxis::Horizontal => size_hint.width,
        SplitAxis::Vertical => size_hint.height,
    };
    effective_inner_gap(gap_policy.inner_gap, container.children.len(), axis_span)
}

fn child_size_hint(axis: SplitAxis, rect: RectI, main_size: i32) -> LayoutSizeHint {
    match axis {
        SplitAxis::Horizontal => {
            LayoutSizeHint { width: main_size.max(0), height: rect.height.max(0) }
        }
        SplitAxis::Vertical => {
            LayoutSizeHint { width: rect.width.max(0), height: main_size.max(0) }
        }
    }
}

fn child_size_hint_for_container(
    container: &ContainerNode,
    size_hint: LayoutSizeHint,
    gap_policy: GapPolicy,
) -> Vec<LayoutSizeHint> {
    let axis_span = match container.axis {
        SplitAxis::Horizontal => size_hint.width,
        SplitAxis::Vertical => size_hint.height,
    }
    .max(0);
    let inner_gap =
        effective_inner_gap_for_container(container.axis, size_hint, gap_policy, container);
    let available = axis_span.saturating_sub(total_inner_gap(inner_gap, container.children.len()));
    weighted_child_main_sizes(container, available)
        .into_iter()
        .map(|main_size| match container.axis {
            SplitAxis::Horizontal => {
                LayoutSizeHint { width: main_size, height: size_hint.height.max(0) }
            }
            SplitAxis::Vertical => {
                LayoutSizeHint { width: size_hint.width.max(0), height: main_size }
            }
        })
        .collect()
}

/// Hyprland dwindle sizes the first child as `span / 2 * split_ratio`.
/// Weights `(applied, 2 - applied)` produce that fraction. The default
/// directional bias stores `2 - split_ratio` when the dragged window is the
/// first child, so the window that stayed in place keeps `split_ratio / 2`.
fn dwindle_child_weights(split_ratio: f32, dragged_is_first: bool) -> (f64, f64) {
    let ratio = f64::from(SplitRatio::sanitize(split_ratio).get());
    let applied = if dragged_is_first { 2.0 - ratio } else { ratio };
    (applied, 2.0 - applied)
}

/// Aspect mode splits left/right only when the tile is wider than it is tall.
/// Precise mode uses the pointer quadrant. The dragged window is the first
/// child when it lands on the left or top of the chosen axis.
fn tiled_drop_side(
    rect: RectI,
    pointer_x: f64,
    pointer_y: f64,
    precise_mouse_move: bool,
) -> (SplitAxis, bool) {
    let left = rect.x as f64;
    let top = rect.y as f64;
    let width = rect.width.max(0) as f64;
    let height = rect.height.max(0) as f64;
    let center_x = left + width / 2.0;
    let center_y = top + height / 2.0;
    let horizontal = if precise_mouse_move {
        let delta_x = pointer_x - center_x;
        let delta_y = pointer_y - center_y;
        let proportions = if width == 0.0 { f64::INFINITY } else { height / width };
        (delta_y / delta_x).abs() < proportions
    } else {
        width > height
    };
    let dragged_is_first = if precise_mouse_move {
        if horizontal {
            pointer_x <= center_x
        } else {
            pointer_y <= center_y
        }
    } else if horizontal {
        pointer_x < center_x
    } else {
        pointer_y < center_y
    };
    let axis = if horizontal { SplitAxis::Horizontal } else { SplitAxis::Vertical };
    (axis, dragged_is_first)
}

fn rect_contains_point(rect: RectI, x: f64, y: f64) -> bool {
    let left = rect.x as f64;
    let top = rect.y as f64;
    x >= left
        && x <= left + rect.width.max(0) as f64
        && y >= top
        && y <= top + rect.height.max(0) as f64
}

fn distance_squared_to_rect(rect: RectI, x: f64, y: f64) -> f64 {
    let left = rect.x as f64;
    let right = left + rect.width.max(0) as f64;
    let top = rect.y as f64;
    let bottom = top + rect.height.max(0) as f64;
    let dx = if x < left {
        left - x
    } else if x > right {
        x - right
    } else {
        0.0
    };
    let dy = if y < top {
        top - y
    } else if y > bottom {
        y - bottom
    } else {
        0.0
    };
    dx * dx + dy * dy
}

fn weighted_child_main_sizes(container: &ContainerNode, available: i32) -> Vec<i32> {
    let total_weight = container
        .children
        .iter()
        .map(|child| child.weight.max(MIN_CHILD_WEIGHT))
        .sum::<f64>()
        .max(MIN_CHILD_WEIGHT);
    container
        .children
        .iter()
        .map(|child| {
            (((available as f64) * child.weight.max(MIN_CHILD_WEIGHT)) / total_weight).round()
                as i32
        })
        .collect::<Vec<_>>()
}

fn content_rect_for_tiled_frame(frame_rect: RectI, insets: DecorationInsets) -> RectI {
    shrink_rect(frame_rect, insets)
}

fn outer_gapped_bounds(usable_bounds: RectI, gap_policy: GapPolicy) -> RectI {
    shrink_rect(
        usable_bounds,
        DecorationInsets {
            top: gap_policy.outer_gap,
            right: gap_policy.outer_gap,
            bottom: gap_policy.outer_gap,
            left: gap_policy.outer_gap,
        },
    )
}

fn shrink_rect(rect: RectI, insets: DecorationInsets) -> RectI {
    if rect.width <= 0 || rect.height <= 0 {
        return rect;
    }

    let max_left = rect.width.saturating_sub(1);
    let left = insets.left.clamp(0, max_left);
    let remaining_width = rect.width.saturating_sub(left);
    let right = insets.right.clamp(0, remaining_width.saturating_sub(1));

    let max_top = rect.height.saturating_sub(1);
    let top = insets.top.clamp(0, max_top);
    let remaining_height = rect.height.saturating_sub(top);
    let bottom = insets.bottom.clamp(0, remaining_height.saturating_sub(1));

    RectI {
        x: rect.x.saturating_add(left),
        y: rect.y.saturating_add(top),
        width: rect.width.saturating_sub(left).saturating_sub(right).max(1),
        height: rect
            .height
            .saturating_sub(top)
            .saturating_sub(bottom)
            .max(1),
    }
}

fn effective_inner_gap(requested_gap: i32, child_count: usize, axis_span: i32) -> i32 {
    let requested_gap = requested_gap.max(0);
    let gap_count = child_count.saturating_sub(1);
    if gap_count == 0 {
        return 0;
    }
    let span = axis_span.max(0);
    let gap_count = gap_count as i32;
    requested_gap.min(span.saturating_div(gap_count))
}

fn total_inner_gap(inner_gap: i32, child_count: usize) -> i32 {
    inner_gap
        .max(0)
        .saturating_mul(child_count.saturating_sub(1) as i32)
}

fn workspace_minimum_sizes(
    state: &State,
    workspace_id: WorkspaceId,
) -> BTreeMap<Resolved<WlSurfaceId>, (i32, i32)> {
    state
        .managed_windows()
        .filter_map(|(surface, managed)| {
            (managed.workspace_id == workspace_id)
                .then_some((surface, minimum_size_for_surface(state, surface)))
        })
        .collect()
}

fn minimum_size_for_surface(state: &State, surface: Resolved<WlSurfaceId>) -> (i32, i32) {
    state
        .window_for_root(surface)
        .and_then(|window| state.toplevel(window))
        .and_then(|toplevel| toplevel.minimum_size())
        .unwrap_or((1, 1))
}

fn is_managed_root_surface_live(state: &State, surface_object_id: Resolved<WlSurfaceId>) -> bool {
    let Some(surface) = surface_object_id.try_ref(state) else {
        return false;
    };
    is_managed_root_surface_candidate_common(state, surface_object_id, surface)
        && surface.has_committed_buffer()
}

fn is_managed_root_surface_candidate_common(
    state: &State,
    surface_object_id: Resolved<WlSurfaceId>,
    surface: &WlSurface,
) -> bool {
    surface.is_root()
        && matches!(surface.role(), Role::XdgSurface | Role::XwaylandSurface)
        && !surface.has_cursor_role()
        && !surface.has_dnd_icon_role()
        && state
            .window_for_root(surface_object_id)
            .is_some_and(|window| state.toplevel(window).is_some())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, ClientId, Resolved},
            protocol_generated::{
                WlSubsurfaceId, WlSurfaceId, XdgSurfaceId, XdgToplevelId, XdgWmBaseId,
            },
            root_geometry::CommittedRootGeometry,
            runner::Dispatcher,
            state::{ObjectId, State},
            test_support::{
                insert_wl_subsurface_client1 as insert_subsurface, resolved_wl_surface as root,
                state_for_tests,
            },
            transform::SizeI,
            wayland_objects::{
                wl_surface::Role,
                xdg_surface::{
                    xdg_toplevel::{ToplevelDecorationMode, XdgToplevel},
                    XdgSurface,
                },
            },
            window::WindowId,
        },
    };

    #[test]
    fn a_root_whose_window_was_destroyed_is_not_a_layout_candidate() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(70));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _, _) = insert_root_xdg_toplevel(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(71),
            XdgSurfaceId::from(72),
            XdgToplevelId::from(73),
            (80, 60),
        );
        let surface = root(&state, surface_object_id);
        assert!(is_managed_root_surface_live(&state, surface));
        let window = state.window_for_root(surface).unwrap();
        state.destroy_window(window);
        assert!(!is_managed_root_surface_live(&state, surface));
    }

    fn insert_root_xdg_toplevel(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        wl_surface_id: WlSurfaceId,
        xdg_surface_id: XdgSurfaceId,
        toplevel_id: XdgToplevelId,
        logical_size: (i32, i32),
    ) -> (ObjectId, ObjectId, ObjectId) {
        let (surface_resolved, _) =
            client.insert_object(state, WlSurface::new(wl_surface_id, client.client_id(), 6));
        let surface_object_id = surface_resolved.object_id();
        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            assert!(surface.set_role(Role::XdgSurface, dispatcher).is_ok());
        }
        state.register_root_surface(surface_resolved);

        let (xdg_surface_resolved, _) = client.insert_object(
            state,
            XdgSurface::new(
                xdg_surface_id,
                Resolved::new(client.client_id(), XdgWmBaseId::from(1), ObjectId::from(1usize)),
                Resolved::new(client.client_id(), wl_surface_id, surface_object_id),
                7,
            ),
        );
        let xdg_surface_object_id = xdg_surface_resolved.object_id();
        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.set_xdg_surface_impl(crate::clients::Resolved::new(
                client.client_id(),
                xdg_surface_id,
                xdg_surface_object_id,
            ));
            surface.test_map(ObjectId::from(900usize), 0, 0, logical_size.0, logical_size.1);
        }
        let window = state.allocate_window_id();
        let (toplevel_resolved, _) = client.insert_object(
            state,
            XdgToplevel::new(
                toplevel_id,
                Resolved::new(client.client_id(), xdg_surface_id, xdg_surface_object_id),
                7,
                window,
            ),
        );
        let toplevel_object_id = toplevel_resolved.object_id();
        state.insert_window(window, surface_resolved, toplevel_object_id);
        (surface_object_id, xdg_surface_object_id, toplevel_object_id)
    }

    fn live_toplevel(state: &mut State) -> crate::clients::Resolved<WlSurfaceId> {
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(3usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = crate::test_support::test_client_and_dispatcher(4);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _, _) = insert_root_xdg_toplevel(
            state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(40),
            XdgSurfaceId::from(41),
            XdgToplevelId::from(42),
            (80, 60),
        );
        root(state, surface_object_id)
    }

    #[test]
    fn layout_changes_sample_a_visual_box_and_keep_the_logical_goal() {
        let mut state = state_for_tests();
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::Windows);
        state.config.animations.set_open_duration_msec(0);
        let surface = live_toplevel(&mut state);
        state.relayout_dirty_workspaces();
        let now = std::time::Instant::now();
        state.advance_visual_animations(now);
        let initial = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert_eq!(
            state
                .managed_window(surface)
                .unwrap()
                .geometry_animation
                .visual_rect(),
            Some(initial)
        );

        assert!(state.set_root_surface_floating(surface, true));
        {
            let mut wl_surface = surface.try_grab(&mut state).unwrap();
            wl_surface.set_root_position(initial.x + 80, initial.y + 40);
        }
        state.mark_root_geometry_dirty(surface);
        state.sync_window_geometry_goal(
            state.window_for_root(surface).unwrap(),
            crate::visual_animation::WindowGeometryMotion::Layout,
        );
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert_ne!(goal, initial);

        state.advance_visual_animations(now);
        let started = state.managed_window(surface).unwrap().geometry_animation;
        assert_eq!(started.goal_rect(), Some(goal));
        assert_eq!(started.visual_rect(), Some(initial));

        let moved = state.advance_visual_animations(now + std::time::Duration::from_millis(100));
        let mid = state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap();
        assert_ne!(mid, goal);
        assert_ne!(mid, initial);
        assert_eq!(
            state
                .committed_root_geometry(surface)
                .unwrap()
                .layout_rect_global,
            goal
        );
        if initial.x != goal.x {
            assert!(mid.x > initial.x.min(goal.x) && mid.x < initial.x.max(goal.x));
        }
        if initial.y != goal.y {
            assert!(mid.y > initial.y.min(goal.y) && mid.y < initial.y.max(goal.y));
        }
        assert!(moved
            .damaged_rects
            .iter()
            .any(|rect| rect.intersects(initial)));
        assert!(moved.damaged_rects.iter().any(|rect| rect.intersects(mid)));
    }

    fn queues_animation_frame(dispatcher: &Dispatcher, output_id: crate::state::OutputId) -> bool {
        dispatcher.test_has_queued_action_for::<crate::scheduler::OutputSchedulerAction, _>(
            crate::scheduler::output_scheduler_id(output_id),
            |action: &crate::scheduler::OutputSchedulerAction| {
                matches!(
                    action,
                    crate::scheduler::OutputSchedulerAction::RequestFrame {
                        reason: crate::scheduler::RequestFrameReason::Animation
                    }
                )
            },
        )
    }

    fn queues_animation_wakeup(dispatcher: &Dispatcher, output_id: crate::state::OutputId) -> bool {
        dispatcher.test_has_queued_action_for::<crate::scheduler::OutputSchedulerAction, _>(
            crate::scheduler::output_scheduler_id(output_id),
            |action: &crate::scheduler::OutputSchedulerAction| {
                matches!(action, crate::scheduler::OutputSchedulerAction::ScheduleAnimation { .. })
            },
        )
    }

    #[test]
    fn crossing_window_renders_and_wakes_both_outputs_until_it_settles() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::Windows);
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::WindowsMove, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::WindowsMove);
        state.config.animations.set_open_duration_msec(0);
        let surface = live_toplevel(&mut state);
        let left_workspace = state.root_surface_workspace(surface).unwrap();
        let left = state.workspace_output_id(left_workspace).unwrap();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(3usize));
        let right = state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 2, 800, 0, 800, 600,
        ));
        let right_workspace = state.active_workspace_for_output(right).unwrap();
        state.relayout_dirty_workspaces();
        let now = Instant::now();
        let mut pump = crate::drm::VisualAnimationPump::new();
        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now);
        let origin = state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap();
        assert!(origin.x2() <= 800);

        assert!(state.assign_root_surface_to_workspace(surface, right_workspace));
        state.relayout_dirty_workspaces();
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert!(goal.x >= 800);
        assert_eq!(
            state
                .managed_window(surface)
                .unwrap()
                .geometry_animation
                .visual_rect(),
            Some(origin)
        );

        let scene = crate::scene::build_scene_model(&state);
        let left_entry = crate::scene::compose_output(&state, &scene, left, None)
            .entries
            .into_iter()
            .find(|entry| entry.root_surface == surface)
            .expect("source output draws the window while its visual box is still there");
        assert!(left_entry.participates_in_render);
        assert!(!left_entry.participates_in_input);
        assert!(!left_entry.participates_in_protocol_visibility);
        let right_entry = crate::scene::compose_output(&state, &scene, right, None)
            .entries
            .into_iter()
            .find(|entry| entry.root_surface == surface)
            .expect("destination output draws the window from its logical rectangle");
        assert!(right_entry.participates_in_render);
        assert!(right_entry.participates_in_input);
        assert!(right_entry.participates_in_protocol_visibility);

        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now);
        assert_eq!(
            state
                .managed_window(surface)
                .unwrap()
                .geometry_animation
                .visual_rect(),
            Some(origin)
        );
        assert!(state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .is_active());
        assert!(queues_animation_wakeup(&dispatcher, left));
        assert!(queues_animation_wakeup(&dispatcher, right));
        assert!(!queues_animation_frame(&dispatcher, left));
        assert!(!queues_animation_frame(&dispatcher, right));

        let moved =
            state.advance_visual_animations_for_output(left, now + Duration::from_millis(100));
        let mid = state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap();
        assert_ne!(mid, origin);
        assert_ne!(mid, goal);
        assert!(moved
            .damaged_rects
            .iter()
            .any(|rect| rect.intersects(origin)));
        assert!(moved.damaged_rects.iter().any(|rect| rect.intersects(mid)));

        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now + Duration::from_millis(200));
        assert_eq!(
            state
                .managed_window(surface)
                .unwrap()
                .geometry_animation
                .visual_rect(),
            Some(goal)
        );
        assert!(!state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .is_active());
        assert!(queues_animation_frame(&dispatcher, left));
        assert!(queues_animation_frame(&dispatcher, right));

        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now + Duration::from_millis(200));
        assert!(!queues_animation_frame(&dispatcher, left));
        assert!(!queues_animation_frame(&dispatcher, right));
        assert!(!queues_animation_wakeup(&dispatcher, left));
        assert!(!queues_animation_wakeup(&dispatcher, right));
    }

    #[test]
    fn disabled_geometry_snap_damages_the_old_and_new_boxes() {
        use std::time::Instant;

        let mut state = state_for_tests();
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::Windows);
        state.config.animations.set_open_duration_msec(0);
        let surface = live_toplevel(&mut state);
        let output_id = state
            .workspace_output_id(state.root_surface_workspace(surface).unwrap())
            .unwrap();
        state.relayout_dirty_workspaces();
        let now = Instant::now();
        let mut pump = crate::drm::VisualAnimationPump::new();
        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now);
        let origin = state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap();

        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 0);
        assert!(state.set_root_surface_floating(surface, true));
        {
            let mut wl_surface = surface.try_grab(&mut state).unwrap();
            wl_surface.set_root_position(origin.x + 120, origin.y + 40);
        }
        state.mark_root_geometry_dirty(surface);
        state.sync_window_geometry_goal(
            state.window_for_root(surface).unwrap(),
            crate::visual_animation::WindowGeometryMotion::Layout,
        );
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert_ne!(goal, origin);

        let snapped = state.advance_visual_animations(now);
        assert_eq!(
            state
                .managed_window(surface)
                .unwrap()
                .geometry_animation
                .visual_rect(),
            Some(goal)
        );
        assert!(!state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .is_active());
        assert!(snapped
            .damaged_rects
            .iter()
            .any(|rect| rect.intersects(origin)));
        assert!(snapped
            .damaged_rects
            .iter()
            .any(|rect| rect.intersects(goal)));

        let mut dispatcher = Dispatcher::default();
        pump.advance_and_schedule(&mut state, &mut dispatcher, now);
        assert!(!queues_animation_frame(&dispatcher, output_id));
        assert!(!queues_animation_wakeup(&dispatcher, output_id));
    }

    #[test]
    fn shadow_makes_a_neighbor_output_draw_the_window_without_taking_input() {
        let mut state = state_for_tests();
        state.config.animations.set_open_duration_msec(0);
        let surface = live_toplevel(&mut state);
        let left_workspace = state.root_surface_workspace(surface).unwrap();
        let left = state.workspace_output_id(left_workspace).unwrap();
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(3usize));
        let right = state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 2, 800, 0, 200, 600,
        ));
        state.relayout_dirty_workspaces();
        let toplevel_id = state
            .window_for_root(surface)
            .and_then(|window| state.window_role_object(window))
            .unwrap();
        {
            let mut toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_id);
            toplevel.set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        state.config.decorations.border.size = 4;
        state.config.decorations.shadow.radius = 40;
        state.config.decorations.shadow.offset_x = 0;
        state.config.decorations.shadow.offset_y = 0;
        state.mark_root_geometry_dirty(surface);
        let frame = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert!(frame.x2() <= 800);
        let influence = state.root_surface_motion_bounds(surface).unwrap();
        assert!(influence.x2() > 800);
        assert!(influence.intersects(state.output_bounds(right).unwrap()));

        let scene = crate::scene::build_scene_model(&state);
        let right_entry = crate::scene::compose_output(&state, &scene, right, None)
            .entries
            .into_iter()
            .find(|entry| entry.root_surface == surface)
            .expect("shadow crossing the edge is drawn on the neighbor");
        assert!(right_entry.participates_in_render);
        assert!(!right_entry.participates_in_input);
        assert!(!right_entry.participates_in_protocol_visibility);
        let left_entry = crate::scene::compose_output(&state, &scene, left, None)
            .entries
            .into_iter()
            .find(|entry| entry.root_surface == surface)
            .unwrap();
        assert!(left_entry.participates_in_input);
        assert!(left_entry.participates_in_protocol_visibility);
    }

    #[test]
    fn managed_map_edge_starts_opening_for_tiled_and_floating_windows() {
        let mut state = state_for_tests();
        state.config.animations.set_open_duration_msec(160);
        let surface = live_toplevel(&mut state);

        let opened = state.relayout_dirty_workspaces();
        assert_eq!(opened, vec![surface]);
        assert!(state.managed_root_surface_state(surface).unwrap().mapped);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Opening {
                progress: crate::visual_animation::UnitValue::ZERO
            }
        );

        state.managed_window_mut(surface).unwrap().mode = crate::state::ManagedWindowMode::Floating;
        state.managed_window_mut(surface).unwrap().mapped = false;
        state
            .managed_window_mut(surface)
            .unwrap()
            .lifecycle_animation
            .settle_idle();
        let reopened = state.relayout_dirty_workspaces();
        assert_eq!(reopened, vec![surface]);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Opening {
                progress: crate::visual_animation::UnitValue::ZERO
            }
        );
        assert!(state.relayout_dirty_workspaces().is_empty());
    }

    #[test]
    fn inactive_workspace_and_zero_duration_do_not_start_opening() {
        let mut state = state_for_tests();
        let surface = live_toplevel(&mut state);
        let active_workspace = state.root_surface_workspace(surface).unwrap();
        let group_id = state
            .workspaces
            .try_get(active_workspace)
            .unwrap()
            .group
            .unwrap();
        let inactive = state.create_workspace(Some(group_id), Some("inactive".to_string()));
        state.assign_root_surface_to_workspace(surface, inactive);
        state.config.animations.set_open_duration_msec(160);

        assert!(state.relayout_dirty_workspaces().is_empty());
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );

        state.assign_root_surface_to_workspace(surface, active_workspace);
        state.managed_window_mut(surface).unwrap().mapped = false;
        state.config.animations.set_open_duration_msec(0);
        assert!(state.relayout_dirty_workspaces().is_empty());
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );
    }

    #[test]
    fn unmap_then_remap_restarts_opening() {
        let mut state = state_for_tests();
        state.config.animations.set_open_duration_msec(120);
        let surface = live_toplevel(&mut state);
        assert_eq!(state.relayout_dirty_workspaces(), vec![surface]);

        {
            let mut wl_surface = state.objects.grab_object::<WlSurface>(surface.object_id());
            wl_surface.test_clear_committed_buffer();
        }
        state.relayout_dirty_workspaces();
        assert!(!state.managed_root_surface_state(surface).unwrap().mapped);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );

        {
            let mut wl_surface = state.objects.grab_object::<WlSurface>(surface.object_id());
            wl_surface.test_map(ObjectId::from(901usize), 0, 0, 80, 60);
        }
        assert_eq!(state.relayout_dirty_workspaces(), vec![surface]);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Opening {
                progress: crate::visual_animation::UnitValue::ZERO
            }
        );
    }

    fn window_visual(state: &State, surface: crate::clients::Resolved<WlSurfaceId>) -> RectI {
        state
            .managed_window(surface)
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap()
    }

    fn open_style_window(
        state: &mut State,
        style: crate::animation_config::AnimationStyle,
    ) -> (crate::clients::Resolved<WlSurfaceId>, RectI, RectI) {
        state.config.animations.set_open_duration_msec(200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::WindowsIn);
        state
            .config
            .animations
            .set_style(crate::animation_config::AnimationNode::WindowsIn, style);
        let surface = live_toplevel(state);
        assert_eq!(state.relayout_dirty_workspaces(), vec![surface]);
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        let output = state
            .output_bounds(
                state
                    .workspace_output_id(state.root_surface_workspace(surface).unwrap())
                    .unwrap(),
            )
            .unwrap();
        (surface, goal, output)
    }

    #[test]
    fn opening_starts_at_the_style_pose_and_keeps_the_logical_goal() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        let (surface, goal, output) = open_style_window(
            &mut state,
            crate::animation_config::AnimationStyle::Popin { percent: 87 },
        );
        let pose = crate::visual_animation::lifecycle_pose(
            crate::animation_config::AnimationStyle::Popin { percent: 87 },
            goal,
            Some(output),
        );
        assert_ne!(pose, goal);
        let now = Instant::now();
        state.advance_visual_animations(now);
        assert_eq!(
            state
                .committed_root_geometry(surface)
                .unwrap()
                .layout_rect_global,
            goal
        );
        assert_eq!(window_visual(&state, surface), pose);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Opening {
                progress: crate::visual_animation::UnitValue::ZERO
            }
        );

        state.advance_visual_animations(now + Duration::from_millis(200));
        assert_eq!(window_visual(&state, surface), goal);
        assert_eq!(
            state
                .committed_root_geometry(surface)
                .unwrap()
                .layout_rect_global,
            goal
        );
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );
    }

    #[test]
    fn slide_and_gnomed_opening_use_their_start_pose() {
        use std::time::Instant;

        let mut state = state_for_tests();
        let (surface, goal, output) = open_style_window(
            &mut state,
            crate::animation_config::AnimationStyle::Slide {
                edge: Some(crate::animation_config::SlideEdge::Left),
            },
        );
        state.advance_visual_animations(Instant::now());
        assert_eq!(
            window_visual(&state, surface),
            crate::visual_animation::lifecycle_pose(
                crate::animation_config::AnimationStyle::Slide {
                    edge: Some(crate::animation_config::SlideEdge::Left),
                },
                goal,
                Some(output),
            )
        );
        assert_eq!(window_visual(&state, surface).width, goal.width);
        assert_eq!(window_visual(&state, surface).x, goal.x - goal.width);

        let mut state = state_for_tests();
        let (surface, goal, output) =
            open_style_window(&mut state, crate::animation_config::AnimationStyle::Gnomed);
        state.advance_visual_animations(Instant::now());
        assert_eq!(
            window_visual(&state, surface),
            crate::visual_animation::lifecycle_pose(
                crate::animation_config::AnimationStyle::Gnomed,
                goal,
                Some(output),
            )
        );
    }

    #[test]
    fn zero_open_duration_snaps_to_the_logical_goal() {
        let mut state = state_for_tests();
        state.config.animations.set_open_duration_msec(0);
        let surface = live_toplevel(&mut state);
        assert!(state.relayout_dirty_workspaces().is_empty());
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        state.advance_visual_animations(std::time::Instant::now());
        assert_eq!(window_visual(&state, surface), goal);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );
    }

    #[test]
    fn opening_alpha_fades_on_fade_in_while_the_pose_uses_windows_in() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::WindowsIn, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::WindowsIn);
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::FadeIn, 0);
        let surface = live_toplevel(&mut state);
        assert_eq!(state.relayout_dirty_workspaces(), vec![surface]);
        let goal = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        let now = Instant::now();
        state.advance_visual_animations(now);
        assert_eq!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .lifecycle_animation
                .sample(),
            crate::visual_animation::WindowLifecycleAnim::Idle
        );
        assert_ne!(window_visual(&state, surface), goal);
        state.advance_visual_animations(now + Duration::from_millis(200));
        assert_eq!(window_visual(&state, surface), goal);
    }

    fn test_committed_geometry(
        frame_rect: RectI,
        client_window_rect: RectI,
        tiled_edges: u32,
        fullscreen: bool,
        root_pos: (i32, i32),
    ) -> CommittedRootGeometry {
        CommittedRootGeometry {
            layout_rect_global: frame_rect,
            root_pos,
            surface_bounds_local: crate::region::RectI::new(
                frame_rect.x.saturating_sub(root_pos.0),
                frame_rect.y.saturating_sub(root_pos.1),
                frame_rect.width,
                frame_rect.height,
            ),
            logical_window_local: crate::region::RectI::new(
                client_window_rect.x.saturating_sub(root_pos.0),
                client_window_rect.y.saturating_sub(root_pos.1),
                client_window_rect.width,
                client_window_rect.height,
            ),
            decoration_mode: None,
            decoration_insets: DecorationInsets::default(),
            tiled_edges,
            fullscreen,
        }
    }

    fn root_child_weight(layout: &WorkspaceLayout, index: usize) -> f64 {
        match layout.nodes.get(&layout.root.unwrap()).unwrap() {
            LayoutNode::Container(container) => container.children[index].weight,
            LayoutNode::Leaf(_) => panic!("root should be a container"),
        }
    }

    fn two_tiled_windows() -> (WorkspaceLayout, SplitDrag) {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(1);
        let right = WindowId::for_tests(2);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let bounds = RectI { x: 0, y: 0, width: 100, height: 50 };
        apply_workspace_layout(
            &mut layout,
            bounds,
            bounds,
            GapPolicy::default(),
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );
        let drag = layout
            .begin_split_drag(
                layout
                    .splits_for_window_edges(left, EDGE_RIGHT)
                    .horizontal
                    .expect("left leaf should support right-edge resize"),
            )
            .expect("split drag");
        (layout, drag)
    }

    #[test]
    fn repeated_resize_step_is_reported_once() {
        let (mut layout, drag) = two_tiled_windows();
        assert!(layout.apply_split_drag(&drag, 10));
        let first_left_weight = root_child_weight(&layout, 0);

        assert!(!layout.apply_split_drag(&drag, 10));
        let second_left_weight = root_child_weight(&layout, 0);

        assert_eq!(first_left_weight, second_left_weight);
    }

    fn arm_window_animation(state: &mut State) {
        state
            .config
            .animations
            .set_duration_msec(crate::animation_config::AnimationNode::Windows, 200);
        state
            .config
            .animations
            .set_linear(crate::animation_config::AnimationNode::Windows);
        state.config.animations.set_open_duration_msec(0);
    }

    fn insert_live_toplevel(
        state: &mut State,
        client: &mut crate::clients::Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        surface_id: u32,
    ) -> crate::clients::Resolved<WlSurfaceId> {
        let (surface_object_id, _, _) = insert_root_xdg_toplevel(
            state,
            client,
            dispatcher,
            WlSurfaceId::from(surface_id),
            XdgSurfaceId::from(surface_id + 1),
            XdgToplevelId::from(surface_id + 2),
            (80, 60),
        );
        root(state, surface_object_id)
    }

    fn settled_side_by_side(state: &mut State) -> (Resolved<WlSurfaceId>, Resolved<WlSurfaceId>) {
        arm_window_animation(state);
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(3usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = crate::test_support::test_client_and_dispatcher(4);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let left = insert_live_toplevel(state, &mut client, &mut client_dispatcher, 40);
        let right = insert_live_toplevel(state, &mut client, &mut client_dispatcher, 50);
        state.relayout_dirty_workspaces();
        state.advance_visual_animations(std::time::Instant::now());
        (left, right)
    }

    fn assert_geometry_snapped(state: &State, surface: Resolved<WlSurfaceId>) {
        let window = state.managed_window(surface).unwrap().geometry_animation;
        let logical = state
            .committed_root_geometry(surface)
            .unwrap()
            .layout_rect_global;
        assert_eq!(window.visual_rect(), Some(logical));
        assert!(!window.is_active());
    }

    #[test]
    fn tiled_pointer_resize_snaps_the_resized_tiles() {
        use std::time::Instant;

        let mut state = state_for_tests();
        let (left, right) = settled_side_by_side(&mut state);
        let left_window = state.window_for_root(left).unwrap();
        let (workspace, drags) = state
            .begin_window_edge_drags(left_window, EDGE_RIGHT)
            .expect("left leaf should support right-edge resize");
        let now = Instant::now();
        state.seat.pointer_interactive_grab =
            Some(crate::input::PointerInteractiveGrab::ResizeSplit {
                workspace,
                drags,
                cursor_edges: EDGE_RIGHT,
                initial_pointer_pos: (0.0, 0.0),
                origin_window: Some(left_window),
            });

        assert!(state.apply_split_drags(workspace, &drags, 40, 0));
        // The configure pass relayouts again while the grab is still held.
        state.relayout_dirty_workspaces();
        state.advance_visual_animations(now);

        assert_geometry_snapped(&state, left);
        assert_geometry_snapped(&state, right);
        assert_ne!(
            state
                .committed_root_geometry(left)
                .unwrap()
                .layout_rect_global
                .width,
            state
                .committed_root_geometry(right)
                .unwrap()
                .layout_rect_global
                .width
        );

        let mut dispatcher = Dispatcher::default();
        crate::input::finish_pointer_interactive_grab(&mut state, &mut dispatcher, 0);
        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(!state.seat.tiled_resize_snap_windows.is_empty());
        state.relayout_dirty_workspaces();
        state.advance_visual_animations(now);
        assert_geometry_snapped(&state, left);
        assert_geometry_snapped(&state, right);
        assert!(state.seat.tiled_resize_snap_windows.is_empty());
    }

    #[test]
    fn tiled_pointer_resize_leaves_an_unaffected_animation_running() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        arm_window_animation(&mut state);
        let backend = crate::state::BackendOutputRef::new(crate::state::DrmId::from(3usize));
        state.insert_output(crate::test_support::test_output_with_crtc_name(
            backend, 1, 0, 0, 800, 600,
        ));
        let (mut client, mut dispatcher) = crate::test_support::test_client_and_dispatcher(4);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let outside = insert_live_toplevel(&mut state, &mut client, &mut client_dispatcher, 40);
        let top = insert_live_toplevel(&mut state, &mut client, &mut client_dispatcher, 50);
        let bottom = insert_live_toplevel(&mut state, &mut client, &mut client_dispatcher, 60);
        state.relayout_dirty_workspaces();
        let now = Instant::now();
        state.advance_visual_animations(now);

        let origin = state
            .committed_root_geometry(outside)
            .unwrap()
            .layout_rect_global;
        let shifted = RectI { x: origin.x + 80, ..origin };
        state
            .managed_window_mut(outside)
            .unwrap()
            .geometry_animation
            .set_goal(shifted, crate::visual_animation::WindowGeometryMotion::Layout);
        state.advance_visual_animations(now);
        state.advance_visual_animations(now + Duration::from_millis(50));
        assert!(state
            .managed_window(outside)
            .unwrap()
            .geometry_animation
            .is_active());

        let top_window = state.window_for_root(top).unwrap();
        let (workspace, drags) = state
            .begin_window_edge_drags(top_window, EDGE_BOTTOM)
            .expect("nested pair should support a bottom-edge resize");
        state.seat.pointer_interactive_grab =
            Some(crate::input::PointerInteractiveGrab::ResizeSplit {
                workspace,
                drags,
                cursor_edges: EDGE_BOTTOM,
                initial_pointer_pos: (0.0, 0.0),
                origin_window: Some(top_window),
            });
        assert!(state.apply_split_drags(workspace, &drags, 0, 40));
        state.relayout_dirty_workspaces();
        state.advance_visual_animations(now + Duration::from_millis(50));

        assert_geometry_snapped(&state, top);
        assert_geometry_snapped(&state, bottom);
        assert!(state
            .managed_window(outside)
            .unwrap()
            .geometry_animation
            .is_active());
    }

    #[test]
    fn floating_left_resize_keeps_the_right_edge_and_clears_after_the_grab() {
        use std::time::Instant;

        let mut state = state_for_tests();
        arm_window_animation(&mut state);
        let surface = live_toplevel(&mut state);
        state.relayout_dirty_workspaces();
        state.advance_visual_animations(Instant::now());
        assert!(state.set_root_surface_floating(surface, true));
        {
            let mut wl_surface = surface.try_grab(&mut state).unwrap();
            wl_surface.test_map(ObjectId::from(901usize), 40, 50, 100, 80);
        }
        state.mark_root_geometry_dirty(surface);
        let window = state.window_for_root(surface).unwrap();
        state
            .sync_window_geometry_goal(window, crate::visual_animation::WindowGeometryMotion::Warp);
        state.advance_visual_animations(Instant::now());

        let geometry = state.ensure_root_geometry_fresh(surface).unwrap();
        let logical_size = surface.try_ref(&state).unwrap().logical_size();
        let target = crate::root_geometry::project_window_target_geometry(geometry, logical_size);
        let right = target.root_pos.0 + target.resize_size.0;
        assert_eq!(target.root_pos, (40, 50));
        assert!(target.resize_size.0 > 40);

        state.seat.pointer_pos = (40.0, 90.0);
        state.seat.start_toplevel_resize(
            ClientId::from_raw(4),
            window,
            surface,
            crate::window::InteractiveSizeConstraints::default(),
            EDGE_LEFT,
            target,
        );
        state.note_floating_resize_anchor(window, EDGE_LEFT, target);
        state.seat.pointer_pos = (70.0, 90.0);
        let mut dispatcher = Dispatcher::default();
        assert!(crate::input::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
        assert_eq!(surface.try_ref(&state).unwrap().root_position(), (40, 50));

        {
            let mut wl_surface = surface.try_grab(&mut state).unwrap();
            wl_surface.test_map(
                ObjectId::from(902usize),
                40,
                50,
                target.resize_size.0 - 40,
                target.resize_size.1,
            );
        }
        state.mark_root_geometry_dirty(surface);
        assert!(state.apply_resize_anchor(surface));
        let moved = state.committed_root_geometry(surface).unwrap();
        let content = moved.content_rect_global();
        assert_eq!(moved.root_pos.0 + content.width, right);
        assert_eq!(moved.root_pos.1, target.root_pos.1);
        assert_ne!(moved.root_pos.0, target.root_pos.0);

        state.clear_resize_anchor_if_idle(surface);
        assert!(state
            .managed_window(surface)
            .unwrap()
            .resize_anchor
            .is_some());

        state.seat.clear_pointer_interactive_grab();
        state.clear_resize_anchor_if_idle(surface);
        assert!(state
            .managed_window(surface)
            .unwrap()
            .resize_anchor
            .is_none());

        state.note_floating_resize_anchor(window, EDGE_LEFT, target);
        {
            let xdg_surface = surface.try_ref(&state).unwrap().xdg_surface().unwrap();
            xdg_surface.grab(&mut state).test_arm_deferred_resize(7);
        }
        state.clear_resize_anchor_if_idle(surface);
        assert!(state
            .managed_window(surface)
            .unwrap()
            .resize_anchor
            .is_some());
    }

    #[test]
    fn clamped_resize_step_is_not_reported_again() {
        let (mut layout, drag) = two_tiled_windows();
        assert!(layout.apply_split_drag(&drag, 10_000));
        assert!(!layout.apply_split_drag(&drag, 20_000));
    }

    #[test]
    fn resize_step_back_to_start_is_reported() {
        let (mut layout, drag) = two_tiled_windows();
        assert!(layout.apply_split_drag(&drag, 10));
        assert!(layout.apply_split_drag(&drag, 0));
    }

    fn apply_bounds(layout: &mut WorkspaceLayout, width: i32, height: i32) {
        let bounds = RectI { x: 0, y: 0, width, height };
        apply_workspace_layout(
            layout,
            bounds,
            bounds,
            GapPolicy::default(),
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );
    }

    fn apply_handles(
        layout: &mut WorkspaceLayout,
        bounds: RectI,
        inner_gap: i32,
        border: i32,
        grab_extent: i32,
    ) {
        apply_workspace_layout(
            layout,
            bounds,
            bounds,
            GapPolicy { outer_gap: 0, inner_gap },
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams { border, grab_extent },
        );
    }

    #[test]
    fn split_handle_covers_both_borders_and_the_inner_gap() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(51);
        let right = WindowId::for_tests(52);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));
        apply_handles(&mut layout, RectI { x: 0, y: 0, width: 200, height: 100 }, 10, 2, 10);

        assert_eq!(layout.split_handles.len(), 1);
        let handle = layout.split_handles[0];
        assert_eq!(handle.axis, SplitAxis::Horizontal);
        assert_eq!(handle.rect, RectI { x: 93, y: 0, width: 14, height: 100 });
        assert_eq!(handle.center, 100.0);
        assert_eq!(handle.split, SplitRef { container: layout.root.unwrap(), first: 0 });

        let (splits, edges) = split_handles_at(&layout.split_handles, (100.0, 50.0), 10.0).unwrap();
        assert_eq!(splits.horizontal, Some(handle.split));
        assert!(splits.vertical.is_none());
        assert_eq!(edges, EDGE_RIGHT);
        assert!(split_handles_at(&layout.split_handles, (20.0, 50.0), 10.0).is_none());
        assert!(split_handles_at(&layout.split_handles, (92.0, 50.0), 10.0).is_none());
        assert!(split_handles_at(&layout.split_handles, (1.0, 50.0), 10.0).is_none());
    }

    #[test]
    fn split_handle_widens_to_the_grab_extent_when_border_and_gap_are_zero() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(53);
        let right = WindowId::for_tests(54);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));
        apply_handles(&mut layout, RectI { x: 0, y: 0, width: 200, height: 80 }, 0, 0, 10);

        assert_eq!(layout.split_handles.len(), 1);
        assert_eq!(layout.split_handles[0].rect, RectI { x: 95, y: 0, width: 10, height: 80 });
        assert_eq!(layout.split_handles[0].center, 100.0);
    }

    #[test]
    fn split_handle_corner_capture_adds_the_perpendicular_split() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(55);
        let top = WindowId::for_tests(56);
        let bottom = WindowId::for_tests(57);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(top));
        assert!(layout.insert_tiled(bottom));
        apply_handles(&mut layout, RectI { x: 0, y: 0, width: 200, height: 100 }, 0, 0, 10);

        let root = layout.root.unwrap();
        let vertical = match layout.nodes.get(&root).unwrap() {
            LayoutNode::Container(columns) => {
                assert_eq!(columns.axis, SplitAxis::Horizontal);
                columns.children[1].node_id
            }
            LayoutNode::Leaf(_) => panic!("root should be a split"),
        };
        assert_eq!(
            layout
                .nodes
                .get(&vertical)
                .and_then(LayoutNode::as_container)
                .map(|rows| rows.axis),
            Some(SplitAxis::Vertical)
        );
        let columns = SplitRef { container: root, first: 0 };
        let rows = SplitRef { container: vertical, first: 0 };

        let (near, near_edges) =
            split_handles_at(&layout.split_handles, (97.0, 30.0), 20.0).unwrap();
        assert_eq!(near, SplitPair { horizontal: Some(columns), vertical: Some(rows) });
        assert_eq!(near_edges, EDGE_BOTTOM | EDGE_RIGHT);

        let (far, far_edges) = split_handles_at(&layout.split_handles, (97.0, 5.0), 20.0).unwrap();
        assert_eq!(far, SplitPair { horizontal: Some(columns), vertical: None });
        assert_eq!(far_edges, EDGE_RIGHT);
    }

    #[test]
    fn fullscreen_and_a_single_tile_publish_no_split_handles() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(58);
        let bounds = RectI { x: 0, y: 0, width: 200, height: 100 };
        assert!(layout.insert_tiled(first));
        apply_handles(&mut layout, bounds, 10, 2, 10);
        assert!(layout.split_handles.is_empty());

        let second = WindowId::for_tests(59);
        assert!(layout.insert_tiled(second));
        apply_handles(&mut layout, bounds, 10, 2, 10);
        assert_eq!(layout.split_handles.len(), 1);
        assert!(layout.set_fullscreen(Some(first)));
        apply_handles(&mut layout, bounds, 10, 2, 10);
        assert!(layout.split_handles.is_empty());
    }

    /// `H[A, V[B, C]]` after three focused inserts.
    fn nested_stack() -> (WorkspaceLayout, WindowId, WindowId, WindowId) {
        let mut layout = WorkspaceLayout::default();
        let a = WindowId::for_tests(21);
        let b = WindowId::for_tests(22);
        let c = WindowId::for_tests(23);
        assert!(layout.insert_tiled(a));
        assert!(layout.insert_tiled(b));
        assert!(layout.insert_tiled(c));
        apply_bounds(&mut layout, 200, 100);
        let root = layout.root.expect("root");
        let LayoutNode::Container(columns) = layout.nodes.get(&root).unwrap() else {
            panic!("root should be a horizontal split");
        };
        assert_eq!(columns.axis, SplitAxis::Horizontal);
        let vertical = columns.children[1].node_id;
        let LayoutNode::Container(rows) = layout.nodes.get(&vertical).unwrap() else {
            panic!("second column should be a vertical stack");
        };
        assert_eq!(rows.axis, SplitAxis::Vertical);
        assert_eq!(rows.children[0].node_id, layout.window_leaves[&b]);
        assert_eq!(rows.children[1].node_id, layout.window_leaves[&c]);
        (layout, a, b, c)
    }

    fn vertical_stack_id(layout: &WorkspaceLayout) -> LayoutNodeId {
        let root = layout.root.expect("root");
        match layout.nodes.get(&root).unwrap() {
            LayoutNode::Container(columns) => columns.children[1].node_id,
            LayoutNode::Leaf(_) => panic!("root should be a container"),
        }
    }

    #[test]
    fn nested_edges_walk_to_the_ancestor_split() {
        let (layout, _a, _b, c) = nested_stack();
        let root = layout.root.unwrap();
        let between_columns = SplitRef { container: root, first: 0 };
        let between_rows = SplitRef { container: vertical_stack_id(&layout), first: 0 };

        assert_eq!(
            layout.splits_for_window_edges(c, EDGE_LEFT),
            SplitPair { horizontal: Some(between_columns), vertical: None }
        );
        assert!(layout.splits_for_window_edges(c, EDGE_RIGHT).is_empty());
        assert_eq!(
            layout.splits_for_window_edges(c, EDGE_TOP),
            SplitPair { horizontal: None, vertical: Some(between_rows) }
        );
        assert_eq!(
            layout.splits_for_window_edges(c, EDGE_TOP | EDGE_LEFT),
            SplitPair {
                horizontal: Some(between_columns),
                vertical: Some(between_rows),
            }
        );
    }

    #[test]
    fn ancestor_split_drag_resizes_every_leaf_under_both_children() {
        let (mut layout, a, b, c) = nested_stack();
        let before_a = layout.tiled_rect(a).unwrap().width;
        let before_b = layout.tiled_rect(b).unwrap().width;
        let before_c = layout.tiled_rect(c).unwrap().width;
        let split = layout
            .splits_for_window_edges(c, EDGE_LEFT)
            .horizontal
            .unwrap();
        let drag = layout.begin_split_drag(split).unwrap();

        assert!(layout.apply_split_drag(&drag, 40));
        apply_bounds(&mut layout, 200, 100);

        assert!(layout.tiled_rect(a).unwrap().width > before_a);
        assert!(layout.tiled_rect(b).unwrap().width < before_b);
        assert!(layout.tiled_rect(c).unwrap().width < before_c);
        assert_eq!(layout.split_drag_affected_windows(&drag), BTreeSet::from([a, b, c]));
    }

    #[test]
    fn split_drag_reresolves_ids_and_aborts_when_the_container_is_removed() {
        let (mut layout, a, b, c) = nested_stack();
        let vertical = vertical_stack_id(&layout);
        let split = layout
            .splits_for_window_edges(c, EDGE_LEFT)
            .horizontal
            .unwrap();
        let drag = layout.begin_split_drag(split).unwrap();
        assert!(layout.apply_split_drag(&drag, 40));

        let d = WindowId::for_tests(24);
        let leaf = layout.alloc_leaf(d, Some(vertical));
        {
            let LayoutNode::Container(container) = layout.nodes.get_mut(&vertical).unwrap() else {
                panic!("vertical stack");
            };
            container
                .children
                .push(LayoutChild { node_id: leaf, weight: 1.0 });
        }
        assert!(layout.split_drag_resolves(&drag));
        assert!(layout.apply_split_drag(&drag, 80));
        assert_eq!(layout.split_drag_affected_windows(&drag), BTreeSet::from([a, b, c, d]));

        assert!(layout.remove_window(b));
        assert!(layout.remove_window(c));
        assert!(layout.nodes.get(&vertical).is_none());
        assert!(!layout.split_drag_resolves(&drag));
        assert!(!layout.apply_split_drag(&drag, 120));
    }

    #[test]
    fn same_axis_nesting_uses_the_ancestor_split_on_the_outer_edge() {
        let mut layout = WorkspaceLayout::default();
        let a = WindowId::for_tests(31);
        let b = WindowId::for_tests(32);
        let c = WindowId::for_tests(33);
        assert!(layout.insert_tiled(a));
        assert!(layout.insert_tiled(b));
        assert!(layout.split_leaf_for_drag(b, c, SplitAxis::Horizontal, false, 0.5));

        let outer = layout.root.unwrap();
        let LayoutNode::Container(outer_node) = layout.nodes.get(&outer).unwrap() else {
            panic!("outer split");
        };
        assert_eq!(outer_node.axis, SplitAxis::Horizontal);
        let inner = outer_node.children[1].node_id;
        assert_eq!(
            layout.splits_for_window_edges(b, EDGE_RIGHT).horizontal,
            Some(SplitRef { container: inner, first: 0 })
        );
        assert_eq!(
            layout.splits_for_window_edges(b, EDGE_LEFT).horizontal,
            Some(SplitRef { container: outer, first: 0 })
        );
        assert!(layout.splits_for_window_edges(c, EDGE_RIGHT).is_empty());
    }

    #[test]
    fn resize_tiled_window_follows_ancestor_splits() {
        let (mut layout, a, b, c) = nested_stack();
        let a_width = layout.tiled_rect(a).unwrap().width;
        let b_height = layout.tiled_rect(b).unwrap().height;

        assert!(!layout.resize_tiled_window(c, EDGE_RIGHT, 40, 0));
        assert!(layout.resize_tiled_window(c, EDGE_LEFT, 40, 0));
        apply_bounds(&mut layout, 200, 100);
        assert!(layout.tiled_rect(a).unwrap().width > a_width);

        assert!(layout.resize_tiled_window(c, EDGE_TOP | EDGE_LEFT, 0, 40));
        apply_bounds(&mut layout, 200, 100);
        assert!(layout.tiled_rect(b).unwrap().height > b_height);
    }

    #[test]
    fn fullscreen_layout_applies_to_floating_surface() {
        let mut layout = WorkspaceLayout::default();
        let floating = WindowId::for_tests(3);
        layout.floating.stack.push(floating);
        assert!(layout.set_fullscreen(Some(floating)));

        let bounds = RectI { x: 10, y: 20, width: 300, height: 200 };
        let operations = apply_workspace_layout(
            &mut layout,
            bounds,
            bounds,
            GapPolicy::default(),
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        assert_eq!(
            operations,
            vec![LayoutOperation {
                window: floating,
                allocation: LayoutAllocation {
                    frame_rect: bounds,
                    tiled_edges: EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
                    fullscreen: true,
                },
            }]
        );
    }

    #[test]
    fn inserting_after_focus_can_create_vertical_subsplit() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(4);
        let second = WindowId::for_tests(5);
        let third = WindowId::for_tests(6);

        assert!(layout.insert_tiled(first));
        assert!(layout.insert_tiled(second));
        assert!(layout.insert_tiled(third));

        let root_id = layout.root.expect("root should exist");
        let root = layout
            .nodes
            .get(&root_id)
            .and_then(LayoutNode::as_container)
            .unwrap();
        assert_eq!(root.axis, SplitAxis::Horizontal);
        assert_eq!(root.children.len(), 2);

        let nested_id = root.children[1].node_id;
        let nested = layout
            .nodes
            .get(&nested_id)
            .and_then(LayoutNode::as_container)
            .unwrap();
        assert_eq!(nested.axis, SplitAxis::Vertical);
        assert_eq!(nested.children.len(), 2);
    }

    #[test]
    fn removing_focused_leaf_selects_remaining_tiled_focus() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(7);
        let second = WindowId::for_tests(8);

        assert!(layout.insert_tiled(first));
        assert!(layout.insert_tiled(second));
        assert!(layout.remove_window(second));

        let focused_leaf = layout
            .focused_leaf
            .expect("remaining tile should stay focused");
        let focused_surface = layout
            .nodes
            .get(&focused_leaf)
            .and_then(LayoutNode::as_leaf)
            .map(|leaf| leaf.window);
        assert_eq!(focused_surface, Some(first));
    }

    #[test]
    fn removing_last_leaf_from_nested_container_cleans_up_empty_ancestors() {
        let mut layout = WorkspaceLayout::default();
        let leaf_surface = WindowId::for_tests(9);
        let leaf_id = layout.alloc_leaf(leaf_surface, None);
        let inner_id = layout.alloc_container(
            SplitAxis::Vertical,
            None,
            vec![LayoutChild { node_id: leaf_id, weight: 1.0 }],
        );
        if let Some(node) = layout.nodes.get_mut(&leaf_id) {
            node.set_parent(Some(inner_id));
        }
        let root_id = layout.alloc_container(
            SplitAxis::Horizontal,
            None,
            vec![LayoutChild { node_id: inner_id, weight: 1.0 }],
        );
        if let Some(node) = layout.nodes.get_mut(&inner_id) {
            node.set_parent(Some(root_id));
        }
        layout.root = Some(root_id);

        assert!(layout.remove_window(leaf_surface));
        assert_eq!(layout.root, None);
        assert!(layout.nodes.is_empty());
    }

    #[test]
    fn layout_handles_minimums_larger_than_available_space() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(10);
        let right = WindowId::for_tests(11);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let bounds = RectI { x: 0, y: 0, width: 5, height: 20 };
        let min_sizes = BTreeMap::from([(left, (10, 1)), (right, (10, 1))]);
        let operations = apply_workspace_layout(
            &mut layout,
            bounds,
            bounds,
            GapPolicy::default(),
            &BTreeMap::default(),
            &min_sizes,
            SplitHandleParams::NONE,
        );

        assert_eq!(operations.len(), 2);
        let total_width = operations
            .iter()
            .map(|operation| operation.allocation.frame_rect.width)
            .sum::<i32>();
        assert_eq!(total_width, bounds.width);
    }

    #[test]
    fn outer_gap_shrinks_workspace_tiling_bounds() {
        let usable_bounds = RectI { x: 10, y: 20, width: 200, height: 100 };
        let bounds = outer_gapped_bounds(usable_bounds, GapPolicy { outer_gap: 8, inner_gap: 0 });

        assert_eq!(bounds, RectI { x: 18, y: 28, width: 184, height: 84 });
    }

    #[test]
    fn inner_gap_inserts_spacing_between_sibling_tiles() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(12);
        let right = WindowId::for_tests(13);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let operations = apply_workspace_layout(
            &mut layout,
            RectI { x: 0, y: 0, width: 100, height: 40 },
            RectI { x: 0, y: 0, width: 100, height: 40 },
            GapPolicy { outer_gap: 0, inner_gap: 10 },
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        assert_eq!(operations.len(), 2);
        assert_eq!(
            operations[0].allocation.frame_rect.x2() + 10,
            operations[1].allocation.frame_rect.x
        );
    }

    #[test]
    fn tiled_edges_are_relative_to_gapped_tiling_bounds() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(14);
        let right = WindowId::for_tests(15);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let operations = apply_workspace_layout(
            &mut layout,
            RectI { x: 5, y: 7, width: 90, height: 30 },
            RectI { x: 0, y: 0, width: 100, height: 40 },
            GapPolicy::default(),
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        let left_leaf_id = layout.window_leaves[&left];
        let right_leaf_id = layout.window_leaves[&right];
        let left_edges = layout.nodes[&left_leaf_id].as_leaf().unwrap().tiled_edges;
        let right_edges = layout.nodes[&right_leaf_id].as_leaf().unwrap().tiled_edges;

        assert_eq!(operations[0].allocation.frame_rect.y, 7);
        assert!(left_edges & EDGE_LEFT != 0);
        assert!(left_edges & EDGE_RIGHT != 0);
        assert!(left_edges & EDGE_TOP != 0);
        assert!(left_edges & EDGE_BOTTOM != 0);
        assert!(right_edges & EDGE_LEFT != 0);
        assert!(right_edges & EDGE_RIGHT != 0);
        assert!(right_edges & EDGE_TOP != 0);
        assert!(right_edges & EDGE_BOTTOM != 0);
    }

    #[test]
    fn decoration_insets_shrink_tiled_content_rect() {
        let frame = RectI { x: 10, y: 20, width: 100, height: 80 };
        let content = content_rect_for_tiled_frame(
            frame,
            DecorationInsets { top: 3, right: 4, bottom: 5, left: 6 },
        );

        assert_eq!(content, RectI { x: 16, y: 23, width: 90, height: 72 });
    }

    #[test]
    fn tiled_geometry_uses_content_size_for_server_side_configures() {
        let mut geometry = test_committed_geometry(
            RectI { x: 20, y: 20, width: 1240, height: 760 },
            RectI { x: 25, y: 25, width: 1230, height: 750 },
            EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
            false,
            (25, 25),
        );
        geometry.decoration_insets = DecorationInsets { left: 5, top: 5, right: 5, bottom: 5 };
        let configure_geometry = crate::root_geometry::project_configure_geometry(geometry);

        assert_eq!(configure_geometry.size, (1230, 750));
    }

    #[test]
    fn window_geometry_derives_per_edge_decoration_insets() {
        let surface = WlSurface::new(WlSurfaceId::from(30u32), ClientId::from_raw(31), 6);
        let mut state = state_for_tests();
        let (surface_resolved, _) =
            Client::new_for_tests(ClientId::from_raw(32)).insert_object(&mut state, surface);
        let surface_object_id = surface_resolved.object_id();
        {
            let mut surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            surface.test_map(ObjectId::from(901usize), 0, 0, 80, 60);
            let insets = root_geometry::decoration_insets_from_window_geometry(
                &surface,
                RectI { x: -4, y: -12, width: 90, height: 80 },
            )
            .unwrap();
            assert_eq!(insets, DecorationInsets { top: 12, right: 6, bottom: 8, left: 4 });
        }
    }

    #[test]
    fn window_geometry_positive_origin_derives_csd_title_bar_insets() {
        let insets = root_geometry::decoration_insets_from_window_geometry_and_logical_size(
            SizeI { width: 800, height: 600 },
            RectI { x: 0, y: 32, width: 800, height: 568 },
        )
        .unwrap();
        assert_eq!(insets, DecorationInsets { top: 32, right: 0, bottom: 0, left: 0 });
    }

    #[test]
    fn tiled_root_position_aligns_negative_origin_geometry_to_frame_rect() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(44));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(45u32),
                XdgSurfaceId::from(46u32),
                XdgToplevelId::from(47u32),
                (80, 60),
            );

        let (child_resolved_insert, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(48u32), client.client_id(), 6),
        );
        let child_surface_object_id = child_resolved_insert.object_id();
        let child_subsurface = insert_subsurface(
            &mut state,
            WlSubsurfaceId::from(49u32),
            child_surface_object_id,
            surface_object_id,
        );
        let parent_resolved = root(&state, surface_object_id);
        let child_resolved = root(&state, child_surface_object_id);
        {
            let root_surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.add_pending_child(child_resolved);
        }
        {
            let child_surface = state
                .objects
                .get_mut(child_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, parent_resolved);
            child_surface.set_subsurface_position(-4, -12);
            child_surface.test_map(ObjectId::from(901usize), 0, 0, 90, 80);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ClientSide);
            toplevel.set_window_geometry(Some(RectI { x: -4, y: -12, width: 90, height: 80 }));
        }

        let surface = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap();
        let geometry = test_committed_geometry(
            RectI { x: 10, y: 20, width: 90, height: 80 },
            RectI { x: 14, y: 32, width: 80, height: 60 },
            EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
            false,
            (14, 32),
        );

        let _ = surface;
        assert_eq!(geometry.root_pos, (14, 32));
    }

    #[test]
    fn tiled_root_position_aligns_positive_origin_geometry_to_frame_rect() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(60));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(61u32),
                XdgSurfaceId::from(62u32),
                XdgToplevelId::from(63u32),
                (800, 600),
            );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ClientSide);
            toplevel.set_window_geometry(Some(RectI { x: 0, y: 32, width: 800, height: 568 }));
        }

        let surface = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap();
        let geometry = test_committed_geometry(
            RectI { x: 10, y: 20, width: 800, height: 568 },
            RectI { x: 10, y: 52, width: 800, height: 536 },
            EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
            false,
            (10, -12),
        );

        let _ = surface;
        assert_eq!(geometry.root_pos, (10, -12));
    }

    #[test]
    fn tiled_root_position_uses_window_geometry_origin_for_server_side_decorations() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(64));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(65u32),
                XdgSurfaceId::from(66u32),
                XdgToplevelId::from(67u32),
                (360, 750),
            );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let xdg_surface = state
                .objects
                .get_mut(xdg_surface_object_id)
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_current_window_geometry(Some(RectI {
                x: 25,
                y: 25,
                width: 360,
                height: 750,
            }));
        }
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_window_geometry(Some(RectI { x: 25, y: 25, width: 360, height: 750 }));
        }
        {
            let surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(902usize), 0, 0, 410, 800);
        }

        let surface_id = root(&state, surface_object_id);
        let surface = state
            .objects
            .try_ref_object::<WlSurface>(surface_object_id)
            .unwrap();
        let geometry = crate::root_geometry::build_committed_root_geometry(
            &state,
            surface_id,
            surface,
            Some(LayoutAllocation {
                frame_rect: RectI { x: 645, y: 20, width: 615, height: 760 },
                tiled_edges: EDGE_TOP | EDGE_BOTTOM | EDGE_LEFT | EDGE_RIGHT,
                fullscreen: false,
            }),
            crate::root_geometry::GeometryBuildOverrides {
                decoration_mode: None,
                explicit_window_geometry: None,
                decoration_insets: Some(DecorationInsets { top: 5, right: 5, bottom: 5, left: 5 }),
                root_pos: Some(surface.root_position()),
            },
        )
        .expect("committed geometry");

        assert_eq!(geometry.root_pos, (625, 0));
        assert_eq!(geometry.logical_window_rect_global(), RectI::new(650, 25, 360, 750));
        assert_eq!(
            geometry.content_rect_global(),
            RectI { x: 650, y: 25, width: 605, height: 750 }
        );
    }

    #[test]
    fn effective_decoration_insets_fall_back_to_config_without_explicit_geometry() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 7;
        let mut client = Client::new_for_tests(ClientId::from_raw(40));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, _toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(41u32),
                XdgSurfaceId::from(42u32),
                XdgToplevelId::from(43u32),
                (80, 60),
            );

        assert_eq!(
            root_geometry::effective_decoration_insets(&state, root(&state, surface_object_id),),
            DecorationInsets { top: 7, right: 7, bottom: 7, left: 7 }
        );
    }

    #[test]
    fn effective_decoration_insets_ignore_csd_window_geometry() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 3;
        let mut client = Client::new_for_tests(ClientId::from_raw(50));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(51u32),
                XdgSurfaceId::from(52u32),
                XdgToplevelId::from(53u32),
                (80, 60),
            );
        let (child_resolved_insert, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(54u32), client.client_id(), 6),
        );
        let child_surface_object_id = child_resolved_insert.object_id();
        let child_subsurface = insert_subsurface(
            &mut state,
            WlSubsurfaceId::from(55u32),
            child_surface_object_id,
            surface_object_id,
        );
        let parent_resolved = root(&state, surface_object_id);
        let child_resolved = root(&state, child_surface_object_id);
        {
            let root_surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.add_pending_child(child_resolved);
        }
        {
            let child_surface = state
                .objects
                .get_mut(child_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, parent_resolved);
            child_surface.set_subsurface_position(-4, -12);
            child_surface.test_map(ObjectId::from(901usize), 0, 0, 90, 80);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ClientSide);
            toplevel.set_window_geometry(Some(RectI { x: -4, y: -12, width: 90, height: 80 }));
        }

        assert_eq!(
            root_geometry::effective_decoration_insets(&state, root(&state, surface_object_id),),
            DecorationInsets { top: 3, right: 3, bottom: 3, left: 3 }
        );
    }

    #[test]
    fn effective_decoration_insets_ignore_oversized_window_geometry() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 3;
        let mut client = Client::new_for_tests(ClientId::from_raw(55));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(56u32),
                XdgSurfaceId::from(57u32),
                XdgToplevelId::from(58u32),
                (80, 60),
            );
        let (child_resolved_insert, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(59u32), client.client_id(), 6),
        );
        let child_surface_object_id = child_resolved_insert.object_id();
        let child_subsurface = insert_subsurface(
            &mut state,
            WlSubsurfaceId::from(60u32),
            child_surface_object_id,
            surface_object_id,
        );
        let parent_resolved = root(&state, surface_object_id);
        let child_resolved = root(&state, child_surface_object_id);
        {
            let root_surface = state
                .objects
                .get_mut(surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.add_pending_child(child_resolved);
        }
        {
            let child_surface = state
                .objects
                .get_mut(child_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, parent_resolved);
            child_surface.set_subsurface_position(-4, -12);
            child_surface.test_map(ObjectId::from(902usize), 0, 0, 90, 80);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(surface_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_decoration_mode(ToplevelDecorationMode::ClientSide);
            toplevel.set_window_geometry(Some(RectI { x: -10, y: -20, width: 120, height: 100 }));
        }

        assert_eq!(
            root_geometry::effective_decoration_insets(&state, root(&state, surface_object_id),),
            DecorationInsets { top: 3, right: 3, bottom: 3, left: 3 }
        );
    }

    #[test]
    fn server_side_decoration_mode_uses_configured_fallback_insets() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 5;
        let mut client = Client::new_for_tests(ClientId::from_raw(60));
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (surface_object_id, _xdg_surface_object_id, toplevel_object_id) =
            insert_root_xdg_toplevel(
                &mut state,
                &mut client,
                &mut client_dispatcher,
                WlSurfaceId::from(61u32),
                XdgSurfaceId::from(62u32),
                XdgToplevelId::from(63u32),
                (80, 60),
            );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.set_window_geometry(Some(RectI { x: -7, y: -9, width: 94, height: 74 }));
        }
        state
            .objects
            .get_mut(toplevel_object_id)
            .downcast_mut::<XdgToplevel>()
            .unwrap()
            .test_set_decoration_mode(ToplevelDecorationMode::ServerSide);

        assert_eq!(
            root_geometry::effective_decoration_insets(&state, root(&state, surface_object_id),),
            DecorationInsets { top: 5, right: 5, bottom: 5, left: 5 }
        );
    }

    #[test]
    fn fullscreen_layout_ignores_gaps_and_insets() {
        let mut layout = WorkspaceLayout::default();
        let surface = WindowId::for_tests(16);
        layout.floating.stack.push(surface);
        assert!(layout.set_fullscreen(Some(surface)));

        let fullscreen_bounds = RectI { x: 3, y: 4, width: 120, height: 70 };
        let operations = apply_workspace_layout(
            &mut layout,
            RectI { x: 20, y: 30, width: 80, height: 20 },
            fullscreen_bounds,
            GapPolicy { outer_gap: 10, inner_gap: 7 },
            &BTreeMap::from([(surface, DecorationInsets { top: 2, right: 2, bottom: 2, left: 2 })]),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        assert_eq!(operations[0].allocation.frame_rect, fullscreen_bounds);
    }

    #[test]
    fn mixed_decoration_insets_shrink_each_leaf_independently() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(160);
        let right = WindowId::for_tests(161);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let operations = apply_workspace_layout(
            &mut layout,
            RectI { x: 0, y: 0, width: 120, height: 80 },
            RectI { x: 0, y: 0, width: 120, height: 80 },
            GapPolicy::default(),
            &BTreeMap::from([
                (left, DecorationInsets { top: 10, right: 0, bottom: 0, left: 0 }),
                (right, DecorationInsets::default()),
            ]),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        assert_eq!(operations.len(), 2);
        assert_eq!(
            content_rect_for_tiled_frame(
                operations[0].allocation.frame_rect,
                DecorationInsets { top: 10, right: 0, bottom: 0, left: 0 }
            )
            .y,
            10
        );
        assert_eq!(
            content_rect_for_tiled_frame(
                operations[1].allocation.frame_rect,
                DecorationInsets::default()
            )
            .y,
            0
        );
    }

    #[test]
    fn minimum_size_fallback_accounts_for_inner_gaps() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(17);
        let right = WindowId::for_tests(18);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let operations = apply_workspace_layout(
            &mut layout,
            RectI { x: 0, y: 0, width: 8, height: 20 },
            RectI { x: 0, y: 0, width: 8, height: 20 },
            GapPolicy { outer_gap: 0, inner_gap: 4 },
            &BTreeMap::default(),
            &BTreeMap::from([(left, (10, 1)), (right, (10, 1))]),
            SplitHandleParams::NONE,
        );

        let total_width = operations
            .iter()
            .map(|operation| operation.allocation.frame_rect.width)
            .sum::<i32>();
        let gap = operations[1].allocation.frame_rect.x - operations[0].allocation.frame_rect.x2();
        assert_eq!(total_width + gap, 8);
    }

    #[test]
    fn node_minimum_size_uses_per_surface_decoration_insets() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(170);
        let right = WindowId::for_tests(171);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));

        let minimum = node_minimum_size(
            &layout,
            layout.root.expect("root should exist"),
            None,
            GapPolicy::default(),
            &BTreeMap::from([
                (left, DecorationInsets { top: 0, right: 4, bottom: 0, left: 2 }),
                (right, DecorationInsets { top: 0, right: 1, bottom: 0, left: 1 }),
            ]),
            &BTreeMap::from([(left, (10, 1)), (right, (10, 1))]),
        );

        assert_eq!(minimum, (28, 1));
    }

    #[test]
    fn nested_container_minimum_uses_effective_gap_from_size_hint() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(19);
        let second = WindowId::for_tests(20);
        let third = WindowId::for_tests(21);
        let first_id = layout.alloc_leaf(first, None);
        let second_id = layout.alloc_leaf(second, None);
        let third_id = layout.alloc_leaf(third, None);
        let container_id = layout.alloc_container(
            SplitAxis::Horizontal,
            None,
            vec![
                LayoutChild { node_id: first_id, weight: 1.0 },
                LayoutChild { node_id: second_id, weight: 1.0 },
                LayoutChild { node_id: third_id, weight: 1.0 },
            ],
        );
        for child_id in [first_id, second_id, third_id] {
            if let Some(node) = layout.nodes.get_mut(&child_id) {
                node.set_parent(Some(container_id));
            }
        }

        let minimum = node_minimum_size(
            &layout,
            container_id,
            Some(LayoutSizeHint { width: 5, height: 30 }),
            GapPolicy { outer_gap: 0, inner_gap: 10 },
            &BTreeMap::default(),
            &BTreeMap::default(),
        );

        assert_eq!(minimum, (7, 1));
    }

    #[test]
    fn nested_splits_propagate_internal_tiled_edge_constraints() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(22);
        let second = WindowId::for_tests(23);
        let third = WindowId::for_tests(24);
        assert!(layout.insert_tiled(first));
        assert!(layout.insert_tiled(second));
        assert!(layout.insert_tiled(third));

        apply_workspace_layout(
            &mut layout,
            RectI { x: 0, y: 0, width: 120, height: 80 },
            RectI { x: 0, y: 0, width: 120, height: 80 },
            GapPolicy::default(),
            &BTreeMap::default(),
            &BTreeMap::default(),
            SplitHandleParams::NONE,
        );

        let first_edges = layout.nodes[&layout.window_leaves[&first]]
            .as_leaf()
            .unwrap()
            .tiled_edges;
        let second_edges = layout.nodes[&layout.window_leaves[&second]]
            .as_leaf()
            .unwrap()
            .tiled_edges;
        let third_edges = layout.nodes[&layout.window_leaves[&third]]
            .as_leaf()
            .unwrap()
            .tiled_edges;

        assert_eq!(first_edges, EDGE_LEFT | EDGE_RIGHT | EDGE_TOP | EDGE_BOTTOM);
        assert_eq!(second_edges, EDGE_LEFT | EDGE_RIGHT | EDGE_TOP | EDGE_BOTTOM);
        assert_eq!(third_edges, EDGE_LEFT | EDGE_RIGHT | EDGE_TOP | EDGE_BOTTOM);
    }

    fn set_tiled_rect(layout: &mut WorkspaceLayout, surface: WindowId, rect: RectI) {
        let leaf_id = layout.window_leaves[&surface];
        let LayoutNode::Leaf(leaf) = layout.nodes.get_mut(&leaf_id).unwrap() else {
            panic!("surface should be a tiled leaf");
        };
        leaf.last_applied_rect = Some(rect);
    }

    fn sibling_split(
        layout: &WorkspaceLayout,
        surface: WindowId,
    ) -> (SplitAxis, Vec<(WindowId, f64)>) {
        let leaf_id = layout.window_leaves[&surface];
        let parent_id = layout.nodes[&leaf_id].parent().unwrap();
        let container = layout.nodes[&parent_id].as_container().unwrap();
        let children = container
            .children
            .iter()
            .map(|child| {
                let LayoutNode::Leaf(leaf) = &layout.nodes[&child.node_id] else {
                    panic!("drag split should place two leaves under the new parent");
                };
                (leaf.window, child.weight)
            })
            .collect();
        (container.axis, children)
    }

    fn assert_tree_links(layout: &WorkspaceLayout) {
        let Some(root) = layout.root else {
            assert!(layout.nodes.is_empty());
            assert!(layout.window_leaves.is_empty());
            assert!(layout.focused_leaf.is_none());
            return;
        };
        assert!(layout.nodes[&root].parent().is_none());
        let mut seen = BTreeSet::new();
        fn visit(
            layout: &WorkspaceLayout,
            node_id: LayoutNodeId,
            seen: &mut BTreeSet<LayoutNodeId>,
        ) {
            assert!(seen.insert(node_id), "layout node is reachable twice");
            match &layout.nodes[&node_id] {
                LayoutNode::Leaf(leaf) => {
                    assert_eq!(layout.window_leaves.get(&leaf.window), Some(&node_id));
                }
                LayoutNode::Container(container) => {
                    assert!(container.children.len() >= 2);
                    for (index, child) in container.children.iter().enumerate() {
                        assert_eq!(layout.nodes[&child.node_id].parent(), Some(node_id));
                        assert!(child.weight >= MIN_CHILD_WEIGHT);
                        if container.focused_child == Some(index) {
                            assert!(layout.nodes.contains_key(&child.node_id));
                        }
                        visit(layout, child.node_id, seen);
                    }
                }
            }
        }
        visit(layout, root, &mut seen);
        assert_eq!(seen.len(), layout.nodes.len());
        if let Some(focused) = layout.focused_leaf {
            assert!(matches!(layout.nodes.get(&focused), Some(LayoutNode::Leaf(_))));
        }
        for surface in layout.tiled_windows() {
            assert!(layout.is_tiled_window(surface));
        }
    }

    #[test]
    fn extracting_the_only_tiled_window_leaves_the_tree_unchanged() {
        let mut layout = WorkspaceLayout::default();
        let only = WindowId::for_tests(30);
        assert!(layout.insert_tiled(only));
        layout.dirty = false;

        assert!(layout.extract_tiled_leaf(only).is_none());
        assert!(layout.is_tiled_window(only));
        assert_eq!(layout.root, layout.focused_leaf);
        assert!(!layout.dirty);
        assert_tree_links(&layout);
    }

    #[test]
    fn extracting_a_nested_leaf_collapses_its_parent_and_restore_puts_it_back() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(31);
        let second = WindowId::for_tests(32);
        let third = WindowId::for_tests(33);
        assert!(layout.insert_tiled(first));
        assert!(layout.insert_tiled(second));
        assert!(layout.insert_tiled(third));
        let before = layout.clone();

        let extracted = layout
            .extract_tiled_leaf(third)
            .expect("third window is tiled");
        assert!(!layout.is_tiled_window(third));
        assert_eq!(layout.tiled_windows(), vec![first, second]);
        let root = layout.nodes[&layout.root.unwrap()].as_container().unwrap();
        assert_eq!(root.axis, SplitAxis::Horizontal);
        assert_eq!(root.children.len(), 2);
        assert_eq!(layout.focused_leaf, Some(layout.window_leaves[&second]));
        assert_ne!(layout.focused_leaf, before.focused_leaf);
        assert_tree_links(&layout);

        layout.restore_extracted_tiled_leaf(extracted);
        assert_eq!(layout.tiled_windows(), before.tiled_windows());
        assert_eq!(layout.focused_leaf, before.focused_leaf);
        assert_eq!(sibling_split(&layout, third).0, SplitAxis::Vertical);
        assert_tree_links(&layout);
    }

    #[test]
    fn dropping_selects_every_side_and_keeps_a_custom_ratio_on_the_window_that_stays() {
        let mut layout = WorkspaceLayout::default();
        let stayed = WindowId::for_tests(34);
        let dragged = WindowId::for_tests(35);
        assert!(layout.insert_tiled(stayed));
        assert!(layout.insert_tiled(dragged));
        let wide = RectI { x: 0, y: 0, width: 200, height: 100 };
        set_tiled_rect(&mut layout, stayed, wide);

        let cases = [
            (10.0, 50.0, false, SplitAxis::Horizontal, true),
            (190.0, 50.0, false, SplitAxis::Horizontal, false),
            (100.0, 10.0, true, SplitAxis::Vertical, true),
            (100.0, 90.0, true, SplitAxis::Vertical, false),
        ];
        for (pointer_x, pointer_y, precise, axis, dragged_is_first) in cases {
            let mut candidate = layout.clone();
            let extracted = candidate.extract_tiled_leaf(dragged).unwrap();
            candidate
                .drop_extracted_tiled_leaf(extracted, pointer_x, pointer_y, precise, 1.4)
                .expect("wide tile is a destination");
            let (split_axis, children) = sibling_split(&candidate, dragged);
            assert_eq!(split_axis, axis);
            let dragged_index = if dragged_is_first { 0 } else { 1 };
            assert_eq!(children[dragged_index].0, dragged);
            assert_eq!(children[1 - dragged_index].0, stayed);
            assert!((children[1 - dragged_index].1 - 1.4).abs() < 1e-5);
            assert!((children[dragged_index].1 - 0.6).abs() < 1e-5);
            assert_eq!(candidate.focused_leaf, Some(candidate.window_leaves[&dragged]));
            assert!(candidate.dirty);
            assert_tree_links(&candidate);
        }
    }

    #[test]
    fn precise_drop_uses_the_pointer_quadrant_of_a_tall_tile() {
        let mut layout = WorkspaceLayout::default();
        let stayed = WindowId::for_tests(36);
        let dragged = WindowId::for_tests(37);
        assert!(layout.insert_tiled(stayed));
        assert!(layout.insert_tiled(dragged));
        set_tiled_rect(&mut layout, stayed, RectI { x: 0, y: 0, width: 100, height: 400 });
        let extracted = layout.extract_tiled_leaf(dragged).unwrap();
        layout
            .drop_extracted_tiled_leaf(extracted, 90.0, 210.0, true, 1.0)
            .unwrap();

        let (axis, children) = sibling_split(&layout, dragged);
        assert_eq!(axis, SplitAxis::Horizontal);
        assert_eq!(children[0].0, stayed);
        assert_eq!(children[1].0, dragged);
        assert_tree_links(&layout);
    }

    #[test]
    fn empty_space_drops_on_the_nearest_tiled_leaf_and_ignores_floating_windows() {
        let mut layout = WorkspaceLayout::default();
        let left = WindowId::for_tests(38);
        let right = WindowId::for_tests(39);
        let dragged = WindowId::for_tests(40);
        let floating = WindowId::for_tests(41);
        assert!(layout.insert_tiled(left));
        assert!(layout.insert_tiled(right));
        assert!(layout.insert_tiled(dragged));
        assert!(layout.set_floating(floating, true));
        set_tiled_rect(&mut layout, left, RectI { x: 0, y: 0, width: 100, height: 100 });
        set_tiled_rect(&mut layout, right, RectI { x: 400, y: 0, width: 100, height: 100 });

        let extracted = layout.extract_tiled_leaf(dragged).unwrap();
        layout
            .drop_extracted_tiled_leaf(extracted, 390.0, 200.0, false, 1.0)
            .unwrap();

        let (_, children) = sibling_split(&layout, dragged);
        assert!(children.iter().any(|(surface, _)| *surface == right));
        assert!(children.iter().all(|(surface, _)| *surface != left));
        assert!(layout.is_floating_window(floating));
        assert!(!layout.is_tiled_window(floating));
        assert_tree_links(&layout);
    }

    #[test]
    fn a_failed_drop_can_restore_the_extracted_window() {
        let mut layout = WorkspaceLayout::default();
        let stayed = WindowId::for_tests(42);
        let dragged = WindowId::for_tests(43);
        assert!(layout.insert_tiled(stayed));
        assert!(layout.insert_tiled(dragged));
        let before = layout.clone();
        let extracted = layout.extract_tiled_leaf(dragged).unwrap();

        let extracted = layout
            .drop_extracted_tiled_leaf(extracted, 10.0, 10.0, false, 1.0)
            .expect_err("a tile without a rect is not a destination");
        assert!(!layout.is_tiled_window(dragged));
        layout.restore_extracted_tiled_leaf(extracted);
        assert_eq!(layout.tiled_windows(), before.tiled_windows());
        assert_eq!(layout.focused_leaf, before.focused_leaf);
        assert_tree_links(&layout);
    }

    #[test]
    fn dropping_into_a_nested_tree_keeps_parent_links() {
        let mut layout = WorkspaceLayout::default();
        let first = WindowId::for_tests(44);
        let second = WindowId::for_tests(45);
        let third = WindowId::for_tests(46);
        assert!(layout.insert_tiled(first));
        assert!(layout.insert_tiled(second));
        assert!(layout.insert_tiled(third));
        set_tiled_rect(&mut layout, first, RectI { x: 0, y: 0, width: 300, height: 100 });
        let extracted = layout.extract_tiled_leaf(third).unwrap();
        layout
            .drop_extracted_tiled_leaf(extracted, 250.0, 40.0, false, 1.0)
            .unwrap();

        let first_parent = layout.nodes[&layout.window_leaves[&first]]
            .parent()
            .unwrap();
        let third_parent = layout.nodes[&layout.window_leaves[&third]]
            .parent()
            .unwrap();
        assert_eq!(first_parent, third_parent);
        assert_ne!(layout.nodes[&layout.window_leaves[&second]].parent(), Some(first_parent));
        assert_eq!(layout.nodes[&first_parent].parent(), Some(layout.root.unwrap()));
        assert_tree_links(&layout);
    }
}
