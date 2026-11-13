use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    time::Instant,
};

use crate::{
    clients::{
        active_cursor_surface_refresh_params, active_dnd_icon_surface_refresh_params,
        root_surface_outputs_at, Client, ClientAction, ClientId, Resolved,
    },
    config,
    cursor::{CursorSourceRef, CursorState},
    drm::request_outputs_for_points,
    input::{
        apply_pointer_press_keyboard_focus, maybe_dismiss_popup_grab_on_button,
        popup_keyboard_focus, refresh_drag_focus, set_keyboard_focus, ActiveGrabState, ImeState,
        PointerInteractiveGrab, ScenePick, SurfaceFocusPick, ToplevelDragEnd, ToplevelDragState,
        WindowHit,
    },
    protocol_generated::{
        Array, ExtDataControlDeviceV1Id, WaylandId, WlDataDeviceId, WlDataOfferId, WlDataSourceId,
        WlKeyboardId, WlPointerId, WlSeatId, WlSurfaceId, XdgPopupId, XdgToplevelId,
        ZwlrDataControlDeviceV1Id, ZwpConfinedPointerV1Id, ZwpLockedPointerV1Id,
        ZwpPrimarySelectionDeviceV1Id, ZwpRelativePointerV1Id,
    },
    root_geometry::WindowTargetGeometry,
    runner::Dispatcher,
    state::{
        DragNegotiationState, DragPhase, DragState, DragTargetState, OutputId,
        PrimarySelectionSourceState, SelectionSourceState, State, WorkspaceId,
    },
    tiling::SplitDragPair,
    wayland_objects::{
        xdg_surface::xdg_toplevel::{InteractiveSizeConstraints, ToplevelDecorationMode},
        zwlr_layer_surface_v1::LayerLayoutObjectSnapshot,
    },
    xkb::{KeyboardModifiers, KeymapBlob},
};

pub use crate::input::SurfaceFocus;
#[cfg(test)]
pub(crate) use crate::input::{
    surface_focus_pick_at, types::PointerFocus, CompositorZone, SurfaceRect,
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

#[derive(Clone, Debug)]
pub enum SeatClientEvent {
    PointerEnter {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        x: f32,
        y: f32,
    },
    PointerLeave {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
    },
    PointerMotion {
        time: u32,
        x: f32,
        y: f32,
    },
    /// Compositor-caused pointer relocation. Version 11 pointers receive `wl_pointer.warp`;
    /// older pointers receive `wl_pointer.motion` so they still learn the position.
    PointerWarp {
        time: u32,
        x: f32,
        y: f32,
    },
    PointerButton {
        serial: u32,
        time: u32,
        button: u32,
        state: u32,
    },
    /// One hardware scroll, expanded per pointer version into a single logical frame.
    PointerScroll(PointerScrollFrame),
    PointerFrame,
    PointerLockLocked {
        lock: Resolved<ZwpLockedPointerV1Id>,
    },
    PointerLockUnlocked {
        lock: Resolved<ZwpLockedPointerV1Id>,
    },
    PointerConfineConfined {
        confine: Resolved<ZwpConfinedPointerV1Id>,
    },
    PointerConfineUnconfined {
        confine: Resolved<ZwpConfinedPointerV1Id>,
    },
    /// Unclipped relative motion for `zwp_relative_pointer_v1` resources of the focused client.
    RelativePointerMotion {
        utime_hi: u32,
        utime_lo: u32,
        dx: f32,
        dy: f32,
        dx_unaccel: f32,
        dy_unaccel: f32,
    },
    /// `begin` for the focused client's gesture objects of `kind`.
    PointerGestureBegin {
        kind: crate::input::gesture::PointerGestureKind,
        serial: u32,
        time: u32,
        surface: Resolved<WlSurfaceId>,
        fingers: u32,
    },
    /// Logical deltas; the client applies its wire scale.
    PointerGestureSwipeUpdate {
        time: u32,
        dx: f32,
        dy: f32,
    },
    /// `scale` is absolute since `begin`; `rotation` is degrees clockwise since the previous event.
    PointerGesturePinchUpdate {
        time: u32,
        dx: f32,
        dy: f32,
        scale: f32,
        rotation: f32,
    },
    PointerGestureEnd {
        kind: crate::input::gesture::PointerGestureKind,
        serial: u32,
        time: u32,
        cancelled: bool,
    },
    KeyboardEnter {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        keys: Array,
    },
    KeyboardLeave {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
    },
    KeyboardKey {
        serial: u32,
        time: u32,
        key: u32,
        state: u32,
    },
    KeyboardModifiers {
        serial: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    },
    /// The focused client is on a different keymap. `keys` empty means send
    /// the keymap while the client stays entered. Otherwise `leave`, keymap,
    /// `enter(keys)`.
    KeyboardKeymap {
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        keymap: Rc<KeymapBlob>,
        keys: Array,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    },
}

/// Physical source of one scroll frame. Device configuration such as natural scrolling stays in
/// libinput; this only names the source reported to the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerScrollSource {
    Wheel,
    Finger,
    Continuous,
    WheelTilt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerScrollAxis {
    Vertical,
    Horizontal,
}

/// Physical motion compared with the logical axis value. Version 9 pointers receive this beside
/// the matching `wl_pointer.axis` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerScrollDirection {
    Identical,
    Inverted,
}

/// One axis inside a scroll frame. A stop carries no distance: `axis_value120` must not be zero,
/// and a stop is not coupled to an axis event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerAxisChange {
    Scroll {
        value: f32,
        /// Wheel steps for `wl_pointer` versions 5 through 7. Versions 8 and newer use `value120`.
        discrete: Option<i32>,
        /// High-resolution wheel distance in multiples of 120. Zero is omitted.
        value120: Option<i32>,
        relative_direction: Option<PointerScrollDirection>,
    },
    Stop,
}

/// Scroll data shared by every pointer resource of the focused client.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerScrollFrame {
    pub time: u32,
    pub source: PointerScrollSource,
    pub vertical: Option<PointerAxisChange>,
    pub horizontal: Option<PointerAxisChange>,
}

/// Version-independent scroll events. [`pointer_scroll_protocol_events`] drops the ones a pointer
/// version cannot receive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerScrollWireEvent {
    Source(PointerScrollSource),
    Discrete { axis: PointerScrollAxis, discrete: i32 },
    Value120 { axis: PointerScrollAxis, value120: i32 },
    RelativeDirection { axis: PointerScrollAxis, direction: PointerScrollDirection },
    Axis { time: u32, axis: PointerScrollAxis, value_bits: u32 },
    Stop { time: u32, axis: PointerScrollAxis },
    Frame,
}

pub(crate) fn pointer_scroll_protocol_events(
    version: u32,
    frame: &PointerScrollFrame,
) -> Vec<PointerScrollWireEvent> {
    use crate::protocol_generated::wl_pointer::{
        AXIS_RELATIVE_DIRECTION_SINCE, AXIS_SOURCE_SINCE, AXIS_STOP_SINCE, AXIS_VALUE120_SINCE,
        FRAME_SINCE,
    };

    let present = [
        frame
            .vertical
            .map(|change| (PointerScrollAxis::Vertical, change)),
        frame
            .horizontal
            .map(|change| (PointerScrollAxis::Horizontal, change)),
    ];
    if present.iter().all(Option::is_none) {
        return Vec::new();
    }

    let mut events = Vec::new();
    if version >= AXIS_SOURCE_SINCE {
        events.push(PointerScrollWireEvent::Source(frame.source));
    }
    for change in present.into_iter().flatten() {
        let (axis, PointerAxisChange::Scroll { value, discrete, value120, relative_direction }) =
            change
        else {
            continue;
        };
        if (AXIS_SOURCE_SINCE..AXIS_VALUE120_SINCE).contains(&version) {
            if let Some(discrete) = discrete.filter(|discrete| *discrete != 0) {
                events.push(PointerScrollWireEvent::Discrete { axis, discrete });
            }
        }
        if version >= AXIS_VALUE120_SINCE {
            if let Some(value120) = value120.filter(|value120| *value120 != 0) {
                events.push(PointerScrollWireEvent::Value120 { axis, value120 });
            }
        }
        if version >= AXIS_RELATIVE_DIRECTION_SINCE {
            if let Some(direction) = relative_direction {
                events.push(PointerScrollWireEvent::RelativeDirection { axis, direction });
            }
        }
        events.push(PointerScrollWireEvent::Axis {
            time: frame.time,
            axis,
            value_bits: value.to_bits(),
        });
    }
    if version >= AXIS_STOP_SINCE {
        for (axis, change) in present.into_iter().flatten() {
            if matches!(change, PointerAxisChange::Stop) {
                events.push(PointerScrollWireEvent::Stop { time: frame.time, axis });
            }
        }
    }
    if version >= FRAME_SINCE
        && events
            .iter()
            .any(|event| !matches!(event, PointerScrollWireEvent::Source(_)))
    {
        events.push(PointerScrollWireEvent::Frame);
    } else if version < FRAME_SINCE {
        events.retain(|event| matches!(event, PointerScrollWireEvent::Axis { .. }));
    }
    events
}

#[derive(Clone, Debug)]
struct PopupGrabState {
    client_id: ClientId,
    popup_stack: Vec<(Resolved<XdgPopupId>, Resolved<WlSurfaceId>)>,
}

#[derive(Clone, Copy, Debug)]
struct PointerPressState {
    serial: u32,
    focus: SurfaceFocus,
}

#[derive(Clone, Copy, Debug)]
struct PointerEnterState {
    serial: u32,
    client_id: ClientId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SeatClientRemovalResult {
    pub cursor_changed: bool,
    pub interactive_grab_cleared: bool,
    pub keyboard_focus_cleared: bool,
    /// The removed client owned the clipboard through an X selection source.
    pub selection_cleared: bool,
    /// The removed client owned the primary selection through an X selection source.
    pub primary_selection_cleared: bool,
}

#[derive(Clone, Copy, Debug)]
struct KeyboardTriggerState {
    serial: u32,
    focus: SurfaceFocus,
}

#[derive(Clone, Copy, Debug)]
struct SerialRange {
    lo: u32,
    hi: u32,
}

const MAX_TRACKED_SERIAL_RANGES: usize = 32;
const MAX_KEYBOARD_FOCUS_HISTORY: usize = 64;

pub struct SeatState {
    current_serial: u32,
    pub(crate) pointer_pos: (f64, f64),
    pub(crate) pointer_focus: Option<SurfaceFocusPick>,
    last_pointer_enter: Option<PointerEnterState>,
    pub(crate) keyboard_focus: Option<SurfaceFocus>,
    keyboard_focus_history: VecDeque<SurfaceFocus>,
    pub(crate) pointer_button_count: u32,
    pressed_buttons: BTreeSet<u32>,
    last_pointer_press: Option<PointerPressState>,
    last_keyboard_trigger: Option<KeyboardTriggerState>,
    cursor: CursorState,
    popup_grab: Option<PopupGrabState>,
    pub(crate) pointer_interactive_grab: Option<ActiveGrabState>,
    /// Tiles to snap on the relayout that follows release of a tiled resize.
    /// The grab is already cleared by then. Emptied once the animation pump
    /// has applied that relayout.
    pub(crate) tiled_resize_snap_windows: BTreeSet<crate::window::WindowId>,
    /// Layout cloned before a tiled drag left the tree. Dropped without restore
    /// when the dragged surface is gone.
    pub(crate) tiled_drag_origin: Option<crate::tiling::ExtractedTiledLeaf>,
    client_serials: BTreeMap<ClientId, VecDeque<SerialRange>>,
    data_device_resources: BTreeMap<ClientId, Vec<Resolved<WlDataDeviceId>>>,
    data_control_devices: BTreeMap<ClientId, Vec<Resolved<ZwlrDataControlDeviceV1Id>>>,
    ext_data_control_devices: BTreeMap<ClientId, Vec<Resolved<ExtDataControlDeviceV1Id>>>,
    primary_selection_devices: BTreeMap<ClientId, Vec<Resolved<ZwpPrimarySelectionDeviceV1Id>>>,
    selection_source: Option<SelectionSourceState>,
    selection_serial: u32,
    primary_selection_source: Option<PrimarySelectionSourceState>,
    primary_selection_serial: u32,
    active_drag: Option<DragState>,
    /// `xdg_toplevel_drag_v1` riding on `active_drag`, when the source has one.
    pub(crate) toplevel_drag: Option<ToplevelDragState>,
    pointer_resources: BTreeMap<ClientId, Vec<Resolved<WlPointerId>>>,
    relative_pointers: BTreeMap<Resolved<WlPointerId>, Vec<Resolved<ZwpRelativePointerV1Id>>>,
    /// `zwp_pointer_gesture_*_v1` objects, per `wl_pointer` they were created for.
    pointer_gestures:
        BTreeMap<Resolved<WlPointerId>, Vec<crate::input::gesture::PointerGestureResource>>,
    /// The libinput gesture in progress, if any. `target` is the client that got `begin`.
    pub(crate) active_pointer_gesture: Option<crate::input::gesture::ActivePointerGesture>,
    pub(crate) active_pointer_lock: Option<Resolved<ZwpLockedPointerV1Id>>,
    pub(crate) active_pointer_confine: Option<Resolved<ZwpConfinedPointerV1Id>>,
    keyboard_resources: BTreeMap<ClientId, Vec<Resolved<WlKeyboardId>>>,
    /// Keymap last sent to each client. Absent means the hardware keymap, id 0.
    client_keymaps: BTreeMap<ClientId, Rc<KeymapBlob>>,
    intercepted_keyboard_keys: BTreeSet<u32>,
    intercepted_pointer_buttons: BTreeSet<u32>,
    pub(crate) ime: ImeState,
    /// Last real libinput event on this seat. Absent until the first one.
    /// A held key or button does not refresh it, and neither do synthetic
    /// pause releases or focus changes.
    last_real_input: Option<Instant>,
    /// Last (surface, outputs) for which an icon or cursor tree refresh was
    /// dispatched on pointer motion. Motion skips the refresh when this is unchanged.
    cursor_outputs_hint: Option<(Resolved<WlSurfaceId>, Vec<OutputId>)>,
    dnd_icon_outputs_hint: Option<(Resolved<WlSurfaceId>, Vec<OutputId>)>,
}

impl Default for SeatState {
    fn default() -> Self {
        let mut cursor = CursorState::default();
        let _ = cursor.ensure_default_named_cursor();
        Self {
            current_serial: 1,
            pointer_pos: (0.0, 0.0),
            pointer_focus: None,
            last_pointer_enter: None,
            keyboard_focus: None,
            keyboard_focus_history: VecDeque::new(),
            pointer_button_count: 0,
            pressed_buttons: BTreeSet::new(),
            last_pointer_press: None,
            last_keyboard_trigger: None,
            cursor,
            popup_grab: None,
            pointer_interactive_grab: None,
            tiled_resize_snap_windows: BTreeSet::new(),
            tiled_drag_origin: None,
            client_serials: BTreeMap::default(),
            data_device_resources: BTreeMap::default(),
            data_control_devices: BTreeMap::default(),
            ext_data_control_devices: BTreeMap::default(),
            primary_selection_devices: BTreeMap::default(),
            selection_source: None,
            selection_serial: 0,
            primary_selection_source: None,
            primary_selection_serial: 0,
            active_drag: None,
            toplevel_drag: None,
            pointer_resources: BTreeMap::default(),
            relative_pointers: BTreeMap::default(),
            pointer_gestures: BTreeMap::default(),
            active_pointer_gesture: None,
            active_pointer_lock: None,
            active_pointer_confine: None,
            keyboard_resources: BTreeMap::default(),
            client_keymaps: BTreeMap::default(),
            intercepted_keyboard_keys: BTreeSet::default(),
            intercepted_pointer_buttons: BTreeSet::default(),
            ime: ImeState::default(),
            last_real_input: None,
            cursor_outputs_hint: None,
            dnd_icon_outputs_hint: None,
        }
    }
}

/// Install or clear the compositor-owned source for one X selection.
///
/// The seat serial advances, the previous source is cancelled, and the new
/// value is published to the focused client and to data-control devices.
pub(crate) fn install_x_selection(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    kind: crate::xwayland::SelectionKind,
    source: Option<crate::state::XSelectionSource>,
) {
    let serial = state.seat.next_serial();
    match kind {
        crate::xwayland::SelectionKind::Clipboard => {
            let previous = state
                .seat
                .replace_selection_source(source.map(SelectionSourceState::Xwayland), serial);
            if let Some(previous) = previous {
                previous.cancelled(dispatcher);
            }
            state.seat.publish_clipboard_selection(dispatcher, None);
        }
        crate::xwayland::SelectionKind::Primary => {
            let previous = state.seat.replace_primary_selection_source(
                source.map(PrimarySelectionSourceState::Xwayland),
                serial,
            );
            if let Some(previous) = previous {
                previous.cancelled(dispatcher);
            }
            state.seat.publish_primary_selection(dispatcher, None);
        }
    }
}

impl SeatState {
    /// Record one real libinput event.
    ///
    /// Idle time starts at this instant. A key or button that stays down does
    /// not call this again, so holding input does not postpone idle.
    pub(crate) fn note_real_input(&mut self, at: Instant) {
        self.last_real_input = Some(at);
    }

    pub(crate) fn last_real_input(&self) -> Option<Instant> {
        self.last_real_input
    }

    /// Drop pointer and keyboard device state after input devices are removed.
    ///
    /// The activity clock stays. Device removal is not a new input, and idle
    /// time keeps running from the last real event.
    pub fn clear_device_state(&mut self) {
        self.pointer_button_count = 0;
        self.pressed_buttons.clear();
        self.last_pointer_press = None;
        self.last_keyboard_trigger = None;
        self.popup_grab = None;
        self.pointer_interactive_grab = None;
        self.tiled_drag_origin = None;
        self.intercepted_keyboard_keys.clear();
        self.intercepted_pointer_buttons.clear();
        self.pointer_focus = None;
        self.keyboard_focus = None;
        self.active_pointer_lock = None;
        self.active_pointer_confine = None;
        self.keyboard_focus_history.clear();
        self.last_pointer_enter = None;
        self.active_drag = None;
        self.active_pointer_gesture = None;
    }

    pub fn register_pointer(&mut self, pointer: Resolved<WlPointerId>) {
        let resources = self
            .pointer_resources
            .entry(pointer.client_id())
            .or_default();
        if !resources.contains(&pointer) {
            resources.push(pointer);
        }
    }

    pub fn unregister_pointer(&mut self, pointer: Resolved<WlPointerId>) {
        remove_resource(&mut self.pointer_resources, pointer.client_id(), pointer);
        self.relative_pointers.remove(&pointer);
        self.pointer_gestures.remove(&pointer);
    }

    pub fn register_relative_pointer(
        &mut self,
        pointer: Resolved<WlPointerId>,
        relative: Resolved<ZwpRelativePointerV1Id>,
    ) {
        let resources = self.relative_pointers.entry(pointer).or_default();
        if !resources.contains(&relative) {
            resources.push(relative);
        }
    }

    pub fn unregister_relative_pointer(
        &mut self,
        pointer: Resolved<WlPointerId>,
        relative: ZwpRelativePointerV1Id,
    ) {
        let Some(resources) = self.relative_pointers.get_mut(&pointer) else {
            return;
        };
        resources.retain(|registered| registered.wayland_id() != relative);
        if resources.is_empty() {
            self.relative_pointers.remove(&pointer);
        }
    }

