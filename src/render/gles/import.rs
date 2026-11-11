use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
};

use {
    drm::buffer::DrmFourcc,
    khronos_types::{egl, gl},
};

use {
    super::loader::ImageKHR,
    crate::{
        drm::{client_format, Format, FormatKind, Modifier},
        render::{RenderDeviceId, Texture},
        wayland_objects::wl_shm_pool::SliceSharedMemory,
    },
};

#[derive(Debug)]
pub struct EglModifier {
    pub modifier: Modifier,
    pub external_only: bool,
}

#[derive(Debug)]
pub struct EglFormat {
    pub format: &'static Format,
    pub implicit_external_only: bool,
    pub modifiers: BTreeMap<Modifier, EglModifier>,
}

#[derive(Debug)]
pub struct OpenGlFormat {
    pub format: &'static Format,
    pub read_modifiers: BTreeSet<Modifier>,
    pub write_modifiers: BTreeSet<Modifier>,
}

#[derive(Debug)]
pub struct EglImage {
    pub(super) display: egl::EGLDisplay,
    pub(super) image: ImageKHR,
    pub(super) external_only: bool,
    pub(super) destroyed: bool,
}

impl Drop for EglImage {
    fn drop(&mut self) {
        if self.destroyed || self.image.ptr.is_null() {
            return;
        }
        unsafe {
            let _ = egl::DestroyImageKHR(self.display, self.image.ptr);
        }
        self.image.ptr = std::ptr::null_mut();
        self.destroyed = true;
    }
}

impl EglImage {
    pub fn into_texture(
        self,
        device_id: RenderDeviceId,
        opaque_alpha: bool,
    ) -> Result<GlTexture, ()> {
        GlTexture::import_image(self, device_id, opaque_alpha)
    }
}

#[derive(Debug)]
pub(super) struct GlChromaPlane {
    pub image: EglImage,
    pub texture: gl::Uint,
}

#[derive(Debug)]
pub struct GlTexture {
    pub(super) image: Option<EglImage>,
    pub(super) texture: gl::Uint,
    pub(super) delete_on_drop: bool,
    pub(super) device_id: RenderDeviceId,
    /// `XRGB8888` has no alpha. The GLES shader samples it as 1.
    pub(super) samples_as_opaque: bool,
    /// Second plane of NV12 or P010. `None` is a single RGBA texture.
    pub(super) chroma: Option<GlChromaPlane>,
    pub(super) yuv_mode: u32,
    pub(super) yuv_size: [f32; 2],
}

impl Drop for GlTexture {
    fn drop(&mut self) {
        if self.delete_on_drop && self.texture != 0 {
            unsafe {
                gl::DeleteTextures(1, &self.texture);
            }
            self.texture = 0;
        }
        if let Some(chroma) = self.chroma.take() {
            if chroma.texture != 0 {
                unsafe {
                    gl::DeleteTextures(1, &chroma.texture);
                }
            }
        }
        self.image.take();
    }
}

impl Texture for GlTexture {
    fn requires_device_context_on_drop(&self) -> bool {
        self.delete_on_drop && self.texture != 0
    }

    fn samples_as_opaque(&self) -> bool {
        self.samples_as_opaque
    }

    fn yuv_mode(&self) -> u32 {
        self.yuv_mode
    }

    fn yuv_size(&self) -> [f32; 2] {
        self.yuv_size
    }
}

/// Pixel layout GLES uses when uploading a DRM format from shared memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GlPixelUpload {
    pub internal_format: gl::GLint,
    pub format: gl::GLenum,
    pub ty: gl::GLenum,
    /// `ARGB2101010` and `XRGB2101010` store blue in the low 10 bits.
    /// `GL_UNSIGNED_INT_2_10_10_10_REV` reads those bits as red.
    pub swap_red_blue: bool,
}

