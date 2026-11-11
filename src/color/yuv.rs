//! YCbCr conversion shared by the CPU and the texture shader.
//!
//! When the client sets no color-representation, YUV is BT.709, limited range,
//! chroma type 0 (left, vertical center). Mesa's EGL default is BT.601.
//! `wp_color_representation_v1` overrides the coefficients, range, and siting.
//! The shader applies the matrix this module builds. It does not pick one.

/// Protocol `alpha_mode` values. Unset means premultiplied electrical.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlphaMode {
    #[default]
    PremultipliedElectrical = 0,
    PremultipliedOptical = 1,
    Straight = 2,
}

impl AlphaMode {
    pub fn shader(self) -> u32 {
        self as u32
    }
}

/// H.273 matrix the shader can apply. `Identity` is RGB and has no YCbCr matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoefficientKind {
    Identity,
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoRange {
    Full,
    Limited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coefficients {
    pub kind: CoefficientKind,
    pub range: VideoRange,
}

/// H.273 `Chroma420SampleLocType` 0 through 5.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromaSite {
    Type0,
    Type1,
    Type2,
    Type3,
    Type4,
    Type5,
}

/// Affine YCbCr→RGB matrix and the chroma site, in luma pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvConversion {
    pub rows: [[f32; 4]; 3],
    pub site: [f32; 2],
}

/// Committed color representation. `None` is the protocol unset value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ColorRepresentation {
    pub alpha_mode: Option<AlphaMode>,
    pub coefficients: Option<Coefficients>,
    pub chroma: Option<ChromaSite>,
}

impl ColorRepresentation {
    pub fn shader_alpha_mode(self) -> u32 {
        self.alpha_mode.unwrap_or_default().shader()
    }

    /// Straight or optical alpha on a buffer that has an alpha channel cannot
    /// be scanned out. Opaque buffers ignore the mode.
    pub fn allows_direct_scanout(self, has_alpha: bool) -> bool {
        !has_alpha || self.alpha_mode.unwrap_or_default() == AlphaMode::PremultipliedElectrical
    }

    pub fn yuv_conversion(self, ten_bit: bool) -> YuvConversion {
        let coefficients = self
            .coefficients
            .unwrap_or(Coefficients { kind: CoefficientKind::Bt709, range: VideoRange::Limited });
        YuvConversion {
            rows: ycbcr_matrix(coefficients, ten_bit),
            site: chroma_site(self.chroma.unwrap_or(ChromaSite::Type0)),
        }
    }
}

/// One commit's representation changes. `None` leaves the field alone.
/// `Some(None)` returns it to unset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepresentationUpdate {
    pub alpha_mode: Option<Option<AlphaMode>>,
    pub coefficients: Option<Option<Coefficients>>,
    pub chroma: Option<Option<ChromaSite>>,
}

impl RepresentationUpdate {
    pub fn unset() -> Self {
        Self {
            alpha_mode: Some(None),
            coefficients: Some(None),
            chroma: Some(None),
        }
    }

    pub fn apply(self, mut current: ColorRepresentation) -> ColorRepresentation {
        if let Some(alpha_mode) = self.alpha_mode {
            current.alpha_mode = alpha_mode;
        }
        if let Some(coefficients) = self.coefficients {
            current.coefficients = coefficients;
        }
        if let Some(chroma) = self.chroma {
            current.chroma = chroma;
        }
        current
    }

    pub fn merge(&mut self, next: Self) {
        if next.alpha_mode.is_some() {
            self.alpha_mode = next.alpha_mode;
        }
        if next.coefficients.is_some() {
            self.coefficients = next.coefficients;
        }
        if next.chroma.is_some() {
            self.chroma = next.chroma;
        }
    }
}

pub fn alpha_from_wire(value: u32) -> Option<AlphaMode> {
    match value {
        0 => Some(AlphaMode::PremultipliedElectrical),
        1 => Some(AlphaMode::PremultipliedOptical),
        2 => Some(AlphaMode::Straight),
        _ => None,
    }
}

/// `None` is a pair this compositor does not advertise.
pub fn coefficients_from_wire(coefficients: u32, range: u32) -> Option<Coefficients> {
    let range = match range {
        1 => VideoRange::Full,
        2 => VideoRange::Limited,
        _ => return None,
    };
    let kind = match coefficients {
        1 => CoefficientKind::Identity,
        2 => CoefficientKind::Bt709,
        4 => CoefficientKind::Bt601,
        6 => CoefficientKind::Bt2020,
        _ => return None,
    };
    if kind == CoefficientKind::Identity && range != VideoRange::Full {
        return None;
    }
    Some(Coefficients { kind, range })
}