    pub fn relative_pointers(
        &self,
        pointer: Resolved<WlPointerId>,
    ) -> &[Resolved<ZwpRelativePointerV1Id>] {
        self.relative_pointers
            .get(&pointer)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn relative_pointers_for_client(
        &self,
        client_id: ClientId,
    ) -> Vec<Resolved<ZwpRelativePointerV1Id>> {
        self.relative_pointers
            .iter()
            .filter(|(pointer, _)| pointer.client_id() == client_id)
            .flat_map(|(_, relatives)| relatives.iter().copied())
            .collect()
    }

    pub(crate) fn register_pointer_gesture(
        &mut self,
        pointer: Resolved<WlPointerId>,
        gesture: crate::input::gesture::PointerGestureResource,
    ) {
        let resources = self.pointer_gestures.entry(pointer).or_default();
        if !resources.contains(&gesture) {
            resources.push(gesture);
        }
    }

    pub(crate) fn unregister_pointer_gesture(
        &mut self,
        pointer: Resolved<WlPointerId>,
        gesture: WaylandId,
    ) {
        let Some(resources) = self.pointer_gestures.get_mut(&pointer) else {
            return;
        };
        resources.retain(|registered| registered.wayland_id() != gesture);
        if resources.is_empty() {
            self.pointer_gestures.remove(&pointer);
        }
    }

    pub(crate) fn pointer_gestures_for_client(
        &self,
        client_id: ClientId,
        kind: crate::input::gesture::PointerGestureKind,
    ) -> Vec<crate::input::gesture::PointerGestureResource> {
        self.pointer_gestures
            .iter()
            .filter(|(pointer, _)| pointer.client_id() == client_id)
            .flat_map(|(_, gestures)| gestures.iter().copied())
            .filter(|gesture| gesture.kind() == kind)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn pointer_gestures(
        &self,
        pointer: Resolved<WlPointerId>,
    ) -> &[crate::input::gesture::PointerGestureResource] {
        self.pointer_gestures
            .get(&pointer)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn register_keyboard(&mut self, keyboard: Resolved<WlKeyboardId>) {
        let resources = self
            .keyboard_resources
            .entry(keyboard.client_id())
            .or_default();
        if !resources.contains(&keyboard) {
            resources.push(keyboard);
        }
    }

    pub fn unregister_keyboard(&mut self, keyboard: Resolved<WlKeyboardId>) {
        remove_resource(&mut self.keyboard_resources, keyboard.client_id(), keyboard);
    }

    pub(crate) fn client_keymap(&self, client_id: ClientId) -> Option<Rc<KeymapBlob>> {
        self.client_keymaps.get(&client_id).cloned()
    }

    pub(crate) fn remember_client_keymap(&mut self, client_id: ClientId, keymap: Rc<KeymapBlob>) {
        self.client_keymaps.insert(client_id, keymap);
    }

    pub fn register_data_device(&mut self, data_device: Resolved<WlDataDeviceId>) {
        let resources = self
            .data_device_resources
            .entry(data_device.client_id())
            .or_default();
        if !resources.contains(&data_device) {
            resources.push(data_device);
        }
    }

    pub fn unregister_data_device(&mut self, data_device: Resolved<WlDataDeviceId>) {
        remove_resource(&mut self.data_device_resources, data_device.client_id(), data_device);
    }

    pub fn register_data_control_device(&mut self, device: Resolved<ZwlrDataControlDeviceV1Id>) {
        let devices = self
            .data_control_devices
            .entry(device.client_id())
            .or_default();
        if !devices.contains(&device) {
            devices.push(device);
        }
    }

    pub fn unregister_data_control_device(&mut self, device: Resolved<ZwlrDataControlDeviceV1Id>) {
        remove_resource(&mut self.data_control_devices, device.client_id(), device);
    }

    pub fn data_control_devices(
        &self,
        client_id: ClientId,
    ) -> Vec<Resolved<ZwlrDataControlDeviceV1Id>> {
        self.data_control_devices
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn data_control_client_ids(&self) -> Vec<ClientId> {
        self.data_control_devices.keys().copied().collect()
    }

    pub fn register_ext_data_control_device(&mut self, device: Resolved<ExtDataControlDeviceV1Id>) {
        let devices = self
            .ext_data_control_devices
            .entry(device.client_id())
            .or_default();
        if !devices.contains(&device) {
            devices.push(device);
        }
    }

    pub fn unregister_ext_data_control_device(
        &mut self,
        device: Resolved<ExtDataControlDeviceV1Id>,
    ) {
        remove_resource(&mut self.ext_data_control_devices, device.client_id(), device);
    }

    pub fn ext_data_control_devices(
        &self,
        client_id: ClientId,
    ) -> Vec<Resolved<ExtDataControlDeviceV1Id>> {
        self.ext_data_control_devices
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn ext_data_control_client_ids(&self) -> Vec<ClientId> {
        self.ext_data_control_devices.keys().copied().collect()
    }

    /// Tell the focused `wl_data_device` and every data-control device that the clipboard changed.
    /// `skip` is a client whose socket is already going away.
    pub fn publish_clipboard_selection(&self, dispatcher: &mut Dispatcher, skip: Option<ClientId>) {
        if let Some(focus) = self.keyboard_focus() {
            if skip != Some(focus.client_id()) {
                dispatcher.dispatch_to(focus.client_id(), ClientAction::RefreshDataDeviceSelection);
            }
        }
        for client_id in self.data_control_client_ids() {
            if skip == Some(client_id) {
                continue;
            }
            dispatcher.dispatch_to(client_id, ClientAction::RefreshDataControlSelection);
        }
        for client_id in self.ext_data_control_client_ids() {
            if skip == Some(client_id) {
                continue;
            }
            dispatcher.dispatch_to(client_id, ClientAction::RefreshExtDataControlSelection);
        }
        dispatcher.dispatch(crate::xwayland::XwaylandAction::SelectionChanged(
            crate::xwayland::SelectionKind::Clipboard,
        ));
    }

    pub fn register_primary_selection_device(
        &mut self,
        device: Resolved<ZwpPrimarySelectionDeviceV1Id>,
    ) {
        let devices = self
            .primary_selection_devices
            .entry(device.client_id())
            .or_default();
        if !devices.contains(&device) {
            devices.push(device);
        }
    }

    pub fn unregister_primary_selection_device(
        &mut self,
        device: Resolved<ZwpPrimarySelectionDeviceV1Id>,
    ) {
        remove_resource(&mut self.primary_selection_devices, device.client_id(), device);
    }

    pub fn primary_selection_devices(
        &self,
        client_id: ClientId,
    ) -> Vec<Resolved<ZwpPrimarySelectionDeviceV1Id>> {
        self.primary_selection_devices
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Tell the keyboard-focused `zwp_primary_selection_device_v1` and every
    /// data-control client that the primary selection changed. Unfocused zwp
    /// devices keep their current offer until focus moves. The data-control
    /// refresh skips version-1 devices. `skip` is a client whose socket is
    /// already going away.
    pub fn publish_primary_selection(&self, dispatcher: &mut Dispatcher, skip: Option<ClientId>) {
        if let Some(focus) = self.keyboard_focus() {
            if skip != Some(focus.client_id()) {
                dispatcher.dispatch_to(focus.client_id(), ClientAction::RefreshPrimarySelection);
            }
        }
        for client_id in self.data_control_client_ids() {
            if skip == Some(client_id) {
                continue;
            }
            dispatcher.dispatch_to(client_id, ClientAction::RefreshDataControlPrimarySelection);
        }
        for client_id in self.ext_data_control_client_ids() {
            if skip == Some(client_id) {
                continue;
            }
            dispatcher.dispatch_to(client_id, ClientAction::RefreshExtDataControlPrimarySelection);
        }
        dispatcher.dispatch(crate::xwayland::XwaylandAction::SelectionChanged(
            crate::xwayland::SelectionKind::Primary,
        ));
    }

    pub fn primary_selection_source(&self) -> Option<PrimarySelectionSourceState> {
        self.primary_selection_source
    }

    pub fn primary_selection_serial(&self) -> u32 {
        self.primary_selection_serial
    }

    pub fn validate_primary_selection_serial(&self, serial: u32) -> bool {
        let current = self.primary_selection_serial;
        current == 0 || current.wrapping_sub(serial) >= (u32::MAX / 2)
    }

    pub fn replace_primary_selection_source(
        &mut self,
        source: Option<PrimarySelectionSourceState>,
        serial: u32,
    ) -> Option<PrimarySelectionSourceState> {
        self.primary_selection_serial = serial;
        std::mem::replace(&mut self.primary_selection_source, source)
    }

    pub fn set_primary_selection_serial(&mut self, serial: u32) {
        self.primary_selection_serial = serial;
    }

    pub fn pointer_resources(&self, client_id: ClientId) -> Vec<Resolved<WlPointerId>> {
        self.pointer_resources
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn keyboard_resources(&self, client_id: ClientId) -> Vec<Resolved<WlKeyboardId>> {
        self.keyboard_resources
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn data_device_resources(&self, client_id: ClientId) -> Vec<Resolved<WlDataDeviceId>> {
        self.data_device_resources
            .get(&client_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn selection_source(&self) -> Option<SelectionSourceState> {
        self.selection_source
    }

    pub fn selection_serial(&self) -> u32 {
        self.selection_serial
    }

    pub fn replace_selection_source(
        &mut self,
        source: Option<SelectionSourceState>,
        serial: u32,
    ) -> Option<SelectionSourceState> {
        self.selection_serial = serial;
        std::mem::replace(&mut self.selection_source, source)
    }

    pub fn validate_selection_serial(&self, serial: u32) -> bool {
        let current = self.selection_serial;
        current == 0 || current.wrapping_sub(serial) >= (u32::MAX / 2)
    }

    pub fn validate_data_transfer_serial(
        &self,
        origin_surface: Resolved<WlSurfaceId>,
        serial: u32,
    ) -> bool {
        self.pointer_button_count > 0
            && self.last_pointer_press.is_some_and(|press| {
                press.serial == serial && press.focus.surface == origin_surface
            })
    }

    pub fn validate_client_serial(&self, client_id: ClientId, serial: u32) -> bool {
        let Some(serials) = self.client_serials.get(&client_id) else {
            return false;
        };
        for range in serials.iter().rev() {
            if serial.wrapping_sub(range.hi) as i32 > 0 {
                return false;
            }
            if serial.wrapping_sub(range.lo) as i32 >= 0 {
                return true;
            }
        }
        false
    }

    pub fn active_drag(&self) -> Option<DragState> {
        self.active_drag.clone()
    }

    pub fn active_drag_mut(&mut self) -> Option<&mut DragState> {
        self.active_drag.as_mut()
    }

    pub fn set_active_drag(&mut self, drag: DragState) -> Option<DragState> {
        std::mem::replace(&mut self.active_drag, Some(drag))
    }

    pub fn replace_active_drag(&mut self, drag: Option<DragState>) -> Option<DragState> {
        std::mem::replace(&mut self.active_drag, drag)
    }

    pub fn clear_active_drag(&mut self) -> Option<DragState> {
        self.replace_active_drag(None)
    }

    pub fn set_active_drag_target(&mut self, target: Option<DragTargetState>) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag.target == target {
            return false;
        }
        drag.target = target;
        drag.negotiation = DragNegotiationState::default();
        drag.phase = DragPhase::Dragging;
        true
    }

    pub fn register_active_drag_offer(
        &mut self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
        has_finish_capable_offer: bool,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
        {
            return false;
        }
        if drag.negotiation.offer_ids.contains(&offer) {
            return false;
        }
        drag.negotiation.offer_ids.push(offer);
        drag.negotiation.has_finish_capable_offer |= has_finish_capable_offer;
        true
    }

    pub fn unregister_active_drag_offer(
        &mut self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
        {
            return false;
        }
        let len_before = drag.negotiation.offer_ids.len();
        drag.negotiation
            .offer_ids
            .retain(|candidate| *candidate != offer);
        len_before != drag.negotiation.offer_ids.len()
    }

    pub fn active_drag_contains_offer(
        &self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
    ) -> bool {
        self.active_drag.as_ref().is_some_and(|drag| {
            drag.target
                .is_some_and(|target| target.client_id() == target_client_id)
                && drag.negotiation.offer_ids.contains(&offer)
        })
    }

    pub fn active_drag_offer_ids(
        &self,
        target_client_id: ClientId,
    ) -> Vec<Resolved<WlDataOfferId>> {
        self.active_drag
            .as_ref()
            .and_then(|drag| {
                drag.target
                    .is_some_and(|target| target.client_id() == target_client_id)
                    .then(|| drag.negotiation.offer_ids.clone())
            })
            .unwrap_or_default()
    }

    pub fn set_active_drag_accepted_mime(
        &mut self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
        accepted_mime: Option<String>,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
            || !drag.negotiation.offer_ids.contains(&offer)
        {
            return false;
        }
        if drag.negotiation.accepted_mime == accepted_mime {
            return false;
        }
        drag.negotiation.accepted_mime = accepted_mime;
        true
    }

    pub fn set_active_drag_negotiation_actions(
        &mut self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
        destination_actions: u32,
        preferred_action: u32,
        chosen_action: u32,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
            || !drag.negotiation.offer_ids.contains(&offer)
        {
            return false;
        }
        if drag.negotiation.destination_actions == destination_actions
            && drag.negotiation.preferred_action == preferred_action
            && drag.negotiation.chosen_action == chosen_action
        {
            return false;
        }
        drag.negotiation.destination_actions = destination_actions;
        drag.negotiation.preferred_action = preferred_action;
        drag.negotiation.chosen_action = chosen_action;
        true
    }

    pub fn set_active_drag_chosen_action(
        &mut self,
        target_client_id: ClientId,
        chosen_action: u32,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
        {
            return false;
        }
        if drag.negotiation.chosen_action == chosen_action {
            return false;
        }
        drag.negotiation.chosen_action = chosen_action;
        true
    }

    pub fn set_active_drag_finished(
        &mut self,
        target_client_id: ClientId,
        offer: Resolved<WlDataOfferId>,
        finished: bool,
    ) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag
            .target
            .is_none_or(|target| target.client_id() != target_client_id)
            || !drag.negotiation.offer_ids.contains(&offer)
        {
            return false;
        }
        if drag.negotiation.finished == finished {
            return false;
        }
        drag.negotiation.finished = finished;
        true
    }

    pub fn set_active_drag_phase(&mut self, phase: DragPhase) -> bool {
        let Some(drag) = self.active_drag.as_mut() else {
            return false;
        };
        if drag.phase == phase {
            return false;
        }
        drag.phase = phase;
        true
    }

    pub fn active_drag_can_drop(&self) -> bool {
        self.active_drag.as_ref().is_some_and(|drag| {
            if drag.phase != DragPhase::Dragging || drag.target.is_none() {
                return false;
            }
            if drag.source.is_none() {
                return true;
            }
            drag.negotiation.accepted_mime.is_some() && drag.negotiation.chosen_action != 0
        })
    }

    pub(crate) fn remove_client(&mut self, client_id: ClientId) -> SeatClientRemovalResult {
        let mut result = SeatClientRemovalResult::default();
        if self.toplevel_drag.as_ref().is_some_and(|drag| {
            drag.source.client_id() == client_id
                || drag
                    .attached
                    .as_ref()
                    .is_some_and(|attached| attached.toplevel.client_id() == client_id)
        }) {
            // The client's windows are going away, so there is nothing to place.
            self.toplevel_drag = None;
        }
        self.client_serials.remove(&client_id);
        self.data_device_resources.remove(&client_id);
        self.data_control_devices.remove(&client_id);
        self.ext_data_control_devices.remove(&client_id);
        self.primary_selection_devices.remove(&client_id);
        self.pointer_resources.remove(&client_id);
        self.relative_pointers
            .retain(|pointer, _| pointer.client_id() != client_id);
        self.pointer_gestures
            .retain(|pointer, _| pointer.client_id() != client_id);
        if let Some(gesture) = self.active_pointer_gesture.as_mut() {
            if gesture
                .target
                .is_some_and(|surface| surface.client_id() == client_id)
            {
                gesture.target = None;
            }
        }
        if self
            .active_pointer_lock
            .is_some_and(|lock| lock.client_id() == client_id)
        {
            self.active_pointer_lock = None;
        }
        if self
            .active_pointer_confine
            .is_some_and(|confine| confine.client_id() == client_id)
        {
            self.active_pointer_confine = None;
        }
        self.keyboard_resources.remove(&client_id);
        self.client_keymaps.remove(&client_id);
        if let Some(selection) = self.selection_source {
            if selection.owner_client_id() == client_id {
                result.selection_cleared = matches!(selection, SelectionSourceState::Xwayland(_));
                self.selection_source = None;
            }
        }
        if let Some(selection) = self.primary_selection_source {
            if selection.owner_client_id() == client_id {
                result.primary_selection_cleared =
                    matches!(selection, PrimarySelectionSourceState::Xwayland(_));
                self.primary_selection_source = None;
            }
        }
        if self
            .pointer_focus
            .is_some_and(|focus| focus.target.client_id() == client_id)
        {
            self.pointer_focus = None;
        }
        if self
            .last_pointer_enter
            .is_some_and(|enter| enter.client_id == client_id)
        {
            self.last_pointer_enter = None;
        }
        if self
            .keyboard_focus
            .is_some_and(|focus| focus.client_id() == client_id)
        {
            self.keyboard_focus = None;
            result.keyboard_focus_cleared = true;
        }
        self.keyboard_focus_history
            .retain(|focus| focus.client_id() != client_id);
        if self
            .popup_grab
            .as_ref()
            .is_some_and(|grab| grab.client_id == client_id)
        {
            self.popup_grab = None;
        }
        if self.pointer_interactive_grab.is_some_and(|grab| {
            // A split resize is not tied to one client. It ends on the next
            // motion when every drag fails to resolve.
            let root_surface = match grab {
                PointerInteractiveGrab::Move { root_surface, .. }
                | PointerInteractiveGrab::Resize { root_surface, .. } => root_surface,
                PointerInteractiveGrab::MoveTiled(session) => session.root_surface,
                PointerInteractiveGrab::ResizeSplit { .. } => return false,
            };
            root_surface.client_id() == client_id
        }) {
            self.clear_pointer_interactive_grab();
            result.interactive_grab_cleared = true;
        }
        if self
            .active_drag
            .as_ref()
            .is_some_and(|drag| drag.source.initiator_client_id() == client_id)
        {
            self.active_drag = None;
        } else if let Some(drag) = self.active_drag.as_mut() {
            if drag
                .target
                .is_some_and(|target| target.client_id() == client_id)
            {
                drag.target = None;
                drag.negotiation = DragNegotiationState::default();
                drag.phase = DragPhase::Cancelled;
            }
        }
        result.cursor_changed = self.cursor.clear_for_client(client_id);
        result
    }

    pub(crate) fn next_serial(&mut self) -> u32 {
        let serial = self.current_serial;
        self.current_serial = self.current_serial.wrapping_add(1).max(1);
        serial
    }

    /// A pointer press `start_drag` will accept for `focus`.
    #[cfg(test)]
    pub(crate) fn test_arm_drag_serial(&mut self, focus: SurfaceFocus) -> u32 {
        let serial = self.next_serial();
        self.record_pointer_button(1, serial, Some(focus));
        serial
    }

    fn record_pointer_button(&mut self, state: u32, serial: u32, focus: Option<SurfaceFocus>) {
        if state == 1 {
            self.pointer_button_count = self.pointer_button_count.saturating_add(1);
            self.last_pointer_press = focus.map(|focus| PointerPressState { serial, focus });
        } else {
            self.pointer_button_count = self.pointer_button_count.saturating_sub(1);
            if self.pointer_button_count == 0 {
                self.last_pointer_press = None;
            }
        }
    }

    fn record_keyboard_trigger(&mut self, serial: u32, focus: SurfaceFocus) {
        self.last_keyboard_trigger = Some(KeyboardTriggerState { serial, focus });
    }

    pub(crate) fn record_client_serial(&mut self, client_id: ClientId, serial: u32) {
        let serials = self.client_serials.entry(client_id).or_default();
        if let Some(back) = serials.back_mut() {
            if serial == back.hi {
                return;
            }
            if back.hi.wrapping_add(1) == serial {
                back.hi = serial;
                return;
            }
        }
        if serials.len() == MAX_TRACKED_SERIAL_RANGES {
            serials.pop_front();
        }
        serials.push_back(SerialRange { lo: serial, hi: serial });
    }

    pub(crate) fn record_pointer_enter(&mut self, client_id: ClientId, serial: u32) {
        self.last_pointer_enter = Some(PointerEnterState { client_id, serial });
    }

    pub(crate) fn clear_pointer_enter(&mut self) {
        self.last_pointer_enter = None;
    }

    pub fn pointer_cursor_request_allowed(&self, client_id: ClientId, serial: u32) -> bool {
        if !self
            .pointer_focus
            .is_some_and(|focus| focus.target.client_id() == client_id)
        {
            return false;
        }
        self.last_pointer_enter
            .is_some_and(|enter| enter.client_id == client_id && enter.serial == serial)
            || (self.pointer_button_count > 0
                && self.last_pointer_press.is_some_and(|press| {
                    press.focus.client_id() == client_id && press.serial == serial
                }))
    }

    pub fn set_named_cursor(&mut self, client_id: ClientId, icon_names: &[&'static str]) -> bool {
        self.cursor.set_named_cursor(client_id, icon_names)
    }

    pub fn set_compositor_named_cursor(&mut self, icon_names: &[&'static str]) -> bool {
        self.cursor.set_compositor_named_cursor(icon_names)
    }

    pub fn set_default_cursor(&mut self) -> bool {
        self.cursor.ensure_default_named_cursor()
    }

    pub(crate) fn invalidate_implicit_grabs(&mut self) {
        self.pointer_button_count = 0;
        self.pressed_buttons.clear();
        self.last_pointer_press = None;
        self.last_keyboard_trigger = None;
        self.client_serials.clear();
        self.intercepted_keyboard_keys.clear();
        self.intercepted_pointer_buttons.clear();
    }

    pub(crate) fn replace_cursor_outputs_hint(
        &mut self,
        hint: Option<(Resolved<WlSurfaceId>, Vec<OutputId>)>,
    ) -> bool {
        if self.cursor_outputs_hint == hint {
            return false;
        }
        self.cursor_outputs_hint = hint;
        true
    }

    pub(crate) fn replace_dnd_icon_outputs_hint(
        &mut self,
        hint: Option<(Resolved<WlSurfaceId>, Vec<OutputId>)>,
    ) -> bool {
        if self.dnd_icon_outputs_hint == hint {
            return false;
        }
        self.dnd_icon_outputs_hint = hint;
        true
    }

    pub(crate) fn clear_dnd_icon_outputs_hint(&mut self) {
        self.dnd_icon_outputs_hint = None;
    }

    pub fn set_cursor_surface(
        &mut self,
        surface: Resolved<WlSurfaceId>,
        hotspot_x: i32,
        hotspot_y: i32,
    ) -> bool {
        self.cursor
            .set_surface_cursor(surface, hotspot_x, hotspot_y)
    }

    pub fn clear_cursor_for_client(&mut self, client_id: ClientId) -> bool {
        self.cursor.clear_for_client(client_id)
    }

    pub fn clear_cursor_surface(&mut self, surface: Resolved<WlSurfaceId>) -> bool {
        self.cursor.clear_surface(surface)
    }

    pub fn apply_cursor_surface_offset(
        &mut self,
        surface: Resolved<WlSurfaceId>,
        dx: i32,
        dy: i32,
    ) -> bool {
        self.cursor.apply_surface_offset(surface, dx, dy)
    }

    pub fn visible_cursor_source_at(&self, now_msec: u32) -> Option<CursorSourceRef<'_>> {
        self.cursor.source_at(now_msec)
    }

    pub fn visible_cursor_source(&self) -> Option<CursorSourceRef<'_>> {
        self.cursor.source()
    }

    pub fn cursor_is_visible(&self) -> bool {
        self.cursor.is_visible()
    }

    pub fn clear_cursor(&mut self) -> bool {
        self.cursor.clear()
    }

    pub fn set_cursor_theme(&mut self, theme_name: String, theme_size: u32) -> bool {
        self.cursor.set_theme(theme_name, theme_size)
    }

    pub(crate) fn default_cursor_image(&mut self) -> Option<crate::cursor::CursorImage> {
        self.cursor.default_image()
    }

    /// The theme's default cursor at `size` pixels. The seat's own cursor stays
    /// at the configured theme size.
    pub(crate) fn default_cursor_image_at(
        &mut self,
        size: u32,
    ) -> Option<crate::cursor::CursorImage> {
        self.cursor.image_at(size)
    }

    pub(crate) fn theme_cursor_size(&self) -> u32 {
        self.cursor.theme_size()
    }

    /// The visible named cursor nearest to `size` pixels. A surface cursor returns nothing.
    pub(crate) fn named_cursor_frame_at(
        &mut self,
        size: u32,
        now_msec: u32,
    ) -> Option<(crate::cursor::CursorImage, Option<u32>)> {
        self.cursor.frame_at_size(size, now_msec)
    }

    #[cfg(test)]
    pub(crate) fn test_block_cursor_icons(&mut self, names: &[&str]) {
        self.cursor.test_block_icons(names);
    }

    #[cfg(test)]
    pub(crate) fn test_cache_cursor_icon(&mut self, name: &str, image: crate::cursor::CursorImage) {
        self.cursor.test_cache_frames(name, vec![image]);
    }

    pub fn pointer_position(&self) -> (f64, f64) {
        self.pointer_pos
    }

    pub fn keyboard_focus(&self) -> Option<SurfaceFocus> {
        self.keyboard_focus
    }

    pub(crate) fn keyboard_focus_history(&self) -> Vec<SurfaceFocus> {
        self.keyboard_focus_history.iter().copied().collect()
    }

    pub(crate) fn remember_keyboard_focus(&mut self, focus: SurfaceFocus) {
        self.keyboard_focus_history
            .retain(|candidate| *candidate != focus);
        self.keyboard_focus_history.push_front(focus);
        while self.keyboard_focus_history.len() > MAX_KEYBOARD_FOCUS_HISTORY {
            let _ = self.keyboard_focus_history.pop_back();
        }
    }

    #[cfg(test)]
    pub fn last_keyboard_trigger_serial(&self) -> Option<u32> {
        self.last_keyboard_trigger.map(|trigger| trigger.serial)
    }

    #[cfg(test)]
    fn test_install_default_cursor(&mut self) -> bool {
        self.cursor.test_install_default_cursor()
    }

    fn intercept_keyboard_key(&mut self, key: u32) {
        self.intercepted_keyboard_keys.insert(key);
    }

    fn take_intercepted_keyboard_key(&mut self, key: u32) -> bool {
        self.intercepted_keyboard_keys.remove(&key)
    }

    fn intercept_pointer_button(&mut self, button: u32) {
        self.intercepted_pointer_buttons.insert(button);
    }

    fn take_intercepted_pointer_button(&mut self, button: u32) -> bool {
        self.intercepted_pointer_buttons.remove(&button)
    }

    pub(crate) fn validate_implicit_grab_serial(
        &self,
        client_id: ClientId,
        surface: Resolved<WlSurfaceId>,
        serial: u32,
    ) -> bool {
        (self.pointer_button_count > 0
            && self.last_pointer_press.is_some_and(|press| {
                press.serial == serial
                    && press.focus.client_id() == client_id
                    && press.focus.surface == surface
            }))
            || self.last_keyboard_trigger.is_some_and(|trigger| {
                trigger.serial == serial
                    && trigger.focus.client_id() == client_id
                    && trigger.focus.surface == surface
            })
    }

    pub(crate) fn popup_grab_client(&self) -> Option<ClientId> {
        self.popup_grab.as_ref().map(|grab| grab.client_id)
    }

    pub(crate) fn popup_keyboard_surface(&self) -> Option<Resolved<WlSurfaceId>> {
        self.popup_grab
            .as_ref()
            .and_then(|grab| grab.popup_stack.last())
            .map(|(_, surface)| *surface)
    }

    pub(crate) fn popup_grab_contains_surface(&self, surface: &Resolved<WlSurfaceId>) -> bool {
        self.popup_grab.as_ref().is_some_and(|grab| {
            grab.client_id == surface.client_id()
                && grab
                    .popup_stack
                    .iter()
                    .any(|(_, candidate)| candidate == surface)
        })
    }

    pub(crate) fn start_popup_grab(
        &mut self,
        client_id: ClientId,
        popup: Resolved<XdgPopupId>,
        popup_surface: Resolved<WlSurfaceId>,
    ) {
        let grab = self
            .popup_grab
            .get_or_insert_with(|| PopupGrabState { client_id, popup_stack: Vec::new() });
        if grab.client_id != client_id {
            *grab = PopupGrabState { client_id, popup_stack: Vec::new() };
        }
        grab.popup_stack
            .retain(|(candidate, _)| *candidate != popup);
        grab.popup_stack.push((popup, popup_surface));
    }

    pub(crate) fn remove_popup_grab(&mut self, popup: Resolved<XdgPopupId>) {
        let Some(grab) = self.popup_grab.as_mut() else {
            return;
        };
        grab.popup_stack
            .retain(|(candidate, _)| *candidate != popup);
        if grab.popup_stack.is_empty() {
            self.popup_grab = None;
        }
    }

    pub(crate) fn dismiss_popup_grab(&mut self) -> Option<(ClientId, Vec<Resolved<XdgPopupId>>)> {
        let grab = self.popup_grab.take()?;
        Some((
            grab.client_id,
            grab.popup_stack
                .into_iter()
                .map(|(popup, _)| popup)
                .collect(),
        ))
    }

    pub(crate) fn start_toplevel_move(
        &mut self,
        _client_id: ClientId,
        window: crate::window::WindowId,
        root_surface: Resolved<WlSurfaceId>,
        initial_geometry: WindowTargetGeometry,
    ) {
        self.pointer_interactive_grab = Some(PointerInteractiveGrab::Move {
            window,
            root_surface,
            initial_pointer_pos: self.pointer_pos,
            initial_geometry,
        });
    }

    pub(crate) fn start_toplevel_resize(
        &mut self,
        _client_id: ClientId,
        window: crate::window::WindowId,
        root_surface: Resolved<WlSurfaceId>,
        size_constraints: InteractiveSizeConstraints,
        edges: u32,
        initial_geometry: WindowTargetGeometry,
    ) {
        self.pointer_interactive_grab = Some(PointerInteractiveGrab::Resize {
            window,
            root_surface,
            size_constraints,
            edges,
            initial_pointer_pos: self.pointer_pos,
            initial_geometry,
            last_size: initial_geometry.resize_size,
        });
    }

    pub(crate) fn start_toplevel_tiled_resize(
        &mut self,
        workspace: WorkspaceId,
        drags: SplitDragPair,
        cursor_edges: u32,
        origin_window: Option<crate::window::WindowId>,
    ) {
        self.pointer_interactive_grab = Some(PointerInteractiveGrab::ResizeSplit {
            workspace,
            drags,
            cursor_edges,
            initial_pointer_pos: self.pointer_pos,
            origin_window,
        });
    }

    pub(crate) fn clear_pointer_interactive_grab(&mut self) {
        self.pointer_interactive_grab = None;
        self.tiled_drag_origin = None;
        self.intercepted_pointer_buttons.clear();
    }

    pub(crate) fn active_tiled_resize(&self) -> Option<(WorkspaceId, SplitDragPair)> {
        match self.pointer_interactive_grab {
            Some(PointerInteractiveGrab::ResizeSplit { workspace, drags, .. }) => {
                Some((workspace, drags))
            }
            _ => None,
        }
    }

    pub(crate) fn resize_grab_window(&self) -> Option<crate::window::WindowId> {
        match self.pointer_interactive_grab {
            Some(PointerInteractiveGrab::Resize { window, .. }) => Some(window),
            _ => None,
        }
    }
}

pub fn handle_pointer_motion(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    dx: f64,
    dy: f64,
    dx_unaccel: f64,
    dy_unaccel: f64,
    time: u32,
    time_usec: u64,
) {
    // The focused client receives the device delta before the pointer is clipped to an output.
    send_relative_pointer_motion(state, dispatcher, dx, dy, dx_unaccel, dy_unaccel, time_usec);
    if state.seat.active_pointer_lock.is_some() {
        return;
    }
    let old_pointer_pos = state.seat.pointer_position();
    let seat = &mut state.seat;
    seat.pointer_pos.0 += dx;
    seat.pointer_pos.1 += dy;
    crate::input::pointer_confine::confine_pointer_motion(state, old_pointer_pos);
    clamp_pointer_to_outputs(state);
    let new_pointer_pos = state.seat.pointer_position();
    log::debug!(
        "seat pointer motion pos=({:.1}, {:.1}) time={}",
        state.seat.pointer_pos.0,
        state.seat.pointer_pos.1,
        time
    );
    if !state.session_lock.input_gated() {
        crate::input::toplevel_drag::motion(state, dispatcher);
    }
    if !state.session_lock.input_gated() && refresh_drag_focus(state, dispatcher, time, true) {
        refresh_visible_cursor_surface_outputs(state, dispatcher);
        refresh_visible_dnd_icon_surface_outputs(state, dispatcher);
        if state.seat.cursor_is_visible() || visible_dnd_icon_owner_client(state).is_some() {
            request_outputs_for_points(state, [old_pointer_pos, new_pointer_pos], dispatcher);
        }
        return;
    }
    if !state.session_lock.input_gated() && apply_pointer_interactive_grab(state, dispatcher) {
        return;
    }
    refresh_pointer_focus(state, dispatcher, time, true);
    refresh_visible_cursor_surface_outputs(state, dispatcher);
    refresh_visible_dnd_icon_surface_outputs(state, dispatcher);
    if state.seat.cursor_is_visible() || visible_dnd_icon_owner_client(state).is_some() {
        request_outputs_for_points(state, [old_pointer_pos, new_pointer_pos], dispatcher);
    }
}

pub fn handle_pointer_button(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    button: u32,
    button_state: u32,
    time: u32,
) {
    if !state.session_lock.input_gated() {
        if let Some(focus) = popup_keyboard_focus(state) {
            set_keyboard_focus(state, dispatcher, Some(focus));
        }
    }
    let interactive_grab_active =
        !state.session_lock.input_gated() && state.seat.pointer_interactive_grab.is_some();
    if !interactive_grab_active {
        refresh_pointer_focus(state, dispatcher, time, false);
    }
    let pointer_focus = state.seat.pointer_focus;
    log::debug!(
        "seat pointer button button={} state={} time={} focus={:?}",
        button,
        button_state,
        time,
        pointer_focus.map(|focus| focus.target)
    );

    let serial = state.seat.next_serial();
    state
        .seat
        .record_pointer_button(button_state, serial, pointer_focus.map(|focus| focus.target));
    if button_state == 1 {
        state.seat.pressed_buttons.insert(button);
    } else {
        state.seat.pressed_buttons.remove(&button);
    }

    if button_state != 1 && state.seat.take_intercepted_pointer_button(button) {
        finish_pointer_interactive_grab(state, dispatcher, time);
        return;
    }

    if let Some(drag) = state.seat.active_drag() {
        if drag.phase == DragPhase::Dragging
            && button_state != 1
            && state.seat.pointer_button_count == 0
        {
            complete_drag_drop(state, dispatcher);
        }
        return;
    }

    if interactive_grab_active {
        if button_state != 1 && state.seat.pointer_button_count == 0 {
            finish_pointer_interactive_grab(state, dispatcher, time);
        }
        return;
    }

    if !state.session_lock.input_gated()
        && maybe_dismiss_popup_grab_on_button(state, dispatcher, button_state, time)
    {
        return;
    }

    if !state.session_lock.input_gated()
        && button_state == 1
        && button == 0x110
        && begin_decoration_pointer_action(state, dispatcher)
    {
        state.seat.intercept_pointer_button(button);
        return;
    }

    if button_state == 1 {
        apply_pointer_press_keyboard_focus(state, dispatcher, pointer_focus);
    }

    if !state.session_lock.input_gated() && button_state == 1 {
        if let Some(action) = state
            .config
            .binding_for_pointer_button(&state.keyboard, button)
        {
            if begin_pointer_binding(state, dispatcher, action) {
                state.seat.intercept_pointer_button(button);
                return;
            }
        }
    }

    let Some(pointer_focus) = state.seat.pointer_focus else {
        if button_state != 1 {
            finish_pointer_interactive_grab(state, dispatcher, time);
        }
        return;
    };

    dispatch_client_event(
        dispatcher,
        pointer_focus.target.client_id(),
        SeatClientEvent::PointerButton { serial, time, button, state: button_state },
    );
    dispatch_client_event(
        dispatcher,
        pointer_focus.target.client_id(),
        SeatClientEvent::PointerFrame,
    );

    if button_state != 1 {
        finish_pointer_interactive_grab(state, dispatcher, time);
    }
}

/// Deliver one scroll frame to the pointer-focused client. Pointer resources expand it according
/// to the `wl_pointer` version they were bound with. A compositor grab that clears pointer focus
/// therefore drops the scroll.
pub fn handle_pointer_scroll(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    frame: PointerScrollFrame,
) {
    if frame.vertical.is_none() && frame.horizontal.is_none() {
        return;
    }
    let Some(pointer_focus) = state.seat.pointer_focus else {
        return;
    };
    dispatch_client_event(
        dispatcher,
        pointer_focus.target.client_id(),
        SeatClientEvent::PointerScroll(frame),
    );
}

pub fn handle_pointer_motion_absolute(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    x: f64,
    y: f64,
    time: u32,
) {
    if state.seat.active_pointer_lock.is_some() {
        return;
    }
    let old_pointer_pos = state.seat.pointer_position();
    state.seat.pointer_pos = (x, y);
    crate::input::pointer_confine::confine_pointer_motion(state, old_pointer_pos);
    clamp_pointer_to_outputs(state);
    let new_pointer_pos = state.seat.pointer_position();
    // log::debug!(
    //     "seat pointer motion absolute pos=({:.1}, {:.1}) time={}",
    //     state.seat.pointer_pos.0,
    //     state.seat.pointer_pos.1,
    //     time
    // );
    if !state.session_lock.input_gated() {
        crate::input::toplevel_drag::motion(state, dispatcher);
    }
    if !state.session_lock.input_gated() && refresh_drag_focus(state, dispatcher, time, true) {
        refresh_visible_cursor_surface_outputs(state, dispatcher);
        refresh_visible_dnd_icon_surface_outputs(state, dispatcher);
        if state.seat.cursor_is_visible() || visible_dnd_icon_owner_client(state).is_some() {
            request_outputs_for_points(state, [old_pointer_pos, new_pointer_pos], dispatcher);
        }
        return;
    }
    if !state.session_lock.input_gated() && apply_pointer_interactive_grab(state, dispatcher) {
        return;
    }
    refresh_pointer_focus(state, dispatcher, time, true);
    refresh_visible_cursor_surface_outputs(state, dispatcher);
    refresh_visible_dnd_icon_surface_outputs(state, dispatcher);
    if state.seat.cursor_is_visible() || visible_dnd_icon_owner_client(state).is_some() {
        request_outputs_for_points(state, [old_pointer_pos, new_pointer_pos], dispatcher);
    }
}

/// Release stuck keys, buttons, focus, and grabs before a device pause.
pub fn gate_for_session_lock(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::gesture::abort_owned_swipe(state, dispatcher);
    cancel_active_drag(state, dispatcher, None);
    if let Some((client_id, popups)) = state.seat.dismiss_popup_grab() {
        dispatcher.dispatch_to(client_id, ClientAction::DismissPopupGrab { popups });
    }
    let time = monotonic_time_msec();
    if state.seat.pointer_interactive_grab.is_some() {
        abort_pointer_interactive_grab(state, dispatcher, time);
    }
    let keys = state.keyboard.pressed_keys().to_u32s();
    let buttons: Vec<u32> = state.seat.pressed_buttons.iter().copied().collect();
    for key in keys {
        handle_keyboard_key(state, dispatcher, key, 0, time);
    }
    for button in buttons {
        handle_pointer_button(state, dispatcher, button, 0, time);
    }
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    set_keyboard_focus(state, dispatcher, None);
    clear_pointer_focus(state, dispatcher);
    state.seat.set_default_cursor();
    state.seat.invalidate_implicit_grabs();
}

pub fn quiesce_for_session_pause(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::gesture::abort_owned_swipe(state, dispatcher);
    // Cancel before replaying buttons. A release would otherwise complete a drop.
    cancel_active_drag(state, dispatcher, None);
    let time = monotonic_time_msec();
    if state.seat.pointer_interactive_grab.is_some() {
        abort_pointer_interactive_grab(state, dispatcher, time);
    }
    let keys = state.keyboard.pressed_keys().to_u32s();
    let buttons: Vec<u32> = state.seat.pressed_buttons.iter().copied().collect();
    for key in keys {
        handle_keyboard_key(state, dispatcher, key, 0, time);
    }
    for button in buttons {
        handle_pointer_button(state, dispatcher, button, 0, time);
    }
    if let Some((client_id, popups)) = state.seat.dismiss_popup_grab() {
        dispatcher.dispatch_to(client_id, ClientAction::DismissPopupGrab { popups });
    }
    set_keyboard_focus(state, dispatcher, None);
    clear_pointer_focus(state, dispatcher);
    if !state.seat.cursor_is_visible() {
        state.seat.set_default_cursor();
    }
    state.seat.pointer_button_count = 0;
    state.seat.pressed_buttons.clear();
    state.seat.last_pointer_press = None;
    state.seat.intercepted_keyboard_keys.clear();
    state.seat.intercepted_pointer_buttons.clear();
}

fn clear_pointer_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::gesture::cancel_pointer_gesture(state, dispatcher, monotonic_time_msec());
    crate::input::pointer_lock::release_pointer_lock_unless_surface(state, dispatcher, None);
    crate::input::pointer_confine::release_pointer_confine_unless_surface(state, dispatcher, None);
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

pub fn handle_keyboard_key(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    key: u32,
    key_state: u32,
    time: u32,
) {
    let changed_modifiers = state.keyboard.update_key(key, key_state == 1);
    log::debug!(
        "seat keyboard key key={} state={} time={} focus_before={:?}",
        key,
        key_state,
        time,
        state.seat.keyboard_focus
    );

    if key_state == 0 && state.seat.take_intercepted_keyboard_key(key) {
        log::debug!("seat swallowed intercepted key release key={} time={}", key, time);
        return;
    }

    if key_state == 1 && state.session_active {
        if let Some(vt) = state.keyboard.vt_for_key(key) {
            log::info!("seat VT switch key={} vt={}", key, vt);
            state.seat.intercept_keyboard_key(key);
            dispatcher.dispatch(crate::dbus::DbusAction::SwitchVt { vt });
            return;
        }
    }

    if key_state == 1 && !state.session_lock.input_gated() {
        if let Some(action) = state.config.binding_for_key(&state.keyboard, key) {
            let bypasses_inhibit =
                matches!(action, config::CompositorAction::ToggleShortcutsInhibit);
            if !bypasses_inhibit
                && crate::wayland_objects::zwp_keyboard_shortcuts_inhibitor_v1::shortcuts_inhibited(
                    state,
                )
            {
                log::debug!(
                    "seat binding inhibited by the focused surface key={} action={:?}",
                    key,
                    action
                );
            } else {
                log::info!("seat executing compositor binding key={} action={:?}", key, action);
                state.seat.intercept_keyboard_key(key);
                config::execute_action(state, dispatcher, action);
                return;
            }
        }
    }

    if crate::input::ime::forward_hardware_key(
        state,
        dispatcher,
        key,
        key_state,
        time,
        changed_modifiers,
    ) {
        return;
    }

    if state.session_lock.input_gated() {
        if key_state == 1 {
            let requested = state
                .seat
                .keyboard_focus
                .or_else(|| state.seat.pointer_focus.map(|focus| focus.target));
            set_keyboard_focus(state, dispatcher, requested);
        }
    } else if state.seat.keyboard_focus.is_none() {
        set_keyboard_focus(state, dispatcher, state.seat.pointer_focus.map(|focus| focus.target));
    }

    let Some(focus) = state.seat.keyboard_focus else {
        return;
    };

    let keymap = state.xkb.keymap_blob();
    let modifiers = state.keyboard.modifiers();
    let mut held_keys = state.keyboard.pressed_keys().to_u32s();
    if key_state == 1 {
        held_keys.retain(|pressed| *pressed != key);
    }
    let held = Array::from_u32s(&held_keys);
    ensure_client_keymap(state, dispatcher, focus, keymap, held, modifiers);

    let serial = state.seat.next_serial();
    state.seat.record_keyboard_trigger(serial, focus);
    log::debug!(
        "seat dispatch keyboard key client={:?} serial={} key={} state={}",
        focus.client_id(),
        serial,
        key,
        key_state
    );
    dispatch_client_event(
        dispatcher,
        focus.client_id(),
        SeatClientEvent::KeyboardKey { serial, time, key, state: key_state },
    );

    if let Some(modifiers) = changed_modifiers {
        log::debug!(
            "seat modifiers changed client={:?} serial={} depressed={} latched={} locked={} group={}",
            focus.client_id(),
            state.seat.current_serial,
            modifiers.mods_depressed,
            modifiers.mods_latched,
            modifiers.mods_locked,
            modifiers.group
        );
        dispatch_modifiers(dispatcher, focus.client_id(), state.seat.next_serial(), modifiers);
    }
}

pub fn resolve_activation_target(
    state: &State,
    surface: Resolved<WlSurfaceId>,
) -> Option<SurfaceFocus> {
    crate::input::resolve_activation_target(state, surface)
}

pub fn validate_activation_serial(
    state: &State,
    client: &Client,
    seat: Resolved<WlSeatId>,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
) -> bool {
    crate::input::validate_activation_serial(state, client, seat, surface, serial)
}

pub fn set_keyboard_focus_from_surface(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    new_focus: Option<SurfaceFocus>,
) {
    crate::input::set_keyboard_focus_from_surface(state, dispatcher, new_focus);
}

pub fn refresh_keyboard_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::refresh_keyboard_focus(state, dispatcher);
}

pub fn refresh_keyboard_focus_with_snapshot(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    snapshot: Option<LayerLayoutObjectSnapshot<'_>>,
) {
    crate::input::refresh_keyboard_focus_with_snapshot(state, dispatcher, snapshot);
}

pub fn refresh_scene_focus(state: &mut State, dispatcher: &mut Dispatcher) {
    refresh_pointer_focus(state, dispatcher, monotonic_time_msec(), false);
    refresh_keyboard_focus(state, dispatcher);
}

pub fn focus_after_client_disconnect(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::focus_after_client_disconnect(state, dispatcher);
}

pub fn refresh_active_drag_focus(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    crate::input::refresh_active_drag_focus(state, dispatcher, time);
}

/// Send `keymap` to `focus` when that client still has a different one.
///
/// `keys` are the keys already down. An empty list keeps the client entered
/// and only sends the keymap and modifiers. A non-empty list wraps the keymap
/// in `leave` and `enter` so the client re-reads those keys with the new map.
pub(crate) fn ensure_client_keymap(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    focus: SurfaceFocus,
    keymap: Rc<KeymapBlob>,
    keys: Array,
    modifiers: KeyboardModifiers,
) {
    let current = state
        .seat
        .client_keymap(focus.client_id())
        .map(|keymap| keymap.id)
        .unwrap_or(0);
    if current == keymap.id {
        return;
    }
    log::debug!(
        "seat keymap switch client={:?} from={} to={}",
        focus.client_id(),
        current,
        keymap.id
    );
    state
        .seat
        .remember_client_keymap(focus.client_id(), Rc::clone(&keymap));
    let serial = state.seat.next_serial();
    dispatch_client_event(
        dispatcher,
        focus.client_id(),
        SeatClientEvent::KeyboardKeymap {
            serial,
            surface: focus.surface,
            keymap,
            keys,
            mods_depressed: modifiers.mods_depressed,
            mods_latched: modifiers.mods_latched,
            mods_locked: modifiers.mods_locked,
            group: modifiers.group,
        },
    );
}

pub(crate) fn monotonic_time_msec() -> u32 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    let millis = (ts.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add((ts.tv_nsec as u64) / 1_000_000);
    millis.min(u64::from(u32::MAX)) as u32
}

pub fn finish_active_drag(state: &mut State, dispatcher: &mut Dispatcher) {
    crate::input::toplevel_drag::end(state, dispatcher, ToplevelDragEnd::Cancelled);
    let Some(drag) = state.seat.clear_active_drag() else {
        return;
    };
    state.seat.clear_dnd_icon_outputs_hint();
    if drag.icon_surface.is_some() {
        dispatch_client_action(
            dispatcher,
            drag.source.initiator_client_id(),
            ClientAction::RefreshSurfaceOutputs,
        );
        request_outputs_for_points(state, [state.seat.pointer_position()], dispatcher);
    }
    if let Some(target) = drag.target {
        dispatcher.dispatch_to(target.client_id(), ClientAction::DataDeviceDragLeave);
    }
    crate::input::pointer_lock::recheck_pointer_capture(state, dispatcher);
}

pub fn cancel_active_drag(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    source: Option<Resolved<WlDataSourceId>>,
) {
    crate::input::toplevel_drag::end(state, dispatcher, ToplevelDragEnd::Cancelled);
    let Some(drag) = state.seat.clear_active_drag() else {
        return;
    };
    state.seat.clear_dnd_icon_outputs_hint();
    if drag.icon_surface.is_some() {
        dispatch_client_action(
            dispatcher,
            drag.source.initiator_client_id(),
            ClientAction::RefreshSurfaceOutputs,
        );
        request_outputs_for_points(state, [state.seat.pointer_position()], dispatcher);
    }
    if let Some(target) = drag.target {
        dispatcher.dispatch_to(target.client_id(), ClientAction::DataDeviceDragLeave);
    }
    let source = source.or(drag.source.resolved());
    if let Some(source) = source {
        dispatcher.dispatch_to(
            source.client_id(),
            ClientAction::DataSourceEvent {
                source,
                event: crate::clients::DataSourceClientEvent::Cancelled,
            },
        );
    }
    crate::input::pointer_lock::recheck_pointer_capture(state, dispatcher);
}

fn apply_pointer_interactive_grab(state: &mut State, dispatcher: &mut Dispatcher) -> bool {
    crate::input::apply_pointer_interactive_grab(state, dispatcher)
}

fn finish_pointer_interactive_grab(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
    crate::input::finish_pointer_interactive_grab(state, dispatcher, time);
}

pub(crate) fn abort_pointer_interactive_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
) -> bool {
    crate::input::abort_pointer_interactive_grab(state, dispatcher, time)
}

pub(crate) fn tiled_resize_active_root_surfaces(
    state: &State,
) -> BTreeSet<crate::clients::Resolved<crate::protocol_generated::WlSurfaceId>> {
    crate::input::tiled_resize_active_root_surfaces(state)
}

pub(crate) fn recover_pointer_after_interactive_grab_loss(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
) {
    crate::input::recover_pointer_after_interactive_grab_loss(state, dispatcher, time);
}

fn begin_pointer_binding(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    action: config::MouseBindingAction,
) -> bool {
    crate::input::begin_pointer_binding(state, dispatcher, action)
}

fn begin_decoration_pointer_action(state: &mut State, dispatcher: &mut Dispatcher) -> bool {
    crate::input::begin_decoration_pointer_action(state, dispatcher)
}

fn scene_pick_at(state: &State, pointer_pos: (f64, f64)) -> ScenePick {
    crate::input::scene_pick_at(state, pointer_pos)
}

fn window_hit_at(state: &State, pointer_pos: (f64, f64)) -> Option<WindowHit> {
    crate::input::window_hit_at(state, pointer_pos)
}

fn decoration_hover_cursor_icon_names(state: &State) -> Option<&'static [&'static str]> {
    crate::input::decoration_hover_cursor_icon_names(state)
}

fn update_idle_pointer_cursor(state: &mut State, dispatcher: &mut Dispatcher) {
    if state.seat.pointer_focus.is_some() {
        return;
    }

    let previous_cursor_owner = visible_cursor_owner_client(state);
    let changed = if let Some(icon_names) = decoration_hover_cursor_icon_names(state) {
        state.seat.set_compositor_named_cursor(icon_names)
    } else {
        state.seat.set_default_cursor()
    };
    if !changed {
        return;
    }

    if let Some(client_id) = previous_cursor_owner {
        dispatch_client_action(dispatcher, client_id, ClientAction::RefreshSurfaceOutputs);
    }
    request_outputs_for_points(state, [state.seat.pointer_position()], dispatcher);
}

pub(crate) fn refresh_pointer_focus(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    time: u32,
    send_motion: bool,
) {
    crate::input::refresh_pointer_focus(state, dispatcher, time, send_motion);
    update_idle_pointer_cursor(state, dispatcher);
}

fn send_relative_pointer_motion(
    state: &State,
    dispatcher: &mut Dispatcher,
    dx: f64,
    dy: f64,
    dx_unaccel: f64,
    dy_unaccel: f64,
    time_usec: u64,
) {
    let Some(focus) = state.seat.pointer_focus else {
        return;
    };
    let client_id = focus.target.client_id();
    if state
        .seat
        .relative_pointers_for_client(client_id)
        .is_empty()
    {
        return;
    }
    dispatch_client_event(
        dispatcher,
        client_id,
        SeatClientEvent::RelativePointerMotion {
            utime_hi: (time_usec >> 32) as u32,
            utime_lo: time_usec as u32,
            dx: dx as f32,
            dy: dy as f32,
            dx_unaccel: dx_unaccel as f32,
            dy_unaccel: dy_unaccel as f32,
        },
    );
}

pub(crate) fn clamp_pointer_to_outputs(state: &mut State) {
    let x = state.seat.pointer_pos.0.round() as i32;
    let y = state.seat.pointer_pos.1.round() as i32;
    let Some((clamped_x, clamped_y)) = state.clamp_point_to_outputs(x, y) else {
        return;
    };
    state.seat.pointer_pos.0 = clamped_x as f64;
    state.seat.pointer_pos.1 = clamped_y as f64;
}

fn output_bounds(state: &State) -> Option<crate::region::RectI> {
    state.output_layout_bounds()
}

pub(crate) fn complete_drag_drop(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(drag) = state.seat.active_drag() else {
        return;
    };
    if state.seat.active_drag_can_drop() {
        let target = drag.target.expect("validated active drag target");
        state.seat.set_active_drag_phase(DragPhase::DropSent);
        crate::input::toplevel_drag::end(state, dispatcher, ToplevelDragEnd::Dropped);
        dispatcher.dispatch_to(target.client_id(), ClientAction::DataDeviceDragDrop);
        if let Some(source) = drag.source.resolved() {
            dispatcher.dispatch_to(
                source.client_id(),
                ClientAction::DataSourceEvent {
                    source,
                    event: crate::clients::DataSourceClientEvent::DndDropPerformed,
                },
            );
        } else {
            finish_active_drag(state, dispatcher);
        }
    } else {
        crate::input::toplevel_drag::end(state, dispatcher, ToplevelDragEnd::ReleasedWithoutTarget);
        cancel_active_drag(state, dispatcher, None);
    }
}

pub(crate) fn dispatch_client_event(
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    event: SeatClientEvent,
) {
    dispatcher.dispatch_to(client_id, ClientAction::SeatEvent { event });
}

pub(crate) fn dispatch_client_action(
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    action: ClientAction,
) {
    dispatcher.dispatch_to(client_id, action);
}

fn visible_cursor_owner_client(state: &State) -> Option<ClientId> {
    match state.seat.visible_cursor_source()? {
        CursorSourceRef::Named { owner_client_id, .. } => owner_client_id,
        CursorSourceRef::Surface { cursor } => Some(cursor.surface.client_id()),
    }
}

fn visible_dnd_icon_owner_client(state: &State) -> Option<ClientId> {
    state
        .seat
        .active_drag()
        .and_then(|drag| drag.icon_surface.map(|_| drag.source.initiator_client_id()))
}

/// Root bounds only, the same rectangle `collect_dnd_render_items` uses for the
/// icon. A subsurface that sticks out of the icon or cursor can get `enter` /
/// `leave` late, until its next commit. Commits already trigger a full refresh.
fn refresh_visible_cursor_surface_outputs(state: &mut State, dispatcher: &mut Dispatcher) {
    let cursor_surface = match state.seat.visible_cursor_source() {
        Some(CursorSourceRef::Surface { cursor }) => Some(cursor.surface),
        _ => None,
    };
    let Some(cursor_surface) = cursor_surface else {
        state.seat.replace_cursor_outputs_hint(None);
        return;
    };
    let Some((surface, base_x, base_y)) =
        active_cursor_surface_refresh_params(state, cursor_surface.client_id())
    else {
        state.seat.replace_cursor_outputs_hint(None);
        return;
    };
    let outputs = root_surface_outputs_at(state, surface, base_x, base_y);
    if !state
        .seat
        .replace_cursor_outputs_hint(Some((surface, outputs)))
    {
        return;
    }
    dispatch_client_action(
        dispatcher,
        surface.client_id(),
        ClientAction::RefreshSurfaceTreeOutputs { root: surface },
    );
}

/// See [`refresh_visible_cursor_surface_outputs`] for the root-bounds limitation.
fn refresh_visible_dnd_icon_surface_outputs(state: &mut State, dispatcher: &mut Dispatcher) {
    let icon_and_owner = state.seat.active_drag().and_then(|drag| {
        drag.icon_surface
            .map(|icon| (icon, drag.source.initiator_client_id()))
    });
    let Some((icon, owner)) = icon_and_owner else {
        state.seat.replace_dnd_icon_outputs_hint(None);
        return;
    };
    let Some((surface, base_x, base_y)) = active_dnd_icon_surface_refresh_params(state, owner)
    else {
        state.seat.replace_dnd_icon_outputs_hint(None);
        return;
    };
    let outputs = root_surface_outputs_at(state, surface, base_x, base_y);
    if !state
        .seat
        .replace_dnd_icon_outputs_hint(Some((surface, outputs)))
    {
        return;
    }
    dispatch_client_action(
        dispatcher,
        owner,
        ClientAction::RefreshSurfaceTreeOutputs { root: icon },
    );
}

pub fn begin_popup_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    popup: Resolved<XdgPopupId>,
    popup_surface: Resolved<WlSurfaceId>,
) {
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    state.seat.start_popup_grab(client_id, popup, popup_surface);
    if let Some(focus) = popup_keyboard_focus(state) {
        set_keyboard_focus(state, dispatcher, Some(focus));
    }
}

pub fn request_popup_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    _parent_surface: Resolved<WlSurfaceId>,
    _serial: u32,
    popup: Resolved<XdgPopupId>,
    popup_surface: Resolved<WlSurfaceId>,
) -> bool {
    if state.session_lock.input_gated() {
        return false;
    }
    if popup_surface.client_id() != client_id || !popup_surface.exists(state) {
        return false;
    }
    begin_popup_grab(state, dispatcher, client_id, popup, popup_surface);
    true
}

pub fn remove_popup_grab(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    popup: Resolved<XdgPopupId>,
    fallback_focus: Option<SurfaceFocus>,
) {
    state.seat.remove_popup_grab(popup);
    if let Some(focus) = popup_keyboard_focus(state) {
        set_keyboard_focus(state, dispatcher, Some(focus));
    } else if state.seat.popup_grab.is_none() {
        set_keyboard_focus(
            state,
            dispatcher,
            fallback_focus.or_else(|| state.seat.pointer_focus.map(|focus| focus.target)),
        );
    }
    crate::input::pointer_lock::recheck_pointer_capture(state, dispatcher);
}

pub fn request_toplevel_move(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
    root_surface: Resolved<WlSurfaceId>,
    xdg_toplevel: Resolved<XdgToplevelId>,
    configure_decoration_mode: ToplevelDecorationMode,
    is_maximized: bool,
    is_fullscreen: bool,
) -> bool {
    crate::input::request_toplevel_move(
        state,
        dispatcher,
        client_id,
        surface,
        serial,
        root_surface,
        xdg_toplevel,
        configure_decoration_mode,
        is_maximized,
        is_fullscreen,
    )
}

pub fn request_toplevel_resize(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    surface: Resolved<WlSurfaceId>,
    serial: u32,
    root_surface: Resolved<WlSurfaceId>,
    xdg_toplevel: Resolved<XdgToplevelId>,
    configure_decoration_mode: ToplevelDecorationMode,
    size_constraints: InteractiveSizeConstraints,
    is_maximized: bool,
    is_fullscreen: bool,
    edges: u32,
) -> bool {
    crate::input::request_toplevel_resize(
        state,
        dispatcher,
        client_id,
        surface,
        serial,
        root_surface,
        xdg_toplevel,
        configure_decoration_mode,
        size_constraints,
        is_maximized,
        is_fullscreen,
        edges,
    )
}

fn remove_resource<K, V>(map: &mut BTreeMap<K, Vec<V>>, key: K, value: V)
where
    K: Ord + Copy,
    V: PartialEq,
{
    let Some(resources) = map.get_mut(&key) else {
        return;
    };
    resources.retain(|resource| *resource != value);
    if resources.is_empty() {
        map.remove(&key);
    }
}

pub fn empty_keys() -> Array {
    Array::from_u32s(&[])
}

pub(crate) fn resend_keyboard_modifiers(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(focus) = state.seat.keyboard_focus else {
        return;
    };
    let modifiers = state.keyboard.modifiers();
    dispatch_modifiers(dispatcher, focus.client_id(), state.seat.next_serial(), modifiers);
}

fn dispatch_modifiers(
    dispatcher: &mut Dispatcher,
    client_id: ClientId,
    serial: u32,
    modifiers: KeyboardModifiers,
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

#[cfg(test)]
mod surface_tree_hit_test_tests {
    use {
        super::surface_focus_pick_at,
        crate::{
            protocol_generated::{WlSubsurfaceId, WlSurfaceId},
            state::ObjectId,
            test_support::{
                insert_root_surface as insert_surface,
                insert_wl_subsurface_client1 as insert_subsurface, resolved_wl_surface as root,
                state_for_tests,
            },
            wayland_objects::wl_surface::WlSurface,
        },
    };

    #[test]
    fn hit_test_prefers_above_then_parent_then_below() {
        let mut state = state_for_tests();
        let root_object_id = insert_surface(&mut state, 1);
        let below = insert_surface(&mut state, 2);
        let above = insert_surface(&mut state, 3);
        let root_resolved = root(&state, root_object_id);
        let below_resolved = root(&state, below);
        let above_resolved = root(&state, above);
        let below_subsurface =
            insert_subsurface(&mut state, WlSubsurfaceId::from(31), below, root_object_id);
        let above_subsurface =
            insert_subsurface(&mut state, WlSubsurfaceId::from(32), above, root_object_id);

        {
            let root_surface = state
                .objects
                .get_mut(root_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.test_map(ObjectId::from(200usize), 0, 0, 20, 20);
            root_surface.add_pending_child(below_resolved);
            root_surface.add_pending_child(above_resolved);
            assert!(root_surface.restack_child_relative(below_resolved, None, false));
            assert!(root_surface.restack_child_relative(above_resolved, None, true));
        }
        {
            let below_surface = state
                .objects
                .get_mut(below)
                .downcast_mut::<WlSurface>()
                .unwrap();
            below_surface.assign_subsurface(below_subsurface, root_resolved);
            below_surface.set_subsurface_position(0, 0);
            below_surface.test_map(ObjectId::from(201usize), 0, 0, 20, 20);
        }
        {
            let above_surface = state
                .objects
                .get_mut(above)
                .downcast_mut::<WlSurface>()
                .unwrap();
            above_surface.assign_subsurface(above_subsurface, root_resolved);
            above_surface.set_subsurface_position(0, 0);
            above_surface.test_map(ObjectId::from(202usize), 0, 0, 20, 20);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(root_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }

        let hit = surface_focus_pick_at(&state, (5.0, 5.0)).unwrap();
        assert_eq!(hit.target.surface_id(), WlSurfaceId::from(3));
    }

    #[test]
    fn hit_test_uses_nested_offsets_and_falls_back_when_child_rejects_input() {
        let mut state = state_for_tests();
        let root_object_id = insert_surface(&mut state, 1);
        let child = insert_surface(&mut state, 2);
        let grandchild = insert_surface(&mut state, 3);
        let root_resolved = root(&state, root_object_id);
        let child_resolved = root(&state, child);
        let grandchild_resolved = root(&state, grandchild);
        let child_subsurface =
            insert_subsurface(&mut state, WlSubsurfaceId::from(41), child, root_object_id);
        let grandchild_subsurface =
            insert_subsurface(&mut state, WlSubsurfaceId::from(42), grandchild, child);

        {
            let root_surface = state
                .objects
                .get_mut(root_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.test_map(ObjectId::from(210usize), 50, 60, 100, 100);
            root_surface.add_pending_child(child_resolved);
            assert!(root_surface.restack_child_relative(child_resolved, None, true));
        }
        {
            let child_surface = state
                .objects
                .get_mut(child)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, root_resolved);
            child_surface.set_subsurface_position(10, 10);
            child_surface.test_map(ObjectId::from(211usize), 0, 0, 40, 40);
            child_surface.add_pending_child(grandchild_resolved);
        }
        {
            let grandchild_surface = state
                .objects
                .get_mut(grandchild)
                .downcast_mut::<WlSurface>()
                .unwrap();
            grandchild_surface.assign_subsurface(grandchild_subsurface, child_resolved);
            grandchild_surface.set_subsurface_position(5, 6);
            grandchild_surface.test_map(ObjectId::from(212usize), 0, 0, 20, 20);
            grandchild_surface.test_set_input_region(crate::region::RegionValue::empty());
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(root_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }
        {
            let mut child_surface = state.objects.grab_object::<WlSurface>(child);
            assert!(child_surface.apply_pending_tree_state(&mut state));
        }

        let fallback_hit = surface_focus_pick_at(&state, (66.0, 77.0)).unwrap();
        assert_eq!(fallback_hit.target.surface_id(), WlSurfaceId::from(2));

        let parent_hit = surface_focus_pick_at(&state, (145.0, 145.0)).unwrap();
        assert_eq!(parent_hit.target.surface_id(), WlSurfaceId::from(1));
    }

    #[test]
    fn hit_test_uses_explicit_root_surface_order() {
        let mut state = state_for_tests();
        let root_a = insert_surface(&mut state, 1);
        let root_b = insert_surface(&mut state, 2);

        {
            let root_a_surface = state
                .objects
                .get_mut(root_a)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_a_surface.test_map(ObjectId::from(220usize), 0, 0, 20, 20);
        }
        {
            let root_b_surface = state
                .objects
                .get_mut(root_b)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_b_surface.test_map(ObjectId::from(221usize), 0, 0, 20, 20);
        }

        state.register_root_surface(root(&state, root_a));

        let hit = surface_focus_pick_at(&state, (5.0, 5.0)).unwrap();
        assert_eq!(hit.target.surface_id(), WlSurfaceId::from(1));
    }

    #[test]
    fn hit_test_prefers_topmost_workspace_root_from_scene_order() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(crate::test_support::test_output_drm1(0, 0, 200, 100));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let lower = insert_surface(&mut state, 11);
        let upper = insert_surface(&mut state, 12);

        {
            let surface = state
                .objects
                .get_mut(lower)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(230usize), 0, 0, 50, 50);
        }
        {
            let surface = state
                .objects
                .get_mut(upper)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(231usize), 0, 0, 50, 50);
        }

        let _ = state.assign_root_surface_to_workspace(root(&state, lower), workspace_id);
        let _ = state.assign_root_surface_to_workspace(root(&state, upper), workspace_id);
        let managed_lower = root(&state, lower);
        crate::test_support::insert_window(&mut state, managed_lower);
        let managed_upper = root(&state, upper);
        crate::test_support::insert_window(&mut state, managed_upper);
        let lower_window = state.window_for_root(managed_lower).unwrap();
        let upper_window = state.window_for_root(managed_upper).unwrap();
        state
            .workspaces
            .try_get_mut(workspace_id)
            .unwrap()
            .layout
            .floating
            .stack = vec![lower_window, upper_window];
        state
            .managed_window_mut(root(&state, lower))
            .unwrap()
            .mapped = true;
        state
            .managed_window_mut(root(&state, upper))
            .unwrap()
            .mapped = true;

        let hit = surface_focus_pick_at(&state, (5.0, 5.0)).unwrap();
        assert_eq!(hit.target.surface_id(), WlSurfaceId::from(12));
    }
}

#[cfg(test)]
mod xdg_seat_tests {
    use {
        super::{
            begin_popup_grab, cancel_active_drag, complete_drag_drop,
            finish_pointer_interactive_grab, handle_keyboard_key, handle_pointer_button,
            handle_pointer_motion, handle_pointer_motion_absolute, handle_pointer_scroll,
            pointer_scroll_protocol_events, quiesce_for_session_pause,
            recover_pointer_after_interactive_grab_loss, refresh_visible_cursor_surface_outputs,
            refresh_visible_dnd_icon_surface_outputs, remove_popup_grab, scene_pick_at,
            set_keyboard_focus, window_hit_at, CompositorZone, PointerAxisChange,
            PointerScrollAxis, PointerScrollDirection, PointerScrollFrame, PointerScrollSource,
            PointerScrollWireEvent, SeatClientEvent, SeatState,
        },
        crate::{
            clients::{
                Client, ClientAction, ClientId, ClientManagerAction, DataSourceClientEvent,
                RawEvent, Resolved,
            },
            config::RuntimeConfig,
            cursor::CursorSourceRef,
            input::SurfaceFocus,
            object::Object,
            protocol_generated::{
                xdg_toplevel, zwlr_layer_surface_v1, Constants, WlPointerId, WlSeatId,
                WlSubsurfaceId, WlSurfaceId, XdgPopupId, XdgPositionerId, XdgSurfaceId,
                XdgToplevelId, XdgWmBaseId, ZwlrLayerShellV1Id, ZwlrLayerSurfaceV1Id,
                ZwpKeyboardShortcutsInhibitorV1Id, ZwpRelativePointerV1Id,
            },
            region::{RectI, RegionValue},
            runner::Dispatcher,
            state::{
                BackendOutputRef, DragPhase, DragSource, DragState, DragTargetState, DrmId,
                ObjectId, State, WorkspaceGroup,
            },
            test_support::{
                fake_focus, focus_for_surface, get_toplevel_request, insert_window,
                insert_wl_subsurface, raw_request_from_event, resolved_wl_surface as root,
                state_for_tests, test_client_and_dispatcher, test_output_drm1,
                test_output_with_crtc_name,
            },
            wayland_objects::{
                wl_pointer::WlPointer,
                wl_seat::WlSeat,
                wl_surface::{Role, WlSurface},
                xdg_popup::XdgPopup,
                xdg_positioner::{PositionerState, XdgPositioner},
                xdg_surface::{
                    xdg_toplevel::{ToplevelDecorationMode, XdgToplevel},
                    XdgSurface,
                },
                xdg_wm_base::XdgWmBase,
                zwlr_layer_surface_v1::{LayerShellLayer, ZwlrLayerSurfaceV1},
                zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
                zwp_relative_pointer_v1::ZwpRelativePointerV1,
            },
        },
    };

    fn assert_named_cursor_matches_any(seat: &SeatState, expected_names: &[&str]) {
        assert!(matches!(
            seat.visible_cursor_source(),
            Some(CursorSourceRef::Named { owner_client_id: None, name, .. })
                if expected_names.contains(&name)
        ));
    }

    fn insert_surface(state: &mut State, client: &mut Client, surface_id: WlSurfaceId) -> ObjectId {
        let (resolved, _) =
            client.insert_object(state, WlSurface::new(surface_id, client.client_id(), 6));
        let object_id = resolved.object_id();
        state.register_root_surface(resolved);
        object_id
    }

    fn ensure_test_root_geometry(state: &mut State, object_id: ObjectId) {
        let root_surface = root(state, object_id);
        state.mark_root_geometry_dirty(root_surface);
        let _ = state.committed_root_geometry(root_surface);
    }

    fn insert_xdg_surface(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        wl_surface_id: WlSurfaceId,
        xdg_surface_id: XdgSurfaceId,
    ) -> (ObjectId, ObjectId) {
        let wl_surface_object_id = insert_surface(state, client, wl_surface_id);
        {
            let wl_surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            assert!(wl_surface.set_role(Role::XdgSurface, dispatcher).is_ok());
        }
        let (xdg_surface_resolved, _) = client.insert_object(
            state,
            XdgSurface::new(
                xdg_surface_id,
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    ObjectId::from(1usize),
                ),
                crate::clients::Resolved::new(
                    client.client_id(),
                    wl_surface_id,
                    wl_surface_object_id,
                ),
                7,
            ),
        );
        let xdg_surface_object_id = xdg_surface_resolved.object_id();
        state
            .objects
            .get_mut(wl_surface_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .set_xdg_surface_impl(crate::clients::Resolved::new(
                client.client_id(),
                xdg_surface_id,
                xdg_surface_object_id,
            ));
        (wl_surface_object_id, xdg_surface_object_id)
    }

    fn popup_grab_request(
        popup_id: XdgPopupId,
        seat_id: WlSeatId,
        serial: u32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(popup_id.into(), 1, 2);
        event.id(seat_id);
        event.uint(serial);
        raw_request_from_event(event)
    }

    fn popup_destroy_request(popup_id: XdgPopupId) -> crate::clients::RawRequest<'static> {
        raw_request_from_event(RawEvent::new(popup_id.into(), 0, 0))
    }

    fn toplevel_move_request(
        toplevel_id: XdgToplevelId,
        seat_id: WlSeatId,
        serial: u32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(toplevel_id.into(), 5, 2);
        event.id(seat_id);
        event.uint(serial);
        raw_request_from_event(event)
    }

    fn toplevel_resize_request(
        toplevel_id: XdgToplevelId,
        seat_id: WlSeatId,
        serial: u32,
        edges: u32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(toplevel_id.into(), 6, 3);
        event.id(seat_id);
        event.uint(serial);
        event.uint(edges);
        raw_request_from_event(event)
    }

    fn get_popup_request(
        xdg_surface_id: XdgSurfaceId,
        popup_id: XdgPopupId,
        parent_id: XdgSurfaceId,
        positioner_id: XdgPositionerId,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(xdg_surface_id.into(), 2, 3);
        event.id(popup_id);
        event.id(parent_id);
        event.id(positioner_id);
        raw_request_from_event(event)
    }

    fn positioner_set_size_request(
        positioner_id: XdgPositionerId,
        width: i32,
        height: i32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(positioner_id.into(), 1, 2);
        event.int(width);
        event.int(height);
        raw_request_from_event(event)
    }

    fn positioner_set_anchor_rect_request(
        positioner_id: XdgPositionerId,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(positioner_id.into(), 2, 4);
        event.int(x);
        event.int(y);
        event.int(width);
        event.int(height);
        raw_request_from_event(event)
    }

    fn assign_toplevel_role(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        xdg_surface_object_id: ObjectId,
        xdg_surface_id: XdgSurfaceId,
        toplevel_id: XdgToplevelId,
    ) -> ObjectId {
        let xdg_surface = state
            .objects
            .grab_object::<XdgSurface>(xdg_surface_object_id);
        let _ = xdg_surface.on(
            client,
            state,
            get_toplevel_request(xdg_surface_id, toplevel_id),
            dispatcher,
        );
        dispatcher.drain_post_actions(client, state);
        client.object_id(toplevel_id)
    }

    fn set_toplevel_window_state(
        state: &mut State,
        toplevel_object_id: ObjectId,
        maximized: bool,
        fullscreen: bool,
    ) {
        let toplevel = state
            .objects
            .get_mut(toplevel_object_id)
            .downcast_mut::<XdgToplevel>()
            .unwrap();
        toplevel.test_set_maximized(maximized);
        toplevel.test_set_fullscreen(fullscreen);
    }

    #[test]
    fn popup_grab_accepts_untracked_serial() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(2)));

        let (_parent_wl_surface_object_id, parent_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(10),
            XdgSurfaceId::from(20),
        );
        let _parent_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            parent_xdg_surface_object_id,
            XdgSurfaceId::from(20),
            XdgToplevelId::from(30),
        );
        let (popup_wl_surface_object_id, popup_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(11),
            XdgSurfaceId::from(21),
        );

