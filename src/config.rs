use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    ffi::OsStr,
    fmt, fs,
    io::ErrorKind,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use {
    anyhow::{anyhow, bail, Context},
    inotify::{EventMask, Inotify, WatchDescriptor, WatchMask},
    serde::Deserialize,
    xkbcommon::xkb,
};

use crate::{
    animation_config::{compile as compile_animations, AnimationsConfig, AnimationsConfigFile},
    clients::{ClientManagerAction, Resolved},
    drm::{kick_visual_animation_pump, request_outputs, request_outputs_for_points},
    io_uring::{
        model::IoUringAction,
        ring::{PollCompleted, RingId},
    },
    process::{ChildExit, ProcessEvent, ProcessManager, ProcessManagerAction, SpawnRequest},
    protocol_generated::WlSurfaceId,
    runner::{Dispatcher, Model, Redispatcher, ShutdownReason},
    state::{FractionalScale, ManagedWindowMode, State, WorkspaceGroupId, WorkspaceId},
    window::WindowId,
    xkb::{KeyboardState, Xkb},
};

const CONFIG_DIR_NAME: &str = "tema";
const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_CURSOR_THEME: &str = "default";
const DEFAULT_CURSOR_SIZE: u32 = 24;
const INOTIFY_BUFFER_SIZE: usize = 4096;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputScaleSetting {
    Auto,
    Fixed(FractionalScale),
}

/// `srgb` is the IEC 61966-2-1 piecewise transfer, `compound_power_2_4`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputColorTransfer {
    Gamma22,
    Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputColorPrimaries {
    Srgb,
    Edid,
}

/// Per-connector color description. SDR minimum luminance stays 0.2 cd/m².
#[derive(Clone, Copy, Debug)]
pub struct OutputColorConfig {
    pub tf: OutputColorTransfer,
    pub primaries: OutputColorPrimaries,
    /// `hdr` asks for a PQ BT.2020 output. It stays SDR unless the EDID and the
    /// connector both allow it.
    pub mode: OutputColorMode,
    /// Reference white, in cd/m². An omitted value is 203 while HDR is active
    /// and 80 otherwise.
    pub sdr_white: f64,
    /// `false` when the file omitted `sdr_white`, so a rejected HDR request
    /// keeps the SDR default of 80.
    pub sdr_white_explicit: bool,
    /// `8` keeps `XRGB8888`. `10` prefers `XRGB2101010`, then `XBGR2101010`.
    /// An active HDR mode prefers those 10-bit formats too.
    pub bitdepth: u8,
    /// Replaces the EDID minimum luminance when set.
    pub min_luminance: Option<f64>,
    /// Replaces the EDID maximum luminance when set.
    pub max_luminance: Option<f64>,
    /// Replaces the EDID maximum frame-average luminance when set.
    pub max_fall: Option<f64>,
}

/// `mode` in `[output.color]`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OutputColorMode {
    Sdr,
    Hdr,
}

impl OutputColorConfig {
    pub const DEFAULT: Self = Self {
        tf: OutputColorTransfer::Gamma22,
        primaries: OutputColorPrimaries::Srgb,
        mode: OutputColorMode::Sdr,
        sdr_white: 80.0,
        sdr_white_explicit: false,
        bitdepth: 8,
        min_luminance: None,
        max_luminance: None,
        max_fall: None,
    };

    /// Reference white actually used. A rejected HDR request does not inherit
    /// the implicit 203 cd/m² default.
    pub fn reference_white(self, hdr_active: bool) -> f64 {
        if hdr_active || self.mode == OutputColorMode::Sdr || self.sdr_white_explicit {
            self.sdr_white
        } else {
            80.0
        }
    }
}

/// A 10-bit scanout buffer is requested by `bitdepth = 10` or by an active HDR mode.
///
/// `hdr_active` is true only after the EDID and connector checks pass.
pub fn ten_bit_scanout(color: &OutputColorConfig, hdr_active: bool) -> bool {
    hdr_active || color.bitdepth == 10
}

impl PartialEq for OutputColorConfig {
    fn eq(&self, other: &Self) -> bool {
        self.tf == other.tf
            && self.primaries == other.primaries
            && self.mode == other.mode
            && self.sdr_white.to_bits() == other.sdr_white.to_bits()
            && self.sdr_white_explicit == other.sdr_white_explicit
            && self.bitdepth == other.bitdepth
            && optional_bits(self.min_luminance) == optional_bits(other.min_luminance)
            && optional_bits(self.max_luminance) == optional_bits(other.max_luminance)
            && optional_bits(self.max_fall) == optional_bits(other.max_fall)
    }
}

impl Eq for OutputColorConfig {}

fn optional_bits(value: Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    path: PathBuf,
    pub general: GeneralConfig,
    pub xwayland: XwaylandConfig,
    pub session_management: SessionManagementConfig,
    pub tiling: TilingConfig,
    pub decorations: DecorationsConfig,
    pub effects: EffectsConfig,
    pub animations: AnimationsConfig,
    pub bindings: Vec<Binding>,
    pub mouse_bindings: Vec<MouseBinding>,
    pub gesture_bindings: Vec<GestureBinding>,
    output_scales: BTreeMap<String, OutputScaleSetting>,
    output_tearing: BTreeMap<String, bool>,
    output_color: BTreeMap<String, OutputColorConfig>,
    pub color_management: ColorManagementConfig,
}

/// `wp_color_manager_v1` is advertised when `enabled` is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorManagementConfig {
    pub enabled: bool,
}

impl Default for ColorManagementConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererSelection {
    OpenGl,
    Auto,
    Vulkan,
}

impl fmt::Display for RendererSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OpenGl => "opengl",
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
        })
    }
}

impl Default for RendererSelection {
    fn default() -> Self {
        Self::OpenGl
    }
}

/// How Xwayland windows are rendered on HiDPI outputs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XwaylandScalingMode {
    /// X apps render at logical resolution and the compositor upscales them.
    #[default]
    Default,
    /// X apps render at the full output resolution and the compositor downscales them.
    Downscaled,
}

