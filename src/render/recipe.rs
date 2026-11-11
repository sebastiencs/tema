use std::rc::Rc;

use crate::{color::ColorTransform, region::DamageRegion, state::RootSurfaceClip, transform::Mat3};

use super::Texture;

use super::visual::{BlurRegion, BlurVisualStyle, BorderGradient, RoundedRing, ShadowVisualStyle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureSource {
    Texture,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextureMask {
    None,
    RoundedClip { clip: RootSurfaceClip },
    CornerRoundedClip { clip: RootSurfaceClip },
    FrameRing { ring: RoundedRing },
}

#[derive(Clone, Debug)]
pub struct TextureElement {
    pub source: TextureSource,
    pub texture: Rc<dyn Texture>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub tex_transform: Mat3,
    pub damage: DamageRegion,
    pub blend: bool,
    pub alpha: u8,
    /// Raw `wp_alpha_modifier_surface_v1` factor. `u32::MAX` is 1.0.
    pub alpha_multiplier: u32,
    /// `None` is the identity color path and selects the current shader.
    pub color: Option<Rc<ColorTransform>>,
    /// Shader alpha mode. Opaque textures ignore it and stay premultiplied electrical.
    pub alpha_mode: u32,
    /// `None` uses BT.709 limited, chroma type 0, when the texture is YUV.
    pub yuv: Option<crate::color::YuvConversion>,
    pub mask: TextureMask,
}

/// Window opacity times the client alpha factor. `u32::MAX` is 1.0.
pub fn effective_alpha(alpha: u8, alpha_multiplier: u32) -> f32 {
    (f32::from(alpha) / 255.0) * (f64::from(alpha_multiplier) / f64::from(u32::MAX)) as f32
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramePhase {
    UnderWindow,
    Content,
    OverWindow,
    Overlay,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectElement {
    WindowBorder {
        ring: RoundedRing,
        gradient: BorderGradient,
        alpha: u8,
    },
    WindowShadow {
        clip: RootSurfaceClip,
        style: ShadowVisualStyle,
        alpha: u8,
    },
    WindowBlurBackdrop {
        ring: RoundedRing,
        style: BlurVisualStyle,
        alpha: u8,
    },
    /// Client-requested blur under one root surface. Drawn in a later phase.
    SurfaceBlurBackdrop {
        region: BlurRegion,
        style: BlurVisualStyle,
        alpha: u8,
    },
    DimOverlay {
        clip: RootSurfaceClip,
        alpha: u8,
    },
}

#[derive(Clone, Debug)]
pub enum FrameElement {
    Texture { phase: FramePhase, element: TextureElement },
    Effect { phase: FramePhase, element: EffectElement },
}

#[derive(Clone, Debug, Default)]
pub struct OutputFrameRecipe {
    pub elements: Vec<FrameElement>,
}

impl OutputFrameRecipe {
    pub fn has_effects(&self) -> bool {
        self.elements
            .iter()
            .any(|element| matches!(element, FrameElement::Effect { .. }))
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}
