//! Logical pixels to physical pixels for one output scale.
//!
//! Geometry scales each edge with round-half-up (toward +∞) and then subtracts, so two
//! rects that share an edge in logical space still share it after scaling. Damage scales
//! outward: `floor` on the minimum edge, `ceil` on the maximum. Lengths use the same
//! rounding as edges. Zero stays zero, and a non-zero length keeps a magnitude of at least 1,
//! so a 1 px radius or shadow offset cannot disappear at a small scale.

use crate::{
    region::{DamageRegion, RectI},
    render::{BlurVisualStyle, RoundedRing, ShadowVisualStyle},
    state::{FractionalScale, RootSurfaceClip},
};

impl FractionalScale {
    pub(crate) const fn is_one(self) -> bool {
        self.as_v120() == Self::DENOMINATOR
    }

    /// `round(logical · scale)`, half going toward +∞.
    pub(crate) fn scale_edge(self, logical: i32) -> i32 {
        scale_div(logical, self.as_v120(), i128::from(Self::DENOMINATOR) / 2)
    }

    pub(crate) fn scale_edge_floor(self, logical: i32) -> i32 {
        scale_div(logical, self.as_v120(), 0)
    }

    pub(crate) fn scale_edge_ceil(self, logical: i32) -> i32 {
        scale_div(logical, self.as_v120(), i128::from(Self::DENOMINATOR) - 1)
    }

    /// `round(logical · scale)` for a fractional coordinate, half going toward +∞.
    ///
    /// An integer coordinate matches [`Self::scale_edge`].
    pub(crate) fn scale_coord(self, logical: f64) -> i32 {
        if !logical.is_finite() {
            return 0;
        }
        let denominator = i128::from(Self::DENOMINATOR);
        let logical_fixed = round_half_up_i128(logical * f64::from(Self::DENOMINATOR));
        let scaled = (logical_fixed * i128::from(self.as_v120()) + (denominator * denominator) / 2)
            .div_euclid(denominator * denominator);
        i32::try_from(scaled).unwrap_or(if scaled < 0 { i32::MIN } else { i32::MAX })
    }

    /// Rounded length. `0` stays `0`. Any other input keeps a magnitude of at least 1.
    pub(crate) fn scale_length(self, logical: i32) -> i32 {
        if logical == 0 {
            return 0;
        }
        let scaled = self.scale_edge(logical);
        if logical > 0 {
            scaled.max(1)
        } else {
            scaled.min(-1)
        }
    }
}

fn round_half_up_i128(value: f64) -> i128 {
    if !value.is_finite() {
        return 0;
    }
    let shifted = (value + 0.5).floor();
    if shifted >= i128::MAX as f64 {
        i128::MAX
    } else if shifted <= i128::MIN as f64 {
        i128::MIN
    } else {
        shifted as i128
    }
}

fn scale_div(logical: i32, numerator: u32, bias: i128) -> i32 {
    let denominator = i128::from(FractionalScale::DENOMINATOR);
    let scaled = (i128::from(logical) * i128::from(numerator) + bias).div_euclid(denominator);
    i32::try_from(scaled).unwrap_or(if scaled < 0 { i32::MIN } else { i32::MAX })
}

impl RectI {
    /// Scale both edges, then subtract. `None` only if a scaled edge does not fit in `i32`.
    pub(crate) fn scaled(self, scale: FractionalScale) -> Option<Self> {
        Self::from_extents(
            scale.scale_edge(self.x),
            scale.scale_edge(self.y),
            scale.scale_edge(self.x2()),
            scale.scale_edge(self.y2()),
        )
    }

    /// Pixel size of a window-local capture. The rect is scaled from its own top-left,
    /// so a 100×100 window is 200×200 at scale 2 wherever it sits.
    pub(crate) fn capture_pixel_size(self, scale: FractionalScale) -> Option<(i32, i32)> {
        let local = Self::new(0, 0, self.width, self.height)?;
        let physical = local.scaled(scale)?;
        Some((physical.width, physical.height))
    }

    /// Smallest physical rect that covers [`RectI::scaled`].
    pub(crate) fn scaled_out(self, scale: FractionalScale) -> Option<Self> {
        Self::from_extents(
            scale.scale_edge_floor(self.x),
            scale.scale_edge_floor(self.y),
            scale.scale_edge_ceil(self.x2()),
            scale.scale_edge_ceil(self.y2()),
        )
    }
}