/// `ARGB8888` and `XRGB8888` upload as little-endian `B, G, R, A`.
///
/// Packed 10-bit formats upload as `GL_RGB10_A2`. That type is ABGR order, so
/// the ARGB variants swap red and blue with `GL_TEXTURE_SWIZZLE_*`.
pub(crate) fn gl_upload_format(fourcc: DrmFourcc) -> Option<GlPixelUpload> {
    match fourcc {
        DrmFourcc::Argb8888 | DrmFourcc::Xrgb8888 => Some(GlPixelUpload {
            internal_format: gl::BGRA_EXT as gl::GLint,
            format: gl::BGRA_EXT as gl::GLenum,
            ty: gl::UNSIGNED_BYTE as gl::GLenum,
            swap_red_blue: false,
        }),
        DrmFourcc::Argb2101010
        | DrmFourcc::Xrgb2101010
        | DrmFourcc::Abgr2101010
        | DrmFourcc::Xbgr2101010 => Some(GlPixelUpload {
            internal_format: gl::RGB10_A2 as gl::GLint,
            format: gl::RGBA as gl::GLenum,
            ty: gl::UNSIGNED_INT_2_10_10_10_REV as gl::GLenum,
            swap_red_blue: matches!(fourcc, DrmFourcc::Argb2101010 | DrmFourcc::Xrgb2101010),
        }),
        // Memory order is R, G, B, A half-floats. `RGBA16F` samples them unclamped.
        DrmFourcc::Abgr16161616f | DrmFourcc::Xbgr16161616f => Some(GlPixelUpload {
            internal_format: gl::RGBA16F as gl::GLint,
            format: gl::RGBA as gl::GLenum,
            ty: gl::HALF_FLOAT as gl::GLenum,
            swap_red_blue: false,
        }),
        _ => None,
    }
}

fn shm_upload(format: &Format, allow_es3: bool) -> Option<GlPixelUpload> {
    let needs_es3 = matches!(format.kind, FormatKind::Rgb10 | FormatKind::Fp16);
    if needs_es3 && !allow_es3 {
        return None;
    }
    gl_upload_format(format.drm)
}

/// GL name for a texture created by `device_id`. A foreign backend or device returns `None`.
pub(super) fn gl_texture_name(
    texture: &dyn Texture,
    device_id: RenderDeviceId,
) -> Option<gl::Uint> {
    let Some(texture) = (texture as &dyn Any).downcast_ref::<GlTexture>() else {
        log::error!("renderer received a texture from a different backend");
        return None;
    };
    if texture.device_id != device_id {
        log::error!(
            "renderer received a texture from DRM device {:?} while drawing for {:?}",
            texture.device_id,
            device_id
        );
        return None;
    }
    Some(texture.texture)
}

pub(super) fn gl_chroma_name(texture: &dyn Texture, device_id: RenderDeviceId) -> Option<gl::Uint> {
    let texture = (texture as &dyn Any).downcast_ref::<GlTexture>()?;
    if texture.device_id != device_id {
        return None;
    }
    texture.chroma.as_ref().map(|plane| plane.texture)
}

fn bind_texture_2d(image: *mut std::ffi::c_void) -> Result<gl::Uint, ()> {
    let texture = unsafe {
        let mut texture = 0;
        gl::GenTextures(1, &mut texture);
        gl::BindTexture(gl::TEXTURE_2D as _, texture);
        gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_WRAP_S as _, gl::CLAMP_TO_EDGE);
        gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_WRAP_T as _, gl::CLAMP_TO_EDGE);
        gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_MIN_FILTER as _, gl::LINEAR as _);
        gl::TexParameteri(gl::TEXTURE_2D as _, gl::TEXTURE_MAG_FILTER as _, gl::LINEAR as _);
        gl::EGLImageTargetTexture2DOES(gl::TEXTURE_2D as _, image);
        let error = gl::GetError();
        gl::BindTexture(gl::TEXTURE_2D as _, 0);
        if error != gl::NO_ERROR as gl::GLenum {
            gl::DeleteTextures(1, &texture);
            return Err(());
        }
        texture
    };
    Ok(texture)
}

pub fn image_target(external_only: bool) -> gl::Enum {
    match external_only {
        true => gl::TEXTURE_EXTERNAL_OES as _,
        false => gl::TEXTURE_2D as _,
    }
}

impl GlTexture {
    pub fn import_image(
        image: EglImage,
        device_id: RenderDeviceId,
        opaque_alpha: bool,
    ) -> Result<GlTexture, ()> {
        let target = image_target(image.external_only);
        let texture = unsafe {
            let mut texture = 0;
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(target, texture);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_S as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_T as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(target, gl::TEXTURE_MIN_FILTER as _, gl::LINEAR as _);
            gl::TexParameteri(target, gl::TEXTURE_MAG_FILTER as _, gl::LINEAR as _);
            gl::EGLImageTargetTexture2DOES(target, image.image.ptr);
            assert_eq!(gl::GetError(), gl::NO_ERROR as gl::Enum);
            gl::BindTexture(target, 0);
            texture
        };
        Ok(GlTexture {
            image: Some(image),
            texture,
            delete_on_drop: true,
            device_id,
            samples_as_opaque: opaque_alpha,
            chroma: None,
            yuv_mode: 0,
            yuv_size: [0.0, 0.0],
        })
    }

