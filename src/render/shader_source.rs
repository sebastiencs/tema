//! GLES shader source assembly.
//!
//! GLSL ES 1.00 has no `#include`. GLES shader files write the same
//! `#include "rounded_rect.glsl"` line as the Vulkan files, and this module
//! pastes the shared snippet in its place before compiling.

pub(crate) const ROUNDED_RECT_GLSL: &str = include_str!("shaders_common/rounded_rect.glsl");

const ROUNDED_RECT_INCLUDE: &str = "#include \"rounded_rect.glsl\"";

/// Replace every `#include "rounded_rect.glsl"` line with the snippet.
pub(crate) fn expand_gles_includes(source: &str) -> String {
    source.replace(ROUNDED_RECT_INCLUDE, ROUNDED_RECT_GLSL)
}

#[cfg(test)]
mod tests {
    use super::{expand_gles_includes, ROUNDED_RECT_GLSL};

    #[test]
    fn include_line_is_replaced_by_the_snippet() {
        let source = "precision highp float;\n#include \"rounded_rect.glsl\"\nvoid main() {}\n";
        let expanded = expand_gles_includes(source);
        assert!(!expanded.contains("#include"));
        assert!(expanded.contains("float roundedRectCoverage("));
        assert!(expanded.starts_with("precision highp float;\n"));
        assert!(expanded.ends_with("void main() {}\n"));
        assert_eq!(expanded.matches(ROUNDED_RECT_GLSL).count(), 1);
    }

    #[test]
    fn gles_shader_files_include_the_snippet() {
        for (name, source) in [
            ("texture_clip.frag", include_str!("../shaders/texture_clip.frag")),
            ("border.frag", include_str!("../shaders/border.frag")),
            ("shadow.frag", include_str!("../shaders/shadow.frag")),
        ] {
            assert_eq!(
                source.matches("#include \"rounded_rect.glsl\"").count(),
                1,
                "{name} must include rounded_rect.glsl once"
            );
        }
    }
}
