//! Integer conversions between tema logical units and the Xwayland wire.
//!
//! `WireScale::ONE` makes every conversion the identity. On the way back the
//! rounding follows the M11 table: positions floor, sizes ceiling, damage
//! rectangles outward and opaque rectangles inward.

use crate::{
    config::XwaylandScalingMode,
    region::{RectI, RegionValue},
    state::FractionalScale,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireScale(u32);

// Phase 3 and later wire these helpers into clients, outputs and the XWM. This
// phase exercises all of them in tests.
#[cfg_attr(not(test), allow(dead_code))]
impl WireScale {
    pub const ONE: Self = Self(1);

    /// A zero scale is a one scale; every other value is used as-is.
    pub const fn new(scale: u32) -> Self {
        Self(if scale == 0 { 1 } else { scale })
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub const fn is_one(self) -> bool {
        self.0 == 1
    }

    fn divisor(self) -> i64 {
        i64::from(self.0)
    }

    pub fn to_wire(self, value: i32) -> i32 {
        to_i32(i64::from(value).saturating_mul(self.divisor()))
    }

    pub fn to_wire_f64(self, value: f64) -> f64 {
        value * f64::from(self.0)
    }

    pub fn size_to_wire(self, size: u32) -> u32 {
        size.saturating_mul(self.0)
    }

    pub fn rect_to_wire(self, rect: RectI) -> RectI {
        RectI {
            x: self.to_wire(rect.x),
            y: self.to_wire(rect.y),
            width: self.to_wire(rect.width),
            height: self.to_wire(rect.height),
        }
    }

    pub fn pos_to_logical(self, value: i32) -> i32 {
        to_i32(i64::from(value).div_euclid(self.divisor()))
    }

    pub fn size_to_logical(self, size: i32) -> i32 {
        to_i32(div_ceil(i64::from(size), self.divisor()))
    }

    pub fn size_to_logical_u32(self, size: u32) -> u32 {
        size.div_ceil(self.0)
    }

    /// One axis. `max` is in wire units; `min_logical` is the same axis' min hint,
    /// already converted with `size_to_logical` (0 when there is no min hint).
    pub fn max_size_to_logical(self, max: i32, min_logical: i32) -> i32 {
        max.div_euclid(self.get() as i32).max(min_logical)
    }

    pub fn rect_to_logical_outward(self, rect: RectI) -> RectI {
        let divisor = self.divisor();
        let x1 = i64::from(rect.x).div_euclid(divisor);
        let y1 = i64::from(rect.y).div_euclid(divisor);
        let x2 = div_ceil(i64::from(rect.x) + i64::from(rect.width), divisor);
        let y2 = div_ceil(i64::from(rect.y) + i64::from(rect.height), divisor);
        rect_from_extents(x1, y1, x2, y2)
    }

    pub fn rect_to_logical_inward(self, rect: RectI) -> Option<RectI> {
        let divisor = self.divisor();
        let x1 = div_ceil(i64::from(rect.x), divisor);
        let y1 = div_ceil(i64::from(rect.y), divisor);
        let x2 = (i64::from(rect.x) + i64::from(rect.width)).div_euclid(divisor);
        let y2 = (i64::from(rect.y) + i64::from(rect.height)).div_euclid(divisor);
        (x2 > x1 && y2 > y1).then(|| rect_from_extents(x1, y1, x2, y2))
    }

    pub fn to_logical_f64(self, value: f64) -> f64 {
        value / f64::from(self.0)
    }

    /// Input regions and pointer-constraint regions. Empty rectangles are
    /// dropped before the outward conversion so they do not grow into a pixel.
    /// `ONE` returns the region unchanged.
    pub fn region_to_logical_outward(self, region: RegionValue) -> RegionValue {
        if self.is_one() {
            return region;
        }
        region.map_rects(|rect| (!rect.is_empty()).then(|| self.rect_to_logical_outward(rect)))
    }
}

fn to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Ceiling division for a positive divisor. `int_roundings` is still unstable
/// on this toolchain, so this uses Euclidean division and its remainder.
fn div_ceil(value: i64, divisor: i64) -> i64 {
    let quotient = value.div_euclid(divisor);
    let remainder = value.rem_euclid(divisor);
    quotient + i64::from(remainder != 0)
}

fn rect_from_extents(x1: i64, y1: i64, x2: i64, y2: i64) -> RectI {
    let x1 = to_i32(x1);
    let y1 = to_i32(y1);
    let x2 = to_i32(x2);
    let y2 = to_i32(y2);
    RectI {
        x: x1,
        y: y1,
        width: x2.saturating_sub(x1),
        height: y2.saturating_sub(y1),
    }
}

/// `default` mode sends the wire at logical size. `downscaled` mode picks one
/// integer scale, the largest ceiling over every output, with no outputs giving 1.
pub fn xwayland_wire_scale(
    mode: XwaylandScalingMode,
    scales: impl Iterator<Item = FractionalScale>,
) -> WireScale {
    match mode {
        XwaylandScalingMode::Default => WireScale::ONE,
        XwaylandScalingMode::Downscaled => {
            let scale = scales
                .map(|scale| scale.ceil().max(1) as u32)
                .max()
                .unwrap_or(1);
            WireScale::new(scale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> RectI {
        RectI::new(x, y, width, height).unwrap()
    }

    fn scale(value: f64) -> FractionalScale {
        FractionalScale::try_from_f64(value).unwrap()
    }

    #[test]
    fn one_is_the_identity_for_every_helper() {
        let one = WireScale::ONE;
        assert_eq!(one.get(), 1);
        assert!(one.is_one());
        assert!(!WireScale::new(2).is_one());
        assert_eq!(WireScale::new(0), one);

        assert_eq!(one.to_wire(7), 7);
        assert_eq!(one.to_wire(-5), -5);
        assert_eq!(one.to_wire_f64(-2.25), -2.25);
        assert_eq!(one.size_to_wire(9), 9);
        assert_eq!(one.rect_to_wire(rect(3, -4, 5, 6)), rect(3, -4, 5, 6));
        assert_eq!(one.pos_to_logical(7), 7);
        assert_eq!(one.pos_to_logical(-5), -5);
        assert_eq!(one.size_to_logical(201), 201);
        assert_eq!(one.size_to_logical(-3), -3);
        assert_eq!(one.size_to_logical_u32(201), 201);
        assert_eq!(one.max_size_to_logical(201, 3), 201);
        assert_eq!(one.max_size_to_logical(3, 4), 4);
        assert_eq!(one.rect_to_logical_outward(rect(1, 1, 3, 3)), rect(1, 1, 3, 3));
        assert_eq!(one.rect_to_logical_inward(rect(1, 0, 2, 2)), Some(rect(1, 0, 2, 2)));
        assert_eq!(one.to_logical_f64(1.5), 1.5);
    }

    #[test]
    fn logical_to_wire_multiplies_exactly_and_saturates() {
        for s in [1, 2, 3] {
            let scale = WireScale::new(s);
            assert_eq!(scale.to_wire(7), 7 * s as i32);
            assert_eq!(scale.to_wire(-5), -5 * s as i32);
            assert_eq!(scale.to_wire(0), 0);
            assert_eq!(scale.to_wire_f64(1.5), 1.5 * f64::from(s));
            assert_eq!(scale.to_wire_f64(-2.25), -2.25 * f64::from(s));
            assert_eq!(scale.size_to_wire(9), 9 * s);
            assert_eq!(
                scale.rect_to_wire(rect(3, -4, 5, 6)),
                rect(3 * s as i32, -4 * s as i32, 5 * s as i32, 6 * s as i32),
            );
        }

        let huge = WireScale::new(u32::MAX);
        assert_eq!(huge.to_wire(2), i32::MAX);
        assert_eq!(huge.to_wire(-2), i32::MIN);
        assert_eq!(huge.size_to_wire(2), u32::MAX);
        assert_eq!(huge.rect_to_wire(rect(1, 1, 1, 1)).width, i32::MAX);
    }

    #[test]
    fn wire_to_logical_positions_floor_and_sizes_ceil() {
        let two = WireScale::new(2);
        let three = WireScale::new(3);
        for s in [1, 2, 3] {
            let scale = WireScale::new(s);
            assert_eq!(scale.pos_to_logical(7 * s as i32), 7);
            assert_eq!(scale.pos_to_logical(-5 * s as i32), -5);
            assert_eq!(scale.size_to_logical(7 * s as i32), 7);
            assert_eq!(scale.size_to_logical(-5 * s as i32), -5);
            assert_eq!(scale.size_to_logical_u32(201), (201 + s - 1) / s,);
            assert_eq!(scale.to_logical_f64(1.5 * f64::from(s)), 1.5);
            assert_eq!(scale.to_logical_f64(-2.25 * f64::from(s)), -2.25);
        }
        assert_eq!(two.pos_to_logical(-1), -1);
        assert_eq!(three.pos_to_logical(-1), -1);
        assert_eq!(two.pos_to_logical(1), 0);
        assert_eq!(three.pos_to_logical(1), 0);
        assert_eq!(two.size_to_logical(201), 101);
        assert_eq!(three.size_to_logical(201), 67);
        assert_eq!(two.size_to_logical(-3), -1);
        assert_eq!(two.size_to_logical(-4), -2);
        assert_eq!(two.size_to_logical(3), 2);
        assert_eq!(two.size_to_logical(201), 101);
        assert_eq!(WireScale::ONE.size_to_logical(-3), -3);
        assert_eq!(three.size_to_logical(-3), -1);
        assert_eq!(two.size_to_logical_u32(201), 101);
        assert_eq!(three.size_to_logical_u32(201), 67);
    }

    #[test]
    fn max_size_floors_but_never_drops_below_the_logical_minimum() {
        let one = WireScale::ONE;
        let two = WireScale::new(2);
        let three = WireScale::new(3);
        assert_eq!(one.max_size_to_logical(801, 200), 801);
        assert_eq!(one.max_size_to_logical(801, 0), 801);
        assert_eq!(one.max_size_to_logical(3, 4), 4);
        assert_eq!(two.max_size_to_logical(801, 0), 400);
        assert_eq!(two.max_size_to_logical(801, 200), 400);
        assert_eq!(two.max_size_to_logical(401, 201), 201);
        assert_eq!(three.max_size_to_logical(801, 0), 267);
        assert_eq!(three.max_size_to_logical(601, 0), 200);
        // Negative maxima follow floor division without a special case.
        assert_eq!(one.max_size_to_logical(-3, 0), 0);
        assert_eq!(two.max_size_to_logical(-3, 0), 0);
        assert_eq!(three.max_size_to_logical(-3, 0), 0);
    }

    #[test]
    fn outward_rects_cover_more_and_inward_rects_cover_less() {
        let one = WireScale::ONE;
        let two = WireScale::new(2);
        let three = WireScale::new(3);
        let damage = rect(1, 1, 3, 3);
        assert_eq!(one.rect_to_logical_outward(damage), damage);
        assert_eq!(two.rect_to_logical_outward(damage), rect(0, 0, 2, 2));
        assert_eq!(three.rect_to_logical_outward(damage), rect(0, 0, 2, 2));
        assert_eq!(two.rect_to_logical_outward(rect(-3, -5, 4, 6)), rect(-2, -3, 3, 4));

        let opaque = rect(1, 0, 2, 2);
        assert_eq!(one.rect_to_logical_outward(opaque), opaque);
        assert_eq!(one.rect_to_logical_inward(damage), Some(damage));
        assert_eq!(two.rect_to_logical_outward(rect(-3, 0, 2, 2)), rect(-2, 0, 2, 1));
        assert_eq!(one.rect_to_logical_inward(opaque), Some(opaque));
        assert_eq!(two.rect_to_logical_inward(opaque), None);
        assert_eq!(two.rect_to_logical_inward(rect(1, 1, 4, 4)), Some(rect(1, 1, 1, 1)));
        assert_eq!(three.rect_to_logical_inward(opaque), None);
        assert_eq!(two.rect_to_logical_inward(rect(0, 0, 100, 100)), Some(rect(0, 0, 50, 50)));
        assert_eq!(three.rect_to_logical_inward(rect(1, 1, 8, 8)), Some(rect(1, 1, 2, 2)));
        assert_eq!(one.rect_to_logical_inward(rect(-3, -5, 4, 6)), Some(rect(-3, -5, 4, 6)),);
        assert_eq!(two.rect_to_logical_inward(rect(-3, -5, 4, 6)), Some(rect(-1, -2, 1, 2)),);

        // A round trip at S = 1 stays exact; at S = 2 the outward rect covers
        // the scaled rectangle, and the inward rect stays inside it.
        let aligned = rect(5, 5, 10, 10);
        assert_eq!(two.rect_to_logical_outward(two.rect_to_wire(aligned)), aligned);
        assert_eq!(two.rect_to_logical_inward(two.rect_to_wire(aligned)), Some(aligned));
        let scaled = two.rect_to_wire(damage);
        assert_eq!(two.rect_to_logical_outward(scaled), damage);
        assert_eq!(two.rect_to_logical_inward(scaled), Some(damage));
    }

    #[test]
    fn scale_choice_follows_the_mode_and_the_output_scales() {
        let downscaled = XwaylandScalingMode::Downscaled;
        let default = XwaylandScalingMode::Default;
        assert_eq!(xwayland_wire_scale(default, [scale(2.0)].into_iter()), WireScale::ONE);
        assert_eq!(xwayland_wire_scale(downscaled, std::iter::empty()), WireScale::ONE);
        assert_eq!(xwayland_wire_scale(downscaled, [scale(1.0)].into_iter()), WireScale::ONE);
        assert_eq!(xwayland_wire_scale(downscaled, [scale(1.25)].into_iter()), WireScale::new(2),);
        assert_eq!(
            xwayland_wire_scale(downscaled, [scale(1.0), scale(2.0)].into_iter()),
            WireScale::new(2),
        );
        assert_eq!(
            xwayland_wire_scale(downscaled, [scale(1.5), scale(3.0)].into_iter()),
            WireScale::new(3),
        );
    }
}
