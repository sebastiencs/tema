//! FP16 linear blend regions.
//!
//! Jay walks the frame front to back and keeps the low bit of a running tag:
//! a translucent surface claims its visible pixels for the blend buffer, and
//! an opaque surface claims its visible pixels for the framebuffer. Opaque
//! pixels never enter the intermediate. The caller passes no blend transfer
//! when the blend description embeds into the framebuffer (SDR gamma, or an
//! output that is already linear), which is jay's first elision.

use crate::{
    color::{ColorTransform, TransferFunction},
    config::RgbaColor,
    region::{DamageRegion, RectI},
    render::{shadow_geometry, EffectElement, FrameElement, TextureMask},
};

/// Visible pixels split between the linear intermediate and the framebuffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlendRegions {
    pub blend: DamageRegion,
    pub direct: DamageRegion,
}

/// `use_blend` is false when the linear description embeds into the output.
/// Every visible pixel then stays on the direct path.
pub fn create_regions(elements: &[FrameElement], bounds: RectI, use_blend: bool) -> BlendRegions {
    let mut covered = DamageRegion::new();
    let mut regions = BlendRegions::default();
    for element in elements.iter().rev() {
        let Some(rect) = element_rect(element).and_then(|rect| rect.intersection(bounds)) else {
            continue;
        };
        let mut visible = DamageRegion::from_rect(rect);
        visible.subtract_region(&covered);
        if use_blend && !element_is_opaque(element) {
            regions.blend.add_region(&visible);
        } else {
            regions.direct.add_region(&visible);
        }
        covered.add_rect(rect);
    }
    regions
}

pub fn intersect_damage(region: &DamageRegion, damage: &DamageRegion) -> DamageRegion {
    let mut intersection = region.clone();
    intersection.intersect_region(damage);
    intersection
}

/// Draw this element into the linear buffer. `None` means the texels are
/// already stored in `output_tf`.
pub fn retarget_color(
    color: Option<ColorTransform>,
    output_tf: TransferFunction,
) -> Option<ColorTransform> {
    Some(match color {
        Some(mut color) => {
            color.dst_tf = TransferFunction::ExtLinear;
            color
        }
        None => ColorTransform::transfer(output_tf, TransferFunction::ExtLinear),
    })
}

pub fn retarget_elements(
    elements: &[FrameElement],
    output_tf: TransferFunction,
) -> Vec<FrameElement> {
    elements
        .iter()
        .map(|element| match element {
            FrameElement::Texture { phase, element } => {
                let mut element = element.clone();
                element.color = retarget_color(element.color.as_deref().copied(), output_tf)
                    .map(std::rc::Rc::new);
                FrameElement::Texture { phase: *phase, element }
            }
            other => other.clone(),
        })
        .collect()
}

/// The copy pass encodes the linear intermediate into the output transfer.
pub fn encode_from_linear(output_tf: TransferFunction) -> ColorTransform {
    ColorTransform::transfer(TransferFunction::ExtLinear, output_tf)
}

/// Effect colors are stored in the output encoding. The linear pass decodes
/// them. `None` keeps the byte premultiply the output pass already uses.
pub fn premul_color(color: RgbaColor, decode: Option<TransferFunction>) -> [f32; 4] {
    let Some(tf) = decode else {
        let bytes = color.to_premultiplied_rgba_bytes();
        return [
            f32::from(bytes[0]) / 255.0,
            f32::from(bytes[1]) / 255.0,
            f32::from(bytes[2]) / 255.0,
            f32::from(bytes[3]) / 255.0,
        ];
    };
    let alpha = f32::from(color.a) / 255.0;
    let channel = |byte: u8| (tf.decode(f64::from(byte) / 255.0) as f32) * alpha;
    [channel(color.r), channel(color.g), channel(color.b), alpha]
}

pub fn shadow_coverage(
    color: RgbaColor,
    coverage: u8,
    decode: Option<TransferFunction>,
) -> [f32; 4] {
    let color = premul_color(color, decode);
    let coverage = f32::from(coverage) / 255.0;
    [color[0] * coverage, color[1] * coverage, color[2] * coverage, color[3] * coverage]
}

fn padded_blur_rect(bounds: RectI, style: crate::render::BlurVisualStyle) -> Option<RectI> {
    let padding = style
        .radius
        .max(1)
        .saturating_mul(i32::from(style.passes.max(1)))
        .saturating_mul(4);
    RectI::from_extents(
        bounds.x.saturating_sub(padding),
        bounds.y.saturating_sub(padding),
        bounds.x2().saturating_add(padding),
        bounds.y2().saturating_add(padding),
    )
}

