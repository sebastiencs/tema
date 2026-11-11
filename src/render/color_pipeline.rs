//! Texture-shader variants. `None` on [`TextureElement`](super::TextureElement)
//! uses [`TexturePipelineKey::identity`], whose GLES source is the current
//! fragment shader with no defines.

use crate::color::{ColorTransform, TransferFunction};

pub const TF_BT1886: u32 = 1;
pub const TF_GAMMA22: u32 = 2;
pub const TF_GAMMA28: u32 = 3;
pub const TF_ST240: u32 = 4;
pub const TF_EXT_LINEAR: u32 = 5;
pub const TF_LOG100: u32 = 6;
pub const TF_LOG316: u32 = 7;
pub const TF_ST2084_PQ: u32 = 11;
pub const TF_ST428: u32 = 12;
pub const TF_COMPOUND_POWER_2_4: u32 = 14;
pub const TF_POWER: u32 = 15;

pub const ALPHA_PREMUL_ELECTRICAL: u32 = 0;
pub const ALPHA_PREMUL_OPTICAL: u32 = 1;
pub const ALPHA_STRAIGHT: u32 = 2;
pub const YUV_NONE: u32 = 0;
/// NV12. The matrix uniform carries coefficients and range. 8-bit codes.
pub const YUV_BT709_LIMITED_8: u32 = 1;
/// P010. The matrix uniform carries coefficients and range. 10-bit codes in the high bits.
pub const YUV_BT709_LIMITED_10: u32 = 2;

pub const SPEC_SRC_TF: u32 = 0;
pub const SPEC_DST_TF: u32 = 1;
pub const SPEC_HAS_MATRIX: u32 = 2;
pub const SPEC_ALPHA_MODE: u32 = 3;
pub const SPEC_YUV_MODE: u32 = 4;
pub const SPEC_TONEMAP: u32 = 5;

const COLOR_GLSL: &str = include_str!("shaders_common/color.glsl");
const TEXTURE_CLIP_FRAG: &str = include_str!("../shaders/texture_clip.frag");

/// One compiled texture program. `blend` is a Vulkan pipeline flag; GLES blend
/// state is not part of the shader.
///
/// The fields are `u32`s because they are specialization constants. Production
/// keys only use the transfer-function ids from [`shader_tf`], alpha modes
/// 0..=2, YUV modes 0..=2, and the perceptual tonemap bit. [`Self::is_admitted`]
/// is that finite set, and both renderer caches refuse a key outside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TexturePipelineKey {
    pub blend: bool,
    pub src_tf: u32,
    pub dst_tf: u32,
    pub has_matrix: bool,
    pub alpha_mode: u32,
    pub yuv_mode: u32,
    pub tonemap: bool,
}

impl TexturePipelineKey {
    /// The program in use today: no color conversion, Vulkan blending on.
    pub const fn identity() -> Self {
        Self {
            blend: true,
            src_tf: TF_GAMMA22,
            dst_tf: TF_GAMMA22,
            has_matrix: false,
            alpha_mode: ALPHA_PREMUL_ELECTRICAL,
            yuv_mode: YUV_NONE,
            tonemap: false,
        }
    }

    pub fn from_color(color: Option<&ColorTransform>, blend: bool) -> Self {
        let Some(color) = color else {
            let mut key = Self::identity();
            key.blend = blend;
            return key;
        };
        Self {
            blend,
            src_tf: shader_tf(color.src_tf),
            dst_tf: shader_tf(color.dst_tf),
            has_matrix: !matrix_is_identity(color.matrix),
            alpha_mode: ALPHA_PREMUL_ELECTRICAL,
            yuv_mode: YUV_NONE,
            tonemap: color.tonemap.is_some(),
        }
    }

    /// Transfer functions, alpha modes, and YUV modes the shaders implement.
    pub fn is_admitted(self) -> bool {
        admitted_tf(self.src_tf)
            && admitted_tf(self.dst_tf)
            && self.alpha_mode <= ALPHA_STRAIGHT
            && self.yuv_mode <= YUV_BT709_LIMITED_10
    }

    /// Same fragment shader as today. Blend does not change that source.
    pub fn selects_current_shader(self) -> bool {
        self.src_tf == self.dst_tf
            && !self.has_matrix
            && self.alpha_mode == ALPHA_PREMUL_ELECTRICAL
            && self.yuv_mode == YUV_NONE
            && !self.tonemap
    }