impl DamageRegion {
    pub(crate) fn scaled(&self, scale: FractionalScale) -> Self {
        scale_region(self, |rect| rect.scaled(scale))
    }

    pub(crate) fn scaled_out(&self, scale: FractionalScale) -> Self {
        scale_region(self, |rect| rect.scaled_out(scale))
    }
}

fn scale_region(
    region: &DamageRegion,
    scale_rect: impl Fn(RectI) -> Option<RectI>,
) -> DamageRegion {
    let mut scaled = DamageRegion::new();
    for rect in region.rects() {
        if let Some(rect) = scale_rect(*rect) {
            scaled.add_rect(rect);
        }
    }
    scaled
}

impl RootSurfaceClip {
    /// Radius is scaled as a length and then clamped to the scaled rect, as [`Self::new`] does.
    pub(crate) fn scaled(self, scale: FractionalScale) -> Option<Self> {
        let outer_rect = self.outer_rect.scaled(scale)?;
        Some(Self::new(outer_rect, scale.scale_length(self.corner_radius), self.rounding_power))
    }
}

impl RoundedRing {
    pub(crate) fn scaled(self, scale: FractionalScale) -> Option<Self> {
        Some(Self {
            outer: self.outer.scaled(scale)?,
            inner: self.inner.scaled(scale)?,
        })
    }
}

impl ShadowVisualStyle {
    pub(crate) fn scaled(self, scale: FractionalScale) -> Self {
        Self {
            radius: scale.scale_length(self.radius),
            offset_x: scale.scale_length(self.offset_x),
            offset_y: scale.scale_length(self.offset_y),
            color: self.color,
            power: self.power,
        }
    }
}

impl BlurVisualStyle {
    pub(crate) fn scaled(self, scale: FractionalScale) -> Self {
        Self { radius: scale.scale_length(self.radius), passes: self.passes }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        config::{RgbaColor, RoundingPower, ShadowPower},
        region::{DamageRegion, RectI},
        render::{BlurVisualStyle, RoundedRing, ShadowVisualStyle},
        state::{FractionalScale, RootSurfaceClip},
    };

    fn rect(x: i32, y: i32, width: i32, height: i32) -> RectI {
        RectI::new(x, y, width, height).unwrap()
    }

    fn assert_covers(outer: RectI, inner: RectI) {
        assert!(outer.x <= inner.x);
        assert!(outer.y <= inner.y);
        assert!(outer.x2() >= inner.x2());
        assert!(outer.y2() >= inner.y2());
    }

    #[test]
    fn output_scale_one_is_identity() {
        let scale = FractionalScale::ONE;
        assert!(scale.is_one());
        assert!(!FractionalScale::TWO.is_one());
        let source = rect(-4, 2, 3, 5);
        assert_eq!(source.scaled(scale), Some(source));
        assert_eq!(source.scaled_out(scale), Some(source));
        assert_eq!(scale.scale_edge(-4), -4);
        assert_eq!(scale.scale_length(-4), -4);
        assert_eq!(scale.scale_length(0), 0);
    }

    #[test]
    fn output_scale_two_doubles() {
        let scale = FractionalScale::TWO;
        assert_eq!(scale.scale_edge(-7), -14);
        assert_eq!(scale.scale_length(3), 6);
        assert_eq!(scale.scale_length(-3), -6);
        assert_eq!(rect(1, -2, 4, 5).scaled(scale), Some(rect(2, -4, 8, 10)));
        assert_eq!(rect(10, 20, 100, 100).capture_pixel_size(scale), Some((200, 200)));
        assert_eq!(
            rect(10, 20, 100, 100).capture_pixel_size(FractionalScale::ONE),
            Some((100, 100))
        );
        assert_eq!(rect(1, -2, 4, 5).scaled_out(scale), Some(rect(2, -4, 8, 10)));
    }

    #[test]
    fn output_scale_fractional_rects_keep_a_shared_edge() {
        let left = rect(0, 0, 3, 3);
        let right = rect(3, 0, 3, 3);
        for scale in [FractionalScale::try_from_f64(1.25).unwrap(), FractionalScale::ONE_POINT_FIVE]
        {
            let left = left.scaled(scale).unwrap();
            let right = right.scaled(scale).unwrap();
            assert_eq!(left.x2(), right.x);
            assert_eq!(left.y2(), right.y2());
            assert!(left.width > 0 && right.width > 0);
        }
        assert_eq!(
            rect(0, 0, 3, 3).scaled(FractionalScale::try_from_f64(1.25).unwrap()),
            Some(rect(0, 0, 4, 4))
        );
        assert_eq!(
            rect(3, 0, 3, 3).scaled(FractionalScale::ONE_POINT_FIVE),
            Some(rect(5, 0, 4, 5))
        );
    }

