use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use khronos_types::{egl, gl};

use {
    super::{
        import::{EglImage, GlTexture},
        loader::EGL,
    },
    crate::{
        drm::Format,
        render::{color_pipeline::TexturePipelineKey, RenderDeviceId},
    },
};

#[derive(Debug)]
pub struct EglRenderBuffer {
    pub(super) egl: EGL,
    pub(super) display: egl::EGLDisplay,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) format: &'static Format,
    pub(super) device_id: RenderDeviceId,
    pub renderbuffer_object: gl::GLuint,
}

impl Drop for EglRenderBuffer {
    fn drop(&mut self) {
        unsafe { gl::DeleteRenderbuffers(1, &self.renderbuffer_object) }
    }
}

impl EglRenderBuffer {
    pub(super) fn make_framebuffer(self, programs: Rc<GlesPrograms>) -> EglFrameBuffer {
        let mut framebuffer_object = 0;

        unsafe {
            gl::GenFramebuffers(1, &mut framebuffer_object);
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::FramebufferRenderbuffer(
                gl::FRAMEBUFFER as _,
                gl::COLOR_ATTACHMENT0 as _,
                gl::RENDERBUFFER as _,
                self.renderbuffer_object,
            );
            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER as _);
            assert_eq!(status, gl::FRAMEBUFFER_COMPLETE as _);
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, 0);
        }

        EglFrameBuffer {
            egl: self.egl.clone(),
            display: self.display,
            width: self.width,
            height: self.height,
            device_id: self.device_id,
            render_buffer: self,
            framebuffer_object,
            programs,
            linear_blend: RefCell::new(None),
            imported_image: None,
        }
    }
}

#[derive(Debug)]
pub struct EglFrameBuffer {
    pub(super) egl: EGL,
    pub(super) display: egl::EGLDisplay,
    pub width: i32,
    pub height: i32,
    pub(super) device_id: RenderDeviceId,
    pub(super) render_buffer: EglRenderBuffer,
    pub framebuffer_object: gl::GLuint,
    /// Shader programs and the dim texture shared with every framebuffer on this device.
    pub(super) programs: Rc<GlesPrograms>,
    /// FP16 intermediate for translucent HDR pixels. Reused while the size matches.
    pub(super) linear_blend: RefCell<Option<LinearBlendTarget>>,
    /// Keeps the imported output image alive for the renderbuffer created from it.
    /// Jay and Smithay store the EGL image the same way. It is not also bound as a texture.
    pub(super) imported_image: Option<EglImage>,
}

impl Drop for EglFrameBuffer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteFramebuffers(1, &self.framebuffer_object);
        }
    }
}

/// RGBA16F color buffer. Sampling is nearest: ES 3.0 does not filter this format.
#[derive(Debug)]
pub(super) struct LinearBlendTarget {
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) framebuffer_object: gl::GLuint,
    pub(super) texture: gl::GLuint,
}

impl LinearBlendTarget {
    pub(super) fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut texture = 0;
        let mut framebuffer_object = 0;
        unsafe {
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D as _, texture);
            gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_WRAP_S as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_WRAP_T as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_MIN_FILTER as _, gl::NEAREST as _);
            gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_MAG_FILTER as _, gl::NEAREST as _);
            gl::TexImage2D(
                gl::TEXTURE_2D as _,
                0,
                gl::RGBA16F as _,
                width,
                height,
                0,
                gl::RGBA as _,
                gl::HALF_FLOAT as _,
                std::ptr::null(),
            );
            gl::BindTexture(gl::TEXTURE_2D as _, 0);
            gl::GenFramebuffers(1, &mut framebuffer_object);
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER as _,
                gl::COLOR_ATTACHMENT0 as _,
                gl::TEXTURE_2D as _,
                texture,
                0,
            );
            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER as _);
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, 0);
            if status != gl::FRAMEBUFFER_COMPLETE as _ {
                gl::DeleteFramebuffers(1, &framebuffer_object);
                gl::DeleteTextures(1, &texture);
                return None;
            }
        }
        Some(Self { width, height, framebuffer_object, texture })
    }
}