    /// Empty for the current shader, so its pipeline is created with no
    /// specialization info. Color variants carry the five constants.
    pub fn specialization_constants(self) -> Vec<(u32, u32)> {
        if self.selects_current_shader() {
            Vec::new()
        } else {
            vec![
                (SPEC_SRC_TF, self.src_tf),
                (SPEC_DST_TF, self.dst_tf),
                (SPEC_HAS_MATRIX, u32::from(self.has_matrix)),
                (SPEC_ALPHA_MODE, self.alpha_mode),
                (SPEC_YUV_MODE, self.yuv_mode),
                (SPEC_TONEMAP, u32::from(self.tonemap)),
            ]
        }
    }
}

fn admitted_tf(tf: u32) -> bool {
    matches!(
        tf,
        TF_BT1886
            | TF_GAMMA22
            | TF_GAMMA28
            | TF_ST240
            | TF_EXT_LINEAR
            | TF_LOG100
            | TF_LOG316
            | TF_ST2084_PQ
            | TF_ST428
            | TF_COMPOUND_POWER_2_4
            | TF_POWER
    )
}

pub fn shader_tf(tf: TransferFunction) -> u32 {
    match tf {
        TransferFunction::Bt1886 { .. } => TF_BT1886,
        TransferFunction::Gamma22 => TF_GAMMA22,
        TransferFunction::Gamma28 => TF_GAMMA28,
        TransferFunction::St240 => TF_ST240,
        TransferFunction::ExtLinear => TF_EXT_LINEAR,
        TransferFunction::Log100 => TF_LOG100,
        TransferFunction::Log316 => TF_LOG316,
        TransferFunction::St2084Pq => TF_ST2084_PQ,
        TransferFunction::St428 => TF_ST428,
        TransferFunction::CompoundPower24 => TF_COMPOUND_POWER_2_4,
        TransferFunction::Power(_) => TF_POWER,
    }
}

/// Identity is `texture_clip.frag` with nothing prepended. Any other key is
/// defines, then `color.glsl`, then the clip shader without a second precision line.
pub fn gles_fragment_source(key: TexturePipelineKey) -> String {
    if key.selects_current_shader() {
        return TEXTURE_CLIP_FRAG.to_string();
    }
    let mut source = String::from("precision highp float;\n");
    source.push_str(&format!("#define SRC_TF {}\n", key.src_tf));
    source.push_str(&format!("#define DST_TF {}\n", key.dst_tf));
    source.push_str(&format!("#define HAS_MATRIX {}\n", u32::from(key.has_matrix)));
    source.push_str(&format!("#define ALPHA_MODE {}\n", key.alpha_mode));
    source.push_str(&format!("#define YUV_MODE {}\n", key.yuv_mode));
    source.push_str(&format!("#define TONEMAP {}\n", u32::from(key.tonemap)));
    let needs_color = key.src_tf != key.dst_tf
        || key.has_matrix
        || key.alpha_mode != ALPHA_PREMUL_ELECTRICAL
        || key.tonemap;
    source.push_str(&format!("#define NEEDS_COLOR_PATH {}\n", u32::from(needs_color)));
    source.push_str("#define COLOR_LOOSE_UNIFORMS 1\n");
    source.push_str(COLOR_GLSL);
    source.push_str(
        TEXTURE_CLIP_FRAG
            .strip_prefix("precision highp float;\n")
            .unwrap_or(TEXTURE_CLIP_FRAG),
    );
    source
}

/// Values uploaded with a color transform. Identity draws leave these unread.
pub struct ColorUniforms {
    pub matrix: [[f32; 4]; 3],
    pub src_bt1886_lb_over_lw: f32,
    pub dst_bt1886_lb_over_lw: f32,
    pub src_power_exponent: f32,
    pub dst_power_exponent: f32,
    pub tonemap_max: f32,
    pub tonemap_dst_max: f32,
    pub tonemap_dst_min: f32,
    pub tonemap_dst_ref: f32,
    pub tonemap_to_lms: [[f32; 4]; 3],
    pub tonemap_from_lms: [[f32; 4]; 3],
}