    #[test]
    fn output_scale_damage_covers_rounded_geometry() {
        let scale = FractionalScale::try_from_f64(1.25).unwrap();
        let source = rect(-3, 2, 3, 4);
        let geometry = source.scaled(scale).unwrap();
        let damage = source.scaled_out(scale).unwrap();
        assert_eq!(geometry, rect(-4, 3, 4, 5));
        assert_eq!(damage, rect(-4, 2, 4, 6));
        assert_covers(damage, geometry);

        let mut region = DamageRegion::new();
        region.add_rect(source);
        region.add_rect(rect(8, -2, 2, 2));
        let scaled = region.scaled_out(scale);
        for (logical, physical) in region.rects().iter().zip(scaled.rects()) {
            assert_covers(*physical, logical.scaled(scale).unwrap());
        }
    }

    #[test]
    fn output_scale_negative_edges_round_half_toward_positive_infinity() {
        let scale = FractionalScale::ONE_POINT_FIVE;
        assert_eq!(scale.scale_edge(-1), -1);
        assert_eq!(scale.scale_edge_floor(-1), -2);
        assert_eq!(scale.scale_edge_ceil(-1), -1);
        assert_eq!(scale.scale_edge(1), 2);
        assert_eq!(scale.scale_coord(1.0), scale.scale_edge(1));
        assert_eq!(scale.scale_coord(10.4), 16);
        assert_eq!(FractionalScale::TWO.scale_coord(-1.5), -3);
        assert_eq!(rect(-4, -2, 3, 2).scaled(scale), Some(rect(-6, -3, 5, 3)));
    }

    #[test]
    fn output_scale_lengths_keep_a_minimum_magnitude() {
        let scale = FractionalScale::from_v120(48);
        assert_eq!(scale.scale_length(0), 0);
        assert_eq!(scale.scale_length(1), 1);
        assert_eq!(scale.scale_length(-1), -1);
        assert_eq!(FractionalScale::TWO.scale_length(1), 2);
    }

    #[test]
    fn output_scale_clips_rings_shadows_and_blur_scale_lengths() {
        let scale = FractionalScale::TWO;
        let clip = RootSurfaceClip::new(rect(1, 2, 10, 8), 3, RoundingPower::DEFAULT);
        let scaled_clip = clip.scaled(scale).unwrap();
        assert_eq!(scaled_clip.outer_rect, rect(2, 4, 20, 16));
        assert_eq!(scaled_clip.corner_radius, 6);
        assert_eq!(scaled_clip.rounding_power, RoundingPower::DEFAULT);

        let cramped = RootSurfaceClip::new(rect(0, 0, 4, 4), 2, RoundingPower::DEFAULT);
        assert_eq!(
            cramped
                .scaled(FractionalScale::try_from_f64(1.25).unwrap())
                .unwrap()
                .corner_radius,
            2
        );

        let ring = RoundedRing { outer: clip, inner: cramped }
            .scaled(scale)
            .unwrap();
        assert_eq!(ring.outer.outer_rect, rect(2, 4, 20, 16));
        assert_eq!(ring.inner.outer_rect, rect(0, 0, 8, 8));

        let shadow = ShadowVisualStyle {
            radius: 5,
            offset_x: -4,
            offset_y: 0,
            color: RgbaColor::new(1, 2, 3, 4),
            power: ShadowPower::DEFAULT,
        }
        .scaled(scale);
        assert_eq!(shadow.radius, 10);
        assert_eq!(shadow.offset_x, -8);
        assert_eq!(shadow.offset_y, 0);
        assert_eq!(shadow.power, ShadowPower::DEFAULT);

        let blur = BlurVisualStyle { radius: 5, passes: 3 }.scaled(scale);
        assert_eq!(blur.radius, 10);
        assert_eq!(blur.passes, 3);
        assert_eq!(
            BlurVisualStyle { radius: 0, passes: 2 }
                .scaled(scale)
                .radius,
            0
        );
    }
}
