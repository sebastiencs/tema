//! HDR output description and the HDMI static metadata infoframe.
//!
//! PQ's encoding range stays the ST 2084 swing. The display's EDID range is the
//! target volume, `max_cll`, and `max_fall`. `ColorManager::intern` rewrites a
//! PQ maximum to `min + 10000`.

use super::primaries::Primaries;

/// Protocol minimum for an HDR output. The named PQ constant stays at 0.
pub const ENCODING_MIN: f64 = 0.005;
/// Reference white when `mode = "hdr"` omits `sdr_white`.
pub const REFERENCE_WHITE: f64 = 203.0;
/// Used when the EDID has no maximum luminance.
pub const FALLBACK_MAX: f64 = 1000.0;

/// What the connector and its EDID say about HDR.
#[derive(Clone, Copy, Debug)]
pub struct DisplayHdr {
    pub pq: bool,
    /// CTA colorimetry bit 7. YCbCr BT.2020 does not count.
    pub bt2020_rgb: bool,
    /// Connector has `Colorspace = BT2020_RGB` and `HDR_OUTPUT_METADATA`.
    pub has_kms_properties: bool,
    pub min_luminance: Option<f64>,
    pub max_luminance: Option<f64>,
    pub max_fall: Option<f64>,
    pub primaries: Option<Primaries>,
}

impl DisplayHdr {
    pub const fn unsupported() -> Self {
        Self {
            pq: false,
            bt2020_rgb: false,
            has_kms_properties: false,
            min_luminance: None,
            max_luminance: None,
            max_fall: None,
            primaries: None,
        }
    }

    pub fn allows_hdr(self) -> bool {
        self.pq && self.bt2020_rgb && self.has_kms_properties
    }
}

/// Luminance overrides from `[output.color]`. `white` is the resolved reference.
#[derive(Clone, Copy, Debug)]
pub struct HdrRequest {
    pub white: f64,
    pub min_luminance: Option<f64>,
    pub max_luminance: Option<f64>,
    pub max_fall: Option<f64>,
}

/// Numbers that identify an HDR output description and its metadata blob.
#[derive(Clone, Copy, Debug)]
pub struct HdrVolume {
    pub encoding_min: f64,
    pub white: f64,
    pub mastering_min: f64,
    pub mastering_max: f64,
    pub max_fall: f64,
    pub target: Primaries,
}

/// `None` when the display cannot do PQ BT.2020 RGB with both KMS properties.
pub fn prepare(request: HdrRequest, display: DisplayHdr) -> Option<HdrVolume> {
    if !display.allows_hdr() {
        return None;
    }
    let encoding_min = nonnegative(request.min_luminance).unwrap_or(ENCODING_MIN);
    let mastering_min = nonnegative(request.min_luminance.or(display.min_luminance)).unwrap_or(0.0);
    let mastering_max =
        positive_above(request.max_luminance.or(display.max_luminance), mastering_min)
            .unwrap_or(FALLBACK_MAX);
    let max_fall = request
        .max_fall
        .or(display.max_fall)
        .filter(|value| value.is_finite() && *value > 0.0 && *value <= mastering_max)
        .unwrap_or(mastering_max);
    Some(HdrVolume {
        encoding_min,
        white: request.white,
        mastering_min,
        mastering_max,
        max_fall,
        target: display.primaries.unwrap_or(Primaries::BT2020),
    })
}

/// HDMI type-1 static metadata. 32 bytes, matching `struct hdr_output_metadata`.
///
/// EOTF is 2 (PQ). Primaries and white are multiplied by 50000. Maximum
/// mastering luminance and the content light levels are in nits. Minimum
/// mastering luminance is multiplied by 10000. There is no SDR infoframe.
pub fn metadata_bytes(volume: &HdrVolume) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[4] = 2;
    write_primary(&mut bytes, 6, volume.target.r);
    write_primary(&mut bytes, 10, volume.target.g);
    write_primary(&mut bytes, 14, volume.target.b);
    write_primary(&mut bytes, 18, volume.target.w);
    write_u16(&mut bytes, 22, nits(volume.mastering_max));
    write_u16(&mut bytes, 24, nits(volume.mastering_min * 10_000.0));
    write_u16(&mut bytes, 26, nits(volume.mastering_max));
    write_u16(&mut bytes, 28, nits(volume.max_fall));
    bytes
}

fn nonnegative(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn positive_above(value: Option<f64>, floor: f64) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > floor)
}