impl fmt::Display for XwaylandScalingMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Default => "default",
            Self::Downscaled => "downscaled",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XwaylandConfig {
    pub enabled: bool,
    pub scaling_mode: XwaylandScalingMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionManagementConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneralConfig {
    pub cursor_theme: String,
    pub cursor_size: u32,
    pub outer_gap: i32,
    pub inner_gap: i32,
    pub renderer: RendererSelection,
    pub import_environment: bool,
    pub allow_tearing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TilingConfig {
    pub drag: TilingDragConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TilingDragConfig {
    /// Logical pixels the pointer must move before a tiled window is picked up.
    /// Zero picks the window up on the first motion, matching Hyprland's default.
    pub threshold: i32,
    /// A wide or tall destination tile chooses the split axis when this is false.
    /// The pointer quadrant chooses left, right, top, or bottom when this is true.
    pub precise_mouse_move: bool,
    /// Hyprland dwindle ratio. `1.0` is an even split, clamped to `0.1..=1.9`.
    pub split_ratio: SplitRatio,
}

#[derive(Clone, Copy, Debug, PartialOrd)]
pub struct SplitRatio(f32);

impl SplitRatio {
    pub const DEFAULT: Self = Self(1.0);
    pub const MIN: f32 = 0.1;
    pub const MAX: f32 = 1.9;

    pub fn sanitize(raw: f32) -> Self {
        if raw.is_finite() {
            Self(raw.clamp(Self::MIN, Self::MAX))
        } else {
            Self::DEFAULT
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for SplitRatio {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PartialEq for SplitRatio {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for SplitRatio {}

/// Default `decorations.resize_grab_extent`, in logical pixels.
pub const RESIZE_GRAB_EXTENT_DEFAULT: i32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationsConfig {
    pub rounding: i32,
    pub rounding_power: RoundingPower,
    /// Logical pixels of resize grab outside a floating frame. Tiled handles use it as
    /// their minimum span. Read at use time so a reload applies on the next pick.
    pub resize_grab_extent: i32,
    pub border: BorderDecorationConfig,
    pub shadow: ShadowDecorationConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderDecorationConfig {
    /// Logical pixels reserved on every edge, and the width of the drawn border.
    /// Zero reserves nothing and draws no border.
    pub size: i32,
    pub active_start_color: RgbaColor,
    pub active_end_color: RgbaColor,
    pub inactive_start_color: RgbaColor,
    pub inactive_end_color: RgbaColor,
    pub gradient_angle_degrees: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowDecorationConfig {
    pub radius: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub color: RgbaColor,
    pub power: ShadowPower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectsConfig {
    pub blur: BlurConfig,
    pub opacity: OpacityConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlurConfig {
    pub enabled: bool,
    pub radius: i32,
    pub passes: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpacityConfig {
    pub active_alpha: u8,
    pub inactive_alpha: u8,
    pub inactive_dim: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecorationInsets {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Clone, Copy, Debug, PartialOrd)]
pub struct RoundingPower(f32);

#[derive(Clone, Copy, Debug, PartialOrd)]
pub struct ShadowPower(f32);

impl RoundingPower {
    pub const DEFAULT: Self = Self(2.0);

    pub fn sanitize(raw: f32) -> Self {
        if raw.is_finite() && raw > 0.0 {
            Self(raw)
        } else {
            Self::DEFAULT
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for RoundingPower {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PartialEq for RoundingPower {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for RoundingPower {}

impl ShadowPower {
    pub const DEFAULT: Self = Self(2.0);

    pub fn sanitize(raw: f32) -> Self {
        if raw.is_finite() && raw > 0.0 {
            Self(raw)
        } else {
            Self::DEFAULT
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for ShadowPower {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PartialEq for ShadowPower {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for ShadowPower {}

impl RgbaColor {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn to_rgba_bytes(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn to_premultiplied_rgba_bytes(self) -> [u8; 4] {
        let alpha = self.a as u16;
        let premultiply = |channel: u8| -> u8 { (((channel as u16) * alpha + 127) / 255) as u8 };
        [premultiply(self.r), premultiply(self.g), premultiply(self.b), self.a]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub trigger: KeyBinding,
    pub action: CompositorAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MouseBinding {
    pub trigger: MouseBindingTrigger,
    pub action: MouseBindingAction,
}

/// Swipe direction of a `[gestures]` binding. `Up` is toward the top of the touchpad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GestureBinding {
    /// The key as written in the config, e.g. `swipe:3:left`.
    pub description: String,
    pub fingers: u32,
    pub direction: SwipeDirection,
    pub action: CompositorAction,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyBinding {
    pub description: String,
    pub keysym: xkb::Keysym,
    pub modifiers: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MouseBindingTrigger {
    pub description: String,
    pub button: u32,
    pub modifiers: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompositorAction {
    Spawn {
        command: String,
    },
    Quit,
    ReloadConfig,
    WorkspaceNext,
    WorkspacePrevious,
    Close,
    FocusNext,
    FocusPrevious,
    ToggleFloating,
    ToggleFullscreen,
    /// Flip the keyboard-shortcuts inhibitor of the focused surface. Never inhibited.
    ToggleShortcutsInhibit,
    /// One-based workspace on the active output's group.
    WorkspaceIndex {
        index: u32,
    },
    /// One-based destination workspace on the active output's group.
    MoveToWorkspace {
        index: u32,
    },
}

/// Tagged binding table. The TOML shape stays `{ action = "...", ... }`. A struct is stricter
/// than serde's internally tagged enums, which ignore fields the selected variant does not
/// declare.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionFile {
    action: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    index: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum MouseBindingAction {
    MoveWindow,
    ResizeWindow,
}

#[derive(Debug)]
pub struct ConfigWatcher {
    inotify: Inotify,
    event_buffer: Box<[u8]>,
    poll_op_id: Option<RingId>,
    watches: HashMap<WatchDescriptor, ConfigWatchSlot>,
    stopped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigWatchSlot {
    /// `config_path` without following symlinks (Hyprland-style `IN_DONT_FOLLOW`).
    LogicalFile,
    /// Symlink target from `canonicalize`, when distinct from the logical path.
    ResolvedTarget,
    /// Ancestor directory used to observe create/delete/rename of the config entry.
    ParentDirectory,
}

#[derive(Debug)]
pub enum ConfigWatcherAction {
    Start,
    Event,
    Failed { error: std::io::Error },
    Stop,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    general: GeneralConfigFile,
    xwayland: XwaylandConfigFile,
    session_management: SessionManagementConfigFile,
    tiling: TilingConfigFile,
    decorations: DecorationsConfigFile,
    effects: EffectsConfigFile,
    animations: AnimationsConfigFile,
    bindings: BTreeMap<String, ActionFile>,
    mouse_bindings: BTreeMap<String, MouseBindingAction>,
    gestures: BTreeMap<String, ActionFile>,
    output: OutputConfigFile,
    color_management: ColorManagementConfigFile,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ColorManagementConfigFile {
    enabled: bool,
}

impl Default for ColorManagementConfigFile {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TilingConfigFile {
    drag: TilingDragConfigFile,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TilingDragConfigFile {
    threshold: i32,
    precise_mouse_move: bool,
    split_ratio: f32,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DecorationsConfigFile {
    rounding: i32,
    rounding_power: f32,
    resize_grab_extent: i32,
    border: BorderDecorationConfigFile,
    shadow: ShadowDecorationConfigFile,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BorderDecorationConfigFile {
    size: i32,
    active_start_color: String,
    active_end_color: String,
    inactive_start_color: String,
    inactive_end_color: String,
    gradient_angle_degrees: i32,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ShadowDecorationConfigFile {
    radius: i32,
    offset_x: i32,
    offset_y: i32,
    color: String,
    power: f32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EffectsConfigFile {
    blur: BlurConfigFile,
    opacity: OpacityConfigFile,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BlurConfigFile {
    enabled: bool,
    radius: i32,
    passes: u8,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpacityConfigFile {
    active_alpha: u8,
    inactive_alpha: u8,
    inactive_dim: u8,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OutputConfigFile {
    #[serde(default)]
    scales: BTreeMap<String, OutputScaleSettingFile>,
    #[serde(default)]
    tearing: BTreeMap<String, bool>,
    #[serde(default)]
    color: BTreeMap<String, OutputColorFile>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OutputColorFile {
    tf: String,
    primaries: String,
    mode: String,
    sdr_white: Option<f64>,
    #[serde(default = "default_output_bitdepth")]
    bitdepth: u8,
    min_luminance: Option<f64>,
    max_luminance: Option<f64>,
    max_fall: Option<f64>,
}

fn default_output_bitdepth() -> u8 {
    8
}

impl Default for OutputColorFile {
    fn default() -> Self {
        Self {
            tf: "gamma22".to_string(),
            primaries: "srgb".to_string(),
            mode: "sdr".to_string(),
            sdr_white: None,
            bitdepth: default_output_bitdepth(),
            min_luminance: None,
            max_luminance: None,
            max_fall: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OutputScaleSettingFile {
    Text(String),
    Number(f64),
}

fn validate_luminance_override(
    name: &str,
    key: &str,
    value: Option<f64>,
    allow_zero: bool,
) -> anyhow::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let ok = value.is_finite() && if allow_zero { value >= 0.0 } else { value > 0.0 };
    if ok {
        Ok(())
    } else {
        bail!("invalid output color {key} for `{name}`")
    }
}

fn output_colors_from_file(
    parsed: BTreeMap<String, OutputColorFile>,
) -> anyhow::Result<BTreeMap<String, OutputColorConfig>> {
    let mut output_color = BTreeMap::new();
    for (name, value) in parsed {
        let tf = match value.tf.as_str() {
            "gamma22" => OutputColorTransfer::Gamma22,
            "srgb" => OutputColorTransfer::Srgb,
            _ => bail!("invalid output color tf for `{name}`; expected \"gamma22\" or \"srgb\""),
        };
        let primaries = match value.primaries.as_str() {
            "srgb" => OutputColorPrimaries::Srgb,
            "edid" => OutputColorPrimaries::Edid,
            _ => {
                bail!("invalid output color primaries for `{name}`; expected \"srgb\" or \"edid\"")
            }
        };
        let mode = match value.mode.as_str() {
            "sdr" => OutputColorMode::Sdr,
            "hdr" => OutputColorMode::Hdr,
            _ => bail!("invalid output color mode for `{name}`; expected \"sdr\" or \"hdr\""),
        };
        let (sdr_white, sdr_white_explicit) = match value.sdr_white {
            Some(white) => {
                if !white.is_finite() || white <= 0.2 {
                    bail!(
                        "invalid output color sdr_white for `{name}`; expected a value greater than 0.2"
                    );
                }
                (white, true)
            }
            None if mode == OutputColorMode::Hdr => (crate::color::hdr::REFERENCE_WHITE, false),
            None => (80.0, false),
        };
        if value.bitdepth != 8 && value.bitdepth != 10 {
            bail!("invalid output color bitdepth for `{name}`; expected 8 or 10");
        }
        validate_luminance_override(&name, "min_luminance", value.min_luminance, true)?;
        validate_luminance_override(&name, "max_luminance", value.max_luminance, false)?;
        validate_luminance_override(&name, "max_fall", value.max_fall, false)?;
        if let (Some(min), Some(max)) = (value.min_luminance, value.max_luminance) {
            if max <= min {
                bail!("invalid output color luminance for `{name}`; max_luminance must exceed min_luminance");
            }
        }
        if let (Some(max), Some(fall)) = (value.max_luminance, value.max_fall) {
            if fall > max {
                bail!(
                    "invalid output color max_fall for `{name}`; it must not exceed max_luminance"
                );
            }
        }
        output_color.insert(
            name,
            OutputColorConfig {
                tf,
                primaries,
                mode,
                sdr_white,
                sdr_white_explicit,
                bitdepth: value.bitdepth,
                min_luminance: value.min_luminance,
                max_luminance: value.max_luminance,
                max_fall: value.max_fall,
            },
        );
    }
    Ok(output_color)
}

fn output_scale_setting_from_file(
    value: OutputScaleSettingFile,
) -> anyhow::Result<OutputScaleSetting> {
    match value {
        OutputScaleSettingFile::Text(s) => {
            if s.eq_ignore_ascii_case("auto") {
                Ok(OutputScaleSetting::Auto)
            } else {
                let f: f64 = s
                    .parse()
                    .with_context(|| format!("invalid output scale `{s}`"))?;
                output_scale_setting_from_f64(f)
            }
        }
        OutputScaleSettingFile::Number(n) => output_scale_setting_from_f64(n),
    }
}

fn output_scale_setting_from_f64(scale: f64) -> anyhow::Result<OutputScaleSetting> {
    let Some(scale) = FractionalScale::try_from_f64(scale) else {
        bail!("output scale must be finite and positive");
    };
    Ok(OutputScaleSetting::Fixed(scale))
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct XwaylandConfigFile {
    enabled: bool,
    scaling_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SessionManagementConfigFile {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GeneralConfigFile {
    cursor_theme: String,
    cursor_size: u32,
    outer_gap: i32,
    inner_gap: i32,
    renderer: String,
    import_environment: bool,
    allow_tearing: bool,
}

impl RuntimeConfig {
    pub fn load_initial(xkb: &Xkb) -> Self {
        let path = default_config_path();
        match Self::load_from_path(&path, xkb) {
            Ok(config) => config,
            Err(error) => {
                log::warn!(
                    "failed to load config from {}: {:#}. Using defaults.",
                    path.display(),
                    error
                );
                Self::default()
            }
        }
    }

    fn load_from_path(path: &Path, xkb: &Xkb) -> anyhow::Result<Self> {
        let file = match fs::read_to_string(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                log::info!("config file {} does not exist, using defaults", path.display());
                return Ok(Self::defaults_with_path(path.to_path_buf()));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read config file {}", path.display()));
            }
        };

        let parsed: ConfigFile =
            toml::from_str(&file).with_context(|| format!("failed to parse {}", path.display()))?;
        Self::compile(path.to_path_buf(), parsed, xkb)
    }

    fn compile(path: PathBuf, parsed: ConfigFile, xkb: &Xkb) -> anyhow::Result<Self> {
        let mut bindings = Vec::with_capacity(parsed.bindings.len());
        let mut seen = BTreeSet::new();
        for (description, action) in parsed.bindings {
            let trigger = parse_binding(&description, xkb.keymap())
                .with_context(|| format!("invalid binding `{description}`"))?;
            if !seen.insert((trigger.keysym, trigger.modifiers)) {
                bail!("duplicate binding for `{description}`");
            }
            let action = compositor_action_from_file(&description, action)?;
            bindings.push(Binding { trigger, action });
        }

        let mut mouse_bindings = Vec::with_capacity(parsed.mouse_bindings.len());
        let mut seen_mouse = BTreeSet::new();
        for (description, action) in parsed.mouse_bindings {
            let trigger = parse_mouse_binding(&description, xkb.keymap())
                .with_context(|| format!("invalid mouse binding `{description}`"))?;
            if !seen_mouse.insert((trigger.button, trigger.modifiers)) {
                bail!("duplicate mouse binding for `{description}`");
            }
            mouse_bindings.push(MouseBinding { trigger, action });
        }

        let mut gesture_bindings = Vec::with_capacity(parsed.gestures.len());
        let mut seen_gestures = BTreeSet::new();
        for (description, action) in parsed.gestures {
            let (fingers, direction) = parse_gesture_binding(&description)?;
            if !seen_gestures.insert((fingers, direction)) {
                bail!("duplicate gesture binding for `{description}`");
            }
            let action = compositor_action_from_file(&description, action)?;
            gesture_bindings.push(GestureBinding { description, fingers, direction, action });
        }

        let mut output_scales = BTreeMap::new();
        for (name, value) in parsed.output.scales {
            let setting = output_scale_setting_from_file(value)
                .with_context(|| format!("invalid output scale for `{name}`"))?;
            output_scales.insert(name, setting);
        }

        let border = BorderDecorationConfig {
            size: sanitize_non_negative_i32(parsed.decorations.border.size),
            active_start_color: parse_rgba_color(&parsed.decorations.border.active_start_color)
                .with_context(|| "invalid decorations.border.active_start_color")?,
            active_end_color: parse_rgba_color(&parsed.decorations.border.active_end_color)
                .with_context(|| "invalid decorations.border.active_end_color")?,
            inactive_start_color: parse_rgba_color(&parsed.decorations.border.inactive_start_color)
                .with_context(|| "invalid decorations.border.inactive_start_color")?,
            inactive_end_color: parse_rgba_color(&parsed.decorations.border.inactive_end_color)
                .with_context(|| "invalid decorations.border.inactive_end_color")?,
            gradient_angle_degrees: parsed.decorations.border.gradient_angle_degrees,
        };
        let shadow = ShadowDecorationConfig {
            radius: sanitize_non_negative_i32(parsed.decorations.shadow.radius),
            offset_x: parsed.decorations.shadow.offset_x,
            offset_y: parsed.decorations.shadow.offset_y,
            color: parse_rgba_color(&parsed.decorations.shadow.color)
                .with_context(|| "invalid decorations.shadow.color")?,
            power: ShadowPower::sanitize(parsed.decorations.shadow.power),
        };
        let effects = EffectsConfig {
            blur: BlurConfig {
                enabled: parsed.effects.blur.enabled,
                radius: sanitize_non_negative_i32(parsed.effects.blur.radius),
                passes: parsed.effects.blur.passes.max(1),
            },
            opacity: OpacityConfig {
                active_alpha: parsed.effects.opacity.active_alpha,
                inactive_alpha: parsed.effects.opacity.inactive_alpha,
                inactive_dim: parsed.effects.opacity.inactive_dim,
            },
        };
        let animations = compile_animations(parsed.animations)?;

        Ok(Self {
            path,
            general: GeneralConfig {
                cursor_theme: sanitize_cursor_theme(parsed.general.cursor_theme),
                cursor_size: sanitize_cursor_size(parsed.general.cursor_size),
                outer_gap: sanitize_non_negative_i32(parsed.general.outer_gap),
                inner_gap: sanitize_non_negative_i32(parsed.general.inner_gap),
                renderer: parse_renderer(&parsed.general.renderer)?,
                import_environment: parsed.general.import_environment,
                allow_tearing: parsed.general.allow_tearing,
            },
            xwayland: XwaylandConfig {
                enabled: parsed.xwayland.enabled,
                scaling_mode: parse_xwayland_scaling_mode(&parsed.xwayland.scaling_mode)?,
            },
            session_management: SessionManagementConfig {
                enabled: parsed.session_management.enabled,
            },
            tiling: TilingConfig {
                drag: TilingDragConfig {
                    threshold: sanitize_non_negative_i32(parsed.tiling.drag.threshold),
                    precise_mouse_move: parsed.tiling.drag.precise_mouse_move,
                    split_ratio: SplitRatio::sanitize(parsed.tiling.drag.split_ratio),
                },
            },
            decorations: DecorationsConfig {
                rounding: sanitize_non_negative_i32(parsed.decorations.rounding),
                rounding_power: RoundingPower::sanitize(parsed.decorations.rounding_power),
                resize_grab_extent: sanitize_non_negative_i32(
                    parsed.decorations.resize_grab_extent,
                ),
                border,
                shadow,
            },
            effects,
            animations,
            bindings,
            mouse_bindings,
            gesture_bindings,
            output_scales,
            output_tearing: parsed.output.tearing,
            output_color: output_colors_from_file(parsed.output.color)?,
            color_management: ColorManagementConfig { enabled: parsed.color_management.enabled },
        })
    }

    fn defaults_with_path(path: PathBuf) -> Self {
        Self {
            path,
            general: GeneralConfig {
                cursor_theme: DEFAULT_CURSOR_THEME.to_string(),
                cursor_size: DEFAULT_CURSOR_SIZE,
                outer_gap: 0,
                inner_gap: 0,
                renderer: RendererSelection::OpenGl,
                import_environment: GeneralConfig::default_import_environment(),
                allow_tearing: true,
            },
            xwayland: XwaylandConfig::default(),
            session_management: SessionManagementConfig::default(),
            tiling: TilingConfig::default(),
            decorations: DecorationsConfig::default(),
            effects: EffectsConfig::default(),
            animations: AnimationsConfig::default(),
            bindings: Vec::new(),
            mouse_bindings: Vec::new(),
            gesture_bindings: Vec::new(),
            output_scales: BTreeMap::new(),
            output_tearing: BTreeMap::new(),
            output_color: BTreeMap::new(),
            color_management: ColorManagementConfig::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn output_scale(&self, output_name: &str) -> Option<OutputScaleSetting> {
        self.output_scales.get(output_name).copied()
    }

    /// Per-output override when present, otherwise `general.allow_tearing`.
    /// `output_name` is the connector name, the same key `[output.scales]` uses.
    pub fn output_allows_tearing(&self, output_name: &str) -> bool {
        self.output_tearing
            .get(output_name)
            .copied()
            .unwrap_or(self.general.allow_tearing)
    }

    /// Per-connector SDR description. A missing connector uses [`OutputColorConfig::DEFAULT`].
    /// Connector names are case-sensitive.
    pub fn output_color(&self, output_name: &str) -> OutputColorConfig {
        self.output_color
            .get(output_name)
            .copied()
            .unwrap_or(OutputColorConfig::DEFAULT)
    }

    pub fn binding_for_key(&self, keyboard: &KeyboardState, key: u32) -> Option<CompositorAction> {
        let keysym = keyboard.key_get_one_sym(key);
        if keysym.raw() == xkb::keysyms::KEY_NoSymbol {
            return None;
        }
        let modifiers = keyboard.effective_modifiers();
        self.bindings
            .iter()
            .find(|binding| binding.trigger.matches(keysym, modifiers))
            .map(|binding| binding.action.clone())
    }

    pub fn binding_for_pointer_button(
        &self,
        keyboard: &KeyboardState,
        button: u32,
    ) -> Option<MouseBindingAction> {
        let modifiers = keyboard.effective_modifiers();
        self.mouse_bindings
            .iter()
            .find(|binding| binding.trigger.matches(button, modifiers))
            .map(|binding| binding.action)
    }

    /// A swipe with this many fingers is reserved by tema.
    pub fn swipe_fingers_bound(&self, fingers: u32) -> bool {
        self.gesture_bindings
            .iter()
            .any(|binding| binding.fingers == fingers)
    }

    pub fn binding_for_swipe(
        &self,
        fingers: u32,
        direction: SwipeDirection,
    ) -> Option<CompositorAction> {
        self.gesture_bindings
            .iter()
            .find(|binding| binding.fingers == fingers && binding.direction == direction)
            .map(|binding| binding.action.clone())
    }

    #[cfg(test)]
    pub(crate) fn parse_for_test(input: &str, xkb: &Xkb) -> anyhow::Result<Self> {
        let parsed: ConfigFile = toml::from_str(input)?;
        Self::compile(PathBuf::from("config.toml"), parsed, xkb)
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::defaults_with_path(default_config_path())
    }
}

impl Default for TilingConfig {
    fn default() -> Self {
        Self { drag: TilingDragConfig::default() }
    }
}

impl Default for TilingDragConfig {
    fn default() -> Self {
        Self {
            threshold: 0,
            precise_mouse_move: false,
            split_ratio: SplitRatio::DEFAULT,
        }
    }
}

impl Default for TilingDragConfigFile {
    fn default() -> Self {
        let defaults = TilingDragConfig::default();
        Self {
            threshold: defaults.threshold,
            precise_mouse_move: defaults.precise_mouse_move,
            split_ratio: defaults.split_ratio.get(),
        }
    }
}

impl GeneralConfig {
    pub fn default_cursor_theme() -> String {
        DEFAULT_CURSOR_THEME.to_string()
    }

    pub fn default_cursor_size() -> u32 {
        DEFAULT_CURSOR_SIZE
    }

    pub fn default_outer_gap() -> i32 {
        0
    }

    pub fn default_inner_gap() -> i32 {
        0
    }

    pub fn default_import_environment() -> bool {
        true
    }

    pub fn default_allow_tearing() -> bool {
        true
    }
}

impl Default for DecorationsConfig {
    fn default() -> Self {
        Self {
            rounding: 0,
            rounding_power: RoundingPower::DEFAULT,
            resize_grab_extent: RESIZE_GRAB_EXTENT_DEFAULT,
            border: BorderDecorationConfig::default(),
            shadow: ShadowDecorationConfig::default(),
        }
    }
}

impl Default for BorderDecorationConfig {
    fn default() -> Self {
        Self {
            size: 1,
            active_start_color: RgbaColor::new(0x50, 0x78, 0xc8, 0xff),
            active_end_color: RgbaColor::new(0x6b, 0x9d, 0xf6, 0xff),
            inactive_start_color: RgbaColor::new(0x33, 0x38, 0x44, 0xff),
            inactive_end_color: RgbaColor::new(0x43, 0x48, 0x54, 0xff),
            gradient_angle_degrees: 0,
        }
    }
}

impl Default for ShadowDecorationConfig {
    fn default() -> Self {
        Self {
            radius: 0,
            offset_x: 0,
            offset_y: 8,
            color: RgbaColor::new(0, 0, 0, 0x90),
            power: ShadowPower::DEFAULT,
        }
    }
}

impl Default for EffectsConfig {
    fn default() -> Self {
        Self {
            blur: BlurConfig::default(),
            opacity: OpacityConfig::default(),
        }
    }
}

impl Default for BlurConfig {
    fn default() -> Self {
        Self { enabled: false, radius: 12, passes: 2 }
    }
}

impl Default for OpacityConfig {
    fn default() -> Self {
        Self { active_alpha: u8::MAX, inactive_alpha: 230, inactive_dim: 24 }
    }
}

impl KeyBinding {
    fn matches(&self, keysym: xkb::Keysym, modifiers: u32) -> bool {
        self.keysym == keysym && self.modifiers == modifiers
    }
}

impl MouseBindingTrigger {
    fn matches(&self, button: u32, modifiers: u32) -> bool {
        self.button == button && self.modifiers == modifiers
    }
}

impl Default for XwaylandConfig {
    fn default() -> Self {
        Self { enabled: true, scaling_mode: XwaylandScalingMode::default() }
    }
}

impl Default for XwaylandConfigFile {
    fn default() -> Self {
        let defaults = XwaylandConfig::default();
        Self {
            enabled: defaults.enabled,
            scaling_mode: defaults.scaling_mode.to_string(),
        }
    }
}

impl Default for SessionManagementConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for SessionManagementConfigFile {
    fn default() -> Self {
        let defaults = SessionManagementConfig::default();
        Self { enabled: defaults.enabled }
    }
}

fn parse_xwayland_scaling_mode(raw: &str) -> anyhow::Result<XwaylandScalingMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "default" => Ok(XwaylandScalingMode::Default),
        "downscaled" => Ok(XwaylandScalingMode::Downscaled),
        _ => {
            bail!("invalid xwayland.scaling_mode `{raw}`; expected \"default\" or \"downscaled\"")
        }
    }
}

impl Default for GeneralConfigFile {
    fn default() -> Self {
        Self {
            cursor_theme: GeneralConfig::default_cursor_theme(),
            cursor_size: GeneralConfig::default_cursor_size(),
            outer_gap: GeneralConfig::default_outer_gap(),
            inner_gap: GeneralConfig::default_inner_gap(),
            renderer: RendererSelection::default().to_string(),
            import_environment: GeneralConfig::default_import_environment(),
            allow_tearing: GeneralConfig::default_allow_tearing(),
        }
    }
}

fn parse_renderer(raw: &str) -> anyhow::Result<RendererSelection> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "opengl" => Ok(RendererSelection::OpenGl),
        "auto" => Ok(RendererSelection::Auto),
        "vulkan" => Ok(RendererSelection::Vulkan),
        _ => {
            bail!("invalid general.renderer `{raw}`; expected \"opengl\", \"auto\", or \"vulkan\"")
        }
    }
}

impl Default for DecorationsConfigFile {
    fn default() -> Self {
        let defaults = DecorationsConfig::default();
        Self {
            rounding: defaults.rounding,
            rounding_power: defaults.rounding_power.get(),
            resize_grab_extent: defaults.resize_grab_extent,
            border: BorderDecorationConfigFile::default(),
            shadow: ShadowDecorationConfigFile::default(),
        }
    }
}

impl Default for BorderDecorationConfigFile {
    fn default() -> Self {
        let defaults = BorderDecorationConfig::default();
        Self {
            size: defaults.size,
            active_start_color: "#5078c8ff".to_string(),
            active_end_color: "#6b9df6ff".to_string(),
            inactive_start_color: "#333844ff".to_string(),
            inactive_end_color: "#434854ff".to_string(),
            gradient_angle_degrees: defaults.gradient_angle_degrees,
        }
    }
}

impl Default for ShadowDecorationConfigFile {
    fn default() -> Self {
        let defaults = ShadowDecorationConfig::default();
        Self {
            radius: defaults.radius,
            offset_x: defaults.offset_x,
            offset_y: defaults.offset_y,
            color: "#00000090".to_string(),
            power: defaults.power.get(),
        }
    }
}

impl Default for BlurConfigFile {
    fn default() -> Self {
        let defaults = BlurConfig::default();
        Self {
            enabled: defaults.enabled,
            radius: defaults.radius,
            passes: defaults.passes,
        }
    }
}

impl Default for OpacityConfigFile {
    fn default() -> Self {
        let defaults = OpacityConfig::default();
        Self {
            active_alpha: defaults.active_alpha,
            inactive_alpha: defaults.inactive_alpha,
            inactive_dim: defaults.inactive_dim,
        }
    }
}

impl ConfigWatcher {
    pub fn try_new(initial_state: &RuntimeConfig) -> std::io::Result<Self> {
        let inotify = Inotify::init()?;
        let mut watcher = Self {
            inotify,
            event_buffer: vec![0; INOTIFY_BUFFER_SIZE].into_boxed_slice(),
            poll_op_id: None,
            watches: HashMap::new(),
            stopped: false,
        };
        watcher.sync_watches(initial_state.path())?;
        Ok(watcher)
    }

    pub fn new(initial_state: &RuntimeConfig) -> Self {
        Self::try_new(initial_state).expect("failed to initialize config inotify watcher")
    }

    fn dispatch_poll(&mut self, dispatcher: &mut Dispatcher) {
        if self.stopped || self.poll_op_id.is_some() {
            return;
        }
        let poll_op_id = RingId::fresh();
        self.poll_op_id = Some(poll_op_id);
        dispatcher.dispatch(IoUringAction::Poll {
            id: Some(poll_op_id),
            fd: self.inotify.as_raw_fd(),
            dispatch_to: Redispatcher::to::<Self>(|result: std::io::Result<PollCompleted>| {
                match result {
                    Ok(_) => ConfigWatcherAction::Event,
                    Err(error) => ConfigWatcherAction::Failed { error },
                }
            }),
        });
    }

    /// Drop every watch and subscribe again from `config_path` (logical path).
    fn sync_watches(&mut self, config_path: &Path) -> std::io::Result<()> {
        for descriptor in self.watches.keys().cloned().collect::<Vec<_>>() {
            let _ = self.inotify.watches().remove(descriptor);
        }
        self.watches.clear();

        let Some(config_dir) = config_path.parent() else {
            return Ok(());
        };
        let parent_watch_path = nearest_existing_directory(config_dir);
        let parent_mask = WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF
            | WatchMask::ATTRIB;
        let descriptor = self
            .inotify
            .watches()
            .add(&parent_watch_path, parent_mask)?;
        self.watches
            .insert(descriptor, ConfigWatchSlot::ParentDirectory);

        let Ok(meta) = fs::symlink_metadata(config_path) else {
            return Ok(());
        };

        let logical_mask = WatchMask::MODIFY
            | WatchMask::CLOSE_WRITE
            | WatchMask::ATTRIB
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF
            | WatchMask::DONT_FOLLOW;
        let descriptor = self.inotify.watches().add(config_path, logical_mask)?;
        self.watches
            .insert(descriptor, ConfigWatchSlot::LogicalFile);

        if meta.file_type().is_symlink() {
            if let Ok(canonical) = fs::canonicalize(config_path) {
                if canonical != config_path {
                    let target_mask = WatchMask::MODIFY
                        | WatchMask::CLOSE_WRITE
                        | WatchMask::ATTRIB
                        | WatchMask::DELETE_SELF
                        | WatchMask::MOVE_SELF;
                    let descriptor = self.inotify.watches().add(&canonical, target_mask)?;
                    self.watches
                        .insert(descriptor, ConfigWatchSlot::ResolvedTarget);
                }
            }
        }

        Ok(())
    }

    fn on_event(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        let config_path = state.config.path().to_path_buf();
        let config_file_name = config_path.file_name().map(|name| name.to_owned());

        let mut should_reload = false;
        let mut should_resync = false;

        loop {
            match self.inotify.read_events(self.event_buffer.as_mut()) {
                Ok(events) => {
                    for event in events {
                        if event.mask.contains(EventMask::Q_OVERFLOW) {
                            should_reload = true;
                            should_resync = true;
                            continue;
                        }

                        if event.mask.contains(EventMask::IGNORED) {
                            if !self.watches.contains_key(&event.wd) {
                                continue;
                            }
                            should_resync = true;
                            continue;
                        }

                        let Some(slot) = self.watches.get(&event.wd).copied() else {
                            continue;
                        };

                        match slot {
                            ConfigWatchSlot::LogicalFile | ConfigWatchSlot::ResolvedTarget => {
                                if event.mask.intersects(
                                    EventMask::MODIFY
                                        | EventMask::CLOSE_WRITE
                                        | EventMask::ATTRIB
                                        | EventMask::DELETE_SELF
                                        | EventMask::MOVE_SELF,
                                ) {
                                    should_reload = true;
                                }
                                if event
                                    .mask
                                    .intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF)
                                {
                                    should_resync = true;
                                }
                            }
                            ConfigWatchSlot::ParentDirectory => {
                                if parent_directory_event_affects_config(
                                    &event,
                                    config_file_name.as_deref(),
                                ) {
                                    should_reload = true;
                                    should_resync = true;
                                }
                            }
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => {
                    log::warn!("failed to read config watcher events: {:?}", error);
                    break;
                }
            }
        }

        if should_resync {
            if let Err(error) = self.sync_watches(&config_path) {
                log::warn!("failed to resync config watcher: {:?}", error);
            }
        }

        if should_reload {
            if let Err(error) = reload_from_disk(state, dispatcher, ReloadTrigger::AutomaticWatch) {
                log::warn!("failed to reload config after inotify event: {:#}", error);
            }
        }
    }
}

#[cfg(test)]
impl ConfigWatcher {
    fn watch_slot_count(&self) -> usize {
        self.watches.len()
    }
}

impl Model for ConfigWatcher {
    type Action = ConfigWatcherAction;
    const START: Option<Self::Action> = Some(ConfigWatcherAction::Start);
    const STOP: Option<Self::Action> = Some(ConfigWatcherAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        match action {
            ConfigWatcherAction::Start if !self.stopped => {
                self.dispatch_poll(dispatcher);
            }
            ConfigWatcherAction::Event if !self.stopped => {
                self.on_event(state, dispatcher);
            }
            ConfigWatcherAction::Failed { error } if !self.stopped => {
                self.poll_op_id = None;
                log::warn!("config watcher poll failed: {:?}", error);
                self.dispatch_poll(dispatcher);
            }
            ConfigWatcherAction::Stop => {
                self.stopped = true;
                if let Some(poll_op_id) = self.poll_op_id.take() {
                    dispatcher.dispatch(IoUringAction::PollRemove { target: poll_op_id });
                }
                for descriptor in self.watches.keys().cloned().collect::<Vec<_>>() {
                    let _ = self.inotify.watches().remove(descriptor);
                }
                self.watches.clear();
            }
            ConfigWatcherAction::Start
            | ConfigWatcherAction::Event
            | ConfigWatcherAction::Failed { .. } => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ReloadTrigger {
    ManualBinding,
    AutomaticWatch,
}

pub fn reload_from_disk(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    trigger: ReloadTrigger,
) -> anyhow::Result<bool> {
    let path = state.config.path().to_path_buf();
    let mut next = RuntimeConfig::load_from_path(&path, &state.xkb)?;
    if next.general.renderer != state.config.general.renderer {
        log::warn!(
            "general.renderer is {requested} in {path} and the running renderer is {running}; restart the compositor to change it",
            requested = next.general.renderer,
            path = path.display(),
            running = state.config.general.renderer,
        );
        next.general.renderer = state.config.general.renderer;
    }
    if next.color_management.enabled != state.config.color_management.enabled {
        log::warn!(
            "color_management.enabled is {requested} in {path}; restart the compositor to change it",
            requested = next.color_management.enabled,
            path = path.display(),
        );
        next.color_management.enabled = state.config.color_management.enabled;
    }
    let enabled_changed = next.xwayland.enabled != state.config.xwayland.enabled;
    let scaling_mode_changed = next.xwayland.scaling_mode != state.config.xwayland.scaling_mode;
    let import_changed = next.general.import_environment != state.config.general.import_environment;
    let session_management_changed = next.session_management != state.config.session_management;
    let previous_blur = state.config.effects.blur;
    let changed = state.config != next;
    state.config = next;
    if import_changed {
        let import_environment = state.config.general.import_environment;
        log::info!("general.import_environment changed to {import_environment}");
        dispatcher
            .dispatch(crate::process::ProcessManagerAction::SetExportEnabled(import_environment));
    }
    if enabled_changed {
        let enabled = state.config.xwayland.enabled;
        log::info!("xwayland.enabled changed to {enabled}");
        dispatcher.dispatch(crate::xwayland::XwaylandAction::SetEnabled(enabled));
    }
    if scaling_mode_changed {
        // Ahead of `apply_output_settings`, so `SetWireScale` is queued before
        // the output broadcasts that function may add.
        dispatcher.dispatch(crate::clients::ClientManagerAction::XwaylandWireScaleChanged);
    }
    apply_general_settings(state, dispatcher);
    if session_management_changed {
        crate::session::sync_session_manager_global(state, dispatcher);
    }
    apply_output_settings(state, dispatcher);
    if changed {
        state.mark_all_workspaces_dirty();
        dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
        dispatcher.dispatch(ClientManagerAction::BackgroundEffectCapabilitiesChanged);
        if state.config.effects.blur != previous_blur {
            let rects = state
                .outputs
                .iter()
                .filter_map(|(output_id, _)| state.output_bounds(output_id))
                .collect::<Vec<_>>();
            crate::drm::queue_global_damage(state, &rects);
        }
        request_outputs(state, state.outputs.iter().map(|(output_id, _)| output_id), dispatcher);
        kick_visual_animation_pump(dispatcher);
    }
    let trigger = match trigger {
        ReloadTrigger::ManualBinding => "manual binding",
        ReloadTrigger::AutomaticWatch => "file watch",
    };
    log::info!("reloaded config from {} via {}", path.display(), trigger);
    Ok(changed)
}

pub fn apply_general_settings(state: &mut State, dispatcher: &mut Dispatcher) {
    let general = state.config.general.clone();
    let changed = state
        .seat
        .set_cursor_theme(general.cursor_theme.clone(), general.cursor_size);
    if changed {
        dispatcher.dispatch(crate::xwayland::XwaylandAction::RefreshHints);
    }
    if changed && state.seat.cursor_is_visible() {
        request_outputs_for_points(state, [state.seat.pointer_position()], dispatcher);
    }
}

pub fn apply_output_settings(state: &mut State, dispatcher: &mut Dispatcher) {
    state.refresh_output_colors();
    crate::drm::request_output_color_redraws(state, dispatcher);
    let changed_outputs = state.apply_output_scales_from_config();
    if changed_outputs.is_empty() {
        return;
    }

    request_outputs(state, changed_outputs.iter().copied(), dispatcher);
    for output_id in changed_outputs {
        dispatcher.dispatch(ClientManagerAction::OutputStateChanged { output_id });
    }
}

fn log_spawn(command: &str, event: &ProcessEvent) {
    match event {
        ProcessEvent::Failed { error } => log::warn!("failed to spawn `{command}`: {error}"),
        ProcessEvent::Exited { exit: ChildExit::Code(0) } | ProcessEvent::Spawned { .. } => {}
        ProcessEvent::Exited { exit } => log::debug!("spawn `{command}` exited {exit:?}"),
        // The forker's own death is already logged. One line per child adds nothing.
        ProcessEvent::Lost => {}
    }
}

pub fn execute_action(state: &mut State, dispatcher: &mut Dispatcher, action: CompositorAction) {
    match action {
        CompositorAction::Spawn { command } => {
            let logged = command.clone();
            dispatcher.dispatch(ProcessManagerAction::Spawn(SpawnRequest {
                program: "sh".into(),
                args: vec!["-lc".into(), command],
                env: Vec::new(),
                fds: Vec::new(),
                notify: Some(Redispatcher::to::<ProcessManager>(move |event| {
                    log_spawn(&logged, &event);
                    ProcessManagerAction::Logged
                })),
            }));
        }
        CompositorAction::Quit => {
            log::info!("quitting compositor from keybinding");
            dispatcher.request_shutdown(ShutdownReason::Quit);
        }
        CompositorAction::ReloadConfig => {
            if let Err(error) = reload_from_disk(state, dispatcher, ReloadTrigger::ManualBinding) {
                log::warn!("failed to reload config: {:#}", error);
            }
        }
        CompositorAction::WorkspaceNext => {
            activate_adjacent_workspace(
                state,
                dispatcher,
                crate::visual_animation::WorkspaceTransitionDirection::Next,
            );
        }
        CompositorAction::WorkspacePrevious => {
            activate_adjacent_workspace(
                state,
                dispatcher,
                crate::visual_animation::WorkspaceTransitionDirection::Previous,
            );
        }
        CompositorAction::Close => {
            let Some(window) = require_focused_window(state, "close") else {
                return;
            };
            if !state.request_window_close(window, dispatcher) {
                log::info!("close ignored; focused window has no toplevel");
            }
        }
        CompositorAction::FocusNext => {
            cycle_focused_workspace(state, dispatcher, FocusCycle::Next);
        }
        CompositorAction::FocusPrevious => {
            cycle_focused_workspace(state, dispatcher, FocusCycle::Previous);
        }
        CompositorAction::ToggleFloating => {
            let Some(window) = require_focused_window(state, "toggle-floating") else {
                return;
            };
            let floating = state
                .managed_window_state(window)
                .is_some_and(|managed| managed.mode == ManagedWindowMode::Floating);
            if state.set_window_floating(window, !floating) {
                if let Some(root) = state.window_root_surface(window) {
                    finish_root_mode_change(state, dispatcher, root);
                }
            }
        }
        CompositorAction::ToggleFullscreen => {
            let Some(window) = require_focused_window(state, "toggle-fullscreen") else {
                return;
            };
            let fullscreen = state
                .managed_window_state(window)
                .is_some_and(|managed| managed.fullscreen);
            if state.set_window_fullscreen(window, !fullscreen) {
                if let Some(root) = state.window_root_surface(window) {
                    finish_root_mode_change(state, dispatcher, root);
                }
            }
        }
        CompositorAction::ToggleShortcutsInhibit => {
            crate::wayland_objects::zwp_keyboard_shortcuts_inhibitor_v1::toggle_focused(
                state, dispatcher,
            );
        }
        CompositorAction::WorkspaceIndex { index } => {
            select_workspace_index(state, dispatcher, index);
        }
        CompositorAction::MoveToWorkspace { index } => {
            move_focused_window_to_workspace(state, dispatcher, index);
        }
    }
}

fn compositor_action_from_file(
    description: &str,
    file: ActionFile,
) -> anyhow::Result<CompositorAction> {
    let ActionFile { action, command, index } = file;
    match action.as_str() {
        "spawn" => {
            reject_index(description, &action, index)?;
            let Some(command) = command.filter(|command| !command.is_empty()) else {
                bail!("binding `{description}` action `spawn` requires `command`");
            };
            Ok(CompositorAction::Spawn { command })
        }
        "quit" => unit_action(description, &action, command, index, CompositorAction::Quit),
        "reload-config" => {
            unit_action(description, &action, command, index, CompositorAction::ReloadConfig)
        }
        "workspace-next" => {
            unit_action(description, &action, command, index, CompositorAction::WorkspaceNext)
        }
        "workspace-previous" => {
            unit_action(description, &action, command, index, CompositorAction::WorkspacePrevious)
        }
        "close" => unit_action(description, &action, command, index, CompositorAction::Close),
        "focus-next" => {
            unit_action(description, &action, command, index, CompositorAction::FocusNext)
        }
        "focus-previous" => {
            unit_action(description, &action, command, index, CompositorAction::FocusPrevious)
        }
        "toggle-floating" => {
            unit_action(description, &action, command, index, CompositorAction::ToggleFloating)
        }
        "toggle-fullscreen" => {
            unit_action(description, &action, command, index, CompositorAction::ToggleFullscreen)
        }
        "toggle-shortcuts-inhibit" => unit_action(
            description,
            &action,
            command,
            index,
            CompositorAction::ToggleShortcutsInhibit,
        ),
        "workspace-index" => indexed_action(description, &action, command, index, |index| {
            CompositorAction::WorkspaceIndex { index }
        }),
        "move-to-workspace" => indexed_action(description, &action, command, index, |index| {
            CompositorAction::MoveToWorkspace { index }
        }),
        other => bail!("binding `{description}` has unknown action `{other}`"),
    }
}

fn unit_action(
    description: &str,
    action: &str,
    command: Option<String>,
    index: Option<u32>,
    value: CompositorAction,
) -> anyhow::Result<CompositorAction> {
    reject_command(description, action, command)?;
    reject_index(description, action, index)?;
    Ok(value)
}

fn indexed_action(
    description: &str,
    action: &str,
    command: Option<String>,
    index: Option<u32>,
    build: impl FnOnce(u32) -> CompositorAction,
) -> anyhow::Result<CompositorAction> {
    reject_command(description, action, command)?;
    let Some(index) = index else {
        bail!("binding `{description}` action `{action}` requires `index`");
    };
    if index == 0 {
        bail!("binding `{description}` uses workspace index 0; indices are one-based");
    }
    Ok(build(index))
}

fn reject_command(description: &str, action: &str, command: Option<String>) -> anyhow::Result<()> {
    if command.is_some() {
        bail!("binding `{description}` action `{action}` does not take `command`");
    }
    Ok(())
}

fn reject_index(description: &str, action: &str, index: Option<u32>) -> anyhow::Result<()> {
    if index.is_some() {
        bail!("binding `{description}` action `{action}` does not take `index`");
    }
    Ok(())
}

/// The active output's workspace group. Output choice follows [`State::active_output_id`]:
/// the pointer output, then the keyboard focus, then the fallback output.
pub(crate) struct ActiveWorkspaceGroup {
    pub group_id: WorkspaceGroupId,
    pub workspace_id: WorkspaceId,
}

pub(crate) fn active_workspace_group(state: &State) -> Option<ActiveWorkspaceGroup> {
    let output_id = state.active_output_id()?;
    let group_id = state.workspace_group_for_output(output_id)?;
    let workspace_id = state.active_workspace_for_group(group_id)?;
    Some(ActiveWorkspaceGroup { group_id, workspace_id })
}

/// Window for the keyboard focus, walked up to a workspace-managed root.
///
/// Sway commands use the seat's focused container. A popup or subsurface here resolves to
/// its managed toplevel. Layer-shell focus and a missing focus have no window.
pub(crate) fn focused_managed_root(state: &State) -> Option<WindowId> {
    let focus = state.seat.keyboard_focus()?;
    let root = state.root_surface_for_surface(focus.surface)?;
    let window = state.window_for_root(root)?;
    state
        .managed_window_state(window)
        .is_some()
        .then_some(window)
}

/// One-based index into the active output's workspace list.
///
/// Sway creates a workspace when a binding names one that does not exist. This compositor
/// keeps the output's existing list, so index 0 and an index past that list are absent.
pub(crate) fn workspace_at_index(state: &State, index: u32) -> Option<WorkspaceId> {
    let active = active_workspace_group(state)?;
    let zero_based = usize::try_from(index.checked_sub(1)?).ok()?;
    state
        .workspace_groups
        .try_get(active.group_id)?
        .workspaces
        .get(zero_based)
        .copied()
}

pub(crate) fn require_focused_window(state: &State, action: &str) -> Option<WindowId> {
    let window = focused_managed_root(state);
    if window.is_none() {
        log::info!("{action} ignored; no focused window");
    }
    window
}

pub(crate) fn require_workspace_index(
    state: &State,
    action: &str,
    index: u32,
) -> Option<WorkspaceId> {
    let workspace_id = workspace_at_index(state, index);
    if workspace_id.is_none() {
        log::info!("{action} ignored; workspace index {index} is unavailable");
    }
    workspace_id
}

enum FocusCycle {
    Next,
    Previous,
}

/// Rotate one stacking band of the focused window's workspace.
///
/// Paint order is tiled windows, back to front, then floating windows. Next moves the
/// backmost window to the front of its band, so repeated next walks tiled windows when
/// any exist and otherwise walks the floating stack. Previous moves the frontmost window
/// to the back of its band, so repeated previous walks floating windows when any exist.
/// A floating raise-to-top would bounce between the top two windows, so the whole band
/// rotates. Tiled windows keep their split slots and move through them.
fn cycle_focused_workspace(state: &mut State, dispatcher: &mut Dispatcher, cycle: FocusCycle) {
    let action = match cycle {
        FocusCycle::Next => "focus-next",
        FocusCycle::Previous => "focus-previous",
    };
    let Some(focused) = require_focused_window(state, action) else {
        return;
    };
    let Some(workspace_id) = state
        .managed_window_state(focused)
        .map(|managed| managed.workspace_id)
    else {
        log::info!("{action} ignored; focused window has no workspace");
        return;
    };
    let Some(workspace) = state.workspaces.try_get(workspace_id) else {
        return;
    };
    if workspace.layout.fullscreen.is_some() {
        return;
    }
    let mut tiled = workspace.layout.tiled_windows();
    let mut floating = workspace.layout.floating_windows();
    if tiled.is_empty() && floating.is_empty() {
        log::info!("{action} ignored; no workspace window");
        return;
    }

    let target = match cycle {
        FocusCycle::Next => {
            if !tiled.is_empty() {
                rotate_back_to_front(&mut tiled);
                tiled[tiled.len() - 1]
            } else {
                rotate_back_to_front(&mut floating);
                floating[floating.len() - 1]
            }
        }
        FocusCycle::Previous => {
            if !floating.is_empty() {
                rotate_front_to_back(&mut floating);
                floating[floating.len() - 1]
            } else {
                rotate_front_to_back(&mut tiled);
                tiled[tiled.len() - 1]
            }
        }
    };

    {
        let Some(workspace) = state.workspaces.try_get_mut(workspace_id) else {
            return;
        };
        if !workspace.layout.reorder_tiled_windows(&tiled)
            || !workspace.layout.set_floating_windows(floating)
        {
            return;
        }
        workspace.layout.focus_window(target);
    }
    let Some(target_root) = state.window_root_surface(target) else {
        return;
    };
    if state.seat.keyboard_focus().map(|focus| focus.surface) != Some(target_root) {
        crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    }
    let outputs = state.workspace_output_ids(workspace_id);
    if !outputs.is_empty() {
        request_outputs(state, outputs, dispatcher);
    }
    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
    crate::input::set_keyboard_focus(
        state,
        dispatcher,
        Some(crate::input::SurfaceFocus::new(target_root)),
    );
}

fn rotate_back_to_front<T>(order: &mut Vec<T>) {
    if order.len() < 2 {
        return;
    }
    let backmost = order.remove(0);
    order.push(backmost);
}

fn rotate_front_to_back<T>(order: &mut Vec<T>) {
    if order.len() < 2 {
        return;
    }
    let Some(frontmost) = order.pop() else {
        return;
    };
    order.insert(0, frontmost);
}

fn finish_root_mode_change(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    root: Resolved<WlSurfaceId>,
) {
    let outputs = state
        .root_surface_workspace(root)
        .map(|workspace_id| state.workspace_output_ids(workspace_id))
        .unwrap_or_default();
    if !outputs.is_empty() {
        request_outputs(state, outputs, dispatcher);
    }
    // A keybinding keeps the focused window. `refresh_scene_focus` would replace keyboard
    // focus with whatever is under the pointer.
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
    crate::input::pointer_lock::recheck_pointer_capture(state, dispatcher);
}

fn publish_workspace_group(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    group_id: WorkspaceGroupId,
) {
    let outputs = state.workspace_group_output_ids(group_id);
    publish_workspace_outputs(state, dispatcher, outputs);
}

fn publish_workspace_outputs(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    outputs: impl IntoIterator<Item = crate::state::OutputId>,
) {
    crate::wayland_objects::ext_workspace_manager_v1::publish_workspace_state_changed(
        state, dispatcher, outputs,
    );
    kick_visual_animation_pump(dispatcher);
}

fn select_workspace_index(state: &mut State, dispatcher: &mut Dispatcher, index: u32) {
    let Some(workspace_id) = require_workspace_index(state, "workspace-index", index) else {
        return;
    };
    let Some(group_id) = state
        .workspaces
        .try_get(workspace_id)
        .and_then(|workspace| workspace.group)
    else {
        return;
    };
    // `activate_workspace` is false when the index is already showing. That skips the slide.
    if !state.activate_workspace(workspace_id) {
        return;
    }
    log::info!("workspace index {index} on group {group_id:?}");
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    publish_workspace_group(state, dispatcher, group_id);
}

fn move_focused_window_to_workspace(state: &mut State, dispatcher: &mut Dispatcher, index: u32) {
    let Some(window) = require_focused_window(state, "move-to-workspace") else {
        return;
    };
    let Some(workspace_id) = require_workspace_index(state, "move-to-workspace", index) else {
        return;
    };
    let source_workspace = state
        .managed_window_state(window)
        .map(|managed| managed.workspace_id);
    let already_there = source_workspace == Some(workspace_id);
    if !already_there && !state.assign_window_to_workspace(window, workspace_id) {
        log::info!("move-to-workspace ignored; could not move the focused window");
        return;
    }
    let activated = state.activate_workspace(workspace_id);
    if already_there && !activated {
        return;
    }
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    // Sway's `move container to workspace` can leave the seat on the source workspace.
    // This binding follows the window, then puts keyboard focus back after publication.
    // Publication refreshes focus from the pointer, so the moved window is focused last.
    log::info!("move window to workspace index {index}");
    let mut outputs = Vec::new();
    if let Some(source_workspace) = source_workspace {
        outputs.extend(state.workspace_output_ids(source_workspace));
    }
    outputs.extend(state.workspace_output_ids(workspace_id));
    publish_workspace_outputs(state, dispatcher, outputs);
    let Some(root) = state.window_root_surface(window) else {
        return;
    };
    crate::input::set_keyboard_focus(
        state,
        dispatcher,
        Some(crate::input::SurfaceFocus::new(root)),
    );
    crate::input::pointer_lock::recheck_pointer_capture(state, dispatcher);
}

fn activate_adjacent_workspace(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    direction: crate::visual_animation::WorkspaceTransitionDirection,
) {
    let Some(group_id) = state.activate_adjacent_workspace(direction) else {
        log::info!("workspace {direction:?} ignored; this output has only one workspace");
        return;
    };
    log::info!("workspace {direction:?} on group {group_id:?}");
    finish_workspace_activation(state, dispatcher, group_id);
}

/// What a workspace switch by binding does after `active_workspace` changed.
pub(crate) fn finish_workspace_activation(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    group_id: WorkspaceGroupId,
) {
    crate::input::pointer_lock::release_pointer_capture(state, dispatcher);
    publish_workspace_group(state, dispatcher, group_id);
}

pub fn default_config_path() -> PathBuf {
    if let Ok(xdg_config_home) = env::var("XDG_CONFIG_HOME") {
        if !xdg_config_home.is_empty() {
            return PathBuf::from(xdg_config_home)
                .join(CONFIG_DIR_NAME)
                .join(CONFIG_FILE_NAME);
        }
    }
    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".config")
                .join(CONFIG_DIR_NAME)
                .join(CONFIG_FILE_NAME);
        }
    }
    PathBuf::from(CONFIG_FILE_NAME)
}

fn parse_gesture_binding(description: &str) -> anyhow::Result<(u32, SwipeDirection)> {
    let parts: Vec<&str> = description.split(':').map(str::trim).collect();
    let [kind, fingers, direction] = parts.as_slice() else {
        bail!("gesture binding `{description}` must look like swipe:<fingers>:<direction>");
    };
    if kind.eq_ignore_ascii_case("pinch") || kind.eq_ignore_ascii_case("hold") {
        bail!("gesture binding `{description}`: only swipe gestures can be bound");
    }
    if !kind.eq_ignore_ascii_case("swipe") {
        bail!("gesture binding `{description}` must look like swipe:<fingers>:<direction>");
    }
    let fingers = fingers
        .parse::<u32>()
        .ok()
        .filter(|fingers| (3..=5).contains(fingers))
        .ok_or_else(|| {
            anyhow::anyhow!("gesture binding `{description}`: finger count must be 3, 4 or 5")
        })?;
    let direction = match direction.to_ascii_lowercase().as_str() {
        "left" => SwipeDirection::Left,
        "right" => SwipeDirection::Right,
        "up" => SwipeDirection::Up,
        "down" => SwipeDirection::Down,
        _ => bail!("gesture binding `{description}`: direction must be left, right, up or down"),
    };
    Ok((fingers, direction))
}

fn parse_binding(description: &str, keymap: &xkb::Keymap) -> anyhow::Result<KeyBinding> {
    let parts: Vec<_> = description
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("binding is empty");
    }
    let (key_name, modifier_names) = parts.split_last().unwrap();
    let keysym_name = normalize_keysym_name(key_name);
    let keysym = xkb::keysym_from_name(&keysym_name, xkb::KEYSYM_CASE_INSENSITIVE);
    if keysym.raw() == xkb::keysyms::KEY_NoSymbol {
        bail!("unknown keysym `{}`", key_name);
    }
    let modifiers = parse_modifier_mask(modifier_names, keymap)?;
    if binding_shadowed_by_vt_switch(keysym, modifier_names) {
        log::warn!(
            "binding `{description}` is shadowed by VT switching; Ctrl+Alt+F1..F12 is built in"
        );
    }
    Ok(KeyBinding { description: description.to_string(), keysym, modifiers })
}

fn binding_shadowed_by_vt_switch(keysym: xkb::Keysym, modifier_names: &[&str]) -> bool {
    if crate::xkb::vt_for_keysym(keysym).is_some() {
        return true;
    }
    let raw = keysym.raw();
    (xkb::keysyms::KEY_F1..=xkb::keysyms::KEY_F12).contains(&raw)
        && modifiers_are_exactly_ctrl_alt(modifier_names)
}

fn modifiers_are_exactly_ctrl_alt(modifier_names: &[&str]) -> bool {
    if modifier_names.len() != 2 {
        return false;
    }
    let mut ctrl = false;
    let mut alt = false;
    for name in modifier_names {
        match normalize_modifier_name(name) {
            Some(name) if name == xkb::MOD_NAME_CTRL && !ctrl => ctrl = true,
            Some(name) if name == xkb::MOD_NAME_ALT && !alt => alt = true,
            _ => return false,
        }
    }
    ctrl && alt
}

fn parse_mouse_binding(
    description: &str,
    keymap: &xkb::Keymap,
) -> anyhow::Result<MouseBindingTrigger> {
    let parts: Vec<_> = description
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("binding is empty");
    }
    let (button_name, modifier_names) = parts.split_last().unwrap();
    let Some(button_name) = button_name.strip_prefix("mouse:") else {
        bail!("mouse binding must end with `mouse:<button>`");
    };
    let button = parse_mouse_button(button_name)?;
    let modifiers = parse_modifier_mask(modifier_names, keymap)?;
    Ok(MouseBindingTrigger { description: description.to_string(), button, modifiers })
}

fn parse_modifier_mask(modifier_names: &[&str], keymap: &xkb::Keymap) -> anyhow::Result<u32> {
    let mut modifiers = 0u32;
    for modifier_name in modifier_names {
        let canonical = normalize_modifier_name(modifier_name)
            .ok_or_else(|| anyhow!("unknown modifier `{}`", modifier_name))?;
        let index = keymap.mod_get_index(&canonical);
        if index == xkb::MOD_INVALID {
            bail!("modifier `{}` is not present in the current keymap", modifier_name);
        }
        if index >= u32::BITS {
            bail!("modifier `{}` index {} exceeds supported mask size", modifier_name, index);
        }
        modifiers |= 1u32 << index;
    }
    Ok(modifiers)
}

fn parse_mouse_button(name: &str) -> anyhow::Result<u32> {
    match name.to_ascii_lowercase().as_str() {
        "left" => Ok(BTN_LEFT),
        "right" => Ok(BTN_RIGHT),
        "middle" => Ok(BTN_MIDDLE),
        other => other
            .parse()
            .with_context(|| format!("unknown mouse button `{name}`")),
    }
}

fn normalize_modifier_name(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "shift" => Some(xkb::MOD_NAME_SHIFT),
        "ctrl" | "control" => Some(xkb::MOD_NAME_CTRL),
        "alt" => Some(xkb::MOD_NAME_ALT),
        "super" | "logo" | "mod4" => Some(xkb::MOD_NAME_LOGO),
        _ => None,
    }
}

fn normalize_keysym_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "enter" => "Return".to_string(),
        "esc" => "Escape".to_string(),
        "del" => "Delete".to_string(),
        "pgup" => "Page_Up".to_string(),
        "pgdown" => "Page_Down".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        other if other.len() == 1 => other.to_string(),
        _ => name.replace('-', "_"),
    }
}

fn sanitize_cursor_theme(theme: String) -> String {
    let trimmed = theme.trim();
    if trimmed.is_empty() {
        DEFAULT_CURSOR_THEME.to_string()
    } else {
        trimmed.to_string()
    }
}

fn sanitize_cursor_size(size: u32) -> u32 {
    size.max(1)
}

fn sanitize_non_negative_i32(value: i32) -> i32 {
    value.max(0)
}

fn parse_rgba_color(raw: &str) -> anyhow::Result<RgbaColor> {
    fn parse_hex_byte(component: &str, raw: &str) -> anyhow::Result<u8> {
        u8::from_str_radix(component, 16)
            .with_context(|| format!("`{raw}` contains an invalid hex component"))
    }

    fn parse_hex_color(hex: &str, raw: &str) -> anyhow::Result<RgbaColor> {
        match hex.len() {
            6 => Ok(RgbaColor::new(
                parse_hex_byte(&hex[0..2], raw)?,
                parse_hex_byte(&hex[2..4], raw)?,
                parse_hex_byte(&hex[4..6], raw)?,
                0xff,
            )),
            8 => Ok(RgbaColor::new(
                parse_hex_byte(&hex[0..2], raw)?,
                parse_hex_byte(&hex[2..4], raw)?,
                parse_hex_byte(&hex[4..6], raw)?,
                parse_hex_byte(&hex[6..8], raw)?,
            )),
            _ => bail!(
                "expected 6 or 8 hex digits in `{raw}` (supported forms: #RRGGBB, #RRGGBBAA, rgb(...), rgba(...), 0xRRGGBB, 0xRRGGBBAA)"
            ),
        }
    }

    let raw = raw.trim();
    let hex = raw
        .strip_prefix('#')
        .or_else(|| raw.strip_prefix("0x"))
        .or_else(|| raw.strip_prefix("0X"))
        .or_else(|| {
            raw.strip_prefix("rgba(")
                .and_then(|value| value.strip_suffix(')'))
        })
        .or_else(|| {
            raw.strip_prefix("rgb(")
                .and_then(|value| value.strip_suffix(')'))
        })
        .unwrap_or(raw)
        .trim();
    if hex.is_empty() {
        bail!("color must not be empty");
    }
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("`{raw}` is not a valid hex color");
    }
    parse_hex_color(hex, raw)
}

impl fmt::Display for RgbaColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
}

fn parent_directory_event_affects_config(
    event: &inotify::Event<&OsStr>,
    config_file_name: Option<&OsStr>,
) -> bool {
    match event.name {
        None => event
            .mask
            .intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF | EventMask::ATTRIB),
        Some(name) => {
            let Some(base) = config_file_name else {
                return false;
            };
            if name != base {
                return false;
            }
            event.mask.intersects(
                EventMask::CLOSE_WRITE
                    | EventMask::CREATE
                    | EventMask::DELETE
                    | EventMask::MOVED_FROM
                    | EventMask::MOVED_TO
                    | EventMask::MODIFY
                    | EventMask::ATTRIB,
            )
        }
    }
}

fn nearest_existing_directory(path: &Path) -> PathBuf {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.is_dir() {
            return candidate.to_path_buf();
        }
        current = candidate.parent();
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::ClientId,
            input::SurfaceFocus,
            protocol_generated::WlSurfaceId,
            runner::Dispatcher,
            state::{BackendOutputRef, DrmId, OutputId},
            test_support::{
                insert_window, state_for_tests, state_for_tests_with_config, test_output,
                test_output_with_crtc_name,
            },
            wayland_objects::wl_surface::WlSurface,
        },
    };

    #[test]
    fn parses_toml_binding_with_aliases() {
        let xkb = Xkb::new();
        let parsed: ConfigFile = toml::from_str(
            r##"
            [general]
            cursor_theme = "Adwaita"
            cursor_size = 32
            outer_gap = 12
            inner_gap = 6

            [decorations]
            rounding = 7
            rounding_power = 3.5

            [decorations.border]
            size = 4
            active_start_color = "#11223344"
            active_end_color = "#22334455"
            inactive_start_color = "rgba(55667788)"
            inactive_end_color = "#66778899"
            gradient_angle_degrees = 15

            [decorations.shadow]
            radius = 18
            offset_x = 2
            offset_y = 9
            color = "#01020380"
            power = 2.5

            [bindings]
            "Super+Enter" = { action = "spawn", command = "alacritty" }
            "Ctrl+Shift+Esc" = { action = "reload-config" }
            "##,
        )
        .unwrap();

        let config = RuntimeConfig::compile(PathBuf::from("config.toml"), parsed, &xkb).unwrap();
        assert_eq!(config.general.cursor_theme, "Adwaita");
        assert_eq!(config.general.cursor_size, 32);
        assert_eq!(config.general.outer_gap, 12);
        assert_eq!(config.general.inner_gap, 6);
        assert_eq!(config.decorations.rounding, 7);
        assert_eq!(config.decorations.rounding_power, RoundingPower::sanitize(3.5));
        assert_eq!(
            config.decorations.border,
            BorderDecorationConfig {
                size: 4,
                active_start_color: RgbaColor::new(0x11, 0x22, 0x33, 0x44),
                active_end_color: RgbaColor::new(0x22, 0x33, 0x44, 0x55),
                inactive_start_color: RgbaColor::new(0x55, 0x66, 0x77, 0x88),
                inactive_end_color: RgbaColor::new(0x66, 0x77, 0x88, 0x99),
                gradient_angle_degrees: 15,
            }
        );
        assert_eq!(
            config.decorations.shadow,
            ShadowDecorationConfig {
                radius: 18,
                offset_x: 2,
                offset_y: 9,
                color: RgbaColor::new(0x01, 0x02, 0x03, 0x80),
                power: ShadowPower::sanitize(2.5),
            }
        );
        assert_eq!(config.bindings.len(), 2);
        let enter = config
            .bindings
            .iter()
            .find(|binding| binding.trigger.description == "Super+Enter")
            .unwrap();
        let escape = config
            .bindings
            .iter()
            .find(|binding| binding.trigger.description == "Ctrl+Shift+Esc")
            .unwrap();
        assert_eq!(enter.trigger.keysym, xkb::keysym_from_name("Return", 0));
        assert_eq!(escape.trigger.keysym, xkb::keysym_from_name("Escape", 0));
    }

    #[test]
    fn negative_gap_border_and_rounding_values_are_clamped_to_zero() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [general]
            outer_gap = -5
            inner_gap = -7

            [decorations]
            rounding = -5
            rounding_power = 0.0
            resize_grab_extent = -4

            [decorations.border]
            size = -9
            "#,
            &xkb,
        )
        .unwrap();

        assert_eq!(config.general.outer_gap, 0);
        assert_eq!(config.general.inner_gap, 0);
        assert_eq!(config.decorations.border.size, 0);
        assert_eq!(config.decorations.rounding, 0);
        assert_eq!(config.decorations.resize_grab_extent, 0);
        assert_eq!(config.decorations.rounding_power, RoundingPower::DEFAULT);
        assert_eq!(config.decorations.shadow.power, ShadowPower::DEFAULT);
    }

    #[test]
    fn decoration_defaults_reserve_a_one_pixel_border() {
        let config = RuntimeConfig::default();
        let parsed = RuntimeConfig::parse_for_test("", &Xkb::new()).unwrap();

        assert_eq!(config.decorations.border.size, 1);
        assert_eq!(parsed.decorations.border.size, 1);
        assert_eq!(
            config.decorations.border.active_start_color,
            RgbaColor::new(0x50, 0x78, 0xc8, 0xff)
        );
        assert_eq!(
            config.decorations.border.inactive_start_color,
            RgbaColor::new(0x33, 0x38, 0x44, 0xff)
        );
        assert_eq!(config.decorations.rounding, 0);
        assert_eq!(config.decorations.resize_grab_extent, RESIZE_GRAB_EXTENT_DEFAULT);
        assert_eq!(parsed.decorations.resize_grab_extent, RESIZE_GRAB_EXTENT_DEFAULT);
        assert_eq!(config.decorations.rounding_power, RoundingPower::DEFAULT);
    }

    #[test]
    fn rejects_removed_decoration_config_keys() {
        let xkb = Xkb::new();
        for source in [
            "[decorations.xdg]\nno_protocol = \"client\"\n",
            "[decorations.border]\nthickness = 2\n",
            "[general.decoration_insets]\ntop = 5\n",
            "[decorations.border]\nrounding = 8\n",
        ] {
            let error = RuntimeConfig::parse_for_test(source, &xkb).unwrap_err();
            assert!(
                error.to_string().contains("unknown field"),
                "expected `{source}` to be rejected, got {error:#}"
            );
        }
    }

    #[test]
    fn parses_supported_color_formats() {
        assert_eq!(parse_rgba_color("#5078c8").unwrap(), RgbaColor::new(0x50, 0x78, 0xc8, 0xff));
        assert_eq!(parse_rgba_color("0x333844ff").unwrap(), RgbaColor::new(0x33, 0x38, 0x44, 0xff));
        assert_eq!(
            parse_rgba_color("rgba(11223344)").unwrap(),
            RgbaColor::new(0x11, 0x22, 0x33, 0x44)
        );
    }

    #[test]
    fn invalid_border_rounding_power_falls_back_to_default() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [decorations]
            rounding = 8
            rounding_power = -1.0
            "#,
            &xkb,
        )
        .unwrap();

        assert_eq!(config.decorations.rounding, 8);
        assert_eq!(config.decorations.rounding_power, RoundingPower::DEFAULT);
    }

    #[test]
    fn invalid_shadow_power_falls_back_to_default() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [decorations.shadow]
            power = 0.0
            "#,
            &xkb,
        )
        .unwrap();

        assert_eq!(config.decorations.shadow.power, ShadowPower::DEFAULT);
    }

    #[test]
    fn rejects_duplicate_compiled_bindings() {
        let xkb = Xkb::new();
        let parsed: ConfigFile = toml::from_str(
            r#"
            [bindings]
            "Super+Enter" = { action = "reload-config" }
            "Mod4+Return" = { action = "spawn", command = "alacritty" }
            "#,
        )
        .unwrap();

        let error = RuntimeConfig::compile(PathBuf::from("config.toml"), parsed, &xkb).unwrap_err();
        assert!(error.to_string().contains("duplicate binding"));
    }

    #[test]
    fn vt_switch_bindings_still_load() {
        assert!(binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_XF86Switch_VT_1),
            &[],
        ));
        assert!(binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_XF86Switch_VT_12),
            &["Super"],
        ));
        assert!(binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_F1),
            &["Ctrl", "Alt"]
        ));
        assert!(binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_F12),
            &["Alt", "control"]
        ));
        assert!(!binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_F1),
            &["Super"]
        ));
        assert!(!binding_shadowed_by_vt_switch(
            xkb::Keysym::from(xkb::keysyms::KEY_F1),
            &["Ctrl", "Alt", "Shift"]
        ));
        assert!(!binding_shadowed_by_vt_switch(xkb::Keysym::from(xkb::keysyms::KEY_F1), &[]));

        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Ctrl+Alt+F1" = { action = "quit" }
            "Ctrl+Alt+XF86Switch_VT_2" = { action = "close" }
            "#,
            &xkb,
        )
        .unwrap();
        assert_eq!(config.bindings.len(), 2);
    }

    #[test]
    fn parses_mouse_bindings_with_aliases() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "Alt+mouse:273" = { action = "resize-window" }
            "#,
            &xkb,
        )
        .unwrap();

        assert_eq!(config.mouse_bindings.len(), 2);
        let move_binding = config
            .mouse_bindings
            .iter()
            .find(|binding| binding.trigger.description == "Super+mouse:left")
            .unwrap();
        let resize_binding = config
            .mouse_bindings
            .iter()
            .find(|binding| binding.trigger.description == "Alt+mouse:273")
            .unwrap();
        assert_eq!(move_binding.trigger.button, BTN_LEFT);
        assert_eq!(move_binding.action, MouseBindingAction::MoveWindow);
        assert_eq!(resize_binding.trigger.button, BTN_RIGHT);
        assert_eq!(resize_binding.action, MouseBindingAction::ResizeWindow);
    }

    #[test]
    fn matches_mouse_binding_for_pressed_modifiers() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "#,
            &xkb,
        )
        .unwrap();
        let mut keyboard = xkb.create_keyboard_state();

        assert_eq!(config.binding_for_pointer_button(&keyboard, BTN_LEFT), None);
        keyboard.update_key(125, true);
        assert_eq!(
            config.binding_for_pointer_button(&keyboard, BTN_LEFT),
            Some(MouseBindingAction::MoveWindow)
        );
    }

    #[test]
    fn rejects_duplicate_mouse_bindings() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [mouse_bindings]
            "Super+mouse:left" = { action = "move-window" }
            "Mod4+mouse:272" = { action = "resize-window" }
            "#,
            &xkb,
        );

        let error = config.unwrap_err();
        assert!(error.to_string().contains("duplicate mouse binding"));
    }

    #[test]
    fn parses_swipe_gesture_bindings() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [gestures]
            "swipe:3:left" = { action = "workspace-next" }
            " Swipe : 4 : UP " = { action = "spawn", command = "foo" }
            "#,
            &xkb,
        )
        .unwrap();

        assert_eq!(config.gesture_bindings.len(), 2);
        assert!(config.gesture_bindings.iter().any(|binding| {
            binding.fingers == 3
                && binding.direction == SwipeDirection::Left
                && binding.action == CompositorAction::WorkspaceNext
        }));
        assert!(config.gesture_bindings.iter().any(|binding| {
            binding.fingers == 4
                && binding.direction == SwipeDirection::Up
                && binding.action == CompositorAction::Spawn { command: "foo".to_string() }
        }));
    }

    #[test]
    fn rejects_pinch_and_hold_gesture_bindings() {
        let xkb = Xkb::new();
        for key in ["pinch:3:in", "hold:3:left"] {
            let input = format!("[gestures]\n\"{key}\" = {{ action = \"quit\" }}\n");
            let error = RuntimeConfig::parse_for_test(&input, &xkb).unwrap_err();
            assert!(format!("{error:#}").contains("only swipe gestures can be bound"), "{key}");
        }
    }

    #[test]
    fn rejects_bad_gesture_binding_keys() {
        let xkb = Xkb::new();
        let cases = [
            ("swipe:2:left", "finger count must be 3, 4 or 5"),
            ("swipe:6:left", "finger count must be 3, 4 or 5"),
            ("swipe:x:left", "finger count must be 3, 4 or 5"),
            ("swipe:3:diagonal", "direction must be left, right, up or down"),
            ("swipe:3", "must look like swipe:<fingers>:<direction>"),
            ("swipe:3:left:extra", "must look like swipe:<fingers>:<direction>"),
        ];
        for (key, message) in cases {
            let input = format!("[gestures]\n\"{key}\" = {{ action = \"quit\" }}\n");
            let error = RuntimeConfig::parse_for_test(&input, &xkb).unwrap_err();
            assert!(format!("{error:#}").contains(message), "{key}: {error:#}");
        }
    }

    #[test]
    fn rejects_duplicate_gesture_bindings() {
        let xkb = Xkb::new();
        let error = RuntimeConfig::parse_for_test(
            r#"
            [gestures]
            "swipe:3:left" = { action = "quit" }
            "swipe:3:LEFT" = { action = "close" }
            "#,
            &xkb,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate gesture binding"));
    }

    #[test]
    fn example_config_ships_three_finger_workspace_swipes() {
        let xkb = Xkb::new();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.toml.example");
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let gestures: Vec<_> = config
            .gesture_bindings
            .iter()
            .map(|binding| (binding.description.as_str(), binding.action.clone()))
            .collect();
        assert_eq!(
            gestures,
            vec![
                ("swipe:3:left", CompositorAction::WorkspaceNext),
                ("swipe:3:right", CompositorAction::WorkspacePrevious),
            ]
        );
    }

    #[test]
    fn gesture_binding_lookups() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [gestures]
            "swipe:3:left" = { action = "workspace-next" }
            "swipe:3:right" = { action = "workspace-previous" }
            "#,
            &xkb,
        )
        .unwrap();

        assert!(config.swipe_fingers_bound(3));
        assert!(!config.swipe_fingers_bound(4));
        assert_eq!(
            config.binding_for_swipe(3, SwipeDirection::Right),
            Some(CompositorAction::WorkspacePrevious)
        );
        assert_eq!(config.binding_for_swipe(3, SwipeDirection::Up), None);
    }

    #[test]
    fn renderer_defaults_to_opengl_and_accepts_auto_and_vulkan() {
        let xkb = Xkb::new();
        let omitted = RuntimeConfig::parse_for_test("[general]\ncursor_size = 24\n", &xkb).unwrap();
        assert_eq!(omitted.general.renderer, RendererSelection::OpenGl);

        let auto = RuntimeConfig::parse_for_test("[general]\nrenderer = \"AuTo\"\n", &xkb).unwrap();
        assert_eq!(auto.general.renderer, RendererSelection::Auto);

        let vulkan =
            RuntimeConfig::parse_for_test("[general]\nrenderer = \"vulkan\"\n", &xkb).unwrap();
        assert_eq!(vulkan.general.renderer, RendererSelection::Vulkan);

        let error =
            RuntimeConfig::parse_for_test("[general]\nrenderer = \"metal\"\n", &xkb).unwrap_err();
        assert!(error.to_string().contains("general.renderer"));
    }

    #[test]
    fn reload_keeps_running_renderer_and_applies_other_settings() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            renderer = "opengl"
            outer_gap = 4
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(
            &path,
            r#"
            [general]
            renderer = "vulkan"
            outer_gap = 16
            "#,
        )
        .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert_eq!(state.config.general.renderer, RendererSelection::OpenGl);
        assert_eq!(state.config.general.outer_gap, 16);
    }

    #[test]
    fn reload_ignores_a_renderer_only_change() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nrenderer = \"auto\"\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nrenderer = \"vulkan\"\n").unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(!changed);
        assert_eq!(state.config.general.renderer, RendererSelection::Auto);
    }

    #[test]
    fn import_environment_defaults_to_true_and_parses_false() {
        let xkb = Xkb::new();
        assert!(RuntimeConfig::default().general.import_environment);
        assert!(
            RuntimeConfig::parse_for_test("", &xkb)
                .unwrap()
                .general
                .import_environment
        );
        assert!(
            RuntimeConfig::parse_for_test("[general]\nimport_environment = true\n", &xkb)
                .unwrap()
                .general
                .import_environment
        );
        let disabled =
            RuntimeConfig::parse_for_test("[general]\nimport_environment = false\n", &xkb).unwrap();
        assert!(!disabled.general.import_environment);

        let error =
            RuntimeConfig::parse_for_test("[general]\nimport_environment = \"yes\"\n", &xkb)
                .unwrap_err();
        assert!(error.to_string().contains("import_environment"), "{error:#}");
    }

    #[test]
    fn reload_applies_a_changed_import_environment() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nimport_environment = true\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nimport_environment = false\n").unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(!state.config.general.import_environment);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::process::ProcessManagerAction>(|action| {
                matches!(action, crate::process::ProcessManagerAction::SetExportEnabled(false))
            }),
            1
        );
    }

    #[test]
    fn reload_enables_import_environment() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nimport_environment = false\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nimport_environment = true\n").unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(state.config.general.import_environment);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::process::ProcessManagerAction>(|action| {
                matches!(action, crate::process::ProcessManagerAction::SetExportEnabled(true))
            }),
            1
        );
    }

    #[test]
    fn reload_leaves_an_unchanged_import_environment_alone() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nimport_environment = true\nouter_gap = 4\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nimport_environment = true\nouter_gap = 16\n").unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(state.config.general.import_environment);
        assert_eq!(state.config.general.outer_gap, 16);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::process::ProcessManagerAction>(|action| {
                matches!(action, crate::process::ProcessManagerAction::SetExportEnabled(_))
            }),
            0
        );
    }

    #[test]
    fn reload_dispatches_import_environment_before_xwayland_enabled() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nimport_environment = true\n\n[xwayland]\nenabled = true\n")
            .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nimport_environment = false\n\n[xwayland]\nenabled = false\n")
            .unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        let import_at =
            dispatcher.test_queued_positions::<crate::process::ProcessManagerAction>(|action| {
                matches!(action, crate::process::ProcessManagerAction::SetExportEnabled(false))
            });
        let enabled_at =
            dispatcher.test_queued_positions::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::SetEnabled(false))
            });
        assert_eq!(import_at.len(), 1);
        assert_eq!(enabled_at.len(), 1);
        assert!(import_at[0] < enabled_at[0]);
    }

    #[test]
    fn xwayland_is_enabled_by_default_and_can_be_turned_off() {
        let xkb = Xkb::new();
        assert!(RuntimeConfig::default().xwayland.enabled);
        assert!(
            RuntimeConfig::parse_for_test("", &xkb)
                .unwrap()
                .xwayland
                .enabled
        );
        assert!(
            RuntimeConfig::parse_for_test("[xwayland]\nenabled = true\n", &xkb)
                .unwrap()
                .xwayland
                .enabled
        );

        let disabled =
            RuntimeConfig::parse_for_test("[xwayland]\nenabled = false\n", &xkb).unwrap();
        assert!(!disabled.xwayland.enabled);
    }

    #[test]
    fn xwayland_scaling_mode_defaults_to_default_and_parses_known_values() {
        let xkb = Xkb::new();
        assert_eq!(XwaylandScalingMode::default(), XwaylandScalingMode::Default);
        assert_eq!(RuntimeConfig::default().xwayland.scaling_mode, XwaylandScalingMode::Default,);
        assert_eq!(
            RuntimeConfig::parse_for_test("", &xkb)
                .unwrap()
                .xwayland
                .scaling_mode,
            XwaylandScalingMode::Default,
        );
        assert_eq!(
            RuntimeConfig::parse_for_test("[xwayland]\nscaling_mode = \"default\"\n", &xkb)
                .unwrap()
                .xwayland
                .scaling_mode,
            XwaylandScalingMode::Default,
        );
        assert_eq!(
            RuntimeConfig::parse_for_test("[xwayland]\nscaling_mode = \"downscaled\"\n", &xkb)
                .unwrap()
                .xwayland
                .scaling_mode,
            XwaylandScalingMode::Downscaled,
        );
        assert_eq!(
            RuntimeConfig::parse_for_test("[xwayland]\nscaling_mode = \"Downscaled\"\n", &xkb)
                .unwrap()
                .xwayland
                .scaling_mode,
            XwaylandScalingMode::Downscaled,
        );
        assert_eq!(XwaylandScalingMode::Downscaled.to_string(), "downscaled");
    }

    #[test]
    fn rejects_an_unknown_xwayland_scaling_mode() {
        let error =
            RuntimeConfig::parse_for_test("[xwayland]\nscaling_mode = \"blurry\"\n", &Xkb::new())
                .unwrap_err();
        assert!(error.to_string().contains("invalid xwayland.scaling_mode"), "{error:#}");
    }

    #[test]
    fn rejects_unknown_xwayland_keys() {
        let error =
            RuntimeConfig::parse_for_test("[xwayland]\nbogus = 1\n", &Xkb::new()).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error:#}");
    }

    #[test]
    fn session_management_defaults_to_enabled_and_rejects_unknown_keys() {
        let xkb = Xkb::new();
        assert!(RuntimeConfig::default().session_management.enabled);
        assert!(
            RuntimeConfig::parse_for_test("", &xkb)
                .unwrap()
                .session_management
                .enabled
        );
        assert!(
            RuntimeConfig::parse_for_test("[session_management]\nenabled = true\n", &xkb)
                .unwrap()
                .session_management
                .enabled
        );
        assert!(
            !RuntimeConfig::parse_for_test("[session_management]\nenabled = false\n", &xkb)
                .unwrap()
                .session_management
                .enabled
        );
        let error =
            RuntimeConfig::parse_for_test("[session_management]\nbogus = 1\n", &xkb).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error:#}");
    }

    #[test]
    fn reload_toggles_the_session_manager_global() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[session_management]\nenabled = true\n").unwrap();
        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        state.session_store = Some(crate::session_store::SessionStore::load(
            temp_dir.to_path_buf().join("sessions.json"),
        ));
        let mut dispatcher = Dispatcher::default();
        crate::session::sync_session_manager_global(&mut state, &mut dispatcher);
        let name = state.session_manager_global.unwrap();

        fs::write(&path, "[session_management]\nenabled = false\n").unwrap();
        reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();
        assert!(state.session_manager_global.is_none());
        assert!(dispatcher.test_has_queued_action(
            |action: &crate::clients::ClientManagerAction| {
                matches!(
                    action,
                    crate::clients::ClientManagerAction::RegistryGlobalRemoved { name: removed }
                        if *removed == name.as_u32()
                )
            }
        ));

        fs::write(&path, "[session_management]\nenabled = true\n").unwrap();
        reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();
        let added = state.session_manager_global.unwrap();
        assert!(dispatcher.test_has_queued_action(
            |action: &crate::clients::ClientManagerAction| {
                matches!(
                    action,
                    crate::clients::ClientManagerAction::RegistryGlobalAdded {
                        name: queued,
                        interface,
                        version: 1,
                    } if *queued == added.as_u32()
                        && interface == "xdg_session_manager_v1"
                )
            }
        ));
    }

    #[test]
    fn reload_applies_a_changed_xwayland_enabled() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            outer_gap = 4

            [xwayland]
            enabled = true
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(
            &path,
            r#"
            [general]
            outer_gap = 16

            [xwayland]
            enabled = false
            "#,
        )
        .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(!state.config.xwayland.enabled);
        assert_eq!(state.config.general.outer_gap, 16);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::SetEnabled(false))
            }),
            1
        );
    }

    #[test]
    fn reload_applies_a_changed_scaling_mode_and_enabled() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[xwayland]\nenabled = false\nscaling_mode = \"default\"\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[xwayland]\nenabled = true\nscaling_mode = \"downscaled\"\n").unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert_eq!(state.config.xwayland.scaling_mode, XwaylandScalingMode::Downscaled);
        assert!(state.config.xwayland.enabled);
        let enabled_at =
            dispatcher.test_queued_positions::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::SetEnabled(true))
            });
        let scale_at = dispatcher.test_queued_positions::<ClientManagerAction>(|action| {
            matches!(action, ClientManagerAction::XwaylandWireScaleChanged)
        });
        assert_eq!(enabled_at.len(), 1);
        assert_eq!(scale_at.len(), 1);
        assert!(enabled_at[0] < scale_at[0]);
    }

    #[test]
    fn reload_to_downscaled_on_a_2x_output_queues_set_wire_scale_first() {
        use {
            crate::{
                clients::{ClientId, ClientManager, ClientManagerAction, ClientOrigin},
                runner::Model,
                state::FractionalScale,
                test_support::test_output_drm0_wh,
                wire_scale::WireScale,
            },
            std::os::unix::net::UnixStream,
        };

        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[xwayland]\nscaling_mode = \"default\"\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        state.insert_output(test_output_drm0_wh(800, 600));

        let socket_dir = mktemp::Temp::new_dir().unwrap();
        let mut manager =
            ClientManager::bind(&socket_dir.to_path_buf().join("wayland-test")).unwrap();
        let mut dispatcher = Dispatcher::default();
        let (socket, _peer) = UnixStream::pair().unwrap();
        manager
            .process(
                &mut state,
                ClientManagerAction::AdoptClient {
                    fd: socket.into(),
                    origin: ClientOrigin { xwayland: true, pid: None },
                    adopted: crate::runner::Redispatcher::None,
                    closed: crate::runner::Redispatcher::None,
                },
                &mut dispatcher,
            )
            .unwrap();
        let _ = dispatcher.test_take_queued_actions::<crate::clients::ClientAction>();

        fs::write(
            &path,
            "[output.scales]\ntest-output = 2\n\n[xwayland]\nscaling_mode = \"downscaled\"\n",
        )
        .unwrap();
        reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();
        assert_eq!(state.outputs.iter().next().unwrap().1.scale, FractionalScale::TWO);

        let manager_actions = dispatcher.test_take_queued_actions::<ClientManagerAction>();
        assert!(manager_actions
            .iter()
            .any(|action| matches!(action, ClientManagerAction::XwaylandWireScaleChanged)));
        for action in manager_actions {
            manager
                .process(&mut state, action, &mut dispatcher)
                .unwrap();
        }

        assert_eq!(state.xwayland_wire_scale, WireScale::new(2));
        let mut marks = Vec::new();
        dispatcher.test_matching_actions_for::<crate::clients::ClientAction, ClientId>(
            ClientId::from_raw(0),
            |action| {
                marks.push(match action {
                    crate::clients::ClientAction::SetWireScale { scale } => scale.get(),
                    crate::clients::ClientAction::OutputStateChanged { .. } => 0,
                    _ => 99,
                });
            },
        );
        let scale_at = marks
            .iter()
            .position(|mark| *mark == 2)
            .expect("set wire scale");
        if let Some(output_at) = marks.iter().position(|mark| *mark == 0) {
            assert!(scale_at < output_at);
        }
    }

    #[test]
    fn reload_applies_an_xwayland_only_change() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[xwayland]\nenabled = false\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[xwayland]\nenabled = true\n").unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(state.config.xwayland.enabled);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::SetEnabled(true))
            }),
            1
        );
    }

    #[test]
    fn reload_leaves_an_unchanged_xwayland_enabled_alone() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            outer_gap = 4

            [xwayland]
            enabled = true
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(
            &path,
            r#"
            [general]
            outer_gap = 16

            [xwayland]
            enabled = true
            "#,
        )
        .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(state.config.xwayland.enabled);
        assert_eq!(state.config.general.outer_gap, 16);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::SetEnabled(_))
            }),
            0
        );
    }

    #[test]
    fn reload_keeps_last_good_config_when_new_file_is_invalid() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config.clone());
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[bindings\n").unwrap();

        let error = reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding)
            .unwrap_err();
        assert!(error.to_string().contains("failed to parse"));
        assert_eq!(state.config, config);
    }

    #[test]
    fn tearing_defaults_on_and_a_per_output_value_wins() {
        let xkb = Xkb::new();
        let defaults = RuntimeConfig::parse_for_test("", &xkb).unwrap();
        assert!(defaults.general.allow_tearing);
        assert!(defaults.output_allows_tearing("DP-1"));
        assert_eq!(defaults.output_color("DP-1"), OutputColorConfig::DEFAULT);

        let config = RuntimeConfig::parse_for_test(
            r#"
            [general]
            allow_tearing = false

            [output.tearing]
            "DP-1" = true
            "#,
            &xkb,
        )
        .unwrap();
        assert!(!config.general.allow_tearing);
        assert!(config.output_allows_tearing("DP-1"));
        assert!(!config.output_allows_tearing("eDP-1"));
        assert!(!config.output_allows_tearing("dp-1"));
    }

    #[test]
    fn output_color_defaults_and_a_per_connector_value_wins() {
        let xkb = Xkb::new();
        let defaults = RuntimeConfig::parse_for_test("", &xkb).unwrap();
        assert_eq!(defaults.output_color("DP-1"), OutputColorConfig::DEFAULT);
        assert_eq!(defaults.output_color("DP-1").tf, OutputColorTransfer::Gamma22);
        assert_eq!(defaults.output_color("DP-1").primaries, OutputColorPrimaries::Srgb);
        assert_eq!(defaults.output_color("DP-1").sdr_white, 80.0);
        assert_eq!(defaults.output_color("DP-1").bitdepth, 8);
        assert!(!ten_bit_scanout(&defaults.output_color("DP-1"), false));
        assert!(ten_bit_scanout(&defaults.output_color("DP-1"), true));

        let config = RuntimeConfig::parse_for_test(
            r#"
            [output.color]
            "DP-1" = { tf = "srgb", primaries = "edid", sdr_white = 100 }
            "DP-2" = { tf = "gamma22" }
            "#,
            &xkb,
        )
        .unwrap();
        assert_eq!(
            config.output_color("DP-1"),
            OutputColorConfig {
                tf: OutputColorTransfer::Srgb,
                primaries: OutputColorPrimaries::Edid,
                mode: OutputColorMode::Sdr,
                sdr_white: 100.0,
                sdr_white_explicit: true,
                bitdepth: 8,
                min_luminance: None,
                max_luminance: None,
                max_fall: None,
            }
        );
        assert_eq!(
            config.output_color("DP-2"),
            OutputColorConfig {
                tf: OutputColorTransfer::Gamma22,
                primaries: OutputColorPrimaries::Srgb,
                mode: OutputColorMode::Sdr,
                sdr_white: 80.0,
                sdr_white_explicit: false,
                bitdepth: 8,
                min_luminance: None,
                max_luminance: None,
                max_fall: None,
            }
        );
        assert_eq!(config.output_color("dp-1"), OutputColorConfig::DEFAULT);
        assert_eq!(config.output_color("eDP-1"), OutputColorConfig::DEFAULT);

        let ten_bit = RuntimeConfig::parse_for_test(
            r#"
            [output.color]
            "DP-1" = { bitdepth = 10 }
            "#,
            &xkb,
        )
        .unwrap();
        assert_eq!(ten_bit.output_color("DP-1").bitdepth, 10);
        assert_eq!(ten_bit.output_color("DP-1").tf, OutputColorTransfer::Gamma22);
        assert!(ten_bit_scanout(&ten_bit.output_color("DP-1"), false));
        assert!(!ten_bit_scanout(&ten_bit.output_color("DP-2"), false));

        let hdr = RuntimeConfig::parse_for_test(
            r#"
            [output.color]
            "DP-1" = { mode = "hdr" }
            "DP-2" = { mode = "hdr", sdr_white = 120, min_luminance = 0.01, max_luminance = 600, max_fall = 200 }
            "#,
            &xkb,
        )
        .unwrap();
        let requested = hdr.output_color("DP-1");
        assert_eq!(requested.mode, OutputColorMode::Hdr);
        assert_eq!(requested.sdr_white, 203.0);
        assert!(!requested.sdr_white_explicit);
        assert_eq!(requested.reference_white(false), 80.0);
        assert_eq!(requested.reference_white(true), 203.0);
        let overridden = hdr.output_color("DP-2");
        assert!(overridden.sdr_white_explicit);
        assert_eq!(overridden.sdr_white, 120.0);
        assert_eq!(overridden.min_luminance, Some(0.01));
        assert_eq!(overridden.max_luminance, Some(600.0));
        assert_eq!(overridden.max_fall, Some(200.0));

        let xkb = Xkb::new();
        for source in [
            r#"[output.color]
            "DP-1" = { tf = "pq" }"#,
            r#"[output.color]
            "DP-1" = { primaries = "bt2020" }"#,
            r#"[output.color]
            "DP-1" = { sdr_white = 0.2 }"#,
            r#"[output.color]
            "DP-1" = { hdr = true }"#,
            r#"[output.color]
            "DP-1" = { mode = "pq" }"#,
            r#"[output.color]
            "DP-1" = { bitdepth = 12 }"#,
            r#"[output.color]
            "DP-1" = { mode = "hdr", max_luminance = 10, max_fall = 20 }"#,
        ] {
            assert!(RuntimeConfig::parse_for_test(source, &xkb).is_err(), "{source}");
        }
    }

    #[test]
    fn color_management_defaults_to_enabled_and_rejects_an_unknown_key() {
        let xkb = Xkb::new();
        let defaults = RuntimeConfig::parse_for_test("", &xkb).unwrap();
        assert!(defaults.color_management.enabled);

        let disabled =
            RuntimeConfig::parse_for_test("[color_management]\nenabled = false\n", &xkb).unwrap();
        assert!(!disabled.color_management.enabled);

        let error =
            RuntimeConfig::parse_for_test("[color_management]\nicc = true\n", &xkb).unwrap_err();
        assert!(error.to_string().contains("icc"), "{error}");
    }

    #[test]
    fn reload_keeps_the_running_color_management_switch() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[color_management]\nenabled = true\n[general]\nouter_gap = 4\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[color_management]\nenabled = false\n[general]\nouter_gap = 4\n")
            .unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();
        assert!(!changed);
        assert!(state.config.color_management.enabled);

        fs::write(&path, "[color_management]\nenabled = false\n[general]\nouter_gap = 12\n")
            .unwrap();
        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();
        assert!(changed);
        assert!(state.config.color_management.enabled);
        assert_eq!(state.config.general.outer_gap, 12);
    }

    #[test]
    fn reload_keeps_last_good_config_when_tearing_value_has_the_wrong_type() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            allow_tearing = false

            [output.tearing]
            "DP-1" = true
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config.clone());
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[output.tearing]\n\"DP-1\" = \"yes\"\n").unwrap();

        let error = reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding)
            .unwrap_err();
        assert!(error.to_string().contains("failed to parse"));
        assert_eq!(state.config, config);
        assert!(state.config.output_allows_tearing("DP-1"));
    }

    #[test]
    fn reload_requests_a_frame_when_tearing_changes() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(&path, "[general]\nallow_tearing = true\n").unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output_id = state.insert_output(test_output(backend, 1, 0, 0, 100, 100, "DP-1"));
        let mut dispatcher = Dispatcher::default();

        fs::write(&path, "[general]\nallow_tearing = true\n\n[output.tearing]\n\"DP-1\" = false\n")
            .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(!state.config.output_allows_tearing("DP-1"));
        assert!(output_frame_requested(&dispatcher, output_id));
    }

    #[test]
    fn reload_dispatches_workspace_state_changed_when_config_changes() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"
            outer_gap = 4

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let mut dispatcher = Dispatcher::default();

        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"
            outer_gap = 16

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(dispatcher.test_has_queued_action::<ClientManagerAction>(|action| matches!(
            action,
            ClientManagerAction::WorkspaceStateChanged
        )));
    }

    #[test]
    fn reload_marks_all_workspaces_dirty_when_config_changes() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"
            outer_gap = 4

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();

        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut state = state_for_tests_with_config(config);
        state.config.path = path.clone();
        let first_workspace = state.create_workspace(None, Some("first".to_string()));
        let second_workspace = state.create_workspace(None, Some("second".to_string()));
        state
            .workspaces
            .try_get_mut(first_workspace)
            .unwrap()
            .layout
            .dirty = false;
        state
            .workspaces
            .try_get_mut(second_workspace)
            .unwrap()
            .layout
            .dirty = false;
        let mut dispatcher = Dispatcher::default();

        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"
            outer_gap = 16

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();

        let changed =
            reload_from_disk(&mut state, &mut dispatcher, ReloadTrigger::ManualBinding).unwrap();

        assert!(changed);
        assert!(
            state
                .workspaces
                .try_get(first_workspace)
                .unwrap()
                .layout
                .dirty
        );
        assert!(
            state
                .workspaces
                .try_get(second_workspace)
                .unwrap()
                .layout
                .dirty
        );
    }

    #[test]
    fn config_watcher_missing_file_installs_parent_watch_only() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("missing.toml");
        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let watcher = ConfigWatcher::new(&config);
        assert_eq!(watcher.watch_slot_count(), 1);
    }

    #[test]
    fn config_watcher_stop_drops_watches_and_ignores_later_events() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("missing.toml");
        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut watcher = ConfigWatcher::new(&config);
        let mut state = crate::test_support::state_for_tests();
        let mut dispatcher = Dispatcher::default();
        watcher
            .process(&mut state, ConfigWatcherAction::Start, &mut dispatcher)
            .unwrap();
        watcher
            .process(&mut state, ConfigWatcherAction::Stop, &mut dispatcher)
            .unwrap();
        assert_eq!(watcher.watch_slot_count(), 0);
        assert!(dispatcher.test_has_queued_action(|action: &IoUringAction| {
            matches!(action, IoUringAction::PollRemove { .. })
        }));
        watcher
            .process(&mut state, ConfigWatcherAction::Event, &mut dispatcher)
            .unwrap();
        assert_eq!(watcher.watch_slot_count(), 0);
    }

    #[test]
    fn config_watcher_regular_file_installs_parent_and_logical_watches() {
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let path = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &path,
            r#"
            [general]
            cursor_theme = "Adwaita"

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();
        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let watcher = ConfigWatcher::new(&config);
        assert_eq!(watcher.watch_slot_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn config_watcher_symlink_installs_parent_logical_and_resolved_watches() {
        use std::os::unix::fs::symlink;
        let temp_dir = mktemp::Temp::new_dir().unwrap();
        let real = temp_dir.to_path_buf().join("real.toml");
        let logical = temp_dir.to_path_buf().join("config.toml");
        fs::write(
            &real,
            r#"
            [general]
            cursor_theme = "Adwaita"

            [bindings]
            "Super+Return" = { action = "reload-config" }
            "#,
        )
        .unwrap();
        symlink(&real, &logical).unwrap();
        let xkb = Xkb::new();
        let config = RuntimeConfig::load_from_path(&logical, &xkb).unwrap();
        let watcher = ConfigWatcher::new(&config);
        assert_eq!(watcher.watch_slot_count(), 3);
    }

    #[test]
    fn animation_tree_parses_through_runtime_config() {
        let xkb = Xkb::new();
        let defaults = RuntimeConfig::parse_for_test("", &xkb).unwrap();
        assert_eq!(defaults.animations.focus().duration_msec(), 303);
        assert_eq!(defaults.animations.opening().duration_msec(), 410);
        assert_eq!(defaults.animations.closing().duration_msec(), 149);
        assert_eq!(defaults.animations.workspace().duration_msec(), 194);
        assert!(defaults.animations.workspace().fades());

        let configured = RuntimeConfig::parse_for_test(
            r#"
            [animations.windowsIn]
            enabled = false

            [animations.windowsOut]
            enabled = false

            [animations.workspaces]
            enabled = false
            style = "slide"
            "#,
            &xkb,
        )
        .unwrap();
        assert!(configured.animations.opening().duration().is_zero());
        assert!(configured.animations.closing().duration().is_zero());
        assert!(configured.animations.workspace().duration().is_zero());
        assert!(!configured.animations.workspace().fades());
    }

    #[test]
    fn example_config_keeps_the_daily_bindings() {
        let xkb = Xkb::new();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.toml.example");
        let config = RuntimeConfig::load_from_path(&path, &xkb).unwrap();
        let mut actions: Vec<_> = config
            .bindings
            .iter()
            .map(|binding| (binding.trigger.description.as_str(), binding.action.clone()))
            .collect();
        actions.sort_by_key(|(description, _)| *description);
        assert_eq!(
            actions,
            vec![
                ("Super+1", CompositorAction::WorkspaceIndex { index: 1 }),
                ("Super+2", CompositorAction::WorkspaceIndex { index: 2 }),
                ("Super+3", CompositorAction::WorkspaceIndex { index: 3 }),
                ("Super+4", CompositorAction::WorkspaceIndex { index: 4 }),
                ("Super+Escape", CompositorAction::Quit),
                ("Super+F", CompositorAction::ToggleFullscreen),
                ("Super+Left", CompositorAction::FocusPrevious),
                ("Super+Page_Down", CompositorAction::WorkspacePrevious),
                ("Super+Page_Up", CompositorAction::WorkspaceNext),
                ("Super+Q", CompositorAction::Close),
                ("Super+Return", CompositorAction::Spawn { command: "alacritty".to_string() }),
                ("Super+Right", CompositorAction::FocusNext),
                ("Super+Shift+1", CompositorAction::MoveToWorkspace { index: 1 }),
                ("Super+Shift+2", CompositorAction::MoveToWorkspace { index: 2 }),
                ("Super+Shift+3", CompositorAction::MoveToWorkspace { index: 3 }),
                ("Super+Shift+4", CompositorAction::MoveToWorkspace { index: 4 }),
                ("Super+Shift+C", CompositorAction::ReloadConfig),
                ("Super+Shift+Escape", CompositorAction::ToggleShortcutsInhibit),
                ("Super+Space", CompositorAction::ToggleFloating),
            ]
        );
        let mouse: Vec<_> = config
            .mouse_bindings
            .iter()
            .map(|binding| (binding.trigger.description.as_str(), binding.action))
            .collect();
        assert_eq!(
            mouse,
            vec![
                ("Ctrl+mouse:left", MouseBindingAction::MoveWindow),
                ("Ctrl+mouse:right", MouseBindingAction::ResizeWindow),
            ]
        );
        assert_eq!(config.tiling.drag, TilingDragConfig::default());
    }

    #[test]
    fn omitted_tiling_drag_uses_hyprland_defaults() {
        let xkb = Xkb::new();
        let config =
            RuntimeConfig::parse_for_test("[general]\ncursor_theme = \"default\"\n", &xkb).unwrap();
        assert_eq!(config.tiling.drag, TilingDragConfig::default());
        assert_eq!(config.tiling.drag.threshold, 0);
        assert!(!config.tiling.drag.precise_mouse_move);
        assert_eq!(config.tiling.drag.split_ratio.get(), 1.0);
    }

    #[test]
    fn tiling_drag_settings_reload_and_out_of_range_values_clamp() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [tiling.drag]
            threshold = 12
            precise_mouse_move = true
            split_ratio = 1.4
            "#,
            &xkb,
        )
        .unwrap();
        assert_eq!(config.tiling.drag.threshold, 12);
        assert!(config.tiling.drag.precise_mouse_move);
        assert_eq!(config.tiling.drag.split_ratio.get(), 1.4);

        let clamped = RuntimeConfig::parse_for_test(
            r#"
            [tiling.drag]
            threshold = -4
            split_ratio = 4.0
            "#,
            &xkb,
        )
        .unwrap();
        assert_eq!(clamped.tiling.drag.threshold, 0);
        assert!(!clamped.tiling.drag.precise_mouse_move);
        assert_eq!(clamped.tiling.drag.split_ratio.get(), SplitRatio::MAX);

        let low_ratio =
            RuntimeConfig::parse_for_test("[tiling.drag]\nsplit_ratio = 0.0\n", &xkb).unwrap();
        assert_eq!(low_ratio.tiling.drag.split_ratio.get(), SplitRatio::MIN);
    }

    #[test]
    fn parses_window_and_indexed_workspace_actions() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Q" = { action = "close" }
            "Super+Left" = { action = "focus-previous" }
            "Super+Right" = { action = "focus-next" }
            "Super+Space" = { action = "toggle-floating" }
            "Super+F" = { action = "toggle-fullscreen" }
            "Super+1" = { action = "workspace-index", index = 1 }
            "Super+Shift+1" = { action = "move-to-workspace", index = 4 }
            "#,
            &xkb,
        )
        .unwrap();

        let actions: Vec<_> = config
            .bindings
            .into_iter()
            .map(|binding| binding.action)
            .collect();
        assert_eq!(
            actions,
            vec![
                CompositorAction::WorkspaceIndex { index: 1 },
                CompositorAction::ToggleFullscreen,
                CompositorAction::FocusPrevious,
                CompositorAction::Close,
                CompositorAction::FocusNext,
                CompositorAction::MoveToWorkspace { index: 4 },
                CompositorAction::ToggleFloating,
            ]
        );
    }

    #[test]
    fn toggle_shortcuts_inhibit_parses() {
        let xkb = Xkb::new();
        let config = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+X" = { action = "toggle-shortcuts-inhibit" }
            "#,
            &xkb,
        )
        .unwrap();
        let actions: Vec<_> = config
            .bindings
            .into_iter()
            .map(|binding| binding.action)
            .collect();
        assert_eq!(actions, vec![CompositorAction::ToggleShortcutsInhibit]);

        for extra in ["command = \"x\"", "index = 1"] {
            let error = RuntimeConfig::parse_for_test(
                &format!(
                    r#"
                    [bindings]
                    "Super+X" = {{ action = "toggle-shortcuts-inhibit", {extra} }}
                    "#
                ),
                &xkb,
            );
            assert!(error.is_err(), "toggle-shortcuts-inhibit accepted `{extra}`");
        }
    }

    #[test]
    fn rejects_a_zero_workspace_index_at_load() {
        let xkb = Xkb::new();
        for action in ["workspace-index", "move-to-workspace"] {
            let error = RuntimeConfig::parse_for_test(
                &format!(
                    r#"
                    [bindings]
                    "Super+1" = {{ action = "{action}", index = 0 }}
                    "#
                ),
                &xkb,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("one-based"),
                "{action} accepted index 0: {error:#}"
            );
        }
    }

    #[test]
    fn rejects_fields_that_do_not_belong_to_the_action() {
        let xkb = Xkb::new();
        let wrong_index = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Q" = { action = "close", index = 1 }
            "#,
            &xkb,
        )
        .unwrap_err();
        assert!(wrong_index.to_string().contains("does not take `index`"));

        let unknown = RuntimeConfig::parse_for_test(
            r#"
            [bindings]
            "Super+Q" = { action = "close", extra = 1 }
            "#,
            &xkb,
        )
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown field"));
    }

    fn insert_managed_root(state: &mut State) -> (OutputId, Resolved<WlSurfaceId>) {
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let output = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let (_, object_id) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(41u32), ClientId::from_raw(1), 6));
        let surface = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(41u32), object_id);
        state.register_root_surface(surface);
        insert_window(state, surface);
        assert!(state.managed_root_surface_state(surface).is_some());
        (output, surface)
    }

    #[test]
    fn focused_window_actions_need_a_managed_root() {
        let mut state = state_for_tests();
        assert!(focused_managed_root(&state).is_none());

        let (_output, surface) = insert_managed_root(&mut state);
        assert!(focused_managed_root(&state).is_none());

        state.seat.keyboard_focus = Some(SurfaceFocus::new(surface));
        assert_eq!(focused_managed_root(&state), state.window_for_root(surface));

        state.remove_managed_window(surface);
        assert!(focused_managed_root(&state).is_none());
    }

    #[test]
    fn spawn_queues_a_shell_command() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        execute_action(
            &mut state,
            &mut dispatcher,
            CompositorAction::Spawn { command: "foo".into() },
        );
        assert!(dispatcher.test_has_queued_action::<ProcessManagerAction>(|action| {
            let ProcessManagerAction::Spawn(request) = action else {
                return false;
            };
            request.program == "sh"
                && request.args.len() == 2
                && request.args[0] == "-lc"
                && request.args[1] == "foo"
                && request.env.is_empty()
                && request.fds.is_empty()
                && request.notify.is_some()
        }));
        assert_eq!(dispatcher.test_count_queued_action::<ProcessManagerAction>(|_| true), 1);
    }

    #[test]
    fn workspace_index_uses_the_active_output_and_ignores_invalid_slots() {
        let mut state = state_for_tests();
        assert!(workspace_at_index(&state, 1).is_none());

        let (output, _surface) = insert_managed_root(&mut state);
        let group_id = state.workspace_group_for_output(output).unwrap();
        let workspaces = state
            .workspace_groups
            .try_get(group_id)
            .unwrap()
            .workspaces
            .clone();
        assert_eq!(workspaces.len(), 4);
        assert_eq!(workspace_at_index(&state, 1), Some(workspaces[0]));
        assert_eq!(workspace_at_index(&state, 4), Some(workspaces[3]));
        assert!(workspace_at_index(&state, 0).is_none());
        assert!(workspace_at_index(&state, 5).is_none());

        let before = state.active_workspace_for_output(output);
        execute_action(
            &mut state,
            &mut Dispatcher::default(),
            CompositorAction::WorkspaceIndex { index: 5 },
        );
        assert_eq!(state.active_workspace_for_output(output), before);
    }

    fn focus_managed_root(state: &mut State) -> Resolved<WlSurfaceId> {
        let (_output, surface) = insert_managed_root(state);
        state.seat.keyboard_focus = Some(SurfaceFocus::new(surface));
        surface
    }

    #[test]
    fn close_without_a_toplevel_is_a_no_op() {
        let mut state = state_for_tests();
        let surface = focus_managed_root(&mut state);
        let mut dispatcher = Dispatcher::default();
        execute_action(&mut state, &mut dispatcher, CompositorAction::Close);
        assert!(!dispatcher.test_has_queued_action::<crate::clients::ClientAction>(|action| {
            matches!(action, crate::clients::ClientAction::RequestToplevelClose { .. })
        }));
        assert!(state.managed_root_surface_state(surface).is_some());
    }

    #[test]
    fn toggle_floating_and_fullscreen_flip_and_restore() {
        let mut state = state_for_tests();
        let surface = focus_managed_root(&mut state);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::ToggleFloating);
        assert_eq!(
            state.managed_root_surface_state(surface).unwrap().mode,
            crate::state::ManagedWindowMode::Floating
        );
        execute_action(&mut state, &mut dispatcher, CompositorAction::ToggleFloating);
        assert_eq!(
            state.managed_root_surface_state(surface).unwrap().mode,
            crate::state::ManagedWindowMode::Tiled
        );

        execute_action(&mut state, &mut dispatcher, CompositorAction::ToggleFullscreen);
        assert!(
            state
                .managed_root_surface_state(surface)
                .unwrap()
                .fullscreen
        );
        execute_action(&mut state, &mut dispatcher, CompositorAction::ToggleFullscreen);
        assert!(
            !state
                .managed_root_surface_state(surface)
                .unwrap()
                .fullscreen
        );
    }

    fn add_root(state: &mut State, raw: u32) -> Resolved<WlSurfaceId> {
        let (_, object_id) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(raw), ClientId::from_raw(1), 6));
        let surface = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(raw), object_id);
        state.register_root_surface(surface);
        insert_window(state, surface);
        surface
    }

    fn layout_mut<'a>(
        state: &'a mut State,
        surface: Resolved<WlSurfaceId>,
    ) -> &'a mut crate::tiling::WorkspaceLayout {
        let workspace_id = state.root_surface_workspace(surface).unwrap();
        &mut state.workspaces.try_get_mut(workspace_id).unwrap().layout
    }

    fn place_tiled(state: &mut State, surface: Resolved<WlSurfaceId>) {
        let window = state.window_for_root(surface).unwrap();
        let layout = layout_mut(state, surface);
        if !layout.is_tiled_window(window) {
            assert!(layout.insert_tiled(window));
        }
    }

    fn place_floating(state: &mut State, surface: Resolved<WlSurfaceId>) {
        let window = state.window_for_root(surface).unwrap();
        {
            let layout = layout_mut(state, surface);
            if !layout.contains_window(window) {
                assert!(layout.insert_tiled(window));
            }
            if !layout.is_floating_window(window) {
                assert!(layout.set_floating(window, true));
            }
        }
        state.managed_window_mut(surface).unwrap().mode = crate::state::ManagedWindowMode::Floating;
    }

    fn scene_orders(
        state: &State,
        surface: Resolved<WlSurfaceId>,
    ) -> (Vec<Resolved<WlSurfaceId>>, Vec<Resolved<WlSurfaceId>>) {
        let workspace_id = state.root_surface_workspace(surface).unwrap();
        let layout = &state.workspaces.try_get(workspace_id).unwrap().layout;
        (
            crate::tiling::tiled_scene_order(state, layout),
            crate::tiling::floating_scene_order(state, layout),
        )
    }

    fn keyboard_surface(state: &State) -> Option<Resolved<WlSurfaceId>> {
        state.seat.keyboard_focus.map(|focus| focus.surface)
    }

    #[test]
    fn focus_cycle_on_one_window_stays_put() {
        let mut state = state_for_tests();
        let surface = focus_managed_root(&mut state);
        place_tiled(&mut state, surface);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(keyboard_surface(&state), Some(surface));
        assert_eq!(scene_orders(&state, surface).0, vec![surface]);

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(keyboard_surface(&state), Some(surface));
        assert_eq!(scene_orders(&state, surface).0, vec![surface]);
    }

    #[test]
    fn focus_cycle_rotates_floating_windows_without_bouncing() {
        let mut state = state_for_tests();
        let first = focus_managed_root(&mut state);
        let second = add_root(&mut state, 42);
        let third = add_root(&mut state, 43);
        let fourth = add_root(&mut state, 44);
        for surface in [first, second, third, fourth] {
            place_floating(&mut state, surface);
        }
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, first).1, vec![second, third, fourth, first]);
        assert_eq!(keyboard_surface(&state), Some(first));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, first).1, vec![third, fourth, first, second]);
        assert_eq!(keyboard_surface(&state), Some(second));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, first).1, vec![fourth, first, second, third]);
        assert_eq!(keyboard_surface(&state), Some(third));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, first).1, vec![first, second, third, fourth]);
        assert_eq!(keyboard_surface(&state), Some(fourth));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, first).1, vec![fourth, first, second, third]);
        assert_eq!(keyboard_surface(&state), Some(third));
    }

    #[test]
    fn focus_cycle_two_windows_swaps_in_either_direction() {
        let mut state = state_for_tests();
        let back = focus_managed_root(&mut state);
        let front = add_root(&mut state, 42);
        place_floating(&mut state, back);
        place_floating(&mut state, front);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, back).1, vec![front, back]);
        assert_eq!(keyboard_surface(&state), Some(back));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, back).1, vec![back, front]);
        assert_eq!(keyboard_surface(&state), Some(front));
    }

    #[test]
    fn focus_cycle_keeps_tiled_and_floating_in_separate_bands() {
        let mut state = state_for_tests();
        let tiled_back = focus_managed_root(&mut state);
        let tiled_front = add_root(&mut state, 42);
        let floating_back = add_root(&mut state, 43);
        let floating_front = add_root(&mut state, 44);
        place_tiled(&mut state, tiled_back);
        place_tiled(&mut state, tiled_front);
        place_floating(&mut state, floating_back);
        place_floating(&mut state, floating_front);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, tiled_back).0, vec![tiled_front, tiled_back]);
        assert_eq!(scene_orders(&state, tiled_back).1, vec![floating_back, floating_front]);
        assert_eq!(keyboard_surface(&state), Some(tiled_back));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusNext);
        assert_eq!(scene_orders(&state, tiled_back).0, vec![tiled_back, tiled_front]);
        assert_eq!(keyboard_surface(&state), Some(tiled_front));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, tiled_back).0, vec![tiled_back, tiled_front]);
        assert_eq!(scene_orders(&state, tiled_back).1, vec![floating_front, floating_back]);
        assert_eq!(keyboard_surface(&state), Some(floating_back));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, tiled_back).1, vec![floating_back, floating_front]);
        assert_eq!(keyboard_surface(&state), Some(floating_front));
    }

    #[test]
    fn focus_cycle_rotates_tiled_windows_when_nothing_floats() {
        let mut state = state_for_tests();
        let first = focus_managed_root(&mut state);
        let second = add_root(&mut state, 42);
        let third = add_root(&mut state, 43);
        place_tiled(&mut state, first);
        place_tiled(&mut state, second);
        place_tiled(&mut state, third);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, first).0, vec![third, first, second]);
        assert_eq!(keyboard_surface(&state), Some(second));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, first).0, vec![second, third, first]);
        assert_eq!(keyboard_surface(&state), Some(first));

        execute_action(&mut state, &mut dispatcher, CompositorAction::FocusPrevious);
        assert_eq!(scene_orders(&state, first).0, vec![first, second, third]);
        assert_eq!(keyboard_surface(&state), Some(third));
    }

    fn group_workspaces(state: &State, output: OutputId) -> Vec<crate::state::WorkspaceId> {
        let group_id = state.workspace_group_for_output(output).unwrap();
        state
            .workspace_groups
            .try_get(group_id)
            .unwrap()
            .workspaces
            .clone()
    }

    fn group_transition(
        state: &State,
        output: OutputId,
    ) -> Option<crate::visual_animation::WorkspaceGroupTransition> {
        let group_id = state.workspace_group_for_output(output).unwrap();
        state.workspace_groups.try_get(group_id).unwrap().transition
    }

    fn output_frame_requested(dispatcher: &Dispatcher, output: OutputId) -> bool {
        dispatcher.test_has_queued_action_for(
            crate::scheduler::output_scheduler::output_scheduler_id(output),
            |action: &crate::scheduler::output_scheduler::OutputSchedulerAction| {
                matches!(
                    action,
                    crate::scheduler::output_scheduler::OutputSchedulerAction::RequestFrame { .. }
                )
            },
        )
    }

    #[test]
    fn workspace_index_slides_within_the_active_output_group() {
        let mut state = state_for_tests();
        state.config.animations.set_workspace_duration_msec(250);
        let (left, _surface) = insert_managed_root(&mut state);
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let right = state.insert_output(test_output_with_crtc_name(backend, 2, 100, 0, 100, 100));
        let workspaces = group_workspaces(&state, left);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceIndex { index: 1 });
        assert_eq!(state.active_workspace_for_output(left), Some(workspaces[0]));
        assert!(group_transition(&state, left).is_none());
        assert!(!output_frame_requested(&dispatcher, left));

        execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceIndex { index: 0 });
        assert_eq!(state.active_workspace_for_output(left), Some(workspaces[0]));

        for index in 2..=4 {
            execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceIndex { index });
            let slot = (index - 1) as usize;
            assert_eq!(state.active_workspace_for_output(left), Some(workspaces[slot]));
            let transition = group_transition(&state, left).unwrap();
            assert_eq!(
                transition.direction(),
                crate::visual_animation::WorkspaceTransitionDirection::Next
            );
            assert_eq!(transition.to_workspace(), workspaces[slot]);
        }
        let stayed = group_transition(&state, left).unwrap();
        execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceIndex { index: 4 });
        let still = group_transition(&state, left).unwrap();
        assert_eq!(still.from_workspace(), stayed.from_workspace());
        assert_eq!(still.to_workspace(), stayed.to_workspace());
        assert_eq!(still.direction(), stayed.direction());

        execute_action(&mut state, &mut dispatcher, CompositorAction::WorkspaceIndex { index: 1 });
        let back = group_transition(&state, left).unwrap();
        assert_eq!(
            back.direction(),
            crate::visual_animation::WorkspaceTransitionDirection::Previous
        );
        assert_eq!(back.from_workspace(), workspaces[3]);
        assert_eq!(back.to_workspace(), workspaces[0]);
        assert!(output_frame_requested(&dispatcher, left));
        assert!(!output_frame_requested(&dispatcher, right));
    }

    #[test]
    fn move_to_workspace_follows_the_window_and_keeps_its_mode() {
        let mut state = state_for_tests();
        state.config.animations.set_workspace_duration_msec(250);
        let surface = focus_managed_root(&mut state);
        assert!(state.set_root_surface_floating(surface, true));
        assert!(state.set_root_surface_fullscreen(surface, true));
        let source = state.root_surface_workspace(surface).unwrap();
        let window = state.window_for_root(surface).unwrap();
        state
            .workspaces
            .try_get_mut(source)
            .unwrap()
            .layout
            .set_fullscreen(Some(window));
        let output = state.workspace_output_id(source).unwrap();
        let workspaces = group_workspaces(&state, output);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::MoveToWorkspace { index: 4 });
        assert_eq!(state.root_surface_workspace(surface), Some(workspaces[3]));
        assert_eq!(state.active_workspace_for_output(output), Some(workspaces[3]));
        assert_eq!(keyboard_surface(&state), Some(surface));
        let managed = state.managed_root_surface_state(surface).unwrap();
        assert_eq!(managed.mode, crate::state::ManagedWindowMode::Floating);
        assert!(managed.fullscreen);
        assert!(state
            .workspaces
            .try_get(source)
            .unwrap()
            .layout
            .fullscreen
            .is_none());
        let transition = group_transition(&state, output).unwrap();
        assert_eq!(
            transition.direction(),
            crate::visual_animation::WorkspaceTransitionDirection::Next
        );
        assert_eq!(transition.from_workspace(), workspaces[0]);
        assert_eq!(transition.to_workspace(), workspaces[3]);

        execute_action(&mut state, &mut dispatcher, CompositorAction::MoveToWorkspace { index: 4 });
        assert_eq!(state.root_surface_workspace(surface), Some(workspaces[3]));
        assert_eq!(state.active_workspace_for_output(output), Some(workspaces[3]));
        assert_eq!(keyboard_surface(&state), Some(surface));
        let again = group_transition(&state, output).unwrap();
        assert_eq!(again.from_workspace(), transition.from_workspace());
        assert_eq!(again.to_workspace(), transition.to_workspace());
    }

    #[test]
    fn move_to_workspace_publishes_the_source_and_destination_outputs() {
        let mut state = state_for_tests();
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let left = state.insert_output(test_output_with_crtc_name(backend, 1, 0, 0, 100, 100));
        let right = state.insert_output(test_output_with_crtc_name(backend, 2, 100, 0, 100, 100));
        let (_, object_id) =
            state.insert_object(WlSurface::new(WlSurfaceId::from(51u32), ClientId::from_raw(1), 6));
        let surface = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(51u32), object_id);
        state.register_root_surface(surface);
        insert_window(&mut state, surface);
        state.seat.keyboard_focus = Some(SurfaceFocus::new(surface));
        let source = state.root_surface_workspace(surface).unwrap();
        state.seat.pointer_pos = (150.0, 10.0);
        let right_workspaces = group_workspaces(&state, right);
        let mut dispatcher = Dispatcher::default();

        execute_action(&mut state, &mut dispatcher, CompositorAction::MoveToWorkspace { index: 1 });
        assert_eq!(state.root_surface_workspace(surface), Some(right_workspaces[0]));
        assert_eq!(state.active_workspace_for_output(left), Some(source));
        assert_eq!(state.active_workspace_for_output(right), Some(right_workspaces[0]));
        assert_eq!(keyboard_surface(&state), Some(surface));
        assert!(output_frame_requested(&dispatcher, left));
        assert!(output_frame_requested(&dispatcher, right));
    }

    #[test]
    fn a_cursor_size_change_asks_the_xwm_to_refresh_hints() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        state.config.general.cursor_size = 32;
        apply_general_settings(&mut state, &mut dispatcher);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::RefreshHints)
            }),
            1
        );
        let _ = dispatcher.test_take_queued_actions::<crate::xwayland::XwaylandAction>();
        apply_general_settings(&mut state, &mut dispatcher);
        assert_eq!(
            dispatcher.test_count_queued_action::<crate::xwayland::XwaylandAction>(|action| {
                matches!(action, crate::xwayland::XwaylandAction::RefreshHints)
            }),
            0
        );
    }
}