fn element_is_opaque(element: &FrameElement) -> bool {
    match element {
        FrameElement::Texture { element, .. } => {
            element.alpha == u8::MAX
                && element.alpha_multiplier == u32::MAX
                && !element.blend
                && matches!(element.mask, TextureMask::None)
        }
        FrameElement::Effect { .. } => false,
    }
}

fn element_rect(element: &FrameElement) -> Option<RectI> {
    match element {
        FrameElement::Texture { element, .. } => {
            RectI::new(element.x, element.y, element.width, element.height)
        }
        FrameElement::Effect { element, .. } => match element {
            EffectElement::WindowBorder { ring, .. } => Some(ring.outer.outer_rect),
            EffectElement::WindowShadow { clip, style, .. } => {
                shadow_geometry(*clip, *style).map(|geometry| geometry.bounds)
            }
            EffectElement::WindowBlurBackdrop { ring, style, .. } => {
                padded_blur_rect(ring.outer.outer_rect, *style)
            }
            EffectElement::SurfaceBlurBackdrop { region, style, .. } => {
                padded_blur_rect(region.bounds, *style)
            }
            EffectElement::DimOverlay { clip, .. } => Some(clip.outer_rect),
        },
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{create_regions, BlendRegions},
        crate::{
            region::{DamageRegion, RectI},
            render::{
                backend::inert_texture, FrameElement, FramePhase, TextureElement, TextureMask,
            },
        },
    };

    fn texture(x: i32, y: i32, width: i32, height: i32, blend: bool) -> FrameElement {
        FrameElement::Texture {
            phase: FramePhase::Content,
            element: TextureElement {
                source: crate::render::TextureSource::Texture,
                texture: inert_texture(),
                x,
                y,
                width,
                height,
                tex_transform: crate::transform::Mat3::identity(),
                damage: DamageRegion::from_rect(RectI::new(x, y, width, height).unwrap()),
                blend,
                alpha: 255,
                alpha_multiplier: u32::MAX,
                color: None,
                alpha_mode: 0,
                yuv: None,
                mask: TextureMask::None,
            },
        }
    }

    fn rects(region: &DamageRegion) -> Vec<RectI> {
        region.rects().to_vec()
    }

    #[test]
    fn a_translucent_window_claims_its_pixels_and_leaves_the_opaque_margin() {
        let bounds = RectI::new(0, 0, 200, 100).unwrap();
        let elements = [texture(0, 0, 200, 100, false), texture(40, 20, 80, 40, true)];
        let regions = create_regions(&elements, bounds, true);
        assert_eq!(rects(&regions.blend), vec![RectI::new(40, 20, 80, 40).unwrap()]);
        let mut expected = DamageRegion::from_rect(bounds);
        expected.subtract_rect(RectI::new(40, 20, 80, 40).unwrap());
        assert_eq!(regions.direct, expected);
    }

    #[test]
    fn an_opaque_window_in_front_hides_the_translucent_one() {
        let bounds = RectI::new(0, 0, 100, 100).unwrap();
        let elements = [texture(0, 0, 100, 100, true), texture(0, 0, 100, 100, false)];
        let regions = create_regions(&elements, bounds, true);
        assert!(regions.blend.is_empty());
        assert_eq!(rects(&regions.direct), vec![bounds]);
    }

    #[test]
    fn embedding_into_the_output_skips_the_intermediate() {
        let bounds = RectI::new(0, 0, 10, 10).unwrap();
        let regions = create_regions(&[texture(0, 0, 10, 10, true)], bounds, false);
        assert_eq!(
            regions,
            BlendRegions {
                blend: DamageRegion::new(),
                direct: DamageRegion::from_rect(bounds)
            }
        );
    }

    #[test]
    fn a_translucent_window_beside_an_opaque_one_still_uses_the_buffer() {
        let bounds = RectI::new(0, 0, 100, 40).unwrap();
        let elements = [texture(0, 0, 40, 40, true), texture(50, 0, 40, 40, false)];
        let regions = create_regions(&elements, bounds, true);
        assert_eq!(rects(&regions.blend), vec![RectI::new(0, 0, 40, 40).unwrap()]);
        assert_eq!(rects(&regions.direct), vec![RectI::new(50, 0, 40, 40).unwrap()]);
    }
}