    /// NV12 or P010 imported as two `TEXTURE_2D` images. Both planes are opaque.
    pub fn from_yuv_planes(
        luma: EglImage,
        chroma: EglImage,
        yuv_mode: u32,
        width: i32,
        height: i32,
        device_id: RenderDeviceId,
    ) -> Result<GlTexture, ()> {
        if luma.external_only || chroma.external_only {
            return Err(());
        }
        let texture = bind_texture_2d(luma.image.ptr)?;
        let chroma_texture = bind_texture_2d(chroma.image.ptr)?;
        Ok(GlTexture {
            image: Some(luma),
            texture,
            delete_on_drop: true,
            device_id,
            samples_as_opaque: true,
            chroma: Some(GlChromaPlane { image: chroma, texture: chroma_texture }),
            yuv_mode,
            yuv_size: [width as f32, height as f32],
        })
    }

    pub fn import_shm(
        mem: SliceSharedMemory,
        width: usize,
        height: usize,
        stride: usize,
        format: &Format,
        allow_es3: bool,
        device_id: RenderDeviceId,
    ) -> Result<GlTexture, ()> {
        let Some(upload) = shm_upload(format, allow_es3) else {
            return Err(());
        };
        let bpp = format.bpp as usize;
        if bpp == 0 || stride % bpp != 0 {
            return Err(());
        }
        Self::upload_pixels(
            mem.as_ptr(),
            width as i32,
            height as i32,
            stride / bpp,
            upload,
            !format.has_alpha,
            device_id,
        )
    }

    pub(super) fn import_shm_bytes(
        bytes: &[u8],
        width: i32,
        height: i32,
        stride: usize,
        fourcc: DrmFourcc,
        allow_es3: bool,
        device_id: RenderDeviceId,
    ) -> Result<GlTexture, ()> {
        if width <= 0 || height <= 0 {
            return Err(());
        }
        let Some(format) = client_format(fourcc) else {
            return Err(());
        };
        let Some(upload) = shm_upload(format, allow_es3) else {
            return Err(());
        };
        let bpp = format.bpp as usize;
        if bpp == 0 {
            return Err(());
        }
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(bpp))
            .ok_or(())?;
        if stride < row_bytes || stride % bpp != 0 {
            return Err(());
        }
        let needed = usize::try_from(height)
            .ok()
            .and_then(|height| stride.checked_mul(height))
            .ok_or(())?;
        if bytes.len() < needed {
            return Err(());
        }
        Self::upload_pixels(
            bytes.as_ptr(),
            width,
            height,
            stride / bpp,
            upload,
            !format.has_alpha,
            device_id,
        )
    }

    fn upload_pixels(
        pixels: *const u8,
        width: i32,
        height: i32,
        row_pixels: usize,
        upload: GlPixelUpload,
        opaque_alpha: bool,
        device_id: RenderDeviceId,
    ) -> Result<Self, ()> {
        let target = gl::TEXTURE_2D as gl::Enum;
        let texture = unsafe {
            let mut texture = 0;
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(target, texture);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_S as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_T as _, gl::CLAMP_TO_EDGE);
            // `RGBA16F` is not texture-filterable on ES 3.0. Point sampling keeps
            // the texture complete and leaves values above 1.0 intact.
            let filter =
                if upload.ty == gl::HALF_FLOAT as gl::GLenum { gl::NEAREST } else { gl::LINEAR };
            gl::TexParameteri(target, gl::TEXTURE_MIN_FILTER as _, filter as _);
            gl::TexParameteri(target, gl::TEXTURE_MAG_FILTER as _, filter as _);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH_EXT as _, row_pixels as i32);
            while gl::GetError() != gl::NO_ERROR as gl::Enum {}
            gl::TexImage2D(
                target,
                0,
                upload.internal_format,
                width,
                height,
                0,
                upload.format,
                upload.ty,
                pixels.cast(),
            );
            if upload.swap_red_blue {
                gl::TexParameteri(target, gl::TEXTURE_SWIZZLE_R as _, gl::BLUE as _);
                gl::TexParameteri(target, gl::TEXTURE_SWIZZLE_B as _, gl::RED as _);
            }
            let error = gl::GetError();
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH_EXT as _, 0);
            gl::BindTexture(target, 0);
            if error != gl::NO_ERROR as gl::Enum {
                gl::DeleteTextures(1, &texture);
                return Err(());
            }
            texture
        };
        Ok(Self {
            image: None,
            texture,
            delete_on_drop: true,
            device_id,
            samples_as_opaque: opaque_alpha,
            chroma: None,
            yuv_mode: 0,
            yuv_size: [0.0, 0.0],
        })
    }

    pub fn from_rgba(
        pixels: &[u8],
        width: i32,
        height: i32,
        device_id: RenderDeviceId,
    ) -> Result<GlTexture, ()> {
        if width <= 0 || height <= 0 {
            return Err(());
        }
        let expected_len = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .map(|height| width * height * 4)
            })
            .ok_or(())?;
        if pixels.len() != expected_len {
            return Err(());
        }

        let target = gl::TEXTURE_2D as gl::Enum;
        let texture = unsafe {
            let mut texture = 0;
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(target, texture);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_S as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(target, gl::TEXTURE_WRAP_T as _, gl::CLAMP_TO_EDGE);
            gl::TexParameteri(target, gl::TEXTURE_MIN_FILTER as _, gl::LINEAR as _);
            gl::TexParameteri(target, gl::TEXTURE_MAG_FILTER as _, gl::LINEAR as _);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT as _, 1);
            gl::TexImage2D(
                target,
                0,
                gl::RGBA as _,
                width,
                height,
                0,
                gl::RGBA as _,
                gl::UNSIGNED_BYTE as _,
                pixels.as_ptr().cast(),
            );
            gl::PixelStorei(gl::UNPACK_ALIGNMENT as _, 4);
            gl::BindTexture(target, 0);
            texture
        };
        Ok(Self {
            image: None,
            texture,
            delete_on_drop: true,
            device_id,
            samples_as_opaque: false,
            chroma: None,
            yuv_mode: 0,
            yuv_size: [0.0, 0.0],
        })
    }
}