        let (popup_resolved, _) = client.insert_object(
            &mut state,
            XdgPopup::new(
                XdgPopupId::from(40),
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgSurfaceId::from(21),
                    popup_xdg_surface_object_id,
                ),
                Some(crate::clients::Resolved::new(
                    client.client_id(),
                    XdgSurfaceId::from(20),
                    parent_xdg_surface_object_id,
                )),
                PositionerState::default(),
                XdgWmBase::INTERFACE_VERSION,
            ),
        );
        let popup_object_id = popup_resolved.object_id();

        {
            let popup = state.objects.grab_object::<XdgPopup>(popup_object_id);
            let _ = popup.on(
                &mut client,
                &mut state,
                popup_grab_request(XdgPopupId::from(40), WlSeatId::from(2), 999),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        let popup = state
            .objects
            .get(popup_object_id)
            .downcast_ref::<XdgPopup>()
            .unwrap();
        assert!(popup.has_explicit_grab());
        assert_eq!(
            state.seat.popup_grab.as_ref().unwrap().popup_stack,
            vec![(
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgPopupId::from(40),
                    popup_object_id,
                ),
                crate::clients::Resolved::new(
                    client.client_id(),
                    WlSurfaceId::from(11),
                    popup_wl_surface_object_id,
                ),
            )]
        );
        assert_eq!(
            state.seat.keyboard_focus,
            Some(focus_for_surface(
                client.client_id(),
                WlSurfaceId::from(11),
                popup_wl_surface_object_id,
            ))
        );
    }

