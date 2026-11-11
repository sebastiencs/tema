//! CTA-861 extension blocks from an EDID blob.
//!
//! `edid-rs` reads the base block and the extension count. The colorimetry
//! data block (extended tag 5) and the HDR static metadata data block
//! (extended tag 6) live in the extensions, so they are parsed here and stored
//! on the connector.
//!
//! Luminance code values use the CTA formulas: `50·2^(cv/32)` for the desired
//! maximum and frame-average, and `max·(cv/255)²/100` for the minimum. A
//! missing byte is `None`. A present `0` is decoded with the formula.

/// BT.2020 bits from the first colorimetry byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CtaColorimetry {
    /// Bit 7, BT.2020 RGB.
    pub bt2020_rgb: bool,
    /// Bit 6, BT.2020 YCbCr.
    pub bt2020_ycc: bool,
    /// Bit 5, BT.2020 constant-luminance YCbCr.
    pub bt2020_cycc: bool,
}

impl CtaColorimetry {
    /// Any BT.2020 colorimetry bit.
    pub fn supports_bt2020(self) -> bool {
        self.bt2020_rgb || self.bt2020_ycc || self.bt2020_cycc
    }
}

/// HDR static metadata data block, extended tag 6.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HdrStaticMetadata {
    /// Traditional gamma, SDR luminance range.
    pub sdr: bool,
    /// Traditional gamma, HDR luminance range.
    pub traditional_hdr: bool,
    /// SMPTE ST 2084 (PQ).
    pub pq: bool,
    /// Hybrid log-gamma.
    pub hlg: bool,
    /// Static metadata type 1 is supported.
    pub static_metadata_type1: bool,
    /// Desired content max luminance, cd/m².
    pub max_luminance: Option<f64>,
    /// Desired content max frame-average luminance, cd/m².
    pub max_fall: Option<f64>,
    /// Desired content min luminance, cd/m².
    pub min_luminance: Option<f64>,
}

/// CTA-861 color information found in the EDID extensions.
///
/// A repeated block replaces the earlier one. An extension whose checksum is
/// wrong is ignored. Other extension tags are ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cta861 {
    pub colorimetry: Option<CtaColorimetry>,
    pub hdr: Option<HdrStaticMetadata>,
}

/// Read CTA-861 color blocks from a whole EDID, base block included.
pub fn parse(bytes: &[u8]) -> Cta861 {
    if bytes.len() < 128 {
        return Cta861::default();
    }
    let extensions = bytes[126] as usize;
    let mut info = Cta861::default();
    for index in 0..extensions {
        let start = 128 * (index + 1);
        let end = start + 128;
        if end > bytes.len() {
            break;
        }
        parse_extension(&bytes[start..end], &mut info);
    }
    info
}

fn parse_extension(block: &[u8], info: &mut Cta861) {
    if block.len() != 128 || block[0] != 0x02 || !checksum_ok(block) || block[1] < 3 {
        return;
    }
    let offset = block[2] as usize;
    if !(4..=127).contains(&offset) {
        return;
    }
    let mut index = 4;
    while index < offset {
        let header = block[index];
        let length = (header & 0x1f) as usize;
        let tag = header >> 5;
        let payload_at = index + 1;
        let payload_end = payload_at + length;
        if payload_end > offset {
            break;
        }
        if tag == 7 {
            parse_extended(&block[payload_at..payload_end], info);
        }
        index = payload_end;
    }
}

fn parse_extended(payload: &[u8], info: &mut Cta861) {
    let Some((&tag, data)) = payload.split_first() else {
        return;
    };
    match tag {
        0x05 => {
            if let Some(bits) = data.first().copied() {
                info.colorimetry = Some(CtaColorimetry {
                    bt2020_rgb: bits & 0x80 != 0,
                    bt2020_ycc: bits & 0x40 != 0,
                    bt2020_cycc: bits & 0x20 != 0,
                });
            }
        }
        0x06 => {
            let Some(eotf) = data.first().copied() else {
                return;
            };
            let max_luminance = data.get(2).copied().map(decode_max_luminance);
            let max_fall = data.get(3).copied().map(decode_max_luminance);
            let min_luminance = match (max_luminance, data.get(4).copied()) {
                (Some(max), Some(cv)) => Some(decode_min_luminance(max, cv)),
                _ => None,
            };
            info.hdr = Some(HdrStaticMetadata {
                sdr: eotf & 0x01 != 0,
                traditional_hdr: eotf & 0x02 != 0,
                pq: eotf & 0x04 != 0,
                hlg: eotf & 0x08 != 0,
                static_metadata_type1: data.get(1).copied().unwrap_or(0) & 0x01 != 0,
                max_luminance,
                max_fall,
                min_luminance,
            });
        }
        _ => {}
    }
}

fn decode_max_luminance(cv: u8) -> f64 {
    50.0 * (f64::from(cv) / 32.0).exp2()
}

fn decode_min_luminance(max: f64, cv: u8) -> f64 {
    max * (f64::from(cv) / 255.0).powi(2) / 100.0
}