#[cfg(test)]
mod tests {
    use {drm::buffer::DrmFourcc, khronos_types::gl};

    use super::gl_upload_format;

    #[test]
    fn argb_and_xrgb_shared_memory_uploads_as_bgra_bytes() {
        let argb = gl_upload_format(DrmFourcc::Argb8888).unwrap();
        let xrgb = gl_upload_format(DrmFourcc::Xrgb8888).unwrap();

        assert_eq!(argb, xrgb);
        assert_eq!(argb.format, gl::BGRA_EXT as gl::GLenum);
        assert_eq!(argb.ty, gl::UNSIGNED_BYTE as gl::GLenum);
        assert!(!argb.swap_red_blue);
        assert!(gl_upload_format(DrmFourcc::Abgr8888).is_none());
    }

    #[test]
    fn ten_bit_shared_memory_uploads_as_rgb10_a2_with_argb_channel_swap() {
        let argb = gl_upload_format(DrmFourcc::Argb2101010).unwrap();
        let abgr = gl_upload_format(DrmFourcc::Abgr2101010).unwrap();
        assert_eq!(argb.internal_format, gl::RGB10_A2 as gl::GLint);
        assert_eq!(argb.format, gl::RGBA as gl::GLenum);
        assert_eq!(argb.ty, gl::UNSIGNED_INT_2_10_10_10_REV as gl::GLenum);
        assert!(argb.swap_red_blue);
        assert!(
            gl_upload_format(DrmFourcc::Xrgb2101010)
                .unwrap()
                .swap_red_blue
        );
        assert!(!abgr.swap_red_blue);
        assert!(
            !gl_upload_format(DrmFourcc::Xbgr2101010)
                .unwrap()
                .swap_red_blue
        );
        assert_eq!(argb.internal_format, abgr.internal_format);
    }

    #[test]
    fn fp16_shared_memory_uploads_as_rgba16f_without_a_channel_swap() {
        let abgr = gl_upload_format(DrmFourcc::Abgr16161616f).unwrap();
        let xbgr = gl_upload_format(DrmFourcc::Xbgr16161616f).unwrap();
        assert_eq!(abgr.internal_format, gl::RGBA16F as gl::GLint);
        assert_eq!(abgr.format, gl::RGBA as gl::GLenum);
        assert_eq!(abgr.ty, gl::HALF_FLOAT as gl::GLenum);
        assert!(!abgr.swap_red_blue);
        assert_eq!(abgr, xbgr);
    }
}