impl Drop for LinearBlendTarget {
    fn drop(&mut self) {
        unsafe {
            if self.framebuffer_object != 0 {
                gl::DeleteFramebuffers(1, &self.framebuffer_object);
            }
            if self.texture != 0 {
                gl::DeleteTextures(1, &self.texture);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TextureProgram {
    pub(super) program: gl::Uint,
    pub(super) position_location: gl::Uint,
    pub(super) texcoord_location: gl::Uint,
    pub(super) sampler_location: gl::Int,
    pub(super) transform_location: gl::Int,
    pub(super) tex_transform_location: gl::Int,
    pub(super) item_size_location: gl::Int,
    pub(super) clip_mode_location: gl::Int,
    pub(super) clip_outer_rect_location: gl::Int,
    pub(super) clip_inner_rect_location: gl::Int,
    pub(super) outer_corner_radius_location: gl::Int,
    pub(super) outer_rounding_power_location: gl::Int,
    pub(super) inner_corner_radius_location: gl::Int,
    pub(super) inner_rounding_power_location: gl::Int,
    pub(super) alpha_location: gl::Int,
    pub(super) opaque_sample_location: gl::Int,
    pub(super) color_matrix_0: gl::Int,
    pub(super) color_matrix_1: gl::Int,
    pub(super) color_matrix_2: gl::Int,
    pub(super) src_bt1886_lb_over_lw: gl::Int,
    pub(super) dst_bt1886_lb_over_lw: gl::Int,
    pub(super) src_power_exponent: gl::Int,
    pub(super) dst_power_exponent: gl::Int,
    pub(super) uv_sampler_location: gl::Int,
    pub(super) yuv_size_location: gl::Int,
    pub(super) yuv_site_location: gl::Int,
    pub(super) yuv_row0: gl::Int,
    pub(super) yuv_row1: gl::Int,
    pub(super) yuv_row2: gl::Int,
    pub(super) tonemap_max: gl::Int,
    pub(super) tonemap_dst_max: gl::Int,
    pub(super) tonemap_dst_min: gl::Int,
    pub(super) tonemap_dst_ref: gl::Int,
    pub(super) tonemap_to_lms_0: gl::Int,
    pub(super) tonemap_to_lms_1: gl::Int,
    pub(super) tonemap_to_lms_2: gl::Int,
    pub(super) tonemap_from_lms_0: gl::Int,
    pub(super) tonemap_from_lms_1: gl::Int,
    pub(super) tonemap_from_lms_2: gl::Int,
}

#[derive(Debug)]
pub(super) struct BlurProgram {
    pub(super) program: gl::Uint,
    pub(super) position_location: gl::Uint,
    pub(super) texcoord_location: gl::Uint,
    pub(super) sampler_location: gl::Int,
    pub(super) direction_location: gl::Int,
    pub(super) texel_size_location: gl::Int,
    pub(super) radius_location: gl::Int,
    pub(super) transform_location: gl::Int,
    pub(super) tex_transform_location: gl::Int,
    pub(super) item_size_location: gl::Int,
}

#[derive(Debug)]
pub(super) struct BorderProgram {
    pub(super) program: gl::Uint,
    pub(super) position_location: gl::Uint,
    pub(super) transform_location: gl::Int,
    pub(super) item_size_location: gl::Int,
    pub(super) outer_rect_location: gl::Int,
    pub(super) inner_rect_location: gl::Int,
    pub(super) outer_corner_radius_location: gl::Int,
    pub(super) outer_rounding_power_location: gl::Int,
    pub(super) inner_corner_radius_location: gl::Int,
    pub(super) inner_rounding_power_location: gl::Int,
    pub(super) start_color_location: gl::Int,
    pub(super) end_color_location: gl::Int,
    pub(super) angle_location: gl::Int,
    pub(super) alpha_location: gl::Int,
}

#[derive(Debug)]
pub(super) struct ShadowProgram {
    pub(super) program: gl::Uint,
    pub(super) position_location: gl::Uint,
    pub(super) transform_location: gl::Int,
    pub(super) item_size_location: gl::Int,
    pub(super) shadow_color_location: gl::Int,
    pub(super) inner_rect_location: gl::Int,
    pub(super) corner_radius_location: gl::Int,
    pub(super) rounding_power_location: gl::Int,
    pub(super) shadow_range_location: gl::Int,
    pub(super) shadow_power_location: gl::Int,
}

/// Programs compiled once per GLES device and shared by that device's swapchain buffers.
#[derive(Debug)]
pub(super) struct GlesPrograms {
    pub(super) texture: RefCell<HashMap<TexturePipelineKey, Result<TextureProgram, String>>>,
    pub(super) blur: OnceCell<Result<BlurProgram, String>>,
    pub(super) border: OnceCell<Result<BorderProgram, String>>,
    pub(super) shadow: OnceCell<Result<ShadowProgram, String>>,
    pub(super) dim: OnceCell<Rc<GlTexture>>,
}

impl GlesPrograms {
    pub(super) fn new() -> Self {
        Self {
            texture: RefCell::new(HashMap::new()),
            blur: OnceCell::new(),
            border: OnceCell::new(),
            shadow: OnceCell::new(),
            dim: OnceCell::new(),
        }
    }

    /// Delete GL objects while this device's context is current.
    pub(super) fn destroy_gl(&mut self) {
        for program in self.texture.borrow_mut().drain() {
            if let (_, Ok(program)) = program {
                unsafe { gl::DeleteProgram(program.program) };
            }
        }
        if let Some(Ok(program)) = self.blur.take() {
            unsafe { gl::DeleteProgram(program.program) };
        }
        if let Some(Ok(program)) = self.border.take() {
            unsafe { gl::DeleteProgram(program.program) };
        }
        if let Some(Ok(program)) = self.shadow.take() {
            unsafe { gl::DeleteProgram(program.program) };
        }
        self.dim.take();
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::GlesPrograms;

    #[test]
    fn one_device_shares_its_program_cache_across_swapchain_buffers() {
        let programs = Rc::new(GlesPrograms::new());
        let first_buffer = Rc::clone(&programs);
        let second_buffer = Rc::clone(&programs);

        assert!(Rc::ptr_eq(&first_buffer, &second_buffer));
    }
}