fn checksum_ok(block: &[u8]) -> bool {
    block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

#[cfg(test)]
mod tests {
    use super::{parse, Cta861};

    fn approx(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} != {right}");
    }

    #[test]
    fn base_block_has_no_cta_color_blocks() {
        let bytes = include_bytes!("../../tests/data/edid/base-only.bin");
        assert_eq!(parse(bytes), Cta861::default());
        assert!(edid_rs::parse(&mut std::io::Cursor::new(&bytes[..])).is_ok());
    }

    #[test]
    fn bt2020_and_hdr_luminance_match_the_cta_formulas() {
        let bytes = include_bytes!("../../tests/data/edid/cta-bt2020-hdr.bin");
        let info = parse(bytes);
        let colorimetry = info.colorimetry.unwrap();
        assert!(colorimetry.bt2020_rgb);
        assert!(colorimetry.bt2020_ycc);
        assert!(colorimetry.bt2020_cycc);
        assert!(colorimetry.supports_bt2020());
        let hdr = info.hdr.unwrap();
        assert!(hdr.sdr && hdr.pq && hdr.hlg);
        assert!(!hdr.traditional_hdr);
        assert!(hdr.static_metadata_type1);
        approx(hdr.max_luminance.unwrap(), 800.0);
        approx(hdr.max_fall.unwrap(), 400.0);
        approx(hdr.min_luminance.unwrap(), 0.0);
        assert_eq!(
            edid_rs::parse(&mut std::io::Cursor::new(&bytes[..]))
                .unwrap()
                .extensions,
            1
        );
    }

    #[test]
    fn a_short_hdr_block_has_no_luminance() {
        let info = parse(include_bytes!("../../tests/data/edid/cta-hdr-no-luminance.bin"));
        assert!(info.colorimetry.is_none());
        let hdr = info.hdr.unwrap();
        assert!(hdr.traditional_hdr);
        assert!(!hdr.sdr && !hdr.pq && !hdr.hlg);
        assert!(!hdr.static_metadata_type1);
        assert!(
            hdr.max_luminance.is_none() && hdr.max_fall.is_none() && hdr.min_luminance.is_none()
        );
    }

    #[test]
    fn code_value_one_uses_the_exponential_luminance() {
        let info = parse(include_bytes!("../../tests/data/edid/cta-hdr-cv1.bin"));
        let colorimetry = info.colorimetry.unwrap();
        assert!(colorimetry.bt2020_cycc);
        assert!(!colorimetry.bt2020_rgb && !colorimetry.bt2020_ycc);
        let hdr = info.hdr.unwrap();
        assert!(hdr.pq && hdr.static_metadata_type1);
        assert!(!hdr.sdr && !hdr.traditional_hdr && !hdr.hlg);
        let max = 50.0 * (1.0_f64 / 32.0).exp2();
        approx(hdr.max_luminance.unwrap(), max);
        approx(hdr.max_fall.unwrap(), max);
        approx(hdr.min_luminance.unwrap(), max / 100.0);
        assert!((max - 51.095).abs() < 0.001);
        assert!((max / 100.0 - 0.511).abs() < 0.001);
    }

    #[test]
    fn the_second_extension_is_read() {
        let info = parse(include_bytes!("../../tests/data/edid/cta-second-extension.bin"));
        assert!(info.colorimetry.unwrap().supports_bt2020());
        approx(info.hdr.unwrap().max_luminance.unwrap(), 800.0);
    }

    #[test]
    fn a_bad_extension_checksum_is_ignored() {
        assert_eq!(
            parse(include_bytes!("../../tests/data/edid/cta-bad-checksum.bin")),
            Cta861::default()
        );
    }

    #[test]
    fn a_bad_first_extension_does_not_hide_a_later_one() {
        let mut bytes = include_bytes!("../../tests/data/edid/cta-second-extension.bin").to_vec();
        bytes[128 + 4] ^= 0xff;
        let info = parse(&bytes);
        assert!(info.colorimetry.unwrap().bt2020_rgb);
        approx(info.hdr.unwrap().max_luminance.unwrap(), 800.0);
    }

    #[test]
    fn truncated_revision_and_other_tags_contribute_nothing() {
        assert_eq!(parse(&[]), Cta861::default());
        assert_eq!(parse(&[0; 100]), Cta861::default());

        let mut bytes = include_bytes!("../../tests/data/edid/cta-bt2020-hdr.bin").to_vec();
        bytes.truncate(200);
        assert_eq!(parse(&bytes), Cta861::default());

        let mut display_id = include_bytes!("../../tests/data/edid/base-only.bin").to_vec();
        display_id[126] = 1;
        display_id.resize(256, 0);
        display_id[128] = 0x70;
        assert_eq!(parse(&display_id), Cta861::default());

        let mut old = include_bytes!("../../tests/data/edid/cta-bt2020-hdr.bin").to_vec();
        old[129] = 1;
        let sum = old[128..255]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        old[255] = 0u8.wrapping_sub(sum);
        assert_eq!(parse(&old), Cta861::default());

        let mut overrun = include_bytes!("../../tests/data/edid/cta-bt2020-hdr.bin").to_vec();
        // Length 31 runs past the detailed-timing offset, so the block is dropped.
        overrun[132] = 0xff;
        let sum = overrun[128..255]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        overrun[255] = 0u8.wrapping_sub(sum);
        assert_eq!(parse(&overrun).hdr, None);
        assert_eq!(parse(&overrun).colorimetry, None);
    }

    #[test]
    fn a_later_block_replaces_an_earlier_one() {
        let mut bytes = include_bytes!("../../tests/data/edid/cta-bt2020-hdr.bin").to_vec();
        // A second colorimetry block, only BT.2020 YCC, inside the unused area.
        // Move the detailed-timing offset forward and fix the checksum.
        let extension = &mut bytes[128..256];
        let offset = extension[2] as usize;
        extension[offset] = 0xe3;
        extension[offset + 1] = 0x05;
        extension[offset + 2] = 0x40;
        extension[offset + 3] = 0x00;
        extension[2] = (offset + 4) as u8;
        let sum = extension[..127]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        extension[127] = 0u8.wrapping_sub(sum);
        let info = parse(&bytes);
        let colorimetry = info.colorimetry.unwrap();
        assert!(colorimetry.bt2020_ycc);
        assert!(!colorimetry.bt2020_rgb && !colorimetry.bt2020_cycc);
        approx(info.hdr.unwrap().max_luminance.unwrap(), 800.0);
    }
}
