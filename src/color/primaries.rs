use std::hash::{Hash, Hasher};

use super::{f64_bits, mat_mul, Matrix3x4};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NamedPrimaries {
    Srgb,
    PalM,
    Pal,
    Ntsc,
    GenericFilm,
    Bt2020,
    Cie1931Xyz,
    DciP3,
    DisplayP3,
    AdobeRgb,
}

impl NamedPrimaries {
    pub fn chromaticities(self) -> Primaries {
        match self {
            Self::Srgb => Primaries::SRGB,
            Self::PalM => Primaries::PAL_M,
            Self::Pal => Primaries::PAL,
            Self::Ntsc => Primaries::NTSC,
            Self::GenericFilm => Primaries::GENERIC_FILM,
            Self::Bt2020 => Primaries::BT2020,
            Self::Cie1931Xyz => Primaries::CIE1931_XYZ,
            Self::DciP3 => Primaries::DCI_P3,
            Self::DisplayP3 => Primaries::DISPLAY_P3,
            Self::AdobeRgb => Primaries::ADOBE_RGB,
        }
    }
}

/// CIE 1931 xy chromaticities of the red, green, blue, and white points.
#[derive(Copy, Clone, Debug)]
pub struct Primaries {
    pub r: (f64, f64),
    pub g: (f64, f64),
    pub b: (f64, f64),
    pub w: (f64, f64),
}

impl PartialEq for Primaries {
    fn eq(&self, other: &Self) -> bool {
        xy_eq(self.r, other.r)
            && xy_eq(self.g, other.g)
            && xy_eq(self.b, other.b)
            && xy_eq(self.w, other.w)
    }
}

impl Eq for Primaries {}

impl Hash for Primaries {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_xy(self.r, state);
        hash_xy(self.g, state);
        hash_xy(self.b, state);
        hash_xy(self.w, state);
    }
}

pub(super) fn xy_eq(left: (f64, f64), right: (f64, f64)) -> bool {
    f64_bits(left.0) == f64_bits(right.0) && f64_bits(left.1) == f64_bits(right.1)
}

fn hash_xy<H: Hasher>(xy: (f64, f64), state: &mut H) {
    f64_bits(xy.0).hash(state);
    f64_bits(xy.1).hash(state);
}

impl Primaries {
    pub const SRGB: Self = Self {
        r: (0.64, 0.33),
        g: (0.30, 0.60),
        b: (0.15, 0.06),
        w: (0.3127, 0.3290),
    };
    pub const PAL_M: Self = Self {
        r: (0.67, 0.33),
        g: (0.21, 0.71),
        b: (0.14, 0.08),
        w: (0.310, 0.316),
    };
    pub const PAL: Self = Self {
        r: (0.64, 0.33),
        g: (0.29, 0.60),
        b: (0.15, 0.06),
        w: (0.3127, 0.3290),
    };
    pub const NTSC: Self = Self {
        r: (0.630, 0.340),
        g: (0.310, 0.595),
        b: (0.155, 0.070),
        w: (0.3127, 0.3290),
    };
    pub const GENERIC_FILM: Self = Self {
        r: (0.681, 0.319),
        g: (0.243, 0.692),
        b: (0.145, 0.049),
        w: (0.310, 0.316),
    };
    pub const BT2020: Self = Self {
        r: (0.708, 0.292),
        g: (0.170, 0.797),
        b: (0.131, 0.046),
        w: (0.3127, 0.3290),
    };
    pub const CIE1931_XYZ: Self = Self {
        r: (1.0, 0.0),
        g: (0.0, 1.0),
        b: (0.0, 0.0),
        w: (1.0 / 3.0, 1.0 / 3.0),
    };
    pub const DCI_P3: Self = Self {
        r: (0.680, 0.320),
        g: (0.265, 0.690),
        b: (0.150, 0.060),
        w: (0.314, 0.351),
    };
    pub const DISPLAY_P3: Self = Self {
        r: (0.680, 0.320),
        g: (0.265, 0.690),
        b: (0.150, 0.060),
        w: (0.3127, 0.3290),
    };
    pub const ADOBE_RGB: Self = Self {
        r: (0.64, 0.33),
        g: (0.21, 0.71),
        b: (0.15, 0.06),
        w: (0.3127, 0.3290),
    };

    /// Columns are the XYZ of each primary, scaled so the white point has `Y = 1`.
    pub fn rgb_to_xyz(self) -> Matrix3x4 {
        let (xyz_from_rgb, _) = matrices(self);
        xyz_from_rgb
    }

    pub fn xyz_to_rgb(self) -> Matrix3x4 {
        let (_, rgb_from_xyz) = matrices(self);
        rgb_from_xyz
    }