    #[test]
    fn popup_grab_accepts_matching_serial() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(2)));

        let (parent_wl_surface_object_id, parent_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(10),
            XdgSurfaceId::from(20),
        );
        let _parent_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            parent_xdg_surface_object_id,
            XdgSurfaceId::from(20),
            XdgToplevelId::from(30),
        );
        let (popup_wl_surface_object_id, popup_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(11),
            XdgSurfaceId::from(21),
        );

        let (popup_resolved, _) = client.insert_object(
            &mut state,
            XdgPopup::new(
                XdgPopupId::from(40),
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgSurfaceId::from(21),
                    popup_xdg_surface_object_id,
                ),
                Some(crate::clients::Resolved::new(
                    client.client_id(),
                    XdgSurfaceId::from(20),
                    parent_xdg_surface_object_id,
                )),
                PositionerState::default(),
                XdgWmBase::INTERFACE_VERSION,
            ),
        );
        let popup_object_id = popup_resolved.object_id();

        let parent_focus = focus_for_surface(
            client.client_id(),
            WlSurfaceId::from(10),
            parent_wl_surface_object_id,
        );
        let serial = state.seat.next_serial();
        state.seat.record_keyboard_trigger(serial, parent_focus);

        {
            let popup = state.objects.grab_object::<XdgPopup>(popup_object_id);
            let _ = popup.on(
                &mut client,
                &mut state,
                popup_grab_request(XdgPopupId::from(40), WlSeatId::from(2), serial),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        let popup = state
            .objects
            .get(popup_object_id)
            .downcast_ref::<XdgPopup>()
            .unwrap();
        assert!(popup.has_explicit_grab());
        assert_eq!(
            state.seat.popup_grab.as_ref().unwrap().popup_stack,
            vec![(
                crate::clients::Resolved::new(
                    client.client_id(),
                    XdgPopupId::from(40),
                    popup_object_id,
                ),
                crate::clients::Resolved::new(
                    client.client_id(),
                    WlSurfaceId::from(11),
                    popup_wl_surface_object_id,
                ),
            )]
        );
        assert_eq!(
            state.seat.keyboard_focus,
            Some(focus_for_surface(
                client.client_id(),
                WlSurfaceId::from(11),
                popup_wl_surface_object_id,
            ))
        );

        let parent_surface = state
            .objects
            .get(parent_wl_surface_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert_eq!(parent_surface.surface_id(), WlSurfaceId::from(10));
        let popup_xdg_surface = state
            .objects
            .get(popup_xdg_surface_object_id)
            .downcast_ref::<XdgSurface>()
            .unwrap();
        assert_eq!(popup_xdg_surface.wl_surface().wayland_id(), WlSurfaceId::from(11));
    }

    #[test]
    fn dismissed_popup_stays_alive_until_client_destroy() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (_parent_wl_surface_object_id, parent_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(10),
            XdgSurfaceId::from(20),
        );
        let (_popup_wl_surface_object_id, popup_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(11),
            XdgSurfaceId::from(21),
        );
        assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            parent_xdg_surface_object_id,
            XdgSurfaceId::from(20),
            XdgToplevelId::from(30),
        );

        let (positioner_resolved, _) =
            client.insert_object(&mut state, XdgPositioner::new(XdgPositionerId::from(50)));
        let positioner_object_id = positioner_resolved.object_id();
        {
            let positioner = state
                .objects
                .grab_object::<XdgPositioner>(positioner_object_id);
            let _ = positioner.on(
                &mut client,
                &mut state,
                positioner_set_size_request(XdgPositionerId::from(50), 40, 30),
                &mut client_dispatcher,
            );
        }
        {
            let positioner = state
                .objects
                .grab_object::<XdgPositioner>(positioner_object_id);
            let _ = positioner.on(
                &mut client,
                &mut state,
                positioner_set_anchor_rect_request(XdgPositionerId::from(50), 0, 0, 10, 10),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(popup_xdg_surface_object_id);
            let _ = xdg_surface.on(
                &mut client,
                &mut state,
                get_popup_request(
                    XdgSurfaceId::from(21),
                    XdgPopupId::from(40),
                    XdgSurfaceId::from(20),
                    XdgPositionerId::from(50),
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        client.dismiss_popup_ids(
            &mut state,
            &mut client_dispatcher,
            vec![crate::clients::Resolved::new(
                client.client_id(),
                XdgPopupId::from(40),
                client.object_id(XdgPopupId::from(40)),
            )],
            None,
        );

        assert!(client.contains_object(XdgPopupId::from(40), &state));
        let popup_xdg_surface = state
            .objects
            .get(popup_xdg_surface_object_id)
            .downcast_ref::<XdgSurface>()
            .unwrap();
        assert_eq!(
            popup_xdg_surface
                .popup_role()
                .map(|popup| popup.wayland_id()),
            Some(XdgPopupId::from(40))
        );

        {
            let popup_object_id = client.object_id(XdgPopupId::from(40));
            let popup = state.objects.grab_object::<XdgPopup>(popup_object_id);
            let _ = popup.on(
                &mut client,
                &mut state,
                popup_destroy_request(XdgPopupId::from(40)),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(!client.contains_object(XdgPopupId::from(40), &state));
        let popup_xdg_surface = state
            .objects
            .get(popup_xdg_surface_object_id)
            .downcast_ref::<XdgSurface>()
            .unwrap();
        assert_eq!(popup_xdg_surface.popup_role(), None);
    }

    #[test]
    fn popup_parent_click_dismisses_grab() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);

        let parent_focus = fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50));
        let popup_focus = fake_focus(ClientId::from_raw(7), WlSurfaceId::from(51));

        begin_popup_grab(
            &mut state,
            &mut dispatcher,
            parent_focus.client_id(),
            crate::clients::Resolved::new(
                parent_focus.client_id(),
                XdgPopupId::from(60),
                ObjectId::from(60usize),
            ),
            *popup_focus,
        );
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: parent_focus,
            rect: super::SurfaceRect { x: 0, y: 0, width: 100, height: 100 },
        });

        assert!(super::maybe_dismiss_popup_grab_on_button(&mut state, &mut dispatcher, 1, 1000));
        assert!(state.seat.popup_grab.is_none());
    }

    struct MappedPopup {
        parent_object_id: ObjectId,
        popup_object_id: ObjectId,
        popup_surface: crate::clients::Resolved<WlSurfaceId>,
        popup_role: crate::clients::Resolved<XdgPopupId>,
    }

    fn map_toplevel_popup(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
    ) -> MappedPopup {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let (parent_object_id, parent_xdg_object_id) = insert_xdg_surface(
            state,
            client,
            &mut client_dispatcher,
            WlSurfaceId::from(170),
            XdgSurfaceId::from(180),
        );
        let (popup_object_id, popup_xdg_object_id) = insert_xdg_surface(
            state,
            client,
            &mut client_dispatcher,
            WlSurfaceId::from(171),
            XdgSurfaceId::from(181),
        );
        state
            .objects
            .get_mut(parent_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(900usize), 0, 0, 400, 300);
        state
            .objects
            .get_mut(popup_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(901usize), 0, 0, 80, 60);

        let parent_xdg = crate::clients::Resolved::new(
            client.client_id(),
            XdgSurfaceId::from(180),
            parent_xdg_object_id,
        );
        let popup_xdg = crate::clients::Resolved::new(
            client.client_id(),
            XdgSurfaceId::from(181),
            popup_xdg_object_id,
        );
        let (_, popup_role_object_id) = state.insert_object(XdgPopup::new(
            XdgPopupId::from(182),
            popup_xdg,
            Some(parent_xdg),
            PositionerState::default(),
            7,
        ));
        let popup_role = crate::clients::Resolved::new(
            client.client_id(),
            XdgPopupId::from(182),
            popup_role_object_id,
        );
        state
            .objects
            .get_mut(popup_role_object_id)
            .downcast_mut::<XdgPopup>()
            .unwrap()
            .apply_acked_configure(RectI { x: 100, y: 80, width: 80, height: 60 });
        {
            let popup_xdg_surface = state
                .objects
                .get_mut(popup_xdg_object_id)
                .downcast_mut::<XdgSurface>()
                .unwrap();
            popup_xdg_surface.test_set_current_window_geometry(Some(RectI {
                x: 0,
                y: 0,
                width: 80,
                height: 60,
            }));
            popup_xdg_surface.test_set_popup_role(popup_role);
        }
        state
            .objects
            .get_mut(parent_xdg_object_id)
            .downcast_mut::<XdgSurface>()
            .unwrap()
            .add_popup_child(popup_role);
        client_dispatcher.drain_post_actions(client, state);

        MappedPopup {
            parent_object_id,
            popup_object_id,
            popup_surface: crate::clients::Resolved::new(
                client.client_id(),
                WlSurfaceId::from(171),
                popup_object_id,
            ),
            popup_role,
        }
    }

    #[test]
    fn pointer_press_on_ungrabbed_popup_keeps_keyboard_on_toplevel() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mapped = map_toplevel_popup(&mut state, &mut client, &mut dispatcher);
        let parent_focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(170), mapped.parent_object_id);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(parent_focus));
        state.seat.pointer_pos = (120.0, 100.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert_eq!(state.seat.keyboard_focus, Some(parent_focus));
        assert_eq!(
            state
                .seat
                .pointer_focus
                .map(|focus| focus.target.surface_id()),
            Some(WlSurfaceId::from(171))
        );
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::SeatEvent {
                    event: SeatClientEvent::PointerButton { button: 0x110, state: 1, .. },
                }
            )
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::KeyboardLeave { .. } }
            )
        ));
    }

    #[test]
    fn pointer_press_on_a_keyboard_none_layer_keeps_focus() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(0usize)),
            1,
            0,
            0,
            200,
            200,
        ));
        let (mut app, mut dispatcher) = test_client_and_dispatcher(1);
        let window_object = insert_surface(&mut state, &mut app, WlSurfaceId::from(5));
        root(&state, window_object).ref_mut(&mut state).test_map(
            ObjectId::from(5usize),
            10,
            10,
            40,
            40,
        );
        let window_focus = focus_for_surface(app.client_id(), WlSurfaceId::from(5), window_object);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(window_focus));

        let (mut osk, _) = test_client_and_dispatcher(2);
        let layer_object = insert_surface(&mut state, &mut osk, WlSurfaceId::from(7));
        let layer_root = root(&state, layer_object);
        layer_root
            .ref_mut(&mut state)
            .test_map(ObjectId::from(7usize), 10, 80, 120, 40);
        let mut layer = ZwlrLayerSurfaceV1::new(
            ZwlrLayerSurfaceV1Id::from(8),
            Resolved::new(osk.client_id(), ZwlrLayerShellV1Id::from(6), ObjectId::from(6usize)),
            4,
            layer_root,
            Some(output_id),
            LayerShellLayer::Overlay,
            "osk".to_string(),
        );
        layer.test_set_mapped(true);
        let (layer_resolved, _) = osk.insert_object(&mut state, layer);
        state.register_layer_surface_object(layer_resolved);
        layer_root
            .ref_mut(&mut state)
            .set_layer_surface_impl(layer_resolved);

        let layer_focus = focus_for_surface(osk.client_id(), WlSurfaceId::from(7), layer_object);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(layer_focus));
        assert_eq!(state.seat.keyboard_focus, None);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(window_focus));

        state.seat.pointer_pos = (20.0, 90.0);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert_eq!(state.seat.keyboard_focus, Some(window_focus));
        assert_eq!(
            state
                .seat
                .pointer_focus
                .map(|focus| focus.target.surface_id()),
            Some(WlSurfaceId::from(7))
        );
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            osk.client_id(),
            |action| matches!(
                action,
                ClientAction::SeatEvent {
                    event: SeatClientEvent::PointerButton { button: 0x110, state: 1, .. },
                }
            )
        ));

        layer_resolved
            .ref_mut(&mut state)
            .test_set_keyboard_interactivity(
                zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand as u32,
            );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 120);
        assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(layer_root));
    }

    #[test]
    fn pointer_press_on_ungrabbed_popup_subsurface_keeps_keyboard_on_toplevel() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mapped = map_toplevel_popup(&mut state, &mut client, &mut dispatcher);
        let child_object_id = insert_surface(&mut state, &mut client, WlSurfaceId::from(172));
        let child_surface = crate::clients::Resolved::new(
            client.client_id(),
            WlSurfaceId::from(172),
            child_object_id,
        );
        state.unregister_root_surface(child_surface);
        {
            let popup = state
                .objects
                .get_mut(mapped.popup_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            popup.test_set_input_region(RegionValue::empty());
            popup.add_pending_child(child_surface);
        }
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.test_mark_subsurface_at(mapped.popup_surface, (0, 0));
            child.test_map(ObjectId::from(902usize), 0, 0, 80, 60);
        }
        {
            let mut popup = state
                .objects
                .grab_object::<WlSurface>(mapped.popup_object_id);
            assert!(popup.apply_pending_tree_state(&mut state));
        }

        let parent_focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(170), mapped.parent_object_id);
        set_keyboard_focus(&mut state, &mut dispatcher, Some(parent_focus));
        state.seat.pointer_pos = (120.0, 100.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert_eq!(state.seat.keyboard_focus, Some(parent_focus));
        assert_eq!(
            state
                .seat
                .pointer_focus
                .map(|focus| focus.target.surface_id()),
            Some(WlSurfaceId::from(172))
        );
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::SeatEvent {
                    event: SeatClientEvent::PointerButton { button: 0x110, state: 1, .. },
                }
            )
        ));
    }

    #[test]
    fn pointer_press_inside_popup_grab_keeps_keyboard_on_popup() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mapped = map_toplevel_popup(&mut state, &mut client, &mut dispatcher);
        begin_popup_grab(
            &mut state,
            &mut dispatcher,
            client.client_id(),
            mapped.popup_role,
            mapped.popup_surface,
        );
        state.seat.pointer_pos = (120.0, 100.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert_eq!(
            state.seat.keyboard_focus.map(|focus| focus.surface),
            Some(mapped.popup_surface)
        );
        assert_eq!(
            state
                .seat
                .pointer_focus
                .map(|focus| focus.target.surface_id()),
            Some(WlSurfaceId::from(171))
        );
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(
                action,
                ClientAction::SeatEvent {
                    event: SeatClientEvent::PointerButton { button: 0x110, state: 1, .. },
                }
            )
        ));
    }

    #[test]
    fn compositor_mouse_move_binding_starts_drag_and_swallows_button_events() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(170),
            XdgSurfaceId::from(180),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(180),
            XdgToplevelId::from(190),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(900usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (70.0, 80.0);
        assert!(state.seat.set_cursor_surface(
            crate::clients::Resolved::new(
                client.client_id(),
                WlSurfaceId::from(999),
                ObjectId::from(998usize),
            ),
            2,
            3,
        ));

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
        assert_eq!(
            state.seat.keyboard_focus,
            Some(focus_for_surface(
                client.client_id(),
                WlSurfaceId::from(170),
                wl_surface_object_id,
            ))
        );
        assert!(state.seat.pointer_focus.is_none());
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::PointerLeave { surface, .. },
                    } if surface.wayland_id() == WlSurfaceId::from(170)
                )
            }
        ));
        assert!(!matches!(
            state.seat.visible_cursor_source(),
            Some(CursorSourceRef::Surface { cursor })
                if cursor.surface.client_id() == client.client_id()
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::PointerButton { button, .. },
                    } if *button == 0x110
                )
            }
        ));

        handle_pointer_button(&mut state, &mut dispatcher, 0x111, 1, 105);
        handle_pointer_button(&mut state, &mut dispatcher, 0x111, 0, 106);

        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::PointerButton { button, .. },
                    } if *button == 0x111
                )
            }
        ));

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 110);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.pointer_focus.is_some());
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::PointerButton { button, state, .. },
                    } if *button == 0x110 && *state == 0
                )
            }
        ));
    }

    #[test]
    fn decoration_top_border_press_starts_resize_without_client_pointer_focus() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(171),
            XdgSurfaceId::from(181),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(181),
            XdgToplevelId::from(191),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(950usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.seat.pointer_pos = (80.0, 44.0);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Top as u32
        ));
        assert_eq!(
            state.seat.keyboard_focus.map(|focus| focus.surface_id()),
            Some(WlSurfaceId::from(171))
        );
        assert!(state.seat.pointer_focus.is_none());
        assert!(state.seat.intercepted_pointer_buttons.contains(&0x110));
    }

    #[test]
    fn compositor_mouse_resize_binding_uses_pointer_thirds_for_edges() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(200),
            XdgSurfaceId::from(210),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(210),
            XdgToplevelId::from(220),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(901usize), 10, 20, 80, 60);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:right" = { action = "resize-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (85.0, 50.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x111, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Right as u32
        ));
    }

    #[test]
    fn decoration_left_border_press_starts_resize_with_left_edge() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(201),
            XdgSurfaceId::from(211),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(211),
            XdgToplevelId::from(221),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(951usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.seat.pointer_pos = (35.0, 90.0);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Left as u32
        ));
        assert!(state.seat.intercepted_pointer_buttons.contains(&0x110));
    }

    #[test]
    fn csd_window_geometry_extends_window_pointer_target_bounds() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(230),
            XdgSurfaceId::from(240),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(240),
            XdgToplevelId::from(250),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(952usize), 40, 50, 120, 80);
        }
        let (child_resolved_insert, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(231), client.client_id(), 6),
        );
        let child_object_id = child_resolved_insert.object_id();
        let child_resolved = root(&state, child_object_id);
        {
            let root_surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            root_surface.add_pending_child(child_resolved);
            assert!(root_surface.restack_child_relative(child_resolved, None, true));
        }
        let child_subsurface = insert_wl_subsurface(
            &mut state,
            ClientId::from_raw(1),
            WlSubsurfaceId::from(232),
            child_object_id,
            wl_surface_object_id,
        );
        let parent_resolved = root(&state, wl_surface_object_id);
        {
            let child_surface = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child_surface.assign_subsurface(child_subsurface, parent_resolved);
            child_surface.set_subsurface_position(-6, -12);
            child_surface.test_map(ObjectId::from(954usize), 0, 0, 126, 92);
        }
        {
            let mut root_surface = state.objects.grab_object::<WlSurface>(wl_surface_object_id);
            assert!(root_surface.apply_pending_tree_state(&mut state));
        }
        {
            let xdg_surface = state
                .objects
                .get_mut(xdg_surface_object_id)
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_current_window_geometry(Some(RectI {
                x: -6,
                y: -12,
                width: 126,
                height: 92,
            }));
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let target = scene_pick_at(&state, (36.0, 44.0))
            .window_target
            .expect("target on CSD chrome");
        assert_eq!(target.root_surface.object_id(), wl_surface_object_id);
        assert_eq!(target.geometry.target_rect, RectI::new(34, 38, 126, 92).unwrap());
        assert_eq!(target.geometry.binding_rect, RectI::new(40, 50, 120, 80).unwrap());

        let hit = window_hit_at(&state, (36.0, 44.0)).expect("shadow-area hit");
        assert_eq!(hit.binding_resize_edges, 0);
    }

    #[test]
    fn positive_origin_csd_titlebar_still_builds_window_target() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(330),
            XdgSurfaceId::from(340),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(340),
            XdgToplevelId::from(350),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(962usize), 40, 50, 800, 600);
        }
        {
            let xdg_surface = state
                .objects
                .get_mut(xdg_surface_object_id)
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_current_window_geometry(Some(RectI {
                x: 0,
                y: 32,
                width: 800,
                height: 568,
            }));
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let target = scene_pick_at(&state, (100.0, 60.0))
            .window_target
            .expect("target on positive-origin CSD titlebar");
        assert_eq!(target.root_surface.object_id(), wl_surface_object_id);
        assert_eq!(target.geometry.target_rect, RectI::new(40, 50, 800, 600).unwrap());
    }

    #[test]
    fn scene_pick_keeps_focus_and_window_target_on_the_same_topmost_root_surface() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (lower_surface_object_id, lower_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(268),
            XdgSurfaceId::from(278),
        );
        let lower_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            lower_xdg_surface_object_id,
            XdgSurfaceId::from(278),
            XdgToplevelId::from(288),
        );
        let (upper_surface_object_id, upper_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(269),
            XdgSurfaceId::from(279),
        );
        let upper_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            upper_xdg_surface_object_id,
            XdgSurfaceId::from(279),
            XdgToplevelId::from(289),
        );
        for toplevel_object_id in [lower_toplevel_object_id, upper_toplevel_object_id] {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let lower_surface = state
                .objects
                .get_mut(lower_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            lower_surface.test_map(ObjectId::from(959usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, lower_surface_object_id);
        {
            let upper_surface = state
                .objects
                .get_mut(upper_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            upper_surface.test_map(ObjectId::from(960usize), 60, 60, 120, 80);
        }
        ensure_test_root_geometry(&mut state, upper_surface_object_id);

        let pick = scene_pick_at(&state, (90.0, 55.0));

        assert!(pick.surface_focus.is_none());
        assert_eq!(
            pick.window_target
                .expect("top decoration hit should still target the top window")
                .root_surface
                .object_id(),
            upper_surface_object_id
        );
    }

    #[test]
    fn unmapped_window_geometry_does_not_create_a_window_target() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(270),
            XdgSurfaceId::from(280),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(280),
            XdgToplevelId::from(290),
        );
        {
            let xdg_surface = state
                .objects
                .get_mut(xdg_surface_object_id)
                .downcast_mut::<XdgSurface>()
                .unwrap();
            xdg_surface.test_set_current_window_geometry(Some(RectI {
                x: 0,
                y: 0,
                width: 120,
                height: 80,
            }));
        }
        let root_surface =
            Resolved::new(client.client_id(), WlSurfaceId::from(270), wl_surface_object_id);
        state.mark_root_geometry_dirty(root_surface);
        let _ = state.committed_root_geometry(root_surface);

        let pick = scene_pick_at(&state, (10.0, 10.0));

        assert!(pick.surface_focus.is_none());
        assert!(pick.window_target.is_none());
    }

    #[test]
    fn window_pointer_hit_classifies_content_hits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(245),
            XdgSurfaceId::from(255),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(255),
            XdgToplevelId::from(265),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(955usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let hit = window_hit_at(&state, (80.0, 90.0)).expect("content hit");
        assert_eq!(hit.target.root_surface.object_id(), wl_surface_object_id);
        assert!(hit.zone.is_none());
    }

    #[test]
    fn window_hit_caches_binding_resize_edges_for_content_hits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(246),
            XdgSurfaceId::from(256),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(256),
            XdgToplevelId::from(266),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(957usize), 10, 20, 90, 60);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let hit = window_hit_at(&state, (85.0, 50.0)).expect("content hit");
        assert!(hit.zone.is_none());
        assert_eq!(hit.binding_resize_edges, xdg_toplevel::ResizeEdge::Right as u32);
    }

    #[test]
    fn window_pointer_hit_classifies_decoration_border_resize_hits() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(266),
            XdgSurfaceId::from(276),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(276),
            XdgToplevelId::from(286),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(956usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let top_border_hit = window_hit_at(&state, (80.0, 44.0)).expect("top border hit");
        assert_eq!(
            top_border_hit.zone,
            Some(CompositorZone::WindowResize {
                target: top_border_hit.target,
                edges: xdg_toplevel::ResizeEdge::Top as u32,
            })
        );

        let resize_hit = window_hit_at(&state, (35.0, 90.0)).expect("resize hit");
        assert_eq!(
            resize_hit.zone,
            Some(CompositorZone::WindowResize {
                target: resize_hit.target,
                edges: xdg_toplevel::ResizeEdge::Left as u32,
            })
        );
    }

    #[test]
    fn client_side_border_and_rounded_corner_resize() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        state.config.decorations.rounding = 18;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(268),
            XdgSurfaceId::from(278),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(278),
            XdgToplevelId::from(288),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(959usize), 40, 50, 120, 200);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let border_hit = window_hit_at(&state, (34.0, 90.0)).expect("client-side border hit");
        assert_eq!(
            border_hit.zone,
            Some(CompositorZone::WindowResize {
                target: border_hit.target,
                edges: xdg_toplevel::ResizeEdge::Left as u32,
            })
        );

        let corner_hit = window_hit_at(&state, (34.0, 44.0)).expect("outer-corner hit");
        assert_eq!(
            corner_hit.zone,
            Some(CompositorZone::WindowResize {
                target: corner_hit.target,
                edges: xdg_toplevel::ResizeEdge::TopLeft as u32,
            })
        );

        state.seat.pointer_pos = (34.0, 90.0);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Left as u32
        ));
    }

    #[test]
    fn rounded_decoration_corner_resizes() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        state.config.decorations.rounding = 18;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(267),
            XdgSurfaceId::from(277),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(277),
            XdgToplevelId::from(287),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(958usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let corner_hit = window_hit_at(&state, (34.0, 38.0)).expect("corner hit");
        assert_eq!(
            corner_hit.zone,
            Some(CompositorZone::WindowResize {
                target: corner_hit.target,
                edges: xdg_toplevel::ResizeEdge::TopLeft as u32,
            })
        );

        let curved = window_hit_at(&state, (41.0, 51.0)).expect("curved content corner");
        assert_eq!(
            curved.zone,
            Some(CompositorZone::WindowResize {
                target: curved.target,
                edges: xdg_toplevel::ResizeEdge::TopLeft as u32,
            })
        );
        assert!(scene_pick_at(&state, (41.0, 51.0)).surface_focus.is_none());
    }

    #[test]
    fn floating_corner_zone_uses_rounding_and_border() {
        let mut state = state_for_tests();
        state.config.decorations.rounding = 20;
        state.config.decorations.border.size = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(261),
            XdgSurfaceId::from(271),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(271),
            XdgToplevelId::from(281),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(961usize), 40, 50, 200, 160);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        let layout = state
            .root_surface_decoration_layout(root(&state, wl_surface_object_id))
            .unwrap();
        let corner = (f64::from(layout.frame_rect.x) + 1.0, f64::from(layout.frame_rect.y) + 1.0);
        let hit = window_hit_at(&state, corner).expect("corner");
        assert!(matches!(
            hit.zone,
            Some(CompositorZone::WindowResize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::TopLeft as u32
        ));

        let interior = (
            f64::from(layout.client_window_rect.x) + 80.0,
            f64::from(layout.client_window_rect.y) + 60.0,
        );
        assert!(window_hit_at(&state, interior).unwrap().zone.is_none());

        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_maximized(true);
        }
        assert!(window_hit_at(&state, corner).unwrap().zone.is_none());
    }

    #[test]
    fn hover_cursor_matches_the_press_zone() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(262),
            XdgSurfaceId::from(272),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(272),
            XdgToplevelId::from(282),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(963usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);
        let frame = state
            .root_surface_decoration_layout(root(&state, wl_surface_object_id))
            .unwrap()
            .frame_rect;

        let mut y = f64::from(frame.y) + 0.5;
        while y < f64::from(frame.y2()) {
            let mut x = f64::from(frame.x) + 0.5;
            while x < f64::from(frame.x2()) {
                state.seat.pointer_pos = (x, y);
                let hover = super::decoration_hover_cursor_icon_names(&state);
                let zone = window_hit_at(&state, (x, y)).and_then(|hit| hit.zone);
                let started = super::begin_decoration_pointer_action(&mut state, &mut dispatcher);
                match zone {
                    None => {
                        assert!(hover.is_none());
                        assert!(!started);
                    }
                    Some(CompositorZone::WindowResize { edges, .. }) => {
                        assert_eq!(hover, Some(crate::input::resize_cursor_icon_names(edges)));
                        assert!(started);
                        assert!(matches!(
                            state.seat.pointer_interactive_grab,
                            Some(super::PointerInteractiveGrab::Resize { edges: grab_edges, .. })
                                if grab_edges == edges
                        ));
                        crate::input::abort_pointer_interactive_grab(
                            &mut state,
                            &mut dispatcher,
                            0,
                        );
                    }
                    Some(CompositorZone::SplitResize { .. }) => {
                        panic!("floating window produced a split zone")
                    }
                }
                x += 24.0;
            }
            y += 24.0;
        }
    }

    #[test]
    fn tiled_outer_border_is_not_a_zone_and_a_shared_border_is() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 8;
        state.config.general.inner_gap = 0;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let left = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(263),
            XdgSurfaceId::from(273),
            XdgToplevelId::from(283),
            964,
            0,
            0,
            200,
            200,
        );
        let _right = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(264),
            XdgSurfaceId::from(274),
            XdgToplevelId::from(284),
            965,
            0,
            0,
            200,
            200,
        );
        let layout = state
            .root_surface_decoration_layout(root(&state, left))
            .unwrap();
        let mid_y = f64::from(layout.frame_rect.y) + f64::from(layout.frame_rect.height) / 2.0;
        let outer = (f64::from(layout.frame_rect.x) + 1.0, mid_y);
        let outer_hit = window_hit_at(&state, outer).expect("outer border");
        assert!(outer_hit.zone.is_none());
        state.seat.pointer_pos = outer;
        assert!(!super::begin_decoration_pointer_action(&mut state, &mut dispatcher));

        let content = (
            f64::from(layout.client_window_rect.x) + 4.0,
            f64::from(layout.client_window_rect.y) + 4.0,
        );
        let content_pick = scene_pick_at(&state, content);
        assert!(content_pick.zone.is_none());
        assert!(content_pick.surface_focus.is_some());

        let shared = (f64::from(layout.frame_rect.x2()) - 1.0, mid_y);
        assert!(matches!(
            window_hit_at(&state, shared).and_then(|hit| hit.zone),
            Some(CompositorZone::SplitResize { .. })
        ));
    }

    #[test]
    fn floating_extent_over_a_tiled_window_is_a_resize_zone() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 10;
        state.config.decorations.resize_grab_extent = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let _left = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(301),
            XdgSurfaceId::from(311),
            XdgToplevelId::from(321),
            1001,
            0,
            0,
            200,
            200,
        );
        let _right = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(302),
            XdgSurfaceId::from(312),
            XdgToplevelId::from(322),
            1002,
            0,
            0,
            200,
            200,
        );
        let floating = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(303),
            XdgSurfaceId::from(313),
            XdgToplevelId::from(323),
            1003,
            0,
            0,
            200,
            200,
        );
        let floating_root = root(&state, floating);
        let floating_window = state.window_for_root(floating_root).unwrap();
        assert!(state.set_window_floating(floating_window, true));
        state
            .objects
            .get_mut(floating)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .set_root_position(400, 200);
        ensure_test_root_geometry(&mut state, floating);
        let frame = state
            .committed_root_geometry(floating_root)
            .unwrap()
            .frame_rect_global();
        let outside =
            (f64::from(frame.x) - 5.0, f64::from(frame.y) + f64::from(frame.height) / 2.0);
        let pick = scene_pick_at(&state, outside);
        assert!(pick.surface_focus.is_none());
        assert!(matches!(
            pick.zone,
            Some(CompositorZone::WindowResize { target, edges })
                if target.root_surface == floating_root
                    && edges == xdg_toplevel::ResizeEdge::Left as u32
        ));
    }

    #[test]
    fn popup_inside_a_floating_extent_keeps_focus_and_has_no_zone() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 10;
        state.config.decorations.resize_grab_extent = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let _tiled = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(304),
            XdgSurfaceId::from(314),
            XdgToplevelId::from(324),
            1004,
            0,
            0,
            200,
            200,
        );
        let floating = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(305),
            XdgSurfaceId::from(315),
            XdgToplevelId::from(325),
            1005,
            0,
            0,
            200,
            200,
        );
        let floating_root = root(&state, floating);
        assert!(state.set_window_floating(state.window_for_root(floating_root).unwrap(), true));
        state
            .objects
            .get_mut(floating)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .set_root_position(400, 200);
        ensure_test_root_geometry(&mut state, floating);
        let frame = state
            .committed_root_geometry(floating_root)
            .unwrap()
            .frame_rect_global();
        let parent_origin = state
            .objects
            .get(floating)
            .downcast_ref::<WlSurface>()
            .unwrap()
            .root_position();
        let popup_origin = (frame.x - 8, frame.y + frame.height / 2);
        let (popup_wl, popup_xdg) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(306),
            XdgSurfaceId::from(316),
        );
        state
            .objects
            .get_mut(popup_wl)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(1006usize), 0, 0, 6, 6);
        let parent_xdg = floating_root
            .try_ref(&state)
            .unwrap()
            .xdg_surface()
            .unwrap();
        let popup_xdg_resolved =
            Resolved::new(client.client_id(), XdgSurfaceId::from(316), popup_xdg);
        let (_, popup_object_id) = state.insert_object(XdgPopup::new(
            XdgPopupId::from(326),
            popup_xdg_resolved,
            Some(parent_xdg),
            PositionerState::default(),
            7,
        ));
        let popup = Resolved::new(client.client_id(), XdgPopupId::from(326), popup_object_id);
        state
            .objects
            .get_mut(popup_object_id)
            .downcast_mut::<XdgPopup>()
            .unwrap()
            .apply_acked_configure(RectI {
                x: popup_origin.0 - parent_origin.0,
                y: popup_origin.1 - parent_origin.1,
                width: 6,
                height: 6,
            });
        state
            .objects
            .get_mut(popup_xdg)
            .downcast_mut::<XdgSurface>()
            .unwrap()
            .test_set_current_window_geometry(Some(RectI { x: 0, y: 0, width: 6, height: 6 }));
        state
            .objects
            .get_mut(popup_xdg)
            .downcast_mut::<XdgSurface>()
            .unwrap()
            .test_set_popup_role(popup);
        state
            .objects
            .get_mut(parent_xdg.object_id())
            .downcast_mut::<XdgSurface>()
            .unwrap()
            .add_popup_child(popup);

        let pick = scene_pick_at(
            &state,
            (f64::from(popup_origin.0) + 2.0, f64::from(popup_origin.1) + 2.0),
        );
        assert!(pick.zone.is_none());
        assert_eq!(
            pick.surface_focus
                .map(|focus| focus.target.surface.object_id()),
            Some(popup_wl)
        );
    }

    #[test]
    fn gap_between_tiles_resizes_until_a_floating_window_covers_it() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 8;
        state.config.general.inner_gap = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let left = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(307),
            XdgSurfaceId::from(317),
            XdgToplevelId::from(327),
            1007,
            0,
            0,
            200,
            200,
        );
        let right = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(308),
            XdgSurfaceId::from(318),
            XdgToplevelId::from(328),
            1008,
            0,
            0,
            200,
            200,
        );
        let left_frame = state
            .committed_root_geometry(root(&state, left))
            .unwrap()
            .frame_rect_global();
        let right_frame = state
            .committed_root_geometry(root(&state, right))
            .unwrap()
            .frame_rect_global();
        let gap = gap_midpoint(left_frame, right_frame);
        let pick = scene_pick_at(&state, gap);
        assert!(pick.surface_focus.is_none());
        assert!(pick.window_target.is_none());
        assert!(matches!(pick.zone, Some(CompositorZone::SplitResize { .. })));
        state.seat.pointer_pos = gap;
        assert!(super::decoration_hover_cursor_icon_names(&state).is_some());
        drop(client_dispatcher);
        assert!(super::begin_decoration_pointer_action(&mut state, &mut dispatcher));
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::ResizeSplit { origin_window: None, .. })
        ));
        crate::input::abort_pointer_interactive_grab(&mut state, &mut dispatcher, 0);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let floating = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(309),
            XdgSurfaceId::from(319),
            XdgToplevelId::from(329),
            1009,
            0,
            0,
            200,
            200,
        );
        let floating_root = root(&state, floating);
        assert!(state.set_window_floating(state.window_for_root(floating_root).unwrap(), true));
        let left_frame = state
            .committed_root_geometry(root(&state, left))
            .unwrap()
            .frame_rect_global();
        let right_frame = state
            .committed_root_geometry(root(&state, right))
            .unwrap()
            .frame_rect_global();
        let gap = gap_midpoint(left_frame, right_frame);
        state.set_root_surface_position(
            floating_root,
            (gap.0.round() as i32 - 40, gap.1.round() as i32 - 40),
        );
        ensure_test_root_geometry(&mut state, floating);
        let covered = scene_pick_at(&state, gap);
        assert!(covered.zone.is_none());
        assert_eq!(covered.surface_focus.map(|focus| focus.target.surface), Some(floating_root));
    }

    #[test]
    fn top_layer_over_a_gap_wins() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 8;
        state.config.general.inner_gap = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let left = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(331),
            XdgSurfaceId::from(341),
            XdgToplevelId::from(351),
            1011,
            0,
            0,
            200,
            200,
        );
        let right = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(332),
            XdgSurfaceId::from(342),
            XdgToplevelId::from(352),
            1012,
            0,
            0,
            200,
            200,
        );
        let left_frame = state
            .committed_root_geometry(root(&state, left))
            .unwrap()
            .frame_rect_global();
        let right_frame = state
            .committed_root_geometry(root(&state, right))
            .unwrap()
            .frame_rect_global();
        let gap = gap_midpoint(left_frame, right_frame);
        let output_id = state.outputs.iter().next().unwrap().0;
        let layer_object = insert_surface(&mut state, &mut client, WlSurfaceId::from(333));
        let layer_root = root(&state, layer_object);
        layer_root.ref_mut(&mut state).test_map(
            ObjectId::from(1013usize),
            gap.0.round() as i32 - 20,
            gap.1.round() as i32 - 20,
            40,
            40,
        );
        let mut layer = ZwlrLayerSurfaceV1::new(
            ZwlrLayerSurfaceV1Id::from(334),
            Resolved::new(client.client_id(), ZwlrLayerShellV1Id::from(6), ObjectId::from(6usize)),
            4,
            layer_root,
            Some(output_id),
            LayerShellLayer::Top,
            "bar".to_string(),
        );
        layer.test_set_mapped(true);
        let (layer_resolved, _) = client.insert_object(&mut state, layer);
        state.register_layer_surface_object(layer_resolved);
        layer_root
            .ref_mut(&mut state)
            .set_layer_surface_impl(layer_resolved);

        let pick = scene_pick_at(&state, gap);
        assert!(pick.zone.is_none());
        assert_eq!(pick.surface_focus.map(|focus| focus.target.surface), Some(layer_root));
    }

    #[test]
    fn zero_border_floating_extent_is_outside_the_content() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 0;
        state.config.decorations.resize_grab_extent = 10;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(335),
            XdgSurfaceId::from(345),
        );
        assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(345),
            XdgToplevelId::from(355),
        );
        state
            .objects
            .get_mut(wl_surface_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap()
            .test_map(ObjectId::from(1014usize), 40, 50, 200, 160);
        ensure_test_root_geometry(&mut state, wl_surface_object_id);
        let content = state
            .committed_root_geometry(root(&state, wl_surface_object_id))
            .unwrap()
            .content_rect_global();
        let inside_extent =
            (f64::from(content.x) - 5.0, f64::from(content.y) + f64::from(content.height) / 2.0);
        let pick = scene_pick_at(&state, inside_extent);
        assert!(pick.surface_focus.is_none());
        assert!(matches!(
            pick.zone,
            Some(CompositorZone::WindowResize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Left as u32
        ));
        let outside_extent =
            (f64::from(content.x) - 11.0, f64::from(content.y) + f64::from(content.height) / 2.0);
        assert!(scene_pick_at(&state, outside_extent).zone.is_none());
    }

    #[test]
    fn decoration_hover_uses_resize_cursors_before_click() {
        let mut state = state_for_tests();
        state.config.decorations.border.size = 12;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(260),
            XdgSurfaceId::from(270),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(270),
            XdgToplevelId::from(280),
        );
        {
            let toplevel = state
                .objects
                .get_mut(toplevel_object_id)
                .downcast_mut::<XdgToplevel>()
                .unwrap();
            toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
        }
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(953usize), 40, 50, 120, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 35.0, 90.0, 100);
        assert_named_cursor_matches_any(&state.seat, super::POINTER_RESIZE_W_CURSOR_ICON_NAMES);

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 80.0, 40.0, 110);
        assert_named_cursor_matches_any(&state.seat, super::POINTER_RESIZE_N_CURSOR_ICON_NAMES);

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 162.0, 90.0, 120);
        assert_named_cursor_matches_any(&state.seat, super::POINTER_RESIZE_E_CURSOR_ICON_NAMES);

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 80.0, 132.0, 130);
        assert_named_cursor_matches_any(&state.seat, super::POINTER_RESIZE_S_CURSOR_ICON_NAMES);

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 80.0, 44.0, 140);
        assert_named_cursor_matches_any(&state.seat, super::POINTER_RESIZE_N_CURSOR_ICON_NAMES);
        assert!(state.seat.pointer_focus.is_none());
    }

    #[test]
    fn compositor_mouse_move_binding_uses_window_bounds_without_pointer_focus() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(230),
            XdgSurfaceId::from(240),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(240),
            XdgToplevelId::from(250),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(902usize), 15, 25, 100, 80);
            surface.test_set_input_region(crate::region::RegionValue::empty());
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (60.0, 55.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(state.seat.pointer_focus.is_none());
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
    }

    #[test]
    fn compositor_mouse_move_binding_accept_maximized_windows() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(260),
            XdgSurfaceId::from(270),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(270),
            XdgToplevelId::from(280),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(903usize), 20, 30, 120, 90);
        }
        set_toplevel_window_state(&mut state, toplevel_object_id, true, false);
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (80.0, 70.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
    }

    #[test]
    fn compositor_mouse_move_binding_accepts_tiled_maximized_hint() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(262),
            XdgSurfaceId::from(272),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(272),
            XdgToplevelId::from(282),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(905usize), 15, 25, 100, 80);
            surface.test_set_input_region(crate::region::RegionValue::empty());
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);
        {
            let mut toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            toplevel.apply_acked_configure(
                100,
                80,
                vec![
                    crate::protocol_generated::xdg_toplevel::State::Maximized as u32,
                    crate::protocol_generated::xdg_toplevel::State::TiledLeft as u32,
                    crate::protocol_generated::xdg_toplevel::State::TiledRight as u32,
                    crate::protocol_generated::xdg_toplevel::State::TiledTop as u32,
                    crate::protocol_generated::xdg_toplevel::State::TiledBottom as u32,
                ],
                ToplevelDecorationMode::ServerSide,
            );
            assert!(!toplevel.is_maximized());
        }

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (60.0, 55.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
    }

    #[test]
    fn compositor_mouse_move_binding_uses_restored_floating_origin_after_tiled_promotion() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            1280,
            720,
        ));
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(261),
            XdgSurfaceId::from(271),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(271),
            XdgToplevelId::from(281),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(904usize), 20, 30, 120, 90);
        }
        if state.root_surface_workspace(root(&state, wl_surface_object_id)) != Some(workspace_id) {
            assert!(state.assign_root_surface_to_workspace(
                root(&state, wl_surface_object_id),
                workspace_id
            ));
        }

        assert!(state.set_root_surface_floating(root(&state, wl_surface_object_id), true));
        state.set_root_surface_position(root(&state, wl_surface_object_id), (250, 180));
        state.mark_root_geometry_dirty(root(&state, wl_surface_object_id));
        let _ = state.committed_root_geometry(root(&state, wl_surface_object_id));
        assert!(state.set_root_surface_floating(root(&state, wl_surface_object_id), false));

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.config.decorations.border.size = 0;
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (70.0, 80.0);

        let tile = state
            .committed_root_geometry(root(&state, wl_surface_object_id))
            .unwrap()
            .layout_rect_global;
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        let preview_width = ((tile.width as f64) * 0.8489).floor() as i32;
        let preview_height = ((tile.height as f64) * 0.8489).floor() as i32;
        let surface = state
            .objects
            .try_ref_object::<WlSurface>(wl_surface_object_id)
            .unwrap();
        assert_eq!(
            surface.root_position(),
            preview_origin((70.0, 80.0), (preview_width, preview_height))
        );
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::MoveTiled(session)) if session.picked_up
        ));
        assert!(state.is_floating_root_surface(root(&state, wl_surface_object_id)));
        assert!(!state.is_tiled_root_surface(root(&state, wl_surface_object_id)));
        let remembered = state
            .managed_root_surface_state(root(&state, wl_surface_object_id))
            .unwrap()
            .remembered_floating_state;
        assert_eq!(
            remembered.map(|state| (state.root_pos, state.configure_size)),
            Some(((250, 180), (120, 90)))
        );
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::InteractiveResizeConfigure { width, height, resizing, .. }
                        if *width == preview_width && *height == preview_height && !*resizing
                )
            }
        ));
    }

    fn enable_move_window_binding(state: &mut State) {
        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        // Reloading the config restores the runtime 1px border. These drag tests
        // are about the move binding, so keep the zero-frame baseline.
        state.config.decorations.border.size = 0;
        state.keyboard.update_key(125, true);
    }

    fn map_toplevel_on_active_output(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
        wl_surface_id: WlSurfaceId,
        xdg_surface_id: XdgSurfaceId,
        toplevel_id: XdgToplevelId,
        buffer_id: usize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> ObjectId {
        let existing_output = state.outputs.iter().next().map(|(output_id, _)| output_id);
        let output_id = existing_output.unwrap_or_else(|| {
            state.insert_output(test_output_with_crtc_name(
                BackendOutputRef::new(DrmId::from(1usize)),
                1,
                0,
                0,
                1280,
                720,
            ))
        });
        let workspace_id = state.active_workspace_for_output(output_id).unwrap();
        let (wl_surface_object_id, xdg_surface_object_id) =
            insert_xdg_surface(state, client, dispatcher, wl_surface_id, xdg_surface_id);
        assign_toplevel_role(
            state,
            client,
            dispatcher,
            xdg_surface_object_id,
            xdg_surface_id,
            toplevel_id,
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(buffer_id), x, y, width, height);
        }
        if state.root_surface_workspace(root(state, wl_surface_object_id)) != Some(workspace_id) {
            assert!(state
                .assign_root_surface_to_workspace(root(state, wl_surface_object_id), workspace_id));
        }
        state.relayout_dirty_workspaces();
        ensure_test_root_geometry(state, wl_surface_object_id);
        wl_surface_object_id
    }

    fn output_bounds_for_surface(state: &State, surface: Resolved<WlSurfaceId>) -> RectI {
        let workspace_id = state.root_surface_workspace(surface).unwrap();
        let output_id = state.workspace_output_id(workspace_id).unwrap();
        state.output_bounds(output_id).unwrap()
    }

    fn preview_origin(pointer: (f64, f64), size: (i32, i32)) -> (i32, i32) {
        (
            (pointer.0 - size.0 as f64 / 2.0).round() as i32,
            (pointer.1 - size.1 as f64 / 2.0).round() as i32,
        )
    }

    fn gap_midpoint(first: RectI, second: RectI) -> (f64, f64) {
        if first.x2() <= second.x {
            (
                f64::from(first.x2() + second.x) / 2.0,
                f64::from(first.y) + f64::from(first.height) / 2.0,
            )
        } else if second.x2() <= first.x {
            (
                f64::from(second.x2() + first.x) / 2.0,
                f64::from(second.y) + f64::from(second.height) / 2.0,
            )
        } else if first.y2() <= second.y {
            (
                f64::from(first.x) + f64::from(first.width) / 2.0,
                f64::from(first.y2() + second.y) / 2.0,
            )
        } else {
            (
                f64::from(second.x) + f64::from(second.width) / 2.0,
                f64::from(second.y2() + first.y) / 2.0,
            )
        }
    }

    fn layout_rect(state: &State, object_id: ObjectId) -> RectI {
        state
            .committed_root_geometry(root(state, object_id))
            .unwrap()
            .layout_rect_global
    }

    fn move_pointer_to(state: &mut State, dispatcher: &mut Dispatcher, x: f64, y: f64, time: u32) {
        let current = state.seat.pointer_pos;
        handle_pointer_motion(
            state,
            dispatcher,
            x - current.0,
            y - current.1,
            x - current.0,
            y - current.1,
            time,
            u64::from(time) * 1000,
        );
    }

    fn tiled_split(
        state: &State,
        surface: Resolved<WlSurfaceId>,
    ) -> (crate::tiling::SplitAxis, Vec<(Resolved<WlSurfaceId>, f64)>) {
        let window = state.window_for_root(surface).unwrap();
        let workspace_id = state.root_surface_workspace(surface).unwrap();
        let layout = &state.workspaces.try_get(workspace_id).unwrap().layout;
        let leaf_id = layout.window_leaves[&window];
        let crate::tiling::LayoutNode::Leaf(leaf) = &layout.nodes[&leaf_id] else {
            panic!("tiled drag surface is not a leaf");
        };
        let parent_id = leaf.parent.expect("dropped window has a split parent");
        let crate::tiling::LayoutNode::Container(container) = &layout.nodes[&parent_id] else {
            panic!("split parent is a container");
        };
        let children = container
            .children
            .iter()
            .map(|child| {
                let crate::tiling::LayoutNode::Leaf(leaf) = &layout.nodes[&child.node_id] else {
                    panic!("drag split child is a leaf");
                };
                (state.window_root_surface(leaf.window).unwrap(), child.weight)
            })
            .collect();
        (container.axis, children)
    }

    fn map_drag_pair(
        state: &mut State,
        client: &mut Client,
        dispatcher: &mut crate::clients::ClientDispatcher<'_>,
    ) -> (ObjectId, ObjectId) {
        let stayed = map_toplevel_on_active_output(
            state,
            client,
            dispatcher,
            WlSurfaceId::from(301),
            XdgSurfaceId::from(311),
            XdgToplevelId::from(321),
            931,
            0,
            0,
            100,
            80,
        );
        let dragged = map_toplevel_on_active_output(
            state,
            client,
            dispatcher,
            WlSurfaceId::from(302),
            XdgSurfaceId::from(312),
            XdgToplevelId::from(322),
            932,
            0,
            0,
            100,
            80,
        );
        (stayed, dragged)
    }

    fn set_server_side_decoration(state: &mut State, object_id: ObjectId) {
        let toplevel_object_id = state
            .window_for_root(root(state, object_id))
            .and_then(|window| state.window_role_object(window))
            .unwrap();
        let mut toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
        toplevel.test_set_decoration_mode(ToplevelDecorationMode::ServerSide);
    }

    fn press_tiled_window(state: &mut State, dispatcher: &mut Dispatcher, object_id: ObjectId) {
        let tile = layout_rect(state, object_id);
        state.seat.pointer_pos = (tile.x as f64 + 10.0, tile.y as f64 + 10.0);
        handle_pointer_button(state, dispatcher, 0x110, 1, 100);
    }

    #[test]
    fn tiled_drag_waits_for_the_pointer_threshold_then_floats_the_preview() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let wl_surface_object_id = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(263),
            XdgSurfaceId::from(273),
            XdgToplevelId::from(283),
            906,
            0,
            0,
            200,
            100,
        );
        enable_move_window_binding(&mut state);
        state.config.tiling.drag.threshold = 30;
        let tile = state
            .committed_root_geometry(root(&state, wl_surface_object_id))
            .unwrap()
            .layout_rect_global;
        state.seat.pointer_pos = (tile.x as f64 + 30.0, tile.y as f64 + 30.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::MoveTiled(session)) if !session.picked_up
        ));
        assert!(state.is_tiled_root_surface(root(&state, wl_surface_object_id)));

        handle_pointer_motion(&mut state, &mut dispatcher, 10.0, 0.0, 10.0, 0.0, 101, 101_000);
        assert!(state.is_tiled_root_surface(root(&state, wl_surface_object_id)));

        handle_pointer_motion(&mut state, &mut dispatcher, 25.0, 0.0, 25.0, 0.0, 102, 102_000);
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::MoveTiled(session)) if session.picked_up
        ));
        assert!(state.is_floating_root_surface(root(&state, wl_surface_object_id)));
        let pointer = state.seat.pointer_pos;
        let preview_width = ((tile.width as f64) * 0.8489).floor() as i32;
        let preview_height = ((tile.height as f64) * 0.8489).floor() as i32;
        let bounds = output_bounds_for_surface(&state, root(&state, wl_surface_object_id));
        let centered = {
            let surface = state
                .objects
                .try_ref_object::<WlSurface>(wl_surface_object_id)
                .unwrap();
            surface.root_position()
        };
        assert_eq!(centered, preview_origin(pointer, (preview_width, preview_height)));
        let center = (
            bounds.x as f64 + bounds.width as f64 / 2.0,
            bounds.y as f64 + bounds.height as f64 / 2.0,
        );
        let current = state.seat.pointer_pos;
        handle_pointer_motion(
            &mut state,
            &mut dispatcher,
            center.0 - current.0,
            center.1 - current.1,
            center.0 - current.0,
            center.1 - current.1,
            103,
            103_000,
        );
        handle_pointer_motion(&mut state, &mut dispatcher, 8.0, -3.0, 8.0, -3.0, 104, 104_000);
        let pointer = state.seat.pointer_pos;
        let surface = state
            .objects
            .try_ref_object::<WlSurface>(wl_surface_object_id)
            .unwrap();
        assert_eq!(
            surface.root_position(),
            preview_origin(pointer, (preview_width, preview_height))
        );
    }

    #[test]
    fn tiled_drag_release_before_the_threshold_leaves_the_window_tiled() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let wl_surface_object_id = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(264),
            XdgSurfaceId::from(274),
            XdgToplevelId::from(284),
            907,
            0,
            0,
            180,
            120,
        );
        enable_move_window_binding(&mut state);
        state.config.tiling.drag.threshold = 40;
        let tile = state
            .committed_root_geometry(root(&state, wl_surface_object_id))
            .unwrap()
            .layout_rect_global;
        state.seat.pointer_pos = (tile.x as f64 + 20.0, tile.y as f64 + 20.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        handle_pointer_motion(&mut state, &mut dispatcher, 12.0, 9.0, 12.0, 9.0, 101, 101_000);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 110);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.is_tiled_root_surface(root(&state, wl_surface_object_id)));
    }

    #[test]
    fn tiled_drag_preview_follows_the_pointer_onto_another_output() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(303),
            XdgSurfaceId::from(313),
            XdgToplevelId::from(323),
            933,
            0,
            0,
            200,
            100,
        );
        enable_move_window_binding(&mut state);
        let bounds = output_bounds_for_surface(&state, root(&state, window));
        let tile = layout_rect(&state, window);
        let preview_size = (
            ((tile.width as f64) * 0.8489).floor() as i32,
            ((tile.height as f64) * 0.8489).floor() as i32,
        );
        state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(2usize)),
            2,
            bounds.x + bounds.width,
            bounds.y,
            bounds.width,
            bounds.height,
        ));
        press_tiled_window(&mut state, &mut dispatcher, window);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            (bounds.x + bounds.width + bounds.width / 2) as f64,
            (bounds.y + bounds.height / 2) as f64,
            120,
        );

        assert!(state.seat.pointer_pos.0 >= (bounds.x + bounds.width) as f64);
        let pointer = state.seat.pointer_pos;
        let surface = state.objects.try_ref_object::<WlSurface>(window).unwrap();
        let origin = preview_origin(pointer, preview_size);
        assert_eq!(surface.root_position(), origin);
        assert!(origin.0 + preview_size.0 > bounds.x + bounds.width);
    }

    #[test]
    fn tiled_drag_release_splits_a_wide_tile_by_aspect_and_keeps_remembered_geometry() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        state.config.tiling.drag.split_ratio = crate::config::SplitRatio::sanitize(1.4);
        let stayed_tile = layout_rect(&state, stayed);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            stayed_tile.x as f64 + stayed_tile.width as f64 - 20.0,
            stayed_tile.y as f64 + stayed_tile.height as f64 / 2.0,
            130,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 140);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.tiled_drag_origin.is_none());
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
        assert!(state.is_tiled_root_surface(root(&state, stayed)));
        let remembered = state
            .managed_root_surface_state(root(&state, dragged))
            .unwrap()
            .remembered_floating_state;
        assert!(remembered.is_none());
        let (axis, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(axis, crate::tiling::SplitAxis::Horizontal);
        assert_eq!(children[0].0, root(&state, stayed));
        assert_eq!(children[1].0, root(&state, dragged));
        assert!((children[0].1 - 1.4).abs() < 1e-5);
        assert!((children[1].1 - 0.6).abs() < 1e-5);
    }

    #[test]
    fn tiled_drag_precise_drop_uses_the_pointer_quadrant() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        state.config.tiling.drag.precise_mouse_move = true;
        let stayed_tile = layout_rect(&state, stayed);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            stayed_tile.x as f64 + stayed_tile.width as f64 / 2.0,
            stayed_tile.y as f64 + 5.0,
            150,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 160);

        let (axis, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(children[0].0, root(&state, dragged));
        assert_eq!(children[1].0, root(&state, stayed));
    }

    #[test]
    fn tiled_drag_release_over_empty_space_splits_the_nearest_tile() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        state.config.general.outer_gap = 80;
        let workspace_id = state.root_surface_workspace(root(&state, stayed)).unwrap();
        crate::tiling::mark_workspace_dirty(&mut state, workspace_id);
        state.relayout_dirty_workspaces();
        ensure_test_root_geometry(&mut state, stayed);
        ensure_test_root_geometry(&mut state, dragged);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        move_pointer_to(&mut state, &mut dispatcher, 1.0, 1.0, 170);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 180);

        assert!(state.is_tiled_root_surface(root(&state, dragged)));
        let (axis, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(axis, crate::tiling::SplitAxis::Horizontal);
        assert_eq!(children[0].0, root(&state, dragged));
        assert_eq!(children[1].0, root(&state, stayed));
    }

    #[test]
    fn tiled_drag_cancel_restores_the_previous_split() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        let before = layout_rect(&state, stayed).width;
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        assert!(layout_rect(&state, stayed).width > before);
        super::abort_pointer_interactive_grab(&mut state, &mut dispatcher, 190);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.tiled_drag_origin.is_none());
        assert_eq!(layout_rect(&state, stayed).width, before);
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
    }

    #[test]
    fn tiled_drag_session_lock_restores_the_previous_split() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        let before = layout_rect(&state, stayed).width;
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        super::gate_for_session_lock(&mut state, &mut dispatcher);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert_eq!(layout_rect(&state, stayed).width, before);
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
    }

    #[test]
    fn tiled_drag_session_pause_restores_the_previous_split() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        let before = layout_rect(&state, stayed).width;
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        super::quiesce_for_session_pause(&mut state, &mut dispatcher);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert_eq!(layout_rect(&state, stayed).width, before);
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
    }

    fn arm_settled_window_geometry(state: &mut State) {
        state.config.animations.set_open_duration_msec(0);
        state.advance_visual_animations(std::time::Instant::now());
    }

    fn enable_window_move_animation(state: &mut State) {
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
    }

    fn window_visual(state: &State, object_id: ObjectId) -> RectI {
        state
            .managed_window(root(state, object_id))
            .unwrap()
            .geometry_animation
            .visual_rect()
            .unwrap()
    }

    #[test]
    fn tiled_drag_pickup_animates_until_the_pointer_moves() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(401),
            XdgSurfaceId::from(411),
            XdgToplevelId::from(421),
            941,
            0,
            0,
            200,
            100,
        );
        arm_settled_window_geometry(&mut state);
        enable_window_move_animation(&mut state);
        enable_move_window_binding(&mut state);
        let tile = layout_rect(&state, window);
        press_tiled_window(&mut state, &mut dispatcher, window);
        let preview = layout_rect(&state, window);
        assert!(preview.width < tile.width);

        let now = Instant::now();
        state.advance_visual_animations(now);
        state.advance_visual_animations(now + Duration::from_millis(100));
        let midway = window_visual(&state, window);
        assert!(midway.width < tile.width && midway.width > preview.width);

        let pointer = state.seat.pointer_pos;
        move_pointer_to(&mut state, &mut dispatcher, pointer.0 + 80.0, pointer.1, 150);
        state.advance_visual_animations(Instant::now());
        assert_eq!(window_visual(&state, window), layout_rect(&state, window));
        assert_ne!(window_visual(&state, window), midway);
    }

    #[test]
    fn tiled_drag_drop_animates_from_the_preview_into_the_tile() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(402),
            XdgSurfaceId::from(412),
            XdgToplevelId::from(422),
            942,
            0,
            0,
            200,
            100,
        );
        arm_settled_window_geometry(&mut state);
        enable_window_move_animation(&mut state);
        enable_move_window_binding(&mut state);
        let tile = layout_rect(&state, window);
        press_tiled_window(&mut state, &mut dispatcher, window);
        let now = Instant::now();
        state.advance_visual_animations(now);
        state.advance_visual_animations(now + Duration::from_millis(100));
        let midway = window_visual(&state, window);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 180);

        assert!(state.is_tiled_root_surface(root(&state, window)));
        assert_eq!(layout_rect(&state, window), tile);
        let dropped = Instant::now();
        state.advance_visual_animations(dropped);
        state.advance_visual_animations(dropped + Duration::from_millis(100));
        let visual = window_visual(&state, window);
        assert!(visual.width > midway.width && visual.width < tile.width);
    }

    #[test]
    fn tiled_drag_cancel_snaps_the_window_and_lets_the_neighbor_animate() {
        use std::time::{Duration, Instant};

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        arm_settled_window_geometry(&mut state);
        enable_window_move_animation(&mut state);
        enable_move_window_binding(&mut state);
        let before = layout_rect(&state, stayed);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        let now = Instant::now();
        state.advance_visual_animations(now);
        state.advance_visual_animations(now + Duration::from_millis(100));
        super::abort_pointer_interactive_grab(&mut state, &mut dispatcher, 190);
        state.advance_visual_animations(Instant::now());

        assert_eq!(layout_rect(&state, stayed).width, before.width);
        assert_eq!(window_visual(&state, dragged), layout_rect(&state, dragged));
        assert_ne!(window_visual(&state, stayed).width, layout_rect(&state, stayed).width);
    }

    #[test]
    fn tiled_drag_motion_warps_the_preview_onto_the_neighbor_output() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(403),
            XdgSurfaceId::from(413),
            XdgToplevelId::from(423),
            943,
            0,
            0,
            200,
            100,
        );
        arm_settled_window_geometry(&mut state);
        enable_window_move_animation(&mut state);
        enable_move_window_binding(&mut state);
        let workspace = state.root_surface_workspace(root(&state, window)).unwrap();
        let bounds = output_bounds_for_surface(&state, root(&state, window));
        state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(2usize)),
            2,
            bounds.x + bounds.width,
            bounds.y,
            bounds.width,
            bounds.height,
        ));
        press_tiled_window(&mut state, &mut dispatcher, window);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            (bounds.x + bounds.width + bounds.width / 2) as f64,
            (bounds.y + bounds.height / 2) as f64,
            160,
        );
        state.advance_visual_animations(std::time::Instant::now());

        let visual = window_visual(&state, window);
        assert_eq!(visual, layout_rect(&state, window));
        assert!(visual.x + visual.width > bounds.x + bounds.width);
        assert_eq!(state.root_surface_workspace(root(&state, window)), Some(workspace));
    }

    #[test]
    fn tiled_drag_pickup_during_a_drop_warps_to_the_new_preview() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(404),
            XdgSurfaceId::from(414),
            XdgToplevelId::from(424),
            944,
            0,
            0,
            200,
            100,
        );
        arm_settled_window_geometry(&mut state);
        enable_window_move_animation(&mut state);
        enable_move_window_binding(&mut state);
        let tile = layout_rect(&state, window);
        press_tiled_window(&mut state, &mut dispatcher, window);
        let bounds = output_bounds_for_surface(&state, root(&state, window));
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            bounds.x as f64 + bounds.width as f64 / 2.0,
            bounds.y as f64 + bounds.height as f64 / 2.0,
            150,
        );
        let now = std::time::Instant::now();
        state.advance_visual_animations(now);
        state.advance_visual_animations(now + std::time::Duration::from_millis(100));
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 180);
        let geometry = state.committed_root_geometry(root(&state, window)).unwrap();
        let surface = geometry.surface_bounds_global().unwrap();
        let visual_now = window_visual(&state, window);
        let pointer =
            crate::render::RectMapping { logical: geometry.layout_rect_global, visual: visual_now }
                .map_point((
                    surface.x as f64 + surface.width as f64 / 2.0,
                    surface.y as f64 + surface.height as f64 / 2.0,
                ));
        state.seat.pointer_pos = pointer;
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 200);
        assert!(state.seat.pointer_interactive_grab.is_some(), "second press missed the window");
        move_pointer_to(&mut state, &mut dispatcher, pointer.0 + 90.0, pointer.1, 210);
        state.advance_visual_animations(now + std::time::Duration::from_millis(120));

        let visual = window_visual(&state, window);
        assert_eq!(visual, layout_rect(&state, window));
        assert!(visual.width < tile.width);
        assert!(state.is_floating_root_surface(root(&state, window)));
    }

    #[test]
    fn tiled_drag_client_loss_does_not_restore_the_preview_into_the_tree() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        let expanded = layout_rect(&state, stayed).width;
        state.seat.remove_client(client.client_id());

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.tiled_drag_origin.is_none());
        assert!(!state.is_tiled_root_surface(root(&state, dragged)));
        assert_eq!(layout_rect(&state, stayed).width, expanded);
    }

    #[test]
    fn tiled_drag_release_of_the_only_window_tiles_it_again() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let window = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(304),
            XdgSurfaceId::from(314),
            XdgToplevelId::from(324),
            934,
            0,
            0,
            160,
            90,
        );
        enable_move_window_binding(&mut state);
        press_tiled_window(&mut state, &mut dispatcher, window);
        assert!(state.is_floating_root_surface(root(&state, window)));
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 200);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.is_tiled_root_surface(root(&state, window)));
        assert!(state
            .managed_root_surface_state(root(&state, window))
            .unwrap()
            .remembered_floating_state
            .is_none());
    }

    #[test]
    fn tiled_drag_press_drops_back_into_the_source_split() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        state.config.decorations.border.size = 12;
        set_server_side_decoration(&mut state, dragged);
        let workspace_id = state.root_surface_workspace(root(&state, dragged)).unwrap();
        crate::tiling::mark_workspace_dirty(&mut state, workspace_id);
        state.relayout_dirty_workspaces();
        ensure_test_root_geometry(&mut state, stayed);
        ensure_test_root_geometry(&mut state, dragged);
        let frame = layout_rect(&state, dragged);
        state.seat.pointer_pos =
            (frame.x as f64 + frame.width as f64 / 2.0, frame.y as f64 + frame.height as f64 / 2.0);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 210);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::MoveTiled(session)) if session.picked_up
        ));
        let stayed_tile = layout_rect(&state, stayed);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            stayed_tile.x as f64 + stayed_tile.width as f64 - 20.0,
            stayed_tile.y as f64 + stayed_tile.height as f64 / 2.0,
            220,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 230);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
        let (axis, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(axis, crate::tiling::SplitAxis::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(children[1].0, root(&state, dragged));
    }

    #[test]
    fn tiled_drag_release_wraps_a_nested_destination() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let first = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(305),
            XdgSurfaceId::from(315),
            XdgToplevelId::from(325),
            935,
            0,
            0,
            100,
            80,
        );
        let second = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(306),
            XdgSurfaceId::from(316),
            XdgToplevelId::from(326),
            936,
            0,
            0,
            100,
            80,
        );
        let nested = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(307),
            XdgSurfaceId::from(317),
            XdgToplevelId::from(327),
            937,
            0,
            0,
            100,
            80,
        );
        enable_move_window_binding(&mut state);
        let (before_axis, before_children) = tiled_split(&state, root(&state, nested));
        assert_eq!(before_axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(before_children.len(), 2);
        press_tiled_window(&mut state, &mut dispatcher, nested);
        let destination = layout_rect(&state, first);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            destination.x as f64 + destination.width as f64 / 2.0,
            destination.y as f64 + destination.height as f64 - 10.0,
            240,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 250);

        assert!(state.is_tiled_root_surface(root(&state, first)));
        assert!(state.is_tiled_root_surface(root(&state, second)));
        let (axis, children) = tiled_split(&state, root(&state, nested));
        assert_eq!(axis, crate::tiling::SplitAxis::Vertical);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].0, root(&state, first));
        assert_eq!(children[1].0, root(&state, nested));
    }

    #[test]
    fn tiled_drag_gaps_still_split_the_tile_under_the_pointer() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        state.config.general.outer_gap = 20;
        state.config.general.inner_gap = 12;
        let workspace_id = state.root_surface_workspace(root(&state, stayed)).unwrap();
        crate::tiling::mark_workspace_dirty(&mut state, workspace_id);
        state.relayout_dirty_workspaces();
        ensure_test_root_geometry(&mut state, stayed);
        ensure_test_root_geometry(&mut state, dragged);
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        let stayed_tile = layout_rect(&state, stayed);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            stayed_tile.x as f64 + stayed_tile.width as f64 - 20.0,
            stayed_tile.y as f64 + stayed_tile.height as f64 / 2.0,
            260,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 270);

        assert!(state.is_tiled_root_surface(root(&state, dragged)));
        let (axis, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(axis, crate::tiling::SplitAxis::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(children[1].0, root(&state, dragged));
    }

    #[test]
    fn tiled_drag_release_on_another_output_stays_on_the_source_workspace() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        enable_move_window_binding(&mut state);
        let workspace_id = state.root_surface_workspace(root(&state, dragged)).unwrap();
        let bounds = output_bounds_for_surface(&state, root(&state, dragged));
        state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(2usize)),
            2,
            bounds.x + bounds.width,
            bounds.y,
            bounds.width,
            bounds.height,
        ));
        press_tiled_window(&mut state, &mut dispatcher, dragged);
        move_pointer_to(
            &mut state,
            &mut dispatcher,
            (bounds.x + bounds.width + bounds.width / 2) as f64,
            (bounds.y + bounds.height / 2) as f64,
            280,
        );
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 290);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert_eq!(state.root_surface_workspace(root(&state, dragged)), Some(workspace_id));
        assert!(state.is_tiled_root_surface(root(&state, dragged)));
        assert!(state.is_tiled_root_surface(root(&state, stayed)));
        let (_, children) = tiled_split(&state, root(&state, dragged));
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn tiled_drag_preview_clamps_to_max_size() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let (_stayed, dragged) = map_drag_pair(&mut state, &mut client, &mut client_dispatcher);
        let dragged_toplevel = state
            .window_for_root(root(&state, dragged))
            .and_then(|window| state.window_role_object(window))
            .unwrap();
        {
            let mut toplevel = state.objects.grab_object::<XdgToplevel>(dragged_toplevel);
            toplevel.test_set_interactive_size_constraints(None, Some((80, 60)));
        }
        enable_move_window_binding(&mut state);
        press_tiled_window(&mut state, &mut dispatcher, dragged);

        assert!(state.is_floating_root_surface(root(&state, dragged)));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::InteractiveResizeConfigure { width, height, .. }
                        if *width == 80 && *height == 60
                )
            }
        ));
    }

    #[test]
    fn tiled_drag_pickup_lets_the_other_tile_fill_the_old_slot_and_clamps_to_min_size() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let stayed = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(265),
            XdgSurfaceId::from(275),
            XdgToplevelId::from(285),
            908,
            0,
            0,
            100,
            80,
        );
        let dragged = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(266),
            XdgSurfaceId::from(276),
            XdgToplevelId::from(286),
            909,
            0,
            0,
            100,
            80,
        );
        let dragged_toplevel = state
            .window_for_root(root(&state, dragged))
            .and_then(|window| state.window_role_object(window))
            .unwrap();
        {
            let mut toplevel = state.objects.grab_object::<XdgToplevel>(dragged_toplevel);
            toplevel.test_set_interactive_size_constraints(Some((900, 1)), None);
        }
        enable_move_window_binding(&mut state);
        let stayed_before = state
            .committed_root_geometry(root(&state, stayed))
            .unwrap()
            .layout_rect_global
            .width;
        let dragged_tile = state
            .committed_root_geometry(root(&state, dragged))
            .unwrap()
            .layout_rect_global;
        state.seat.pointer_pos = (dragged_tile.x as f64 + 10.0, dragged_tile.y as f64 + 10.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);

        assert!(state.is_floating_root_surface(root(&state, dragged)));
        assert!(state.is_tiled_root_surface(root(&state, stayed)));
        let stayed_after = state
            .committed_root_geometry(root(&state, stayed))
            .unwrap()
            .layout_rect_global
            .width;
        assert!(stayed_after > stayed_before);
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::InteractiveResizeConfigure { width, .. } if *width == 900
                )
            }
        ));
    }

    #[test]
    fn floating_move_keeps_its_size_and_follows_the_pointer() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let wl_surface_object_id = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(267),
            XdgSurfaceId::from(277),
            XdgToplevelId::from(287),
            910,
            0,
            0,
            100,
            80,
        );
        assert!(state.set_root_surface_floating(root(&state, wl_surface_object_id), true));
        state.set_root_surface_position(root(&state, wl_surface_object_id), (40, 50));
        state.mark_root_geometry_dirty(root(&state, wl_surface_object_id));
        let _ = state.committed_root_geometry(root(&state, wl_surface_object_id));
        enable_move_window_binding(&mut state);
        state.seat.pointer_pos = (50.0, 60.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
        handle_pointer_motion(&mut state, &mut dispatcher, 12.0, -4.0, 12.0, -4.0, 110, 110_000);
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 120);

        assert!(state.seat.pointer_interactive_grab.is_none());
        let surface = state
            .objects
            .try_ref_object::<WlSurface>(wl_surface_object_id)
            .unwrap();
        assert_eq!(surface.root_position(), (52, 46));
        assert!(state.is_floating_root_surface(root(&state, wl_surface_object_id)));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| matches!(action, ClientAction::InteractiveResizeConfigure { .. })
        ));
    }

    #[test]
    fn protocol_move_of_a_tiled_window_does_not_float_it() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(8)));
        let wl_surface_object_id = map_toplevel_on_active_output(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(268),
            XdgSurfaceId::from(278),
            XdgToplevelId::from(288),
            911,
            0,
            0,
            90,
            70,
        );
        let toplevel_object_id = client.object_id(XdgToplevelId::from(288));
        let focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(268), wl_surface_object_id);
        let serial = state.seat.next_serial();
        state.seat.record_pointer_button(1, serial, Some(focus));
        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_move_request(XdgToplevelId::from(288), WlSeatId::from(8), serial),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
        assert!(state.is_tiled_root_surface(root(&state, wl_surface_object_id)));
        handle_pointer_motion(&mut state, &mut dispatcher, 15.0, 8.0, 15.0, 8.0, 130, 130_000);
        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 0, 140);
        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.is_tiled_root_surface(root(&state, wl_surface_object_id)));
        assert!(!state.is_floating_root_surface(root(&state, wl_surface_object_id)));
    }

    #[test]
    fn compositor_mouse_resize_binding_rejects_fullscreen_windows() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(290),
            XdgSurfaceId::from(300),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(300),
            XdgToplevelId::from(310),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(904usize), 0, 0, 200, 150);
        }
        let workspace_id = state.create_workspace(None, None);
        let group_id = state.workspace_groups.insert(WorkspaceGroup {
            outputs: Default::default(),
            workspaces: vec![workspace_id],
            active_workspace: Some(workspace_id),
            transition: None,
        });
        state.workspaces.try_get_mut(workspace_id).unwrap().group = Some(group_id);
        assert!(state
            .assign_root_surface_to_workspace(root(&state, wl_surface_object_id), workspace_id));
        set_toplevel_window_state(&mut state, toplevel_object_id, false, true);
        assert!(state.set_root_surface_fullscreen(root(&state, wl_surface_object_id), true));

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:right" = { action = "resize-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (150.0, 100.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x111, 1, 100);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::PointerButton { button, state, .. },
                    } if *button == 0x111 && *state == 1
                )
            }
        ));
    }

    #[test]
    fn compositor_mouse_resize_binding_uses_compositor_fullscreen_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (first_surface_object_id, first_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(320),
            XdgSurfaceId::from(330),
        );
        let first_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            first_xdg_surface_object_id,
            XdgSurfaceId::from(330),
            XdgToplevelId::from(340),
        );
        let (second_surface_object_id, second_xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(350),
            XdgSurfaceId::from(360),
        );
        let second_toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            second_xdg_surface_object_id,
            XdgSurfaceId::from(360),
            XdgToplevelId::from(370),
        );
        {
            let first_surface = state
                .objects
                .get_mut(first_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            first_surface.test_map(ObjectId::from(905usize), 0, 0, 200, 150);
        }
        {
            let second_surface = state
                .objects
                .get_mut(second_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            second_surface.test_map(ObjectId::from(906usize), 240, 0, 200, 150);
        }
        let workspace_id = state.create_workspace(None, None);
        let group_id = state.workspace_groups.insert(WorkspaceGroup {
            outputs: Default::default(),
            workspaces: vec![workspace_id],
            active_workspace: Some(workspace_id),
            transition: None,
        });
        state.workspaces.try_get_mut(workspace_id).unwrap().group = Some(group_id);
        assert!(state
            .assign_root_surface_to_workspace(root(&state, first_surface_object_id), workspace_id));
        assert!(state.assign_root_surface_to_workspace(
            root(&state, second_surface_object_id),
            workspace_id
        ));
        assert!(state.set_root_surface_floating(root(&state, first_surface_object_id), true));
        set_toplevel_window_state(&mut state, first_toplevel_object_id, false, true);
        set_toplevel_window_state(&mut state, second_toplevel_object_id, false, false);
        assert!(state.set_root_surface_fullscreen(root(&state, first_surface_object_id), true));
        assert!(state.set_root_surface_fullscreen(root(&state, second_surface_object_id), true));
        assert!(state.set_root_surface_fullscreen(root(&state, second_surface_object_id), false));

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:right" = { action = "resize-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (150.0, 100.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x111, 1, 100);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { .. })
        ));
    }

    #[test]
    fn compositor_mouse_grab_surface_loss_runs_full_teardown() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(320),
            XdgSurfaceId::from(330),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(330),
            XdgToplevelId::from(340),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(905usize), 30, 40, 120, 90);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (60.0, 70.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        assert!(state.seat.pointer_interactive_grab.is_some());
        assert!(state.remove_object(wl_surface_object_id).is_some());

        handle_pointer_motion(&mut state, &mut dispatcher, 5.0, 0.0, 5.0, 0.0, 110, 110_000);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.intercepted_pointer_buttons.is_empty());
        assert!(matches!(
            state.seat.visible_cursor_source(),
            Some(CursorSourceRef::Named { owner_client_id: None, .. })
        ));
        assert!(state.seat.pointer_focus.is_none());
    }

    #[test]
    fn removing_grab_client_clears_intercepted_buttons_and_recovers_cursor() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(350),
            XdgSurfaceId::from(360),
        );
        let _toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(360),
            XdgToplevelId::from(370),
        );
        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(906usize), 10, 10, 100, 80);
        }
        ensure_test_root_geometry(&mut state, wl_surface_object_id);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.keyboard.update_key(125, true);
        state.seat.pointer_pos = (50.0, 50.0);

        handle_pointer_button(&mut state, &mut dispatcher, 0x110, 1, 100);
        assert!(state.seat.pointer_interactive_grab.is_some());
        assert!(!state.seat.intercepted_pointer_buttons.is_empty());

        let removal = state.seat.remove_client(client.client_id());
        assert!(removal.interactive_grab_cleared);
        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(state.seat.intercepted_pointer_buttons.is_empty());

        recover_pointer_after_interactive_grab_loss(&mut state, &mut dispatcher, 0);

        assert!(matches!(
            state.seat.visible_cursor_source(),
            Some(CursorSourceRef::Named { owner_client_id: None, .. })
        ));
    }

    #[test]
    fn split_resize_grab_survives_client_removal() {
        let mut state = state_for_tests();
        let (client, _dispatcher) = test_client_and_dispatcher(7);

        state.seat.pointer_interactive_grab = Some(super::PointerInteractiveGrab::ResizeSplit {
            workspace: crate::state::WorkspaceId::from_parts(0, 1),
            drags: crate::tiling::SplitDragPair::default(),
            cursor_edges: xdg_toplevel::ResizeEdge::Right as u32,
            initial_pointer_pos: (10.0, 20.0),
            origin_window: Some(crate::window::WindowId::for_tests(1)),
        });
        state.seat.intercepted_pointer_buttons.insert(0x111);

        let removal = state.seat.remove_client(client.client_id());

        assert!(!removal.interactive_grab_cleared);
        assert!(state.seat.pointer_interactive_grab.is_some());
        assert!(state.seat.intercepted_pointer_buttons.contains(&0x111));
    }

    #[test]
    fn tiled_resize_release_dispatches_workspace_state_changed() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);

        state.seat.pointer_interactive_grab = Some(super::PointerInteractiveGrab::ResizeSplit {
            workspace: crate::state::WorkspaceId::from_parts(0, 1),
            drags: crate::tiling::SplitDragPair::default(),
            cursor_edges: xdg_toplevel::ResizeEdge::Right as u32,
            initial_pointer_pos: (10.0, 20.0),
            origin_window: Some(crate::window::WindowId::for_tests(1)),
        });

        finish_pointer_interactive_grab(&mut state, &mut dispatcher, 0);

        assert!(state.seat.pointer_interactive_grab.is_none());
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::WorkspaceStateChanged)
        }));
    }

    #[test]
    fn compositor_binding_swallows_key_press_and_release() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);

        state.config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Return" = { action = "spawn", command = ":" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 0, 30);

        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::KeyboardKey { key, .. },
                    } if *key == 28
                )
            }
        ));
    }

    fn queued_switch_vts(dispatcher: &Dispatcher) -> Vec<u32> {
        let mut vts = Vec::new();
        dispatcher.test_for_each_queued_action(|action: &crate::dbus::DbusAction| {
            if let crate::dbus::DbusAction::SwitchVt { vt } = action {
                vts.push(*vt);
            }
        });
        vts
    }

    fn client_saw_key(dispatcher: &Dispatcher, key: u32) -> bool {
        dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent {
                        event: SeatClientEvent::KeyboardKey { key: event_key, .. },
                    } if *event_key == key
                )
            },
        )
    }

    fn press_ctrl_alt(state: &mut State, dispatcher: &mut Dispatcher) {
        handle_keyboard_key(state, dispatcher, 29, 1, 10);
        handle_keyboard_key(state, dispatcher, 56, 1, 11);
    }

    /// Real surface `surface_id` of `client`, registered as a window root, focused.
    fn focused_window_surface(
        state: &mut State,
        client: &mut Client,
        surface_id: u32,
    ) -> Resolved<WlSurfaceId> {
        let (surface, _) = client.insert_object(
            state,
            WlSurface::new(WlSurfaceId::from(surface_id), client.client_id(), 6),
        );
        insert_window(state, surface);
        state.seat.keyboard_focus = Some(SurfaceFocus::new(surface));
        surface
    }

    /// Focused window surface 10 with inhibitor 11 attached, `user_enabled = enabled`.
    fn inhibited_focus(
        state: &mut State,
        client: &mut Client,
        enabled: bool,
    ) -> (Resolved<WlSurfaceId>, Resolved<ZwpKeyboardShortcutsInhibitorV1Id>) {
        let surface = focused_window_surface(state, client, 10);
        let (inhibitor, _) = client.insert_object(
            state,
            ZwpKeyboardShortcutsInhibitorV1::new(
                ZwpKeyboardShortcutsInhibitorV1Id::from(11),
                surface,
                enabled,
            ),
        );
        surface.ref_mut(state).set_shortcuts_inhibitor(inhibitor);
        (surface, inhibitor)
    }

    fn quit_binding_state(state: &mut State) {
        state.config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Return" = { action = "quit" }
            "#,
            &state.xkb,
        )
        .unwrap();
    }

    #[test]
    fn an_inhibited_surface_receives_a_bound_chord() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        inhibited_focus(&mut state, &mut client, true);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 0, 30);

        assert!(!dispatcher.shutdown_requested());
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(
                    action,
                    ClientAction::SeatEvent { event: SeatClientEvent::KeyboardKey { key: 28, .. } }
                )
            }),
            2
        );
    }

    #[test]
    fn a_disabled_inhibitor_lets_the_binding_run() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        inhibited_focus(&mut state, &mut client, false);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);

        assert!(dispatcher.shutdown_requested());
        assert!(!client_saw_key(&dispatcher, 28));
    }

    #[test]
    fn an_inhibitor_on_another_surface_does_not_inhibit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        inhibited_focus(&mut state, &mut client, true);
        focused_window_surface(&mut state, &mut client, 12);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);

        assert!(dispatcher.shutdown_requested());
    }

    #[test]
    fn vt_switch_still_works_while_inhibited() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        inhibited_focus(&mut state, &mut client, true);

        press_ctrl_alt(&mut state, &mut dispatcher);
        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);

        assert_eq!(queued_switch_vts(&dispatcher), vec![1]);
        assert!(!client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn a_removed_inhibitor_no_longer_inhibits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        let (surface, inhibitor) = inhibited_focus(&mut state, &mut client, true);
        surface
            .ref_mut(&mut state)
            .clear_shortcuts_inhibitor(inhibitor);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);

        assert!(dispatcher.shutdown_requested());
        assert!(!client_saw_key(&dispatcher, 28));
    }

    #[test]
    fn a_surface_without_a_window_does_not_inhibit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        quit_binding_state(&mut state);
        let (surface, _) = inhibited_focus(&mut state, &mut client, true);
        let window = state.window_for_root(surface).unwrap();
        state.destroy_window(window);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);

        assert!(dispatcher.shutdown_requested());
    }

    fn toggle_binding_state(state: &mut State) {
        state.config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Return" = { action = "quit" }
            "Super+Shift+Escape" = { action = "toggle-shortcuts-inhibit" }
            "#,
            &state.xkb,
        )
        .unwrap();
    }

    /// `active` values of every queued `ShortcutsInhibitorState` for client 7, in order.
    fn queued_inhibitor_states(dispatcher: &Dispatcher) -> Vec<bool> {
        let mut states = Vec::new();
        dispatcher.test_matching_actions_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| {
                if let ClientAction::ShortcutsInhibitorState { active, .. } = action {
                    states.push(*active);
                }
            },
        );
        states
    }

    fn toggle_chord(state: &mut State, dispatcher: &mut Dispatcher, time: u32) {
        handle_keyboard_key(state, dispatcher, 125, 1, time); // Super down
        handle_keyboard_key(state, dispatcher, 42, 1, time + 1); // Shift down
        handle_keyboard_key(state, dispatcher, 1, 1, time + 2); // Escape down: toggle
        handle_keyboard_key(state, dispatcher, 1, 0, time + 3); // Escape up (swallowed)
        handle_keyboard_key(state, dispatcher, 42, 0, time + 4); // Shift up
        handle_keyboard_key(state, dispatcher, 125, 0, time + 5); // Super up
    }

    #[test]
    fn the_toggle_runs_while_inhibited_and_restores_bindings() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        toggle_binding_state(&mut state);
        let (surface, inhibitor) = inhibited_focus(&mut state, &mut client, true);

        toggle_chord(&mut state, &mut dispatcher, 10);

        assert!(!inhibitor.rref(&state).user_enabled());
        assert!(surface.rref(&state).shortcuts_inhibit_denied());
        assert_eq!(queued_inhibitor_states(&dispatcher), vec![false]);
        assert!(!client_saw_key(&dispatcher, 1));

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 20);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 21);
        assert!(dispatcher.shutdown_requested());
    }

    #[test]
    fn toggling_twice_reactivates() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        toggle_binding_state(&mut state);
        let (surface, inhibitor) = inhibited_focus(&mut state, &mut client, true);

        toggle_chord(&mut state, &mut dispatcher, 10);
        toggle_chord(&mut state, &mut dispatcher, 20);

        assert!(inhibitor.rref(&state).user_enabled());
        assert!(!surface.rref(&state).shortcuts_inhibit_denied());
        assert_eq!(queued_inhibitor_states(&dispatcher), vec![false, true]);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 30);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 31);
        assert!(!dispatcher.shutdown_requested());
        assert!(client_saw_key(&dispatcher, 28));
    }

    #[test]
    fn the_toggle_without_an_inhibitor_does_nothing() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        toggle_binding_state(&mut state);
        let surface = focused_window_surface(&mut state, &mut client, 10);

        toggle_chord(&mut state, &mut dispatcher, 10);

        assert!(queued_inhibitor_states(&dispatcher).is_empty());
        assert!(!client_saw_key(&dispatcher, 1));
        assert!(!surface.rref(&state).shortcuts_inhibit_denied());
    }

    #[test]
    fn a_recreated_inhibitor_stays_denied() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        toggle_binding_state(&mut state);
        let (surface, inhibitor11) = inhibited_focus(&mut state, &mut client, true);
        toggle_chord(&mut state, &mut dispatcher, 10);

        // The client destroys inhibitor 11 and creates 12, as the manager does.
        surface
            .ref_mut(&mut state)
            .clear_shortcuts_inhibitor(inhibitor11);
        let granted = !surface.rref(&state).shortcuts_inhibit_denied();
        assert!(!granted);
        let (inhibitor12, _) = client.insert_object(
            &mut state,
            ZwpKeyboardShortcutsInhibitorV1::new(
                ZwpKeyboardShortcutsInhibitorV1Id::from(12),
                surface,
                granted,
            ),
        );
        surface
            .ref_mut(&mut state)
            .set_shortcuts_inhibitor(inhibitor12);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 20);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 21);
        assert!(dispatcher.shutdown_requested());
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 0, 22);
        handle_keyboard_key(&mut state, &mut dispatcher, 125, 0, 23);

        toggle_chord(&mut state, &mut dispatcher, 30);
        assert!(inhibitor12.rref(&state).user_enabled());
        assert_eq!(queued_inhibitor_states(&dispatcher), vec![false, true]);
    }

    #[test]
    fn a_release_after_toggling_off_reaches_the_client() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        toggle_binding_state(&mut state);
        inhibited_focus(&mut state, &mut client, true);

        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 11);
        assert!(client_saw_key(&dispatcher, 28));
        assert!(!dispatcher.shutdown_requested());

        // Shift and Escape toggle the inhibitor off while Super and Return are held.
        handle_keyboard_key(&mut state, &mut dispatcher, 42, 1, 12);
        handle_keyboard_key(&mut state, &mut dispatcher, 1, 1, 13);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 0, 14);

        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(
                    action,
                    ClientAction::SeatEvent { event: SeatClientEvent::KeyboardKey { key: 28, .. } }
                )
            }),
            2
        );
        assert!(!dispatcher.shutdown_requested());
    }

    #[test]
    fn ctrl_alt_f1_switches_vt_and_swallows_the_key() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));

        press_ctrl_alt(&mut state, &mut dispatcher);
        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);
        assert_eq!(queued_switch_vts(&dispatcher), vec![1]);
        assert!(!client_saw_key(&dispatcher, 59));

        handle_keyboard_key(&mut state, &mut dispatcher, 59, 0, 30);
        assert_eq!(queued_switch_vts(&dispatcher), vec![1]);
        assert!(!client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn ctrl_alt_f1_switches_vt_while_the_session_is_locked() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));
        state
            .session_lock
            .set_phase_for_tests(crate::session_lock::SessionLockPhase::Locked);

        press_ctrl_alt(&mut state, &mut dispatcher);
        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);
        assert_eq!(queued_switch_vts(&dispatcher), vec![1]);
        assert!(!client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn ctrl_alt_f1_does_not_switch_vt_while_the_session_is_paused() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));
        state.session_active = false;

        press_ctrl_alt(&mut state, &mut dispatcher);
        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);
        assert!(queued_switch_vts(&dispatcher).is_empty());
        assert!(client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn vt_switch_runs_before_a_binding_on_the_same_chord() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Ctrl+Alt+XF86Switch_VT_1" = { action = "quit" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));

        press_ctrl_alt(&mut state, &mut dispatcher);
        assert_eq!(
            state.config.binding_for_key(&state.keyboard, 59),
            Some(crate::config::CompositorAction::Quit)
        );
        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);
        assert_eq!(queued_switch_vts(&dispatcher), vec![1]);
        assert!(!dispatcher.shutdown_requested());
        assert!(!client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn plain_f1_reaches_the_focused_client() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));

        handle_keyboard_key(&mut state, &mut dispatcher, 59, 1, 20);
        assert!(queued_switch_vts(&dispatcher).is_empty());
        assert!(client_saw_key(&dispatcher, 59));
    }

    #[test]
    fn seat_input_handlers_do_not_record_activity() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        handle_keyboard_key(&mut state, &mut dispatcher, 30, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 30, 0, 20);
        handle_pointer_button(&mut state, &mut dispatcher, 272, 1, 10);
        handle_pointer_button(&mut state, &mut dispatcher, 272, 0, 20);
        handle_pointer_motion(&mut state, &mut dispatcher, 4.0, -2.0, 4.0, -2.0, 10, 10_000);
        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 12.0, 8.0, 10);
        handle_pointer_scroll(
            &mut state,
            &mut dispatcher,
            PointerScrollFrame {
                time: 10,
                source: PointerScrollSource::Wheel,
                vertical: None,
                horizontal: None,
            },
        );
        crate::input::set_keyboard_focus(
            &mut state,
            &mut dispatcher,
            Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10))),
        );
        assert!(state.seat.last_real_input().is_none());
    }

    #[test]
    fn a_held_button_does_not_move_the_activity_clock() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let at = std::time::Instant::now() - std::time::Duration::from_secs(30);
        state.seat.note_real_input(at);
        handle_pointer_button(&mut state, &mut dispatcher, 272, 1, 10);
        assert_eq!(state.seat.pointer_button_count, 1);
        assert_eq!(state.seat.last_real_input(), Some(at));
    }

    #[test]
    fn pause_releases_do_not_move_the_activity_clock() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let at = std::time::Instant::now();
        state.seat.note_real_input(at);
        handle_keyboard_key(&mut state, &mut dispatcher, 30, 1, 10);
        handle_pointer_button(&mut state, &mut dispatcher, 272, 1, 10);
        state.session_active = false;
        quiesce_for_session_pause(&mut state, &mut dispatcher);
        assert_eq!(state.seat.last_real_input(), Some(at));
        assert!(state.keyboard.pressed_keys().0.is_empty());
        assert!(state.seat.pressed_buttons.is_empty());
    }

    #[test]
    fn clearing_device_state_keeps_the_activity_clock() {
        let mut state = state_for_tests();
        let at = std::time::Instant::now();
        state.seat.note_real_input(at);
        state.seat.clear_device_state();
        assert_eq!(state.seat.last_real_input(), Some(at));
    }

    #[test]
    fn a_swallowed_binding_keeps_activity_recorded_before_routing() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        state.config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Return" = { action = "spawn", command = ":" }
            "#,
            &state.xkb,
        )
        .unwrap();
        state.seat.keyboard_focus = Some(fake_focus(ClientId::from_raw(7), WlSurfaceId::from(10)));
        let at = std::time::Instant::now();
        state.seat.note_real_input(at);
        handle_keyboard_key(&mut state, &mut dispatcher, 125, 1, 10);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 1, 20);
        handle_keyboard_key(&mut state, &mut dispatcher, 28, 0, 30);
        assert_eq!(state.seat.last_real_input(), Some(at));
    }

    #[test]
    fn removing_last_popup_grab_restores_parent_focus() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);

        let parent_surface_id = WlSurfaceId::from(50);
        let popup_surface_id = WlSurfaceId::from(51);
        let parent_surface_object_id = insert_surface(&mut state, &mut client, parent_surface_id);
        let popup_surface_object_id = insert_surface(&mut state, &mut client, popup_surface_id);
        let parent_focus =
            focus_for_surface(client.client_id(), parent_surface_id, parent_surface_object_id);
        let popup_focus =
            focus_for_surface(client.client_id(), popup_surface_id, popup_surface_object_id);

        state.seat.keyboard_focus = Some(parent_focus);
        begin_popup_grab(
            &mut state,
            &mut dispatcher,
            parent_focus.client_id(),
            crate::clients::Resolved::new(
                parent_focus.client_id(),
                XdgPopupId::from(60),
                ObjectId::from(60usize),
            ),
            *popup_focus,
        );
        state.seat.keyboard_focus = Some(popup_focus);

        remove_popup_grab(
            &mut state,
            &mut dispatcher,
            crate::clients::Resolved::new(
                parent_focus.client_id(),
                XdgPopupId::from(60),
                ObjectId::from(60usize),
            ),
            Some(parent_focus),
        );

        assert!(state.seat.popup_grab.is_none());
        assert_eq!(state.seat.keyboard_focus, Some(parent_focus));
    }

    #[test]
    fn toplevel_resize_rejects_none_edge_without_starting_grab() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(3)));

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(70),
            XdgSurfaceId::from(80),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(80),
            XdgToplevelId::from(90),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(500usize), 40, 50, 120, 80);
        }

        let focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(70), wl_surface_object_id);
        let serial = state.seat.next_serial();
        state.seat.record_pointer_button(1, serial, Some(focus));

        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_resize_request(
                    XdgToplevelId::from(90),
                    WlSeatId::from(3),
                    serial,
                    xdg_toplevel::ResizeEdge::None as u32,
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(state.seat.pointer_interactive_grab.is_none());

        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_resize_request(
                    XdgToplevelId::from(90),
                    WlSeatId::from(3),
                    serial,
                    xdg_toplevel::ResizeEdge::Right as u32,
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::Right as u32
        ));
    }

    #[test]
    fn toplevel_resize_accepts_keyboard_trigger_serial() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(4)));

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(100),
            XdgSurfaceId::from(110),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(110),
            XdgToplevelId::from(120),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(700usize), 10, 20, 80, 60);
        }

        let focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(100), wl_surface_object_id);
        let serial = state.seat.next_serial();
        state.seat.record_keyboard_trigger(serial, focus);

        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_resize_request(
                    XdgToplevelId::from(120),
                    WlSeatId::from(4),
                    serial,
                    xdg_toplevel::ResizeEdge::BottomRight as u32,
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Resize { edges, .. })
                if edges == xdg_toplevel::ResizeEdge::BottomRight as u32
        ));
    }

    #[test]
    fn toplevel_move_accepts_pointer_trigger_serial() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(6)));

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(101),
            XdgSurfaceId::from(111),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(111),
            XdgToplevelId::from(121),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(701usize), 20, 30, 90, 70);
        }

        let focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(101), wl_surface_object_id);
        let serial = state.seat.next_serial();
        state.seat.record_pointer_button(1, serial, Some(focus));

        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_move_request(XdgToplevelId::from(121), WlSeatId::from(6), serial),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(matches!(
            state.seat.pointer_interactive_grab,
            Some(super::PointerInteractiveGrab::Move { .. })
        ));
    }

    #[test]
    fn interactive_resize_clamps_left_edge_to_toplevel_constraints() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        client.insert_object(&mut state, WlSeat::new(WlSeatId::from(5)));

        let (wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(130),
            XdgSurfaceId::from(140),
        );
        let toplevel_object_id = assign_toplevel_role(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            xdg_surface_object_id,
            XdgSurfaceId::from(140),
            XdgToplevelId::from(150),
        );

        {
            let surface = state
                .objects
                .get_mut(wl_surface_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            surface.test_map(ObjectId::from(800usize), 40, 50, 100, 80);
        }

        {
            let mut toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            toplevel.test_set_interactive_size_constraints(Some((90, 0)), Some((110, 0)));
        }

        let focus =
            focus_for_surface(client.client_id(), WlSurfaceId::from(130), wl_surface_object_id);
        let serial = state.seat.next_serial();
        state.seat.record_pointer_button(1, serial, Some(focus));
        state.seat.pointer_pos = (40.0, 90.0);

        {
            let toplevel = state.objects.grab_object::<XdgToplevel>(toplevel_object_id);
            let _ = toplevel.on(
                &mut client,
                &mut state,
                toplevel_resize_request(
                    XdgToplevelId::from(150),
                    WlSeatId::from(5),
                    serial,
                    xdg_toplevel::ResizeEdge::Left as u32,
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        handle_pointer_motion(&mut state, &mut dispatcher, 60.0, 0.0, 60.0, 0.0, 110, 110_000);

        let surface = state
            .objects
            .get(wl_surface_object_id)
            .downcast_ref::<WlSurface>()
            .unwrap();
        assert_eq!(surface.root_position(), (40, 50));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client.client_id(),
            |action| {
                matches!(
                    action,
                    ClientAction::InteractiveResizeConfigure {
                        xdg_toplevel,
                        width,
                        height,
                        resizing,
                    } if xdg_toplevel.wayland_id() == XdgToplevelId::from(150)
                        && *width == 90
                        && *height == 80
                        && *resizing
                )
            }
        ));
    }

    #[test]
    fn get_popup_allows_null_parent_for_layer_shell_workflows() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (_wl_surface_object_id, xdg_surface_object_id) = insert_xdg_surface(
            &mut state,
            &mut client,
            &mut client_dispatcher,
            WlSurfaceId::from(130),
            XdgSurfaceId::from(140),
        );
        let (positioner_resolved, _) =
            client.insert_object(&mut state, XdgPositioner::new(XdgPositionerId::from(150)));
        let positioner_object_id = positioner_resolved.object_id();

        {
            let positioner = state
                .objects
                .grab_object::<XdgPositioner>(positioner_object_id);
            let _ = positioner.on(
                &mut client,
                &mut state,
                positioner_set_size_request(XdgPositionerId::from(150), 40, 30),
                &mut client_dispatcher,
            );
        }
        {
            let positioner = state
                .objects
                .grab_object::<XdgPositioner>(positioner_object_id);
            let _ = positioner.on(
                &mut client,
                &mut state,
                positioner_set_anchor_rect_request(XdgPositionerId::from(150), 0, 0, 10, 10),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface_object_id);
            let _ = xdg_surface.on(
                &mut client,
                &mut state,
                get_popup_request(
                    XdgSurfaceId::from(140),
                    XdgPopupId::from(160),
                    XdgSurfaceId::from(0),
                    XdgPositionerId::from(150),
                ),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(client.contains_object(XdgPopupId::from(160), &state));

        {
            let xdg_surface = state
                .objects
                .grab_object::<XdgSurface>(xdg_surface_object_id);
            let _ = xdg_surface.on(
                &mut client,
                &mut state,
                get_toplevel_request(XdgSurfaceId::from(140), XdgToplevelId::from(170)),
                &mut client_dispatcher,
            );
        }
        client_dispatcher.drain_post_actions(&mut client, &mut state);

        assert!(!client.contains_object(XdgToplevelId::from(170), &state));
    }

    #[test]
    fn no_focus_fallback_installs_default_cursor() {
        let mut seat = SeatState::default();
        assert!(seat.set_cursor_surface(
            crate::clients::Resolved::new(
                ClientId::from_raw(7),
                WlSurfaceId::from(50),
                ObjectId::from(80usize),
            ),
            4,
            5,
        ));

        assert!(matches!(
            seat.visible_cursor_source(),
            Some(CursorSourceRef::Surface { cursor })
                if cursor.surface.client_id() == ClientId::from_raw(7)
                    && cursor.surface.object_id() == ObjectId::from(80usize)
        ));

        assert!(seat.test_install_default_cursor());
        assert!(matches!(
            seat.visible_cursor_source(),
            Some(CursorSourceRef::Named { owner_client_id: None, .. })
        ));
    }

    #[test]
    fn explicit_client_cursor_clear_can_be_followed_by_default_fallback() {
        let mut seat = SeatState::default();
        seat.test_install_default_cursor();
        assert!(seat.set_cursor_surface(
            crate::clients::Resolved::new(
                ClientId::from_raw(7),
                WlSurfaceId::from(50),
                ObjectId::from(80usize),
            ),
            4,
            5,
        ));

        assert!(seat.clear_cursor_for_client(ClientId::from_raw(7)));
        assert!(seat.visible_cursor_source().is_none());

        assert!(seat.test_install_default_cursor());
        assert!(matches!(
            seat.visible_cursor_source(),
            Some(CursorSourceRef::Named { owner_client_id: None, .. })
        ));
    }

    #[test]
    fn client_cursor_stays_visible_without_default_fallback() {
        let mut seat = SeatState::default();
        assert!(seat.set_cursor_surface(
            crate::clients::Resolved::new(
                ClientId::from_raw(7),
                WlSurfaceId::from(50),
                ObjectId::from(80usize),
            ),
            4,
            5,
        ));

        assert!(matches!(
            seat.visible_cursor_source(),
            Some(CursorSourceRef::Surface { cursor })
                if cursor.surface.client_id() == ClientId::from_raw(7)
                    && cursor.surface.object_id() == ObjectId::from(80usize)
        ));
    }

    #[test]
    fn pointer_cursor_request_accepts_exact_enter_serial() {
        let mut seat = SeatState::default();
        seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 100, height: 100 },
        });
        seat.record_pointer_enter(ClientId::from_raw(7), 100);

        assert!(seat.pointer_cursor_request_allowed(ClientId::from_raw(7), 100));
        assert!(!seat.pointer_cursor_request_allowed(ClientId::from_raw(7), 99));
    }

    #[test]
    fn pointer_cursor_request_rejects_wrong_enter_serial() {
        let mut seat = SeatState::default();
        seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 100, height: 100 },
        });
        seat.record_pointer_enter(ClientId::from_raw(7), 100);

        assert!(!seat.pointer_cursor_request_allowed(ClientId::from_raw(7), 200));
    }

    #[test]
    fn pointer_cursor_request_accepts_active_grab_serial_for_focused_client() {
        let mut seat = SeatState::default();
        let focus = fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50));
        seat.pointer_focus = Some(super::PointerFocus {
            target: focus,
            rect: super::SurfaceRect { x: 0, y: 0, width: 100, height: 100 },
        });
        seat.record_pointer_enter(ClientId::from_raw(7), 10);
        seat.record_pointer_button(1, 20, Some(focus));

        assert!(seat.pointer_cursor_request_allowed(ClientId::from_raw(7), 20));
        assert!(!seat.pointer_cursor_request_allowed(ClientId::from_raw(7), 19));
    }

    #[test]
    fn local_drag_drop_without_source_finishes_immediately() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        let target_client_id = ClientId::from_raw(42);

        state.seat.set_active_drag(DragState {
            source: DragSource::None { initiator_client_id: target_client_id },
            origin_surface: crate::clients::Resolved::new(
                target_client_id,
                WlSurfaceId::from(10),
                ObjectId::from(10usize),
            ),
            icon_surface: None,
            target: Some(DragTargetState {
                surface: crate::clients::Resolved::new(
                    target_client_id,
                    WlSurfaceId::from(11),
                    ObjectId::from(0usize),
                ),
            }),
            negotiation: Default::default(),
            phase: DragPhase::Dragging,
        });

        complete_drag_drop(&mut state, &mut dispatcher);

        assert!(state.seat.active_drag().is_none());
        assert!(dispatcher
            .test_has_queued_action_for::<ClientAction, ClientId>(target_client_id, |action| {
                matches!(action, ClientAction::DataDeviceDragDrop)
            }));
    }

    #[test]
    fn invalid_drag_drop_cancels_source() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        let source_client_id = ClientId::from_raw(50);
        let target_client_id = ClientId::from_raw(51);
        let source_id = crate::protocol_generated::WlDataSourceId::from(12);

        state.seat.set_active_drag(DragState {
            source: DragSource::DataSource {
                source: crate::clients::Resolved::new(
                    source_client_id,
                    source_id,
                    ObjectId::from(0usize),
                ),
            },
            origin_surface: crate::clients::Resolved::new(
                source_client_id,
                WlSurfaceId::from(20),
                ObjectId::from(20usize),
            ),
            icon_surface: None,
            target: Some(DragTargetState {
                surface: crate::clients::Resolved::new(
                    target_client_id,
                    WlSurfaceId::from(21),
                    ObjectId::from(0usize),
                ),
            }),
            negotiation: Default::default(),
            phase: DragPhase::Dragging,
        });

        complete_drag_drop(&mut state, &mut dispatcher);

        assert!(state.seat.active_drag().is_none());
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            source_client_id,
            |action| {
                matches!(
                    action,
                    ClientAction::DataSourceEvent {
                        source: queued_source,
                        event: DataSourceClientEvent::Cancelled,
                    } if queued_source.wayland_id() == source_id
                )
            }
        ));
    }

    #[test]
    fn cancelling_drag_with_icon_refreshes_source_outputs() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        let source_client_id = ClientId::from_raw(60);

        state.seat.set_active_drag(DragState {
            source: DragSource::None { initiator_client_id: source_client_id },
            origin_surface: crate::clients::Resolved::new(
                source_client_id,
                WlSurfaceId::from(30),
                ObjectId::from(30usize),
            ),
            icon_surface: Some(crate::clients::Resolved::new(
                source_client_id,
                WlSurfaceId::from(31),
                ObjectId::from(123usize),
            )),
            target: None,
            negotiation: Default::default(),
            phase: DragPhase::Dragging,
        });

        cancel_active_drag(&mut state, &mut dispatcher, None);

        assert!(state.seat.active_drag().is_none());
        assert!(dispatcher
            .test_has_queued_action_for::<ClientAction, ClientId>(source_client_id, |action| {
                matches!(action, ClientAction::RefreshSurfaceOutputs)
            }));
    }

    fn mapped_pointer_surface(
        state: &mut State,
        client_id: ClientId,
        surface_id: u32,
        width: i32,
        height: i32,
    ) -> Resolved<WlSurfaceId> {
        let mut client = Client::new_for_tests(client_id);
        let wayland_id = WlSurfaceId::from(surface_id);
        let (surface, _) = client.insert_object(state, WlSurface::new(wayland_id, client_id, 6));
        state
            .objects
            .grab_object::<WlSurface>(surface.object_id())
            .test_map(ObjectId::from(surface_id as usize + 500), 0, 0, width, height);
        surface
    }

    fn drag_with_icon(state: &mut State, client_id: ClientId, icon: Resolved<WlSurfaceId>) {
        state.seat.set_active_drag(DragState {
            source: DragSource::None { initiator_client_id: client_id },
            origin_surface: Resolved::new(
                client_id,
                WlSurfaceId::from(30),
                ObjectId::from(30usize),
            ),
            icon_surface: Some(icon),
            target: None,
            negotiation: Default::default(),
            phase: DragPhase::Dragging,
        });
    }

    fn tree_refresh_queued(
        dispatcher: &Dispatcher,
        client_id: ClientId,
        icon: Resolved<WlSurfaceId>,
    ) -> bool {
        dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(client_id, |action| {
            matches!(action, ClientAction::RefreshSurfaceTreeOutputs { root } if *root == icon)
        })
    }

    #[test]
    fn drag_motion_refreshes_only_icon_tree() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        let source_client_id = ClientId::from_raw(60);
        state.insert_output(test_output_drm1(0, 0, 200, 120));
        let icon = mapped_pointer_surface(&mut state, source_client_id, 31, 20, 20);
        state.seat.pointer_pos = (50.0, 50.0);
        drag_with_icon(&mut state, source_client_id, icon);

        refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);

        assert!(tree_refresh_queued(&dispatcher, source_client_id, icon));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            source_client_id,
            |action| matches!(action, ClientAction::RefreshSurfaceOutputs)
        ));
    }

    #[test]
    fn dnd_icon_refresh_skipped_while_outputs_unchanged() {
        let mut state = state_for_tests();
        let source_client_id = ClientId::from_raw(60);
        state.insert_output(test_output_drm1(0, 0, 200, 120));
        let icon = mapped_pointer_surface(&mut state, source_client_id, 31, 20, 20);
        state.seat.pointer_pos = (50.0, 50.0);
        drag_with_icon(&mut state, source_client_id, icon);

        let mut dispatcher = Dispatcher::default();
        refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);
        assert!(tree_refresh_queued(&dispatcher, source_client_id, icon));

        state.seat.pointer_pos = (60.0, 60.0);
        let mut dispatcher = Dispatcher::default();
        refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);
        assert!(!tree_refresh_queued(&dispatcher, source_client_id, icon));
    }

    #[test]
    fn dnd_icon_refresh_when_crossing_outputs() {
        let mut state = state_for_tests();
        let source_client_id = ClientId::from_raw(60);
        state.insert_output(test_output_drm1(0, 0, 200, 120));
        state.insert_output(test_output_drm1(200, 0, 200, 120));
        let icon = mapped_pointer_surface(&mut state, source_client_id, 31, 20, 20);
        drag_with_icon(&mut state, source_client_id, icon);

        for (x, expect_refresh) in [(50.0, true), (190.0, true), (300.0, true), (310.0, false)] {
            state.seat.pointer_pos = (x, 50.0);
            let mut dispatcher = Dispatcher::default();
            refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);
            assert_eq!(
                tree_refresh_queued(&dispatcher, source_client_id, icon),
                expect_refresh,
                "pointer x={x}"
            );
        }
    }

    #[test]
    fn new_drag_does_not_reuse_previous_icon_hint() {
        let mut state = state_for_tests();
        let (_client, mut dispatcher) = test_client_and_dispatcher(7);
        let source_client_id = ClientId::from_raw(60);
        state.insert_output(test_output_drm1(0, 0, 200, 120));
        let icon = mapped_pointer_surface(&mut state, source_client_id, 31, 20, 20);
        state.seat.pointer_pos = (50.0, 50.0);
        drag_with_icon(&mut state, source_client_id, icon);
        refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);
        assert!(tree_refresh_queued(&dispatcher, source_client_id, icon));

        cancel_active_drag(&mut state, &mut dispatcher, None);
        drag_with_icon(&mut state, source_client_id, icon);
        state.seat.clear_dnd_icon_outputs_hint();

        let mut dispatcher = Dispatcher::default();
        refresh_visible_dnd_icon_surface_outputs(&mut state, &mut dispatcher);
        assert!(tree_refresh_queued(&dispatcher, source_client_id, icon));
    }

    #[test]
    fn cursor_refresh_skipped_while_outputs_unchanged() {
        let mut state = state_for_tests();
        let client_id = ClientId::from_raw(60);
        state.insert_output(test_output_drm1(0, 0, 200, 120));
        let cursor = mapped_pointer_surface(&mut state, client_id, 31, 20, 20);
        state.seat.set_cursor_surface(cursor, 0, 0);
        state.seat.pointer_pos = (50.0, 50.0);

        let mut dispatcher = Dispatcher::default();
        refresh_visible_cursor_surface_outputs(&mut state, &mut dispatcher);
        assert!(tree_refresh_queued(&dispatcher, client_id, cursor));

        state.seat.pointer_pos = (60.0, 60.0);
        let mut dispatcher = Dispatcher::default();
        refresh_visible_cursor_surface_outputs(&mut state, &mut dispatcher);
        assert!(!tree_refresh_queued(&dispatcher, client_id, cursor));
    }

    fn axis_bits(axis: PointerScrollAxis, value: f32) -> PointerScrollWireEvent {
        PointerScrollWireEvent::Axis { time: 40, axis, value_bits: value.to_bits() }
    }

    fn wheel_scroll() -> PointerScrollFrame {
        PointerScrollFrame {
            time: 40,
            source: PointerScrollSource::Wheel,
            vertical: Some(PointerAxisChange::Scroll {
                value: -15.0,
                discrete: Some(-1),
                value120: Some(-120),
                relative_direction: Some(PointerScrollDirection::Inverted),
            }),
            horizontal: Some(PointerAxisChange::Scroll {
                value: 15.0,
                discrete: Some(1),
                value120: Some(120),
                relative_direction: Some(PointerScrollDirection::Identical),
            }),
        }
    }

    #[test]
    fn scroll_versions_keep_one_ordered_frame() {
        use PointerScrollWireEvent::*;

        let frame = wheel_scroll();
        assert_eq!(
            pointer_scroll_protocol_events(4, &frame),
            vec![
                axis_bits(PointerScrollAxis::Vertical, -15.0),
                axis_bits(PointerScrollAxis::Horizontal, 15.0),
            ]
        );
        assert_eq!(
            pointer_scroll_protocol_events(5, &frame),
            vec![
                Source(PointerScrollSource::Wheel),
                Discrete { axis: PointerScrollAxis::Vertical, discrete: -1 },
                axis_bits(PointerScrollAxis::Vertical, -15.0),
                Discrete { axis: PointerScrollAxis::Horizontal, discrete: 1 },
                axis_bits(PointerScrollAxis::Horizontal, 15.0),
                Frame,
            ]
        );
        assert_eq!(
            pointer_scroll_protocol_events(8, &frame),
            vec![
                Source(PointerScrollSource::Wheel),
                Value120 { axis: PointerScrollAxis::Vertical, value120: -120 },
                axis_bits(PointerScrollAxis::Vertical, -15.0),
                Value120 { axis: PointerScrollAxis::Horizontal, value120: 120 },
                axis_bits(PointerScrollAxis::Horizontal, 15.0),
                Frame,
            ]
        );
        assert_eq!(
            pointer_scroll_protocol_events(9, &frame),
            vec![
                Source(PointerScrollSource::Wheel),
                Value120 { axis: PointerScrollAxis::Vertical, value120: -120 },
                RelativeDirection {
                    axis: PointerScrollAxis::Vertical,
                    direction: PointerScrollDirection::Inverted,
                },
                axis_bits(PointerScrollAxis::Vertical, -15.0),
                Value120 { axis: PointerScrollAxis::Horizontal, value120: 120 },
                RelativeDirection {
                    axis: PointerScrollAxis::Horizontal,
                    direction: PointerScrollDirection::Identical,
                },
                axis_bits(PointerScrollAxis::Horizontal, 15.0),
                Frame,
            ]
        );
    }

    #[test]
    fn scroll_stop_frame_has_no_axis_distance() {
        use PointerScrollWireEvent::*;

        let frame = PointerScrollFrame {
            time: 40,
            source: PointerScrollSource::Finger,
            vertical: Some(PointerAxisChange::Stop),
            horizontal: Some(PointerAxisChange::Scroll {
                value: 2.0,
                discrete: None,
                value120: Some(0),
                relative_direction: None,
            }),
        };
        assert!(pointer_scroll_protocol_events(4, &frame)
            .iter()
            .all(|event| matches!(event, Axis { .. })));
        assert_eq!(
            pointer_scroll_protocol_events(5, &frame),
            vec![
                Source(PointerScrollSource::Finger),
                axis_bits(PointerScrollAxis::Horizontal, 2.0),
                Stop { time: 40, axis: PointerScrollAxis::Vertical },
                Frame,
            ]
        );
        let stopped = PointerScrollFrame {
            time: 40,
            source: PointerScrollSource::Finger,
            vertical: Some(PointerAxisChange::Stop),
            horizontal: Some(PointerAxisChange::Stop),
        };
        assert!(pointer_scroll_protocol_events(4, &stopped).is_empty());
        assert_eq!(
            pointer_scroll_protocol_events(8, &stopped),
            vec![
                Source(PointerScrollSource::Finger),
                Stop { time: 40, axis: PointerScrollAxis::Vertical },
                Stop { time: 40, axis: PointerScrollAxis::Horizontal },
                Frame,
            ]
        );
        assert!(pointer_scroll_protocol_events(
            9,
            &PointerScrollFrame { vertical: None, horizontal: None, ..stopped }
        )
        .is_empty());
    }

    #[test]
    fn scroll_is_delivered_only_to_the_pointer_focus() {
        let mut state = state_for_tests();
        let mut dispatcher = crate::runner::Dispatcher::default();
        let client_id = ClientId::from_raw(7);
        let frame = wheel_scroll();

        handle_pointer_scroll(&mut state, &mut dispatcher, frame);
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client_id,
            |action| matches!(action, ClientAction::SeatEvent { .. })
        ));

        state.seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(client_id, WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 10, height: 10 },
        });
        handle_pointer_scroll(&mut state, &mut dispatcher, frame);
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client_id,
            |action| {
                matches!(
                    action,
                    ClientAction::SeatEvent { event: SeatClientEvent::PointerScroll(scroll) }
                        if *scroll == frame
                )
            }
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client_id,
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::PointerFrame }
            )
        ));
    }

    fn register_relative_pointer(
        state: &mut State,
        client: &mut Client,
        pointer_id: u32,
        relative_id: u32,
    ) -> Resolved<ZwpRelativePointerV1Id> {
        let (pointer, _) =
            client.insert_object(state, WlPointer::new(WlPointerId::from(pointer_id), 9));
        let (relative, _) = client.insert_object(
            state,
            ZwpRelativePointerV1::new(ZwpRelativePointerV1Id::from(relative_id), pointer),
        );
        state.seat.register_relative_pointer(pointer, relative);
        relative
    }

    #[test]
    fn relative_motion_reaches_only_the_focused_client_with_the_unclipped_delta() {
        let mut state = state_for_tests();
        state.insert_output(crate::test_support::test_output_drm0_wh(100, 80));
        let (mut focused, mut dispatcher) = test_client_and_dispatcher(7);
        let (mut other, _) = test_client_and_dispatcher(8);
        register_relative_pointer(&mut state, &mut focused, 5, 7);
        register_relative_pointer(&mut state, &mut other, 6, 8);
        state.seat.pointer_pos = (99.0, 10.0);
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 20, height: 20 },
        });

        handle_pointer_motion(
            &mut state,
            &mut dispatcher,
            40.0,
            -2.25,
            3.0,
            -0.5,
            20,
            (1u64 << 32) | 5,
        );

        assert_eq!(state.seat.pointer_pos.0, 99.0);
        assert_eq!(state.seat.pointer_pos.1, 8.0);
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| matches!(
                action,
                ClientAction::SeatEvent {
                    event: SeatClientEvent::RelativePointerMotion {
                        utime_hi: 1,
                        utime_lo: 5,
                        dx,
                        dy,
                        dx_unaccel,
                        dy_unaccel,
                    }
                } if *dx == 40.0 && *dy == -2.25 && *dx_unaccel == 3.0 && *dy_unaccel == -0.5
            )
        ));
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(8),
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::RelativePointerMotion { .. } }
            )
        ));
    }

    #[test]
    fn absolute_motion_does_not_synthesize_relative_motion() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        register_relative_pointer(&mut state, &mut client, 5, 7);
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 20, height: 20 },
        });

        handle_pointer_motion_absolute(&mut state, &mut dispatcher, 12.0, 8.0, 20);

        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::RelativePointerMotion { .. } }
            )
        ));
    }

    #[test]
    fn relative_motion_stops_after_the_object_or_client_is_gone() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let relative = register_relative_pointer(&mut state, &mut client, 5, 7);
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 20, height: 20 },
        });
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        state
            .objects
            .grab(relative.object_id())
            .on(
                &mut client,
                &mut state,
                crate::clients::RawRequest {
                    object_id: 7,
                    request_id: 0,
                    arguments: crate::clients::Arguments::from_words(
                        std::collections::VecDeque::new(),
                    ),
                    fds: None,
                },
                &mut client_dispatcher,
            )
            .unwrap();
        drop(client_dispatcher);

        handle_pointer_motion(&mut state, &mut dispatcher, 4.0, 1.0, 4.0, 1.0, 20, 20);
        assert!(!dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            ClientId::from_raw(7),
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::RelativePointerMotion { .. } }
            )
        ));

        register_relative_pointer(&mut state, &mut client, 6, 8);
        state.seat.remove_client(ClientId::from_raw(7));
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: fake_focus(ClientId::from_raw(7), WlSurfaceId::from(50)),
            rect: super::SurfaceRect { x: 0, y: 0, width: 20, height: 20 },
        });
        handle_pointer_motion(&mut state, &mut dispatcher, 4.0, 1.0, 4.0, 1.0, 21, 21);
        assert!(state
            .seat
            .relative_pointers_for_client(ClientId::from_raw(7))
            .is_empty());
        assert!(!dispatcher.test_has_queued_action(|action: &ClientAction| matches!(
            action,
            ClientAction::SeatEvent {
                event: SeatClientEvent::RelativePointerMotion { utime_lo: 21, .. }
            }
        )));
    }

    #[test]
    fn session_pause_releases_buttons_and_leaves_focus() {
        let mut state = state_for_tests();
        let mut dispatcher = crate::runner::Dispatcher::default();
        let client_id = ClientId::from_raw(7);
        let surface = fake_focus(client_id, WlSurfaceId::from(50));
        state.keyboard.update_key(30, true);
        state.seat.pressed_buttons.insert(0x110);
        state.seat.pointer_button_count = 1;
        state.seat.keyboard_focus = Some(surface);
        state.seat.remember_keyboard_focus(surface);
        state.seat.pointer_focus = Some(super::PointerFocus {
            target: surface,
            rect: super::SurfaceRect { x: 0, y: 0, width: 10, height: 10 },
        });

        super::quiesce_for_session_pause(&mut state, &mut dispatcher);

        assert!(state.keyboard.pressed_keys().0.is_empty());
        assert!(state.seat.pressed_buttons.is_empty());
        assert_eq!(state.seat.pointer_button_count, 0);
        assert!(state.seat.keyboard_focus.is_none());
        assert!(state.seat.pointer_focus.is_none());
        assert!(state.seat.keyboard_focus_history().contains(&surface));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client_id,
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::KeyboardLeave { .. } }
            )
        ));
        assert!(dispatcher.test_has_queued_action_for::<ClientAction, ClientId>(
            client_id,
            |action| matches!(
                action,
                ClientAction::SeatEvent { event: SeatClientEvent::PointerLeave { .. } }
            )
        ));
    }
}