pub fn chroma_from_wire(value: u32) -> Option<ChromaSite> {
    match value {
        1 => Some(ChromaSite::Type0),
        2 => Some(ChromaSite::Type1),
        3 => Some(ChromaSite::Type2),
        4 => Some(ChromaSite::Type3),
        5 => Some(ChromaSite::Type4),
        6 => Some(ChromaSite::Type5),
        _ => None,
    }
}

pub fn coefficients_match_format(coefficients: Option<Coefficients>, yuv: bool) -> bool {
    match coefficients {
        None => true,
        Some(Coefficients { kind: CoefficientKind::Identity, .. }) => !yuv,
        Some(_) => yuv,
    }
}

pub fn chroma_matches_format(chroma: Option<ChromaSite>, yuv: bool) -> bool {
    chroma.is_none() || yuv
}

/// Rows map `vec4(y, cb, cr, 1)` UNORM samples to RGB.
///
/// Limited 8-bit uses codes 16/219 and 128/224. Limited 10-bit uses 64/876 and
/// 512/896 after expanding a P010 sample (`code << 6` in a 16-bit word). Full
/// range keeps Y as the sample and centers chroma on 128 (8-bit) or 512 (10-bit).
pub fn ycbcr_matrix(coefficients: Coefficients, ten_bit: bool) -> [[f32; 4]; 3] {
    let kind = match coefficients.kind {
        CoefficientKind::Identity => CoefficientKind::Bt709,
        kind => kind,
    };
    let (kr, kb) = match kind {
        CoefficientKind::Bt601 => (0.299_f32, 0.114_f32),
        CoefficientKind::Bt709 | CoefficientKind::Identity => (0.2126_f32, 0.0722_f32),
        CoefficientKind::Bt2020 => (0.2627_f32, 0.0593_f32),
    };
    let kg = 1.0 - kr - kb;
    let (y_scale, y_bias, c_scale, c_bias) = expand(coefficients.range, ten_bit);
    let kcr = 2.0 * (1.0 - kr);
    let kcb = 2.0 * (1.0 - kb);
    let g_cb = -(2.0 * kb * (1.0 - kb) / kg);
    let g_cr = -(2.0 * kr * (1.0 - kr) / kg);
    [
        [y_scale, 0.0, kcr * c_scale, y_bias + kcr * c_bias],
        [y_scale, g_cb * c_scale, g_cr * c_scale, y_bias + (g_cb + g_cr) * c_bias],
        [y_scale, kcb * c_scale, 0.0, y_bias + kcb * c_bias],
    ]
}

fn expand(range: VideoRange, ten_bit: bool) -> (f32, f32, f32, f32) {
    if ten_bit {
        let to_code = 65535.0 / 64.0;
        match range {
            VideoRange::Limited => {
                (to_code / 876.0, -64.0 / 876.0, to_code / 896.0, -512.0 / 896.0)
            }
            VideoRange::Full => (to_code / 1023.0, 0.0, to_code / 1023.0, -512.0 / 1023.0),
        }
    } else {
        match range {
            VideoRange::Limited => (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0),
            VideoRange::Full => (1.0, 0.0, 1.0, -128.0 / 255.0),
        }
    }
}

pub fn chroma_site(site: ChromaSite) -> [f32; 2] {
    match site {
        ChromaSite::Type0 => [0.0, 0.5],
        ChromaSite::Type1 => [0.5, 0.5],
        ChromaSite::Type2 => [0.0, 0.0],
        ChromaSite::Type3 => [0.5, 0.0],
        ChromaSite::Type4 => [0.0, 1.0],
        ChromaSite::Type5 => [0.5, 1.0],
    }
}

pub fn apply_rows(rows: [[f32; 4]; 3], y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let sample = [y, cb, cr, 1.0];
    let dot = |row: [f32; 4]| {
        row[0] * sample[0] + row[1] * sample[1] + row[2] * sample[2] + row[3] * sample[3]
    };
    [dot(rows[0]), dot(rows[1]), dot(rows[2])]
}

/// Expand an NV12 or P010 sample with the default BT.709 limited matrix.
pub fn bt709_limited_rgb(y: f32, cb: f32, cr: f32, ten_bit: bool) -> [f32; 3] {
    apply_rows(
        ycbcr_matrix(
            Coefficients { kind: CoefficientKind::Bt709, range: VideoRange::Limited },
            ten_bit,
        ),
        y,
        cb,
        cr,
    )
}