pub fn color_uniforms(color: Option<&ColorTransform>) -> ColorUniforms {
    let Some(color) = color else {
        return ColorUniforms {
            matrix: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            src_bt1886_lb_over_lw: 0.0,
            dst_bt1886_lb_over_lw: 0.0,
            src_power_exponent: 1.0,
            dst_power_exponent: 1.0,
            tonemap_max: 0.0,
            tonemap_dst_max: 1.0,
            tonemap_dst_min: 0.0,
            tonemap_dst_ref: 1.0,
            tonemap_to_lms: identity_rows(),
            tonemap_from_lms: identity_rows(),
        };
    };
    let row = |index: usize| {
        let row = color.matrix[index];
        [row[0] as f32, row[1] as f32, row[2] as f32, row[3] as f32]
    };
    let (src_bt1886_lb_over_lw, src_power_exponent) = tf_uniforms(color.src_tf);
    let (dst_bt1886_lb_over_lw, dst_power_exponent) = tf_uniforms(color.dst_tf);
    let tone = color.tonemap;
    ColorUniforms {
        matrix: [row(0), row(1), row(2)],
        src_bt1886_lb_over_lw,
        dst_bt1886_lb_over_lw,
        src_power_exponent,
        dst_power_exponent,
        tonemap_max: tone.map(|tone| tone.max_luminance).unwrap_or(0.0),
        tonemap_dst_max: tone.map(|tone| tone.dst_max).unwrap_or(1.0),
        tonemap_dst_min: tone.map(|tone| tone.dst_min).unwrap_or(0.0),
        tonemap_dst_ref: tone.map(|tone| tone.dst_ref).unwrap_or(1.0),
        tonemap_to_lms: tone.map(|tone| tone.to_lms).unwrap_or_else(identity_rows),
        tonemap_from_lms: tone.map(|tone| tone.from_lms).unwrap_or_else(identity_rows),
    }
}

fn identity_rows() -> [[f32; 4]; 3] {
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]
}

fn tf_uniforms(tf: TransferFunction) -> (f32, f32) {
    match tf {
        TransferFunction::Bt1886 { lb_over_lw } => (lb_over_lw, 1.0),
        TransferFunction::Power(exponent) => (0.0, exponent),
        _ => (0.0, 1.0),
    }
}

