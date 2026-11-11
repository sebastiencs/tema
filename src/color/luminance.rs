use std::hash::{Hash, Hasher};

use super::{
    f64_bits, intent::RenderIntent, primaries::xy_to_xyz, tf::TransferFunction, Matrix3x4,
};

#[derive(Copy, Clone, Debug)]
pub struct Luminances {
    pub min: f64,
    pub max: f64,
    pub white: f64,
}

impl PartialEq for Luminances {
    fn eq(&self, other: &Self) -> bool {
        f64_bits(self.min) == f64_bits(other.min)
            && f64_bits(self.max) == f64_bits(other.max)
            && f64_bits(self.white) == f64_bits(other.white)
    }
}

impl Eq for Luminances {}

impl Hash for Luminances {
    fn hash<H: Hasher>(&self, state: &mut H) {
        f64_bits(self.min).hash(state);
        f64_bits(self.max).hash(state);
        f64_bits(self.white).hash(state);
    }
}

impl Luminances {
    pub const SRGB: Self = Self { min: 0.2, max: 80.0, white: 80.0 };
    pub const BT1886: Self = Self { min: 0.01, max: 100.0, white: 100.0 };
    /// PQ minimum is 0 cd/m². The protocol text says 0.005; the plan uses 0.
    pub const ST2084_PQ: Self = Self { min: 0.0, max: 10_000.0, white: 203.0 };
    /// Reference white is 203 so a PQ white-balance scale collapses to `80 / 10000`.
    pub const WINDOWS_SCRGB: Self = Self { min: 0.0, max: 80.0, white: 203.0 };

    pub fn for_transfer_function(tf: TransferFunction) -> Self {
        match tf {
            TransferFunction::St2084Pq => Self::ST2084_PQ,
            TransferFunction::Bt1886 { .. } => Self::BT1886,
            _ => Self::SRGB,
        }
    }
}

/// Maps source linear light onto the destination luminance range, in XYZ.
///
/// The offset is zero for intents with black-point compensation. `to_white`
/// is the destination primary white point.
pub fn white_balance(
    from: Luminances,
    to: Luminances,
    to_white: (f64, f64),
    intent: RenderIntent,
) -> Option<Matrix3x4> {
    let destination_span = to.max - to.min;
    let source_white = from.white - from.min;
    if !destination_span.is_finite()
        || destination_span.abs() <= 1e-12
        || !source_white.is_finite()
        || source_white.abs() <= 1e-12
    {
        return None;
    }
    let scale = (from.max - from.min) / destination_span * (to.white - to.min) / source_white;
    let offset =
        if intent.compensates_black_point() { 0.0 } else { (from.min - to.min) / destination_span };
    if !scale.is_finite() || !offset.is_finite() {
        return None;
    }
    let slope = scale - offset;
    let [x, y, z] = xy_to_xyz(to_white)?;
    let matrix = [
        [slope, 0.0, 0.0, offset * x],
        [0.0, slope, 0.0, offset * y],
        [0.0, 0.0, slope, offset * z],
    ];
    matrix
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(matrix)
}

#[cfg(test)]
mod tests {
    use {
        super::{white_balance, Luminances},
        crate::color::{intent::RenderIntent, primaries::Primaries, tf::TransferFunction},
    };

    #[test]
    fn defaults_follow_the_transfer_function() {
        assert_eq!(Luminances::for_transfer_function(TransferFunction::Gamma22), Luminances::SRGB);
        assert_eq!(
            Luminances::for_transfer_function(TransferFunction::Bt1886 { lb_over_lw: 0.0 }),
            Luminances::BT1886
        );
        assert_eq!(
            Luminances::for_transfer_function(TransferFunction::St2084Pq),
            Luminances::ST2084_PQ
        );
        assert_eq!(Luminances::ST2084_PQ.min, 0.0);
    }

    #[test]
    fn black_point_compensation_drops_the_offset() {
        let perceptual = white_balance(
            Luminances::SRGB,
            Luminances::ST2084_PQ,
            Primaries::BT2020.w,
            RenderIntent::Perceptual,
        )
        .unwrap();
        let relative = white_balance(
            Luminances::SRGB,
            Luminances::ST2084_PQ,
            Primaries::BT2020.w,
            RenderIntent::Relative,
        )
        .unwrap();
        assert_eq!(perceptual[0][3], 0.0);
        assert_eq!(perceptual[1][3], 0.0);
        assert_eq!(perceptual[2][3], 0.0);
        assert!(relative[1][3].abs() > 0.0);
    }

    #[test]
    fn a_zero_luminance_span_has_no_matrix() {
        let flat = Luminances { min: 80.0, max: 80.0, white: 80.0 };
        assert!(white_balance(flat, Luminances::SRGB, Primaries::SRGB.w, RenderIntent::Perceptual)
            .is_none());
        assert!(white_balance(
            Luminances { min: 10.0, max: 80.0, white: 10.0 },
            Luminances::SRGB,
            (0.3, 0.0),
            RenderIntent::Relative,
        )
        .is_none());
    }
}