/// Chroma texture coordinate for luma `uv`. `site` is in luma pixels.
pub fn chroma_coord(uv: [f32; 2], luma_size: [f32; 2], site: [f32; 2]) -> [f32; 2] {
    let luma_index = [uv[0] * luma_size[0] - 0.5, uv[1] * luma_size[1] - 0.5];
    let chroma_index = [(luma_index[0] - site[0]) * 0.5, (luma_index[1] - site[1]) * 0.5];
    let chroma_size = [luma_size[0] * 0.5, luma_size[1] * 0.5];
    [(chroma_index[0] + 0.5) / chroma_size[0], (chroma_index[1] + 0.5) / chroma_size[1]]
}

pub fn encode_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::{
        apply_rows, bt709_limited_rgb, chroma_coord, chroma_site, encode_byte, ycbcr_matrix,
        ChromaSite, CoefficientKind, Coefficients, VideoRange,
    };

    #[test]
    fn limited_black_and_white_are_the_8_bit_and_10_bit_codes() {
        let black = bt709_limited_rgb(16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, false);
        let white = bt709_limited_rgb(235.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, false);
        assert!(black.iter().all(|channel| channel.abs() < 1e-5));
        assert!(white.iter().all(|channel| (channel - 1.0).abs() < 1e-5));

        let p010 = |code: u32| ((code << 6) as f32) / 65535.0;
        let ten_black = bt709_limited_rgb(p010(64), p010(512), p010(512), true);
        let ten_white = bt709_limited_rgb(p010(940), p010(512), p010(512), true);
        assert!(ten_black.iter().all(|channel| channel.abs() < 1e-4));
        assert!(ten_white.iter().all(|channel| (channel - 1.0).abs() < 1e-4));
        assert_eq!(encode_byte(white[0]), 255);
        assert_eq!(encode_byte(black[0]), 0);
    }

    #[test]
    fn type_0_siting_samples_the_left_chroma_of_a_2x2_checker() {
        let size = [4.0, 4.0];
        let site = chroma_site(ChromaSite::Type0);
        let at = |x: f32, y: f32| chroma_coord([(x + 0.5) / 4.0, (y + 0.5) / 4.0], size, site);
        let top_left = at(0.0, 0.0);
        let top_right = at(2.0, 0.0);
        let bottom_left = at(0.0, 3.0);
        let bottom_right = at(2.0, 3.0);
        assert!((top_left[0] - 0.25).abs() < 1e-5 && top_left[1] < 0.25);
        assert!((top_right[0] - 0.75).abs() < 1e-5 && top_right[1] < 0.25);
        assert!((bottom_left[0] - 0.25).abs() < 1e-5 && bottom_left[1] > 0.75);
        assert!((bottom_right[0] - 0.75).abs() < 1e-5 && bottom_right[1] > 0.75);
    }

    #[test]
    fn straight_alpha_blocks_scanout_only_when_the_buffer_has_alpha() {
        use super::{AlphaMode, ColorRepresentation};
        let straight = ColorRepresentation {
            alpha_mode: Some(AlphaMode::Straight),
            ..ColorRepresentation::default()
        };
        assert!(!straight.allows_direct_scanout(true));
        assert!(straight.allows_direct_scanout(false));
        assert!(ColorRepresentation::default().allows_direct_scanout(true));
    }

    #[test]
    fn bt601_and_bt709_differ_on_the_same_limited_sample() {
        let y = 126.0 / 255.0;
        let cb = 64.0 / 255.0;
        let cr = 192.0 / 255.0;
        let bt709 = bt709_limited_rgb(y, cb, cr, false);
        let bt601 = apply_rows(
            ycbcr_matrix(
                Coefficients { kind: CoefficientKind::Bt601, range: VideoRange::Limited },
                false,
            ),
            y,
            cb,
            cr,
        );
        let bt2020 = apply_rows(
            ycbcr_matrix(
                Coefficients { kind: CoefficientKind::Bt2020, range: VideoRange::Full },
                false,
            ),
            y,
            cb,
            cr,
        );
        assert!(
            (bt709[0] - bt601[0]).abs() > 0.02,
            "BT.601 and BT.709 must not collapse, got {bt709:?} vs {bt601:?}"
        );
        assert!(
            (bt709[0] - bt2020[0]).abs() > 0.02,
            "full-range BT.2020 must not match limited BT.709, got {bt2020:?} vs {bt709:?}"
        );
    }
}
