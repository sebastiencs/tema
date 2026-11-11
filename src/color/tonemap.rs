//! Hyprland's PQ ICtCp shoulder, applied after the destination matrix.
//!
//! The source peak is `max_cll` when the client set one, otherwise the target
//! maximum, otherwise the encoding maximum. Perceptual tone maps when that
//! peak is above `1.01 ×` the destination encoding maximum. The other intents
//! leave the matrix result for the encode to clip.
//!
//! Values are destination nits: the white-balance matrix has already mapped
//! the source reference white onto the destination reference white. The LMS
//! matrices use the same column-major constants as `tonemap.glsl`.

use super::{description::ImageDescription, primaries::Primaries, tf::TransferFunction, Matrix3x4};

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;
const HDR_MAX: f32 = 10_000.0;

/// GLSL `mat3(...)` columns. `M * v` matches `tonemap.glsl`.
const BT2020_TO_LMS: [[f32; 3]; 3] =
    [[0.3592, -0.1922, 0.0070], [0.6976, 1.1004, 0.0749], [-0.0358, 0.0755, 0.8434]];
const LMS_TO_BT2020: [[f32; 3]; 3] = [
    [2.0701800566956135096, 0.36498825003265747974, -0.049595542238932107896],
    [-1.3264568761030210255, 0.68046736285223514102, -0.049421161186757487412],
    [0.20661600684785517081, -0.045421753075853231409, 1.1879959417328034394],
];
const ICTCP_PQ: [[f32; 3]; 3] = [
    [0.5, 1.61376953125, 4.378173828125],
    [0.5, -3.323486328125, -4.24560546875],
    [0.0, 1.709716796875, -0.132568359375],
];
const ICTCP_PQ_INV: [[f32; 3]; 3] = [
    [1.0, 1.0, 1.0],
    [0.0086090370379327566, -0.0086090370379327566, 0.560031335710679118],
    [0.11102962500302595656, -0.11102962500302595656, -0.32062717498731885185],
];

/// Uniforms for one perceptual shoulder. Rows match the shader's `dot`s.
#[derive(Clone, Copy, Debug)]
pub struct ToneMap {
    pub max_luminance: f32,
    pub dst_max: f32,
    pub dst_min: f32,
    pub dst_ref: f32,
    pub to_lms: [[f32; 4]; 3],
    pub from_lms: [[f32; 4]; 3],
}

impl ToneMap {
    /// `None` when the source peak fits in the destination, or the range is unusable.
    pub fn prepare(src: &ImageDescription, dst: &ImageDescription) -> Option<Self> {
        let dst_lum = dst.luminances();
        let src_lum = src.luminances();
        if !usable(dst_lum.max) || !usable(dst_lum.white) || !(dst_lum.max > dst_lum.min) {
            return None;
        }
        let peak = source_peak(src);
        if !(peak > dst_lum.max * 1.01) {
            return None;
        }
        let max_luminance = peak_in_destination_nits(src_lum, dst_lum, peak);
        if !max_luminance.is_finite() || max_luminance <= 0.0 {
            return None;
        }
        let to_lms =
            pad_rows(mul_columns(BT2020_TO_LMS, columns_from(dst.primaries().rgb_to_xyz())));
        let from_lms =
            pad_rows(mul_columns(columns_from(dst.primaries().xyz_to_rgb()), LMS_TO_BT2020));
        Some(Self {
            max_luminance,
            dst_max: dst_lum.max as f32,
            dst_min: dst_lum.min as f32,
            dst_ref: dst_lum.white as f32,
            to_lms,
            from_lms,
        })
    }

    /// Destination-linear RGB in, destination-linear RGB out.
    pub fn apply(self, rgb: [f64; 3]) -> [f64; 3] {
        let linear = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
        let span = self.dst_max - self.dst_min;
        if !span.is_finite() || span <= 1e-6 || self.dst_ref <= 0.0 {
            return rgb;
        }
        let nits = [
            linear[0] * span + self.dst_min,
            linear[1] * span + self.dst_min,
            linear[2] * span + self.dst_min,
        ];
        let mapped = if self.max_luminance < self.dst_max * 1.01 {
            clamp3(nits, 0.0, self.dst_max)
        } else {
            shoulder(self, nits)
        };
        [
            f64::from((mapped[0] - self.dst_min) / span),
            f64::from((mapped[1] - self.dst_min) / span),
            f64::from((mapped[2] - self.dst_min) / span),
        ]
    }
}

