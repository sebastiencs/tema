use std::rc::Rc;

use super::{
    description::{ImageDescription, LinearPart},
    intent::RenderIntent,
    luminance::white_balance,
    mat_mul,
    primaries::{bradford, xy_eq},
    tf::TransferFunction,
    tonemap::ToneMap,
};

pub type Matrix3x4 = [[f64; 4]; 3];

/// Linear 3×4 color matrix plus the transfer functions it sits between.
///
/// `None` from [`ColorTransform::between`] means the interned linear parts and
/// the transfer functions are equal, so a renderer can skip the color path.
#[derive(Copy, Clone, Debug)]
pub struct ColorTransform {
    pub matrix: Matrix3x4,
    pub src_tf: TransferFunction,
    pub dst_tf: TransferFunction,
    /// Perceptual shoulder. `None` clips through the destination encode.
    pub tonemap: Option<ToneMap>,
}

impl ColorTransform {
    /// Decode `src_tf` and encode `dst_tf` with an identity matrix.
    pub fn transfer(src_tf: TransferFunction, dst_tf: TransferFunction) -> Self {
        Self {
            matrix: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            src_tf,
            dst_tf,
            tonemap: None,
        }
    }

    pub fn between(
        src: &ImageDescription,
        dst: &ImageDescription,
        intent: RenderIntent,
    ) -> Option<Self> {
        if src.linear.id == dst.linear.id && src.tf == dst.tf {
            return None;
        }
        let matrix = if Rc::ptr_eq(&src.shared, &dst.shared) {
            src.shared.cached_matrix(&src.linear, &dst.linear, intent)
        } else {
            linear_matrix(&src.linear, &dst.linear, intent)
        };
        let Some(matrix) = matrix else {
            log::warn!("color transform matrix is singular; leaving the pixels unconverted");
            return None;
        };
        let tonemap = (intent == RenderIntent::Perceptual)
            .then(|| ToneMap::prepare(src, dst))
            .flatten();
        Some(Self { matrix, src_tf: src.tf, dst_tf: dst.tf, tonemap })
    }

    /// Decode, apply the 3×4 matrix, and encode. This is the CPU reference for
    /// the texture shader.
    pub fn map_rgb(self, encoded: [f64; 3]) -> [f64; 3] {
        let linear = [
            self.src_tf.decode(encoded[0]),
            self.src_tf.decode(encoded[1]),
            self.src_tf.decode(encoded[2]),
        ];
        let mapped = [
            map_channel(&self.matrix[0], linear),
            map_channel(&self.matrix[1], linear),
            map_channel(&self.matrix[2], linear),
        ];
        let mapped = self
            .tonemap
            .map(|tone| tone.apply(mapped))
            .unwrap_or(mapped);
        [
            self.dst_tf.encode(mapped[0]),
            self.dst_tf.encode(mapped[1]),
            self.dst_tf.encode(mapped[2]),
        ]
    }
}

fn map_channel(row: &[f64; 4], rgb: [f64; 3]) -> f64 {
    row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2] + row[3]
}

pub(super) fn linear_matrix(
    src: &LinearPart,
    dst: &LinearPart,
    intent: RenderIntent,
) -> Option<Matrix3x4> {
    let mut matrix = dst.rgb_from_xyz;
    if src.luminances != dst.luminances {
        matrix = mat_mul(
            matrix,
            white_balance(src.luminances, dst.luminances, dst.primaries.w, intent)?,
        );
    }
    if !xy_eq(src.primaries.w, dst.primaries.w) && intent.adapts_white_point() {
        matrix = mat_mul(matrix, bradford(src.primaries.w, dst.primaries.w)?);
    }
    let matrix = mat_mul(matrix, src.xyz_from_rgb);
    matrix
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(matrix)
}

#[cfg(test)]
mod tests {
    use {
        super::ColorTransform,
        crate::color::{
            assert_matrix_close, description::ImageDescriptionParams, intent::RenderIntent,
            luminance::Luminances, manager::ColorManager, primaries::Primaries,
            tf::TransferFunction,
        },
    };

    fn described(
        manager: &ColorManager,
        primaries: Primaries,
    ) -> std::rc::Rc<crate::color::ImageDescription> {
        manager.intern(ImageDescriptionParams {
            tf: TransferFunction::ExtLinear,
            primaries,
            named_primaries: None,
            luminances: Luminances::SRGB,
            target_primaries: primaries,
            target_luminances: (Luminances::SRGB.min, Luminances::SRGB.max),
            max_cll: None,
            max_fall: None,
        })
    }