    /// Custom primaries from CIE 1931 xy. `None` when a coordinate is non-finite,
    /// outside `[0, 1]`, has `y` below `1e-4` (white `y == 0` divides in
    /// `xy_to_xyz`), has `x + y` above 1, repeats another primary, or the
    /// resulting RGB-to-XYZ matrix is singular.
    pub fn from_chromaticities(
        r: (f64, f64),
        g: (f64, f64),
        b: (f64, f64),
        w: (f64, f64),
    ) -> Option<Self> {
        if ![r, g, b, w].into_iter().all(chromaticity_in_gamut) {
            return None;
        }
        if r == g || g == b || r == b {
            return None;
        }
        let primaries = Self { r, g, b, w };
        let matrix = primaries.rgb_to_xyz();
        if matrix.iter().flatten().any(|value| !value.is_finite()) {
            return None;
        }
        if matrix_det(matrix).abs() <= 1e-8 {
            return None;
        }
        Some(primaries)
    }
}

fn chromaticity_in_gamut((x, y): (f64, f64)) -> bool {
    x.is_finite()
        && y.is_finite()
        && (0.0..=1.0).contains(&x)
        && (0.0..=1.0).contains(&y)
        && y > 1.0e-4
        && x + y <= 1.0
}

fn matrix_det(matrix: Matrix3x4) -> f64 {
    let (a, b, c) = (matrix[0][0], matrix[0][1], matrix[0][2]);
    let (d, e, f) = (matrix[1][0], matrix[1][1], matrix[1][2]);
    let (g, h, i) = (matrix[2][0], matrix[2][1], matrix[2][2]);
    a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
}