fn matrix_is_identity(matrix: [[f64; 4]; 3]) -> bool {
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    for row in 0..3 {
        for col in 0..4 {
            if (matrix[row][col] - identity[row][col]).abs() > 1e-6 {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        gles_fragment_source, TexturePipelineKey, ALPHA_PREMUL_ELECTRICAL, SPEC_DST_TF,
        SPEC_HAS_MATRIX, SPEC_SRC_TF, TF_GAMMA22, TF_ST2084_PQ,
    };

    #[test]
    fn identity_key_compiles_the_current_fragment_shader() {
        let source = gles_fragment_source(TexturePipelineKey::identity());
        assert_eq!(source, include_str!("../shaders/texture_clip.frag"));
        assert!(!source.contains("#define SRC_TF"));
        assert!(!source.contains("decode_tf"));
        assert!(TexturePipelineKey::identity()
            .specialization_constants()
            .is_empty());
    }

    #[test]
    fn a_color_key_prepends_defines_and_the_shared_snippet() {
        let mut key = TexturePipelineKey::identity();
        key.src_tf = TF_ST2084_PQ;
        key.dst_tf = TF_GAMMA22;
        key.has_matrix = true;
        let source = gles_fragment_source(key);
        assert!(source.starts_with("precision highp float;\n#define SRC_TF 11\n"));
        assert!(source.contains("#define DST_TF 2\n"));
        assert!(source.contains("#define HAS_MATRIX 1\n"));
        assert!(source.contains("#define NEEDS_COLOR_PATH 1\n"));
        assert!(source.contains("#define COLOR_LOOSE_UNIFORMS 1\n"));
        assert!(source.contains("uniform vec4 u_color_matrix_0;"));
        assert!(source.contains("vec3 decode_tf(int tf, vec3 value)"));
        assert!(source.contains("vec3 encode_tf(int tf, vec3 value)"));
        assert!(source
            .contains("apply_color(color, 1, SRC_TF, DST_TF, HAS_MATRIX, ALPHA_MODE, 1, TONEMAP)"));
        assert!(source.contains("color *= u_alpha"));
        assert_eq!(source.matches("precision highp float;").count(), 1);
        let constants = key.specialization_constants();
        assert_eq!(
            constants,
            vec![
                (SPEC_SRC_TF, TF_ST2084_PQ),
                (SPEC_DST_TF, TF_GAMMA22),
                (SPEC_HAS_MATRIX, 1),
                (super::SPEC_ALPHA_MODE, ALPHA_PREMUL_ELECTRICAL),
                (super::SPEC_YUV_MODE, super::YUV_NONE),
                (super::SPEC_TONEMAP, 0),
            ]
        );
    }

    #[test]
    fn blend_does_not_change_the_gles_source() {
        let mut unblended = TexturePipelineKey::identity();
        unblended.blend = false;
        assert!(unblended.selects_current_shader());
        assert_eq!(
            gles_fragment_source(unblended),
            gles_fragment_source(TexturePipelineKey::identity())
        );
    }

    #[test]
    fn shader_ids_match_the_glsl_defines() {
        let glsl = include_str!("shaders_common/color.glsl");
        for (name, value) in [
            ("TF_GAMMA22", TF_GAMMA22),
            ("TF_ST2084_PQ", TF_ST2084_PQ),
            ("TF_EXT_LINEAR", super::TF_EXT_LINEAR),
            ("TF_COMPOUND_POWER_2_4", super::TF_COMPOUND_POWER_2_4),
            ("ALPHA_PREMUL_ELECTRICAL", ALPHA_PREMUL_ELECTRICAL),
            ("YUV_NONE", super::YUV_NONE),
        ] {
            assert!(
                glsl.contains(&format!("#define {name} {value}")),
                "missing #define {name} {value}"
            );
        }
    }

    #[test]
    fn texture_pipeline_keys_span_a_finite_set() {
        use std::collections::HashSet;

        use crate::color::TransferFunction;

        let functions = [
            TransferFunction::Bt1886 { lb_over_lw: 0.0 },
            TransferFunction::Bt1886 { lb_over_lw: 0.01 },
            TransferFunction::Gamma22,
            TransferFunction::Gamma28,
            TransferFunction::St240,
            TransferFunction::ExtLinear,
            TransferFunction::Log100,
            TransferFunction::Log316,
            TransferFunction::St2084Pq,
            TransferFunction::St428,
            TransferFunction::CompoundPower24,
            TransferFunction::Power(1.8),
            TransferFunction::Power(2.4),
        ];
        let tf_ids = functions
            .into_iter()
            .map(super::shader_tf)
            .collect::<HashSet<_>>();
        assert_eq!(tf_ids.len(), 11);
        assert_eq!(
            super::shader_tf(TransferFunction::Power(1.8)),
            super::shader_tf(TransferFunction::Power(2.4))
        );
        let mut keys = HashSet::new();
        for blend in [false, true] {
            for has_matrix in [false, true] {
                for tonemap in [false, true] {
                    for alpha_mode in 0..=2 {
                        for yuv_mode in 0..=2 {
                            for src_tf in &tf_ids {
                                for dst_tf in &tf_ids {
                                    let key = TexturePipelineKey {
                                        blend,
                                        src_tf: *src_tf,
                                        dst_tf: *dst_tf,
                                        has_matrix,
                                        alpha_mode,
                                        yuv_mode,
                                        tonemap,
                                    };
                                    assert!(key.is_admitted());
                                    keys.insert(key);
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(keys.len(), 2 * 2 * 2 * 3 * 3 * 11 * 11);
        let mut garbage = TexturePipelineKey::identity();
        garbage.src_tf = 99;
        assert!(!garbage.is_admitted());
        garbage = TexturePipelineKey::identity();
        garbage.alpha_mode = 3;
        assert!(!garbage.is_admitted());
        garbage = TexturePipelineKey::identity();
        garbage.yuv_mode = 3;
        assert!(!garbage.is_admitted());
    }

    #[test]
    fn untagged_default_output_uses_the_current_shader() {
        use crate::color::{ColorManager, ColorTransform, RenderIntent};

        let manager = ColorManager::new();
        let output = manager.srgb_gamma22();
        let untagged = ColorManager::untagged_srgb_gamma22();
        assert!(ColorTransform::between(&untagged, &output, RenderIntent::Perceptual).is_none());
        assert_eq!(manager.transform_cache_len(), 0);
        let key = TexturePipelineKey::from_color(None, true);
        assert!(key.selects_current_shader());
        assert!(key.specialization_constants().is_empty());
        assert_eq!(gles_fragment_source(key), include_str!("../shaders/texture_clip.frag"));
    }
}