    #[test]
    fn reference_primaries_transforms() {
        let manager = ColorManager::new();
        let srgb = described(&manager, Primaries::SRGB);
        let bt2020 = described(&manager, Primaries::BT2020);
        let display_p3 = described(&manager, Primaries::DISPLAY_P3);
        let dci_p3 = described(&manager, Primaries::DCI_P3);
        let to_bt2020 = ColorTransform::between(&srgb, &bt2020, RenderIntent::Perceptual).unwrap();
        let from_bt2020 =
            ColorTransform::between(&bt2020, &srgb, RenderIntent::Perceptual).unwrap();
        let to_p3 = ColorTransform::between(&srgb, &display_p3, RenderIntent::Perceptual).unwrap();
        let from_p3 =
            ColorTransform::between(&display_p3, &srgb, RenderIntent::Perceptual).unwrap();
        let to_dci = ColorTransform::between(&srgb, &dci_p3, RenderIntent::Perceptual).unwrap();
        assert_matrix_close(
            to_bt2020.matrix,
            [
                [0.627404, 0.329283, 0.043313, 0.0],
                [0.069097, 0.919540, 0.011362, 0.0],
                [0.016391, 0.088013, 0.895595, 0.0],
            ],
            0.001,
        );
        assert_matrix_close(
            from_bt2020.matrix,
            [
                [1.660491, -0.587641, -0.072850, 0.0],
                [-0.124550, 1.132900, -0.008349, 0.0],
                [-0.018151, -0.100579, 1.118730, 0.0],
            ],
            0.001,
        );
        assert_matrix_close(
            to_p3.matrix,
            [
                [0.822462, 0.177538, 0.0, 0.0],
                [0.033194, 0.966806, 0.0, 0.0],
                [0.017083, 0.072397, 0.910520, 0.0],
            ],
            0.001,
        );
        assert_matrix_close(
            from_p3.matrix,
            [
                [1.224940, -0.224940, 0.0, 0.0],
                [-0.042057, 1.042057, 0.0, 0.0],
                [-0.019638, -0.078636, 1.098274, 0.0],
            ],
            0.001,
        );
        assert_matrix_close(
            to_dci.matrix,
            [
                [0.868580, 0.128919, 0.002501, 0.0],
                [0.034540, 0.961811, 0.003648, 0.0],
                [0.016771, 0.071040, 0.912189, 0.0],
            ],
            0.001,
        );
    }

    #[test]
    fn equal_descriptions_are_identity() {
        let manager = ColorManager::new();
        let srgb = manager.srgb_gamma22();
        assert!(ColorTransform::between(&srgb, &srgb, RenderIntent::Relative).is_none());
    }

    #[test]
    fn same_linear_part_with_a_different_tf_is_an_identity_matrix() {
        let manager = ColorManager::new();
        let srgb = manager.srgb_gamma22();
        let mut params = ImageDescriptionParams::srgb_gamma22();
        params.tf = TransferFunction::ExtLinear;
        let linear = manager.intern(params);
        let transform = ColorTransform::between(&srgb, &linear, RenderIntent::Perceptual).unwrap();
        assert_eq!(transform.src_tf, TransferFunction::Gamma22);
        assert_eq!(transform.dst_tf, TransferFunction::ExtLinear);
        assert_matrix_close(
            transform.matrix,
            [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            1e-12,
        );
    }

    #[test]
    fn pq_reference_white_lands_on_srgb_white() {
        let manager = ColorManager::new();
        let mut params = ImageDescriptionParams::srgb_gamma22();
        params.tf = TransferFunction::St2084Pq;
        params.luminances = Luminances::ST2084_PQ;
        let pq = manager.intern(params);
        let transform =
            ColorTransform::between(&pq, &manager.srgb_gamma22(), RenderIntent::Perceptual)
                .unwrap();
        let white = TransferFunction::St2084Pq.encode(203.0 / 10_000.0);
        let mapped = transform.map_rgb([white, white, white]);
        for channel in mapped {
            assert!((channel - 1.0).abs() < 1e-6, "{channel}");
        }
        assert!(transform.tonemap.is_none());
    }

    #[test]
    fn absolute_intent_skips_bradford() {
        let manager = ColorManager::new();
        let srgb = described(&manager, Primaries::SRGB);
        let dci = described(&manager, Primaries::DCI_P3);
        let adapted = ColorTransform::between(&srgb, &dci, RenderIntent::Perceptual).unwrap();
        let absolute =
            ColorTransform::between(&srgb, &dci, RenderIntent::AbsoluteNoAdaptation).unwrap();
        assert!((adapted.matrix[0][0] - absolute.matrix[0][0]).abs() > 0.001);
    }
}
