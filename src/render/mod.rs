pub mod backend;
mod blend;
mod color_pipeline;
mod factory;
pub mod gles;
pub mod recipe;
pub mod visual;
mod vulkan;

/// Color written into output pixels that no surface covers. `#282828`.
pub const OUTPUT_BACKGROUND: [f32; 4] = [40.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0, 1.0];

#[cfg(test)]
pub(crate) mod mock;

pub use factory::create_renderer;

#[cfg(test)]
pub use {backend::inert_texture, visual::rounded_ring};

pub use {
    backend::{
        FormatSupport, GraphicsApi, OffscreenTarget, RenderCompletion, RenderDeviceId, RenderFrame,
        RenderSyncMode, RenderTarget, Renderer, RendererCapabilities, RendererError, ShmImage,
        Texture,
    },
    recipe::{
        effective_alpha, EffectElement, FrameElement, FramePhase, OutputFrameRecipe,
        TextureElement, TextureMask, TextureSource,
    },
    visual::{
        map_layout_rect, place_visual_frame, shadow_geometry, BlurRegion, BlurVisualStyle,
        BorderGradient, BorderVisualStyle, RectMapping, RootSurfaceVisualState, RoundedRing,
        ShadowVisualStyle, SurfaceEffectStyle, WindowClip, WindowRenderMapping,
    },
};