/// Content peak in cd/m². `max_cll` wins over the target maximum.
pub fn source_peak(description: &ImageDescription) -> f64 {
    if let Some(max_cll) = description.max_cll() {
        if max_cll.is_finite() && max_cll > 0.0 {
            return max_cll;
        }
    }
    let target = description.target_luminances().1;
    if target.is_finite() && target > 0.0 {
        return target;
    }
    description.luminances().max
}

fn peak_in_destination_nits(src: super::Luminances, dst: super::Luminances, peak: f64) -> f32 {
    let src_span = src.max - src.min;
    let dst_span = dst.max - dst.min;
    let src_white = src.white - src.min;
    if src_span.abs() <= 1e-12 || dst_span.abs() <= 1e-12 || src_white.abs() <= 1e-12 {
        return peak as f32;
    }
    let fraction = (peak - src.min) / src_span;
    let scale = src_span / dst_span * (dst.white - dst.min) / src_white;
    (fraction * scale * dst_span + dst.min) as f32
}

fn shoulder(tone: ToneMap, nits: [f32; 3]) -> [f32; 3] {
    let lms = encode_pq(mul_rows(tone.to_lms, scale3(nits, 1.0 / HDR_MAX)));
    let mut ictcp = mul_vec(ICTCP_PQ, lms);
    let luminance = decode_pq(ictcp[0]) * HDR_MAX;
    let new_luminance = reel(luminance, tone.max_luminance, tone.dst_max, tone.dst_ref);
    ictcp[0] = encode_pq_channel((new_luminance / HDR_MAX).clamp(0.0, 1.0));
    let rgb = scale3(decode_pq_vec(mul_vec(ICTCP_PQ_INV, ictcp)), HDR_MAX);
    let restored = mul_rows(tone.from_lms, rgb);
    clamp3(restored, 0.0, tone.dst_max)
}

fn reel(luminance: f32, max_luminance: f32, dst_max: f32, dst_ref: f32) -> f32 {
    let src_scale = max_luminance / dst_ref;
    if !src_scale.is_finite() || src_scale.abs() <= 1e-6 {
        return luminance.clamp(0.0, dst_max);
    }
    let luminance_ratio = (luminance / dst_ref).max(0.0);
    let dst_scale = dst_max / dst_ref;
    let v = (dst_scale * (1.0 + src_scale) - src_scale) / (src_scale * src_scale);
    (luminance_ratio * (1.0 + luminance_ratio * v) / (1.0 + luminance_ratio)) * dst_ref
}

fn encode_pq(rgb: [f32; 3]) -> [f32; 3] {
    [encode_pq_channel(rgb[0]), encode_pq_channel(rgb[1]), encode_pq_channel(rgb[2])]
}

