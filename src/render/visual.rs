use std::rc::Rc;

use crate::{
    config::{RgbaColor, RoundingPower, ShadowPower},
    region::{DamageRegion, RectI, RegionValue},
    state::{FractionalScale, RootSurfaceClip},
    transform::Mat3,
};

/// Rectangles kept for one client blur region. Waybar rasterises rounded
/// corners as 1px spans, about two per radius pixel, so a bar stays well
/// under this. Past it, rows collapse into bands.
pub const BLUR_REGION_RECT_CAP: usize = 256;

/// Rounded-rect clip applied to a window's root-surface tree.
///
/// The kind decides how pixels outside the rounded box are treated:
/// - [`WindowClip::Frame`] hard-clips to the compositor-owned (SSD) frame.
/// - [`WindowClip::Corners`] only discards pixels inside the window box that
///   fall outside a rounded corner, so pixels outside the box — for example
///   client-drawn CSD shadows — are kept. This matches how Hyprland rounds
///   corners.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowClip {
    Frame(RootSurfaceClip),
    Corners(RootSurfaceClip),
}

impl WindowClip {
    pub fn clip(self) -> RootSurfaceClip {
        match self {
            Self::Frame(clip) | Self::Corners(clip) => clip,
        }
    }

    /// Move the clip to `outer_rect`, keeping the kind and the configured
    /// radius/power. Used when a geometry animation resizes the visual box.
    pub fn reposition(
        self,
        outer_rect: RectI,
        corner_radius: i32,
        rounding_power: RoundingPower,
    ) -> Self {
        let clip = RootSurfaceClip::new(outer_rect, corner_radius, rounding_power);
        match self {
            Self::Frame(_) => Self::Frame(clip),
            Self::Corners(_) => Self::Corners(clip),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorderGradient {
    pub start_color: RgbaColor,
    pub end_color: RgbaColor,
    pub angle_degrees: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorderVisualStyle {
    pub gradient: BorderGradient,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowVisualStyle {
    pub radius: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub color: RgbaColor,
    pub power: ShadowPower,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlurVisualStyle {
    pub radius: i32,
    pub passes: u8,
}

/// Client blur in global compositor coordinates.
///
/// Rects are clipped to the surface, sorted by `(y, x)`, non-overlapping, and
/// capped at [`BLUR_REGION_RECT_CAP`]. A rounded window clip is applied by the
/// caller before the cap, so a corner that the window itself discards is not
/// blurred.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlurRegion {
    pub bounds: RectI,
    pub rects: Rc<[RectI]>,
}

impl BlurRegion {
    /// `region` is surface-local. `(0, 0)` maps to `surface_rect_global`'s
    /// origin, which is the root texture's top-left. Anything outside that
    /// rectangle is dropped.
    pub fn from_surface_region(
        region: &RegionValue,
        surface_rect_global: RectI,
        max_rects: usize,
    ) -> Option<Self> {
        let local = RectI::new(0, 0, surface_rect_global.width, surface_rect_global.height)?;
        let mut damage = region.to_region_clipped(local);
        damage.translate(surface_rect_global.x, surface_rect_global.y);
        damage.intersect_rect(surface_rect_global);
        Self::from_damage(&damage, max_rects)
    }

    pub fn from_damage(damage: &DamageRegion, max_rects: usize) -> Option<Self> {
        blur_region_from_rects(damage.rects().iter().copied(), max_rects)
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Self {
        let rects = self
            .rects
            .iter()
            .map(|rect| rect.translate(dx, dy))
            .collect::<Vec<_>>();
        Self {
            bounds: self.bounds.translate(dx, dy),
            rects: Rc::from(rects),
        }
    }

    pub fn scaled(&self, scale: FractionalScale) -> Option<Self> {
        if scale.is_one() {
            return Some(self.clone());
        }
        let mut rects = Vec::with_capacity(self.rects.len());
        for rect in self.rects.iter() {
            rects.push(rect.scaled(scale)?);
        }
        blur_region_from_rects(rects, self.rects.len().max(1))
    }

    /// Same logical-to-visual map the scene applies to the root texture.
    pub fn mapped(&self, mapping: RectMapping) -> Option<Self> {
        blur_region_from_rects(
            self.rects
                .iter()
                .copied()
                .map(|rect| mapping.map_rect(rect)),
            BLUR_REGION_RECT_CAP,
        )
    }
}

/// Map the unit square onto `rect` inside a blurred texture that covers `bounds`.
pub fn blur_source_transform(bounds: RectI, rect: RectI) -> Mat3 {
    let width = bounds.width as f32;
    let height = bounds.height as f32;
    Mat3::translation((rect.x - bounds.x) as f32 / width, (rect.y - bounds.y) as f32 / height)
        .mul(Mat3::scale(rect.width as f32 / width, rect.height as f32 / height))
}

fn blur_region_from_rects(
    rects: impl IntoIterator<Item = RectI>,
    max_rects: usize,
) -> Option<BlurRegion> {
    if max_rects == 0 {
        return None;
    }
    let mut damage = DamageRegion::new();
    for rect in rects {
        damage.add_rect(rect);
    }
    if damage.is_empty() {
        return None;
    }
    let mut rects = damage.rects().to_vec();
    rects.sort_by_key(|rect| (rect.y, rect.x, rect.x2()));
    if rects.len() > max_rects {
        // Smallest band that keeps the result within the cap. `N` is
        // `ceil(span / max_rects)` rows. Each band becomes the bounding
        // rectangle of the pieces that intersect it, so adjacent bands share
        // an edge and do not overlap.
        rects = collapse_row_bands(&rects, max_rects);
    }
    let bounds = union_blur_rects(&rects)?;
    Some(BlurRegion { bounds, rects: Rc::from(rects) })
}

fn collapse_row_bands(rects: &[RectI], max_rects: usize) -> Vec<RectI> {
    let y1 = rects.iter().map(|rect| rect.y).min().unwrap_or(0);
    let y2 = rects.iter().map(|rect| rect.y2()).max().unwrap_or(y1);
    let span = (y2.saturating_sub(y1)).max(1) as usize;
    let band_rows = span.div_ceil(max_rects.max(1)).max(1) as i32;
    let mut collapsed = Vec::new();
    let mut y = y1;
    while y < y2 {
        let y_end = y.saturating_add(band_rows).min(y2);
        let mut pieces = Vec::new();
        for rect in rects {
            let top = rect.y.max(y);
            let bottom = rect.y2().min(y_end);
            if bottom > top {
                if let Some(piece) = RectI::new(rect.x, top, rect.width, bottom - top) {
                    pieces.push(piece);
                }
            }
        }
        if let Some(bounds) = union_blur_rects(&pieces) {
            collapsed.push(bounds);
        }
        y = y_end;
    }
    collapsed
}

fn union_blur_rects(rects: &[RectI]) -> Option<RectI> {
    let mut iter = rects.iter().copied().filter(|rect| !rect.is_empty());
    let first = iter.next()?;
    let mut acc = first;
    for rect in iter {
        acc = RectI::from_extents(
            acc.x.min(rect.x),
            acc.y.min(rect.y),
            acc.x2().max(rect.x2()),
            acc.y2().max(rect.y2()),
        )?;
    }
    Some(acc)
}

/// Drop the pixels a rounded window clip discards.
///
/// `Frame` hard-clips to the rounded box. `Corners` only removes the pixels
/// inside the box that fall outside a rounded corner, leaving anything drawn
/// outside the box (client shadows) in place.
pub fn clip_damage_to_window_clip(damage: &mut DamageRegion, clip: WindowClip) {
    let rounded = clip.clip();
    if matches!(clip, WindowClip::Frame(_)) {
        damage.intersect_rect(rounded.outer_rect);
    }
    subtract_rounded_corner_outsides(damage, rounded);
}

fn subtract_rounded_corner_outsides(damage: &mut DamageRegion, clip: RootSurfaceClip) {
    let radius = clip.corner_radius;
    if radius <= 0 || damage.is_empty() {
        return;
    }
    let outer = clip.outer_rect;
    let right = outer.x2().saturating_sub(radius);
    let bottom = outer.y2().saturating_sub(radius);
    let corners = [
        (outer.x, outer.y, true),
        (right, outer.y, false),
        (outer.x, bottom, true),
        (right, bottom, false),
    ];
    for (cx, cy, from_left) in corners {
        let Some(corner) = RectI::new(cx, cy, radius, radius) else {
            continue;
        };
        if !damage.intersects_rect(corner) {
            continue;
        }
        let x_end = cx.saturating_add(radius);
        for y in cy..cy.saturating_add(radius) {
            let outside = if from_left {
                let mut x = cx;
                while x < x_end && !clip.contains_point(f64::from(x) + 0.5, f64::from(y) + 0.5) {
                    x += 1;
                }
                RectI::new(cx, y, x.saturating_sub(cx), 1)
            } else {
                let mut x = x_end;
                while x > cx && !clip.contains_point(f64::from(x - 1) + 0.5, f64::from(y) + 0.5) {
                    x -= 1;
                }
                RectI::new(x, y, x_end.saturating_sub(x), 1)
            };
            if let Some(outside) = outside {
                damage.subtract_rect(outside);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoundedRing {
    pub outer: RootSurfaceClip,
    pub inner: RootSurfaceClip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceEffectStyle {
    pub blur: Option<BlurVisualStyle>,
    pub opacity_alpha: u8,
    pub dim_alpha: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootSurfaceVisualState {
    pub outer_rect: RectI,
    pub content_rect: RectI,
    /// Client-surface clip. The radius is `decorations.rounding` on the content rect.
    pub clip: Option<WindowClip>,
    /// Outer frame, with Hyprland's outer corner radius. Shadow and dim use it.
    pub frame_clip: RootSurfaceClip,
    pub border: Option<BorderVisualStyle>,
    pub border_alpha: u8,
    pub shadow: Option<ShadowVisualStyle>,
    pub shadow_alpha: u8,
    pub effects: SurfaceEffectStyle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowGeometry {
    pub bounds: RectI,
    pub inner_rect: RectI,
    pub shadow_range: i32,
}

/// Outer corner radius from `CHyprBorderDecoration`.
///
/// `rounding + border_size - border_size * (sqrt(2) - 1) * max(2 - power, 0)`.
/// With the default power of 2 the correction is zero.
pub fn border_outer_radius(rounding: i32, border_size: i32, rounding_power: RoundingPower) -> i32 {
    let rounding = rounding.max(0);
    let border_size = border_size.max(0);
    let correction = f64::from(border_size)
        * (std::f64::consts::SQRT_2 - 1.0)
        * (2.0 - f64::from(rounding_power.get())).max(0.0);
    round_to_i32(f64::from(rounding.saturating_add(border_size)) - correction).max(0)
}

/// Shadow corner radius. Hyprland uses the outer radius only when `rounding` is positive.
pub fn shadow_corner_radius(rounding: i32, border_size: i32, rounding_power: RoundingPower) -> i32 {
    if rounding <= 0 {
        0
    } else {
        border_outer_radius(rounding, border_size, rounding_power)
    }
}

/// Border ring. The inner radius is `rounding`; the outer radius is [`border_outer_radius`].
pub fn rounded_ring(
    outer_rect: RectI,
    inner_rect: RectI,
    rounding: i32,
    border_size: i32,
    rounding_power: RoundingPower,
) -> RoundedRing {
    RoundedRing {
        outer: RootSurfaceClip::new(
            outer_rect,
            border_outer_radius(rounding, border_size, rounding_power),
            rounding_power,
        ),
        inner: RootSurfaceClip::new(inner_rect, rounding.max(0), rounding_power),
    }
}

pub fn shadow_geometry(clip: RootSurfaceClip, style: ShadowVisualStyle) -> Option<ShadowGeometry> {
    let shadow_range = style.radius.max(1);
    let bounds = RectI::new(
        clip.outer_rect
            .x
            .saturating_add(style.offset_x.saturating_sub(shadow_range)),
        clip.outer_rect
            .y
            .saturating_add(style.offset_y.saturating_sub(shadow_range)),
        clip.outer_rect
            .width
            .saturating_add(shadow_range.saturating_mul(2)),
        clip.outer_rect
            .height
            .saturating_add(shadow_range.saturating_mul(2)),
    )?;
    let inner_rect = RectI::new(
        clip.outer_rect.x.saturating_sub(bounds.x),
        clip.outer_rect.y.saturating_sub(bounds.y),
        clip.outer_rect.width,
        clip.outer_rect.height,
    )?;
    Some(ShadowGeometry { bounds, inner_rect, shadow_range })
}

/// One rectangle mapped onto another.
///
/// `None` on [`WindowRenderMapping`] means identity. A mapping exists only when
/// the two rectangles differ and the logical one has a positive size, which is
/// the guard the renderer and pointer code used before this type existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectMapping {
    pub logical: RectI,
    pub visual: RectI,
}

impl RectMapping {
    /// `None` when the rectangles are equal or `logical` cannot be a scale source.
    pub fn between(logical: RectI, visual: RectI) -> Option<Self> {
        (logical != visual && logical.width > 0 && logical.height > 0)
            .then_some(Self { logical, visual })
    }

    pub fn map_rect(self, rect: RectI) -> RectI {
        map_layout_rect(self.logical, self.visual, rect)
    }

    pub fn map_point(self, point: (f64, f64)) -> (f64, f64) {
        map_layout_point(self.logical, self.visual, point)
    }

    pub fn unmap_point(self, point: (f64, f64)) -> (f64, f64) {
        unmap_layout_point(self.logical, self.visual, point)
    }
}

/// Logical-to-visual rectangles for one window.
///
/// The frame pair moves the border and shadow. The content pair moves the
/// client surfaces and pointer coordinates. Hyprland stores the animated box
/// on the window (`m_realPosition` / `m_realSize`); sway leaves a tiled buffer
/// at the content origin and does not stretch it. The stretch here is the
/// extra step while the client has not answered a size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowRenderMapping {
    /// Border, shadow, rounded clip: tile frame -> visual frame.
    pub frame: Option<RectMapping>,
    /// Surfaces of the root tree and pointer coordinates:
    /// client content -> visual content.
    pub content: Option<RectMapping>,
}

/// Map `rect` from the logical layout rectangle into the sampled visual rectangle.
///
/// The layout rectangle itself maps exactly. Other rectangles, including popups that stick out,
/// scale around its origin.
pub fn map_layout_rect(logical: RectI, visual: RectI, rect: RectI) -> RectI {
    if logical.width <= 0 || logical.height <= 0 || rect == logical {
        return if rect == logical { visual } else { rect };
    }
    let scale_x = f64::from(visual.width) / f64::from(logical.width);
    let scale_y = f64::from(visual.height) / f64::from(logical.height);
    let x = f64::from(visual.x) + (f64::from(rect.x) - f64::from(logical.x)) * scale_x;
    let y = f64::from(visual.y) + (f64::from(rect.y) - f64::from(logical.y)) * scale_y;
    RectI::new(
        round_to_i32(x),
        round_to_i32(y),
        round_to_i32(f64::from(rect.width) * scale_x).max(0),
        round_to_i32(f64::from(rect.height) * scale_y).max(0),
    )
    .unwrap_or(rect)
}

/// Map a global point from the logical layout rectangle into the sampled visual rectangle.
pub fn map_layout_point(logical: RectI, visual: RectI, point: (f64, f64)) -> (f64, f64) {
    let Some((scale_x, scale_y)) = layout_scale(logical, visual) else {
        return point;
    };
    (
        f64::from(visual.x) + (point.0 - f64::from(logical.x)) * scale_x,
        f64::from(visual.y) + (point.1 - f64::from(logical.y)) * scale_y,
    )
}

/// Map a global point from the sampled visual rectangle back into the logical layout rectangle.
pub fn unmap_layout_point(logical: RectI, visual: RectI, point: (f64, f64)) -> (f64, f64) {
    let Some((scale_x, scale_y)) = layout_scale(logical, visual) else {
        return point;
    };
    if scale_x == 0.0 || scale_y == 0.0 {
        return point;
    }
    (
        f64::from(logical.x) + (point.0 - f64::from(visual.x)) / scale_x,
        f64::from(logical.y) + (point.1 - f64::from(visual.y)) / scale_y,
    )
}

fn layout_scale(logical: RectI, visual: RectI) -> Option<(f64, f64)> {
    if logical.width <= 0 || logical.height <= 0 || logical == visual {
        return None;
    }
    Some((
        f64::from(visual.width) / f64::from(logical.width),
        f64::from(visual.height) / f64::from(logical.height),
    ))
}

/// Move a window's decorations onto `visual_layout` without scaling their pixel thickness.
///
/// The client surfaces use [`map_layout_rect`]. The border inset, corner radius, and shadow spread
/// stay in pixels, matching the way Hyprland redraws chrome around `m_realSize`.
pub fn place_visual_frame(
    visual: &mut RootSurfaceVisualState,
    logical_layout: RectI,
    visual_layout: RectI,
) {
    if logical_layout.width <= 0 || logical_layout.height <= 0 || logical_layout == visual_layout {
        return;
    }
    let outer = visual.outer_rect;
    let content = visual.content_rect;
    let visual_outer = map_layout_rect(logical_layout, visual_layout, outer);
    let left = content.x.saturating_sub(outer.x);
    let top = content.y.saturating_sub(outer.y);
    let right = outer.x2().saturating_sub(content.x2());
    let bottom = outer.y2().saturating_sub(content.y2());
    let content_inside = content.x >= outer.x
        && content.y >= outer.y
        && content.x2() <= outer.x2()
        && content.y2() <= outer.y2();
    visual.outer_rect = visual_outer;
    visual.content_rect = if content_inside {
        inset_rect(visual_outer, left, top, right, bottom).unwrap_or(visual_outer)
    } else {
        map_layout_rect(logical_layout, visual_layout, content)
    };
    if let Some(clip) = visual.clip {
        let radius = clip.clip().corner_radius;
        let power = clip.clip().rounding_power;
        visual.clip = Some(clip.reposition(visual.content_rect, radius, power));
    }
    let frame_radius = visual.frame_clip.corner_radius;
    let frame_power = visual.frame_clip.rounding_power;
    visual.frame_clip = RootSurfaceClip::new(visual.outer_rect, frame_radius, frame_power);
}

fn inset_rect(outer: RectI, left: i32, top: i32, right: i32, bottom: i32) -> Option<RectI> {
    let width = outer.width.saturating_sub(left.saturating_add(right));
    let height = outer.height.saturating_sub(top.saturating_add(bottom));
    if width <= 0 || height <= 0 {
        return None;
    }
    RectI::new(outer.x.saturating_add(left), outer.y.saturating_add(top), width, height)
}

fn round_to_i32(value: f64) -> i32 {
    let rounded = value.round();
    if rounded >= f64::from(i32::MAX) {
        i32::MAX
    } else if rounded <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        rounded as i32
    }
}

impl RootSurfaceVisualState {
    /// True when an opaque border is drawn over the edge of the client clip.
    ///
    /// The ring's inner shape is the content clip's shape
    /// (`push_root_surface_effect_elements`), so the clip can fade 1px further
    /// out and stay hidden under the border.
    pub fn border_covers_clip_edge(self) -> bool {
        let Some(WindowClip::Frame(clip)) = self.clip else {
            return false;
        };
        let Some(border) = self.border else {
            return false;
        };
        self.border_alpha == u8::MAX
            && border.gradient.start_color.a == u8::MAX
            && border.gradient.end_color.a == u8::MAX
            && clip.outer_rect == self.content_rect
    }

    /// Clip for the drop shadow. A square shadow when `rounding` is zero, otherwise the outer frame.
    pub fn shadow_clip(self) -> RootSurfaceClip {
        if self.clip.is_some() {
            self.frame_clip
        } else {
            RootSurfaceClip::new(self.frame_clip.outer_rect, 0, self.frame_clip.rounding_power)
        }
    }

    pub fn has_visible_decoration_ring(self) -> bool {
        self.outer_rect != self.content_rect
    }

    pub fn influence_bounds(self) -> RectI {
        let mut x1 = self.outer_rect.x;
        let mut y1 = self.outer_rect.y;
        let mut x2 = self.outer_rect.x2();
        let mut y2 = self.outer_rect.y2();
        let shadow_clip = self.shadow_clip();
        if let Some(shadow) = self.shadow {
            if let Some(shadow_geometry) = shadow_geometry(shadow_clip, shadow) {
                x1 = x1.min(shadow_geometry.bounds.x);
                y1 = y1.min(shadow_geometry.bounds.y);
                x2 = x2.max(shadow_geometry.bounds.x2());
                y2 = y2.max(shadow_geometry.bounds.y2());
            }
        }
        RectI::from_extents(x1, y1, x2, y2).unwrap_or(self.outer_rect)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            border_outer_radius, rounded_ring, shadow_geometry, BlurVisualStyle, BorderGradient,
            BorderVisualStyle, RootSurfaceVisualState, RoundedRing, ShadowVisualStyle,
            SurfaceEffectStyle, WindowClip,
        },
        crate::{
            config::{RgbaColor, RoundingPower, ShadowPower},
            region::RectI,
            state::RootSurfaceClip,
        },
    };

    #[test]
    fn layout_point_mapping_round_trips_through_a_scaled_visual_rect() {
        let logical = RectI::new(10, 20, 100, 80).unwrap();
        let visual = RectI::new(40, 30, 50, 40).unwrap();
        let point = (60.0, 60.0);
        let mapped = super::map_layout_point(logical, visual, point);
        let restored = super::unmap_layout_point(logical, visual, mapped);
        assert!((restored.0 - point.0).abs() < 1e-9);
        assert!((restored.1 - point.1).abs() < 1e-9);
        assert!((mapped.0 - 65.0).abs() < 1e-9);
        assert!((mapped.1 - 50.0).abs() < 1e-9);
    }

    #[test]
    fn shadow_geometry_offsets_inner_rect_within_shifted_shadow_bounds() {
        let clip = RootSurfaceClip::new(
            RectI::new(100, 200, 300, 150).unwrap(),
            16,
            RoundingPower::DEFAULT,
        );
        let style = ShadowVisualStyle {
            radius: 12,
            offset_x: 4,
            offset_y: 8,
            color: RgbaColor::new(0, 0, 0, 128),
            power: ShadowPower::DEFAULT,
        };

        let geometry = shadow_geometry(clip, style).unwrap();

        assert_eq!(geometry.bounds, RectI::new(92, 196, 324, 174).unwrap());
        assert_eq!(geometry.inner_rect, RectI::new(8, 4, 300, 150).unwrap());
        assert_eq!(geometry.shadow_range, 12);
    }

    #[test]
    fn rounded_ring_uses_hyprland_inner_and_outer_radii() {
        let power = RoundingPower::DEFAULT;
        let outer = RectI::new(40, 50, 120, 80).unwrap();
        let inner = RectI::new(44, 54, 112, 72).unwrap();

        assert_eq!(
            rounded_ring(outer, inner, 10, 4, power),
            RoundedRing {
                outer: RootSurfaceClip::new(outer, 14, power),
                inner: RootSurfaceClip::new(inner, 10, power),
            }
        );
        assert_eq!(border_outer_radius(0, 4, power), 4);
        let tighter = RoundingPower::sanitize(1.0);
        let correction = 4.0 * (std::f64::consts::SQRT_2 - 1.0);
        assert_eq!(border_outer_radius(10, 4, tighter), (14.0 - correction).round() as i32);
    }

    #[test]
    fn influence_bounds_include_shadow_extent_without_clip() {
        let visual = RootSurfaceVisualState {
            outer_rect: RectI::new(100, 200, 300, 150).unwrap(),
            content_rect: RectI::new(100, 200, 300, 150).unwrap(),
            clip: None,
            frame_clip: RootSurfaceClip::new(
                RectI::new(100, 200, 300, 150).unwrap(),
                0,
                RoundingPower::DEFAULT,
            ),
            border: None,
            border_alpha: u8::MAX,
            shadow: Some(ShadowVisualStyle {
                radius: 10,
                offset_x: -3,
                offset_y: 6,
                color: RgbaColor::new(0, 0, 0, 128),
                power: ShadowPower::DEFAULT,
            }),
            shadow_alpha: u8::MAX,
            effects: SurfaceEffectStyle {
                blur: Some(BlurVisualStyle { radius: 8, passes: 2 }),
                opacity_alpha: u8::MAX,
                dim_alpha: 0,
            },
        };

        assert_eq!(visual.influence_bounds(), RectI::new(87, 196, 320, 170).unwrap());
    }

    #[test]
    fn visible_decoration_ring_detects_asymmetric_frame_growth() {
        let visual = RootSurfaceVisualState {
            outer_rect: RectI::new(100, 172, 300, 178).unwrap(),
            content_rect: RectI::new(100, 200, 300, 150).unwrap(),
            clip: None,
            frame_clip: RootSurfaceClip::new(
                RectI::new(100, 200, 300, 150).unwrap(),
                0,
                RoundingPower::DEFAULT,
            ),
            border: None,
            border_alpha: u8::MAX,
            shadow: None,
            shadow_alpha: u8::MAX,
            effects: SurfaceEffectStyle { blur: None, opacity_alpha: u8::MAX, dim_alpha: 0 },
        };

        assert!(visual.has_visible_decoration_ring());
    }

    #[test]
    fn layout_mapping_scales_attached_popups_and_keeps_the_frame_exact() {
        let logical = RectI::new(10, 20, 100, 80).unwrap();
        let visual = RectI::new(30, 40, 50, 40).unwrap();
        assert_eq!(super::map_layout_rect(logical, visual, logical), visual);
        let popup = RectI::new(110, 20, 20, 16).unwrap();
        assert_eq!(
            super::map_layout_rect(logical, visual, popup),
            RectI::new(80, 40, 10, 8).unwrap()
        );
    }

    #[test]
    fn placed_frame_keeps_border_thickness_and_corner_radius() {
        let mut visual = RootSurfaceVisualState {
            outer_rect: RectI::new(0, 0, 100, 80).unwrap(),
            content_rect: RectI::new(4, 6, 92, 70).unwrap(),
            clip: Some(WindowClip::Frame(RootSurfaceClip::new(
                RectI::new(4, 6, 92, 70).unwrap(),
                12,
                RoundingPower::DEFAULT,
            ))),
            frame_clip: RootSurfaceClip::new(
                RectI::new(0, 0, 100, 80).unwrap(),
                16,
                RoundingPower::DEFAULT,
            ),
            border: None,
            border_alpha: u8::MAX,
            shadow: None,
            shadow_alpha: u8::MAX,
            effects: SurfaceEffectStyle { blur: None, opacity_alpha: u8::MAX, dim_alpha: 0 },
        };
        let logical = visual.outer_rect;
        super::place_visual_frame(&mut visual, logical, RectI::new(10, 20, 40, 30).unwrap());
        assert_eq!(visual.outer_rect, RectI::new(10, 20, 40, 30).unwrap());
        assert_eq!(visual.content_rect, RectI::new(14, 26, 32, 20).unwrap());
        let clip = visual.clip.unwrap();
        assert_eq!(
            clip,
            WindowClip::Frame(RootSurfaceClip::new(
                RectI::new(14, 26, 32, 20).unwrap(),
                12,
                RoundingPower::DEFAULT,
            ))
        );
        assert_eq!(
            visual.frame_clip,
            RootSurfaceClip::new(RectI::new(10, 20, 40, 30).unwrap(), 16, RoundingPower::DEFAULT,)
        );
    }

    #[test]
    fn border_covers_clip_edge_needs_an_opaque_border_over_a_frame_clip() {
        let content_rect = RectI::new(4, 6, 92, 70).unwrap();
        let opaque_border = BorderVisualStyle {
            gradient: BorderGradient {
                start_color: RgbaColor::new(1, 2, 3, 255),
                end_color: RgbaColor::new(4, 5, 6, 255),
                angle_degrees: 0,
            },
        };
        let visual = RootSurfaceVisualState {
            outer_rect: RectI::new(0, 0, 100, 80).unwrap(),
            content_rect,
            clip: Some(WindowClip::Frame(RootSurfaceClip::new(
                content_rect,
                12,
                RoundingPower::DEFAULT,
            ))),
            frame_clip: RootSurfaceClip::new(
                RectI::new(0, 0, 100, 80).unwrap(),
                16,
                RoundingPower::DEFAULT,
            ),
            border: Some(opaque_border),
            border_alpha: u8::MAX,
            shadow: None,
            shadow_alpha: u8::MAX,
            effects: SurfaceEffectStyle { blur: None, opacity_alpha: u8::MAX, dim_alpha: 0 },
        };
        assert!(visual.border_covers_clip_edge());

        let translucent_border = RootSurfaceVisualState { border_alpha: 200, ..visual };
        assert!(!translucent_border.border_covers_clip_edge());

        let mut half_transparent_end = opaque_border;
        half_transparent_end.gradient.end_color = RgbaColor::new(4, 5, 6, 128);
        let transparent_end =
            RootSurfaceVisualState { border: Some(half_transparent_end), ..visual };
        assert!(!transparent_end.border_covers_clip_edge());

        let no_border = RootSurfaceVisualState { border: None, ..visual };
        assert!(!no_border.border_covers_clip_edge());

        let corners = RootSurfaceVisualState {
            clip: Some(WindowClip::Corners(RootSurfaceClip::new(
                content_rect,
                12,
                RoundingPower::DEFAULT,
            ))),
            ..visual
        };
        assert!(!corners.border_covers_clip_edge());

        let shifted =
            RootSurfaceVisualState { content_rect: RectI::new(5, 6, 92, 70).unwrap(), ..visual };
        assert!(!shifted.border_covers_clip_edge());
    }

    #[test]
    fn placed_frame_keeps_corner_clip_kind_for_csd_windows() {
        let mut visual = RootSurfaceVisualState {
            outer_rect: RectI::new(0, 0, 100, 80).unwrap(),
            content_rect: RectI::new(0, 0, 100, 80).unwrap(),
            clip: Some(WindowClip::Corners(RootSurfaceClip::new(
                RectI::new(0, 0, 100, 80).unwrap(),
                12,
                RoundingPower::DEFAULT,
            ))),
            frame_clip: RootSurfaceClip::new(
                RectI::new(0, 0, 100, 80).unwrap(),
                12,
                RoundingPower::DEFAULT,
            ),
            border: None,
            border_alpha: u8::MAX,
            shadow: None,
            shadow_alpha: u8::MAX,
            effects: SurfaceEffectStyle { blur: None, opacity_alpha: u8::MAX, dim_alpha: 0 },
        };
        let logical = visual.outer_rect;
        super::place_visual_frame(&mut visual, logical, RectI::new(10, 20, 40, 30).unwrap());
        let clip = visual.clip.unwrap();
        assert_eq!(
            clip,
            WindowClip::Corners(RootSurfaceClip::new(
                RectI::new(10, 20, 40, 30).unwrap(),
                12,
                RoundingPower::DEFAULT,
            ))
        );
        assert_eq!(
            visual.frame_clip,
            RootSurfaceClip::new(RectI::new(10, 20, 40, 30).unwrap(), 12, RoundingPower::DEFAULT,)
        );
    }

    fn covers(rects: &[RectI], x: i32, y: i32) -> bool {
        rects.iter().any(|rect| rect.contains(x, y))
    }

    #[test]
    fn blur_region_clips_translates_and_scales() {
        let surface = RectI::new(10, 20, 40, 30).unwrap();
        let mut spec = crate::region::RegionSpec::default();
        spec.add_rect(RectI::new(-5, -5, 100, 100).unwrap());
        let region = crate::region::RegionValue::from_spec(spec);
        let blur = super::BlurRegion::from_surface_region(&region, surface, 256).unwrap();
        assert_eq!(blur.rects.as_ref(), &[surface]);
        assert_eq!(blur.bounds, surface);

        let moved = blur.translate(3, -4);
        assert_eq!(moved.bounds, surface.translate(3, -4));
        assert_eq!(moved.rects.as_ref(), &[surface.translate(3, -4)]);

        let scaled = blur.scaled(crate::state::FractionalScale::TWO).unwrap();
        assert_eq!(scaled.bounds, RectI::new(20, 40, 80, 60).unwrap());
        assert_eq!(scaled.rects.as_ref(), &[RectI::new(20, 40, 80, 60).unwrap()]);
    }

    #[test]
    fn blur_region_collapses_past_the_rect_cap() {
        let mut spec = crate::region::RegionSpec::default();
        for y in 0..300 {
            spec.add_rect(RectI::new(0, y, 4, 1).unwrap());
        }
        let surface = RectI::new(0, 0, 4, 300).unwrap();
        let blur = super::BlurRegion::from_surface_region(
            &crate::region::RegionValue::from_spec(spec),
            surface,
            super::BLUR_REGION_RECT_CAP,
        )
        .unwrap();
        assert_eq!(blur.rects.len(), 150);
        assert!(blur.rects.len() <= super::BLUR_REGION_RECT_CAP);
        assert_eq!(blur.rects[0], RectI::new(0, 0, 4, 2).unwrap());
        assert_eq!(blur.rects[149], RectI::new(0, 298, 4, 2).unwrap());
        assert_eq!(blur.bounds, surface);
    }

    #[test]
    fn rounded_frame_clip_drops_the_corner_and_pixels_outside_the_box() {
        let outer = RectI::new(0, 0, 20, 20).unwrap();
        let clip = RootSurfaceClip::new(outer, 8, RoundingPower::DEFAULT);
        let mut frame = crate::region::DamageRegion::from_rect(RectI::new(-2, -2, 24, 24).unwrap());
        let mut corners = frame.clone();
        super::clip_damage_to_window_clip(&mut frame, WindowClip::Frame(clip));
        super::clip_damage_to_window_clip(&mut corners, WindowClip::Corners(clip));
        let frame = super::BlurRegion::from_damage(&frame, 256).unwrap();
        let corners = super::BlurRegion::from_damage(&corners, 256).unwrap();
        assert!(!covers(&frame.rects, 0, 0));
        assert!(!covers(&frame.rects, -1, 10));
        assert!(covers(&frame.rects, 10, 10));
        assert!(!covers(&corners.rects, 0, 0));
        assert!(covers(&corners.rects, -1, 10));
        assert!(covers(&corners.rects, 10, 10));
    }
}