/// Bradford chromatic adaptation from `src_white` to `dst_white`.
///
/// `None` when either white point makes a cone response zero or non-finite.
/// The Bradford matrix itself is constant; a failed inverse is the same result.
pub fn bradford(src_white: (f64, f64), dst_white: (f64, f64)) -> Option<Matrix3x4> {
    const BFD: [[f64; 3]; 3] =
        [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let inverse = invert3(BFD)?;
    let src = mul3(BFD, xy_to_xyz(src_white)?);
    let dst = mul3(BFD, xy_to_xyz(dst_white)?);
    let mut scale = [[0.0; 4]; 3];
    for channel in 0..3 {
        if !src[channel].is_finite() || src[channel].abs() <= 1e-12 || !dst[channel].is_finite() {
            return None;
        }
        scale[channel][channel] = dst[channel] / src[channel];
    }
    let bfd = pad3(BFD);
    let adapted = mat_mul(mat_mul(pad3(inverse), scale), bfd);
    adapted
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(adapted)
}

pub(super) fn xy_to_xyz(xy: (f64, f64)) -> Option<[f64; 3]> {
    let (x, y) = xy;
    if !x.is_finite() || !y.is_finite() || y.abs() <= 1e-12 {
        return None;
    }
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    xyz.iter().all(|value| value.is_finite()).then_some(xyz)
}

fn matrices(primaries: Primaries) -> (Matrix3x4, Matrix3x4) {
    let (xw, yw) = primaries.w;
    let xw = xw / yw;
    let zw = (1.0 - primaries.w.0 - yw) / yw;
    let (xr, yr) = primaries.r;
    let (xg, yg) = primaries.g;
    let (xb, yb) = primaries.b;
    let zr = 1.0 - xr - yr;
    let zg = 1.0 - xg - yg;
    let zb = 1.0 - xb - yb;
    let srx = yg * zb - zg * yb;
    let sry = zg * xb - xg * zb;
    let srz = xg * yb - yg * xb;
    let sgx = zr * yb - yr * zb;
    let sgz = yr * xb - xr * yb;
    let sgy = xr * zb - zr * xb;
    let sbx = yr * zg - zr * yg;
    let sby = zr * xg - xr * zg;
    let sbz = xr * yg - yr * xg;
    let det = srz + sgz + sbz;
    let sr = srx * xw + sry + srz * zw;
    let sg = sgx * xw + sgy + sgz * zw;
    let sb = sbx * xw + sby + sbz * zw;
    let det_inv = 1.0 / det;
    let srp = sr * det_inv;
    let sgp = sg * det_inv;
    let sbp = sb * det_inv;
    let xyz_from_rgb = [
        [srp * xr, sgp * xg, sbp * xb, 0.0],
        [srp * yr, sgp * yg, sbp * yb, 0.0],
        [srp * zr, sgp * zg, sbp * zb, 0.0],
    ];
    let rgb_from_xyz = [
        [srx / sr, sry / sr, srz / sr, 0.0],
        [sgx / sg, sgy / sg, sgz / sg, 0.0],
        [sbx / sb, sby / sb, sbz / sb, 0.0],
    ];
    (xyz_from_rgb, rgb_from_xyz)
}

fn pad3(matrix: [[f64; 3]; 3]) -> Matrix3x4 {
    [
        [matrix[0][0], matrix[0][1], matrix[0][2], 0.0],
        [matrix[1][0], matrix[1][1], matrix[1][2], 0.0],
        [matrix[2][0], matrix[2][1], matrix[2][2], 0.0],
    ]
}

fn mul3(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

/// `None` when a pivot is ~0 or the inverse is non-finite. Dividing by that
/// pivot would be infinity, which must not enter a color transform.
fn invert3(matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let mut rows = [
        [matrix[0][0], matrix[0][1], matrix[0][2], 1.0, 0.0, 0.0],
        [matrix[1][0], matrix[1][1], matrix[1][2], 0.0, 1.0, 0.0],
        [matrix[2][0], matrix[2][1], matrix[2][2], 0.0, 0.0, 1.0],
    ];
    for col in 0..3 {
        let mut pivot = col;
        for row in (col + 1)..3 {
            if rows[row][col].abs() > rows[pivot][col].abs() {
                pivot = row;
            }
        }
        rows.swap(col, pivot);
        let divisor = rows[col][col];
        if !divisor.is_finite() || divisor.abs() <= 1e-12 {
            return None;
        }
        for value in &mut rows[col] {
            *value /= divisor;
        }
        for row in 0..3 {
            if row == col {
                continue;
            }
            let factor = rows[row][col];
            for column in 0..6 {
                rows[row][column] -= factor * rows[col][column];
            }
        }
    }
    let inverse = [
        [rows[0][3], rows[0][4], rows[0][5]],
        [rows[1][3], rows[1][4], rows[1][5]],
        [rows[2][3], rows[2][4], rows[2][5]],
    ];
    inverse
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(inverse)
}

#[cfg(test)]
mod tests {
    use {super::Primaries, crate::color::assert_matrix_close};

    #[test]
    fn srgb_bt2020_and_display_p3_match_reference_xyz() {
        assert_matrix_close(
            Primaries::SRGB.rgb_to_xyz(),
            [
                [0.4124564, 0.3575761, 0.1804375, 0.0],
                [0.2126729, 0.7151522, 0.0721750, 0.0],
                [0.0193339, 0.1191920, 0.9503041, 0.0],
            ],
            0.001,
        );
        assert_matrix_close(
            Primaries::BT2020.rgb_to_xyz(),
            [
                [0.636958, 0.144617, 0.168881, 0.0],
                [0.262700, 0.677998, 0.059302, 0.0],
                [0.000000, 0.028073, 1.060985, 0.0],
            ],
            0.001,
        );
        assert!(Primaries::from_chromaticities(
            Primaries::DISPLAY_P3.r,
            Primaries::DISPLAY_P3.g,
            Primaries::DISPLAY_P3.b,
            Primaries::DISPLAY_P3.w,
        )
        .is_some());
        assert!(Primaries::from_chromaticities((0.0, 0.0), (0.0, 1.0), (0.0, 0.0), (0.0, 0.0))
            .is_none());
        assert!(Primaries::from_chromaticities(
            Primaries::SRGB.r,
            Primaries::SRGB.g,
            Primaries::SRGB.b,
            (0.3127, 0.0),
        )
        .is_none());
        assert_matrix_close(
            Primaries::DISPLAY_P3.rgb_to_xyz(),
            [
                [0.486571, 0.265668, 0.198217, 0.0],
                [0.228975, 0.691739, 0.079287, 0.0],
                [0.000000, 0.045113, 1.043944, 0.0],
            ],
            0.001,
        );
    }

    #[test]
    fn singular_and_non_finite_chromaticities_do_not_panic() {
        assert!(super::invert3([[0.0; 3]; 3]).is_none());
        assert!(super::invert3([[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]]).is_none());
        assert!(super::invert3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0],]).is_some());
        assert!(super::bradford(Primaries::SRGB.w, Primaries::BT2020.w).is_some());
        assert!(super::bradford((0.3127, 0.0), Primaries::SRGB.w).is_none());
        assert!(super::bradford((f64::NAN, 0.3), Primaries::SRGB.w).is_none());
        assert!(super::xy_to_xyz((0.3, 0.0)).is_none());

        let samples = [
            0.0,
            1e-8,
            -0.2,
            0.15,
            0.33,
            0.64,
            1.0,
            1.5,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for r_x in samples {
            for r_y in samples {
                for g_y in samples {
                    let result = Primaries::from_chromaticities(
                        (r_x, r_y),
                        (0.30, g_y),
                        (0.15, 0.06),
                        (0.3127, 0.3290),
                    );
                    if let Some(primaries) = result {
                        let matrix = primaries.rgb_to_xyz();
                        assert!(
                            matrix.iter().flatten().all(|value| value.is_finite()),
                            "accepted primaries produced a non-finite matrix"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rgb_and_xyz_are_inverses_and_white_has_unit_y() {
        for primaries in [
            Primaries::SRGB,
            Primaries::BT2020,
            Primaries::DISPLAY_P3,
            Primaries::DCI_P3,
            Primaries::CIE1931_XYZ,
        ] {
            let forward = primaries.rgb_to_xyz();
            let back = primaries.xyz_to_rgb();
            let y = forward[1][0] + forward[1][1] + forward[1][2];
            assert!((y - 1.0).abs() < 1e-9, "white Y = {y} for {primaries:?}");
            let product = crate::color::mat_mul(back, forward);
            assert_matrix_close(
                product,
                [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
                1e-9,
            );
        }
    }
}