fn encode_pq_channel(value: f32) -> f32 {
    let y = value.max(0.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

fn decode_pq_vec(rgb: [f32; 3]) -> [f32; 3] {
    [decode_pq(rgb[0]), decode_pq(rgb[1]), decode_pq(rgb[2])]
}

fn decode_pq(value: f32) -> f32 {
    if value <= 0.0 {
        return 0.0;
    }
    let p = value.powf(1.0 / PQ_M2);
    let numerator = (p - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * p;
    (numerator / denominator).powf(1.0 / PQ_M1)
}

fn usable(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

/// Columns of a 3×4 matrix, dropping the translation.
fn columns_from(matrix: Matrix3x4) -> [[f32; 3]; 3] {
    let mut columns = [[0.0; 3]; 3];
    for column in 0..3 {
        for row in 0..3 {
            columns[column][row] = matrix[row][column] as f32;
        }
    }
    columns
}

fn mul_columns(left: [[f32; 3]; 3], right: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for column in 0..3 {
        out[column] = mul_vec(left, right[column]);
    }
    out
}

fn mul_vec(columns: [[f32; 3]; 3], value: [f32; 3]) -> [f32; 3] {
    [
        columns[0][0] * value[0] + columns[1][0] * value[1] + columns[2][0] * value[2],
        columns[0][1] * value[0] + columns[1][1] * value[1] + columns[2][1] * value[2],
        columns[0][2] * value[0] + columns[1][2] * value[1] + columns[2][2] * value[2],
    ]
}

fn pad_rows(columns: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [columns[0][0], columns[1][0], columns[2][0], 0.0],
        [columns[0][1], columns[1][1], columns[2][1], 0.0],
        [columns[0][2], columns[1][2], columns[2][2], 0.0],
    ]
}

fn mul_rows(rows: [[f32; 4]; 3], value: [f32; 3]) -> [f32; 3] {
    let dotted = |row: [f32; 4]| row[0] * value[0] + row[1] * value[1] + row[2] * value[2];
    [dotted(rows[0]), dotted(rows[1]), dotted(rows[2])]
}

fn scale3(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn clamp3(value: [f32; 3], low: f32, high: f32) -> [f32; 3] {
    [value[0].clamp(low, high), value[1].clamp(low, high), value[2].clamp(low, high)]
}

#[cfg(test)]
mod tests {
    use {
        super::{source_peak, ToneMap, BT2020_TO_LMS, LMS_TO_BT2020},
        crate::color::{
            description::ImageDescriptionParams, manager::ColorManager, tf::TransferFunction,
            ColorTransform, RenderIntent,
        },
    };

    fn round_trip_is_identity() {
        let product = super::mul_columns(LMS_TO_BT2020, BT2020_TO_LMS);
        for column in 0..3 {
            for row in 0..3 {
                let expected = if column == row { 1.0 } else { 0.0 };
                let delta = (product[column][row] - expected).abs();
                assert!(delta < 1e-4, "lms column {column} row {row} = {}", product[column][row]);
            }
        }
    }

    #[test]
    fn lms_constants_round_trip() {
        round_trip_is_identity();
    }

    #[test]
    fn max_cll_is_the_peak_and_a_low_peak_does_not_tone_map() {
        let manager = ColorManager::new();
        let mut params = ImageDescriptionParams::windows_bt2100();
        params.max_cll = Some(50.0);
        let quiet = manager.intern(params);
        assert_eq!(source_peak(&quiet), 50.0);
        assert!(ToneMap::prepare(&quiet, &manager.srgb_gamma22()).is_none());

        let loud = manager.windows_bt2100();
        assert_eq!(source_peak(&loud), 10_000.0);
        assert!(ToneMap::prepare(&loud, &manager.srgb_gamma22()).is_some());
        assert!(ColorTransform::between(&loud, &manager.srgb_gamma22(), RenderIntent::Relative)
            .unwrap()
            .tonemap
            .is_none());
    }

    #[test]
    fn perceptual_rolls_a_highlight_below_the_clip_and_keeps_a_dark_pixel() {
        let manager = ColorManager::new();
        let src = manager.windows_bt2100();
        let dst = manager.srgb_gamma22();
        let perceptual = ColorTransform::between(&src, &dst, RenderIntent::Perceptual).unwrap();
        let relative = ColorTransform::between(&src, &dst, RenderIntent::RelativeBpc).unwrap();
        assert!(perceptual.tonemap.is_some());
        let peak = TransferFunction::St2084Pq.encode(1.0);
        let bright_p = perceptual.map_rgb([peak, peak, peak]);
        let bright_r = relative.map_rgb([peak, peak, peak]);
        assert!(bright_r[0] > 0.99, "relative clips the peak, got {}", bright_r[0]);
        assert!(
            bright_p[0] < bright_r[0] - 0.05,
            "perceptual peak {} should sit below the clip {}",
            bright_p[0],
            bright_r[0]
        );
        let dark = TransferFunction::St2084Pq.encode(1.0 / 10_000.0);
        let dark_p = perceptual.map_rgb([dark, dark, dark]);
        let dark_r = relative.map_rgb([dark, dark, dark]);
        assert!((dark_p[0] - dark_r[0]).abs() < 0.02, "{dark_p:?} vs {dark_r:?}");
    }
}