fn nits(value: f64) -> u16 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

fn write_primary(bytes: &mut [u8; 32], offset: usize, (x, y): (f64, f64)) {
    write_u16(bytes, offset, chromaticity(x));
    write_u16(bytes, offset + 2, chromaticity(y));
}

fn chromaticity(value: f64) -> u16 {
    nits(value * 50_000.0)
}

fn write_u16(bytes: &mut [u8; 32], offset: usize, value: u16) {
    let encoded = value.to_le_bytes();
    bytes[offset] = encoded[0];
    bytes[offset + 1] = encoded[1];
}

#[cfg(test)]
mod tests {
    use {
        super::{
            metadata_bytes, prepare, DisplayHdr, HdrRequest, ENCODING_MIN, FALLBACK_MAX,
            REFERENCE_WHITE,
        },
        crate::color::Primaries,
    };

    fn request() -> HdrRequest {
        HdrRequest {
            white: REFERENCE_WHITE,
            min_luminance: None,
            max_luminance: None,
            max_fall: None,
        }
    }

    fn capable(max: f64, fall: f64, min: f64) -> DisplayHdr {
        DisplayHdr {
            pq: true,
            bt2020_rgb: true,
            has_kms_properties: true,
            min_luminance: Some(min),
            max_luminance: Some(max),
            max_fall: Some(fall),
            primaries: Some(Primaries::DISPLAY_P3),
        }
    }

    #[test]
    fn hdr_requires_pq_bt2020_rgb_and_both_properties() {
        assert!(capable(800.0, 400.0, 0.0).allows_hdr());
        let mut ycc = capable(800.0, 400.0, 0.0);
        ycc.bt2020_rgb = false;
        assert!(!ycc.allows_hdr());
        assert!(prepare(request(), ycc).is_none());
        let mut no_pq = capable(800.0, 400.0, 0.0);
        no_pq.pq = false;
        assert!(prepare(request(), no_pq).is_none());
        let mut no_kms = capable(800.0, 400.0, 0.0);
        no_kms.has_kms_properties = false;
        assert!(prepare(request(), no_kms).is_none());
    }

    #[test]
    fn volume_uses_the_edid_and_a_missing_maximum_is_1000() {
        let volume = prepare(request(), capable(800.0, 400.0, 0.01)).unwrap();
        assert_eq!(volume.encoding_min, ENCODING_MIN);
        assert_eq!(volume.white, REFERENCE_WHITE);
        assert_eq!(volume.mastering_min, 0.01);
        assert_eq!(volume.mastering_max, 800.0);
        assert_eq!(volume.max_fall, 400.0);
        assert_eq!(volume.target, Primaries::DISPLAY_P3);

        let mut missing = capable(800.0, 400.0, 0.01);
        missing.max_luminance = None;
        missing.max_fall = None;
        missing.min_luminance = None;
        missing.primaries = None;
        let volume = prepare(request(), missing).unwrap();
        assert_eq!(volume.mastering_max, FALLBACK_MAX);
        assert_eq!(volume.max_fall, FALLBACK_MAX);
        assert_eq!(volume.mastering_min, 0.0);
        assert_eq!(volume.target, Primaries::BT2020);
    }

    #[test]
    fn overrides_replace_the_edid_range() {
        let request = HdrRequest {
            white: 100.0,
            min_luminance: Some(0.05),
            max_luminance: Some(600.0),
            max_fall: Some(300.0),
        };
        let volume = prepare(request, capable(800.0, 400.0, 0.01)).unwrap();
        assert_eq!(volume.encoding_min, 0.05);
        assert_eq!(volume.mastering_min, 0.05);
        assert_eq!(volume.mastering_max, 600.0);
        assert_eq!(volume.max_fall, 300.0);
        assert_eq!(volume.white, 100.0);
    }

    #[test]
    fn metadata_is_a_pq_type1_infoframe() {
        let volume = prepare(request(), capable(800.0, 400.0, 0.0)).unwrap();
        let bytes = metadata_bytes(&volume);
        assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
        assert_eq!(bytes[4], 2);
        assert_eq!(bytes[5], 0);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), (0.680_f64 * 50_000.0).round() as u16);
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 800);
        assert_eq!(u16::from_le_bytes([bytes[24], bytes[25]]), 0);
        assert_eq!(u16::from_le_bytes([bytes[26], bytes[27]]), 800);
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 400);
        assert_eq!(&bytes[30..], &[0, 0]);
    }
}
