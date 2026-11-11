use std::{
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
};

use khronos_types::{egl, gl};

use {
    super::{
        import::{gl_chroma_name, gl_texture_name, GlTexture},
        offscreen::TextureFrameBuffer,
        readback::read_flip_y_framebuffer,
        resources::{
            BlurProgram, BorderProgram, EglFrameBuffer, GlesPrograms, ShadowProgram, TextureProgram,
        },
        sync::EGL_SYNC_NATIVE_FENCE_ANDROID,
        RenderItem, RenderItemMask,
    },
    crate::{
        drm::RenderSyncMode,
        region::{DamageRegion, RectI},
        render::{
            shadow_geometry, BlurVisualStyle, EffectElement, FrameElement, OutputFrameRecipe,
            RenderCompletion, RenderDeviceId, Texture, TextureSource,
        },
        state::RootSurfaceClip,
        transform::{Mat3, OutputPresentationTransform},
    },
};

pub(super) struct GlesDraw<'a> {
    pub(super) programs: &'a GlesPrograms,
    pub(super) device_id: RenderDeviceId,
    pub(super) width: i32,
    pub(super) height: i32,
    /// Decodes effect colors out of this output transfer while drawing into the linear buffer.
    pub(super) linear_blend: Option<crate::color::TransferFunction>,
}

impl EglFrameBuffer {
    fn draw(&self) -> GlesDraw<'_> {
        GlesDraw {
            programs: &self.programs,
            device_id: self.device_id,
            width: self.width,
            height: self.height,
            linear_blend: None,
        }
    }

    pub fn clear(&self) {
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, self.framebuffer_object);
            gl::Viewport(0, 0, self.width, self.height);
            set_output_clear_color();
            gl::Clear(gl::COLOR_BUFFER_BIT as _);
            gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
        }
        unsafe {
            gl::Flush();
        }
    }

    fn output_bounds(&self) -> Option<RectI> {
        RectI::new(0, 0, self.width, self.height)
    }

    fn clear_damage(
        &self,
        damage: &DamageRegion,
        preserve_contents: bool,
        output_transform: OutputPresentationTransform,
        clear_color: Option<[f32; 4]>,
    ) {
        let Some(output_bounds) = self.output_bounds() else {
            return;
        };
        let mut clipped_damage = damage.clone();
        clipped_damage.intersect_rect(output_bounds);

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, self.framebuffer_object);
            gl::Viewport(0, 0, self.width, self.height);
            match clear_color {
                Some([red, green, blue, alpha]) => gl::ClearColor(red, green, blue, alpha),
                None => set_output_clear_color(),
            }
            gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
        }

        if !preserve_contents || clipped_damage.extents() == Some(output_bounds) {
            unsafe {
                gl::Disable(gl::SCISSOR_TEST as _);
                gl::Clear(gl::COLOR_BUFFER_BIT as _);
                gl::Flush();
            }
            return;
        }

        unsafe {
            gl::Enable(gl::SCISSOR_TEST as _);
            for rect in clipped_damage.rects() {
                gl::Scissor(
                    rect.x,
                    scissor_y(self.height, *rect, output_transform),
                    rect.width,
                    rect.height,
                );
                gl::Clear(gl::COLOR_BUFFER_BIT as _);
            }
            gl::Disable(gl::SCISSOR_TEST as _);
            gl::Flush();
        }
    }

    pub fn clear_with(&self, _r: f32, _g: f32, _b: f32, _a: f32) {
        // let ops = self.take_render_ops();
        // self.render(ops, Some(&Color { r, g, b, a }));
    }

    fn finish_render(&self, render_sync_mode: RenderSyncMode) -> RenderCompletion {
        match render_sync_mode {
            RenderSyncMode::ExplicitInFence => match self.export_native_render_fence() {
                Ok(in_fence_fd) => {
                    if crate::logger::ENABLE_NOISY_LOG {
                        log::debug!(
                            "egl finish_render: exported explicit render fence fd={}",
                            in_fence_fd.as_raw_fd()
                        );
                    }
                    RenderCompletion::explicit(in_fence_fd)
                }
                Err(error) => {
                    log::warn!(
                        "egl finish_render: explicit fence export failed, falling back to glFinish: {:?}",
                        error
                    );
                    unsafe { gl::Finish() };
                    RenderCompletion::blocking_fallback()
                }
            },
            RenderSyncMode::BlockingFinishFallback => {
                unsafe { gl::Finish() };
                RenderCompletion::blocking_fallback()
            }
        }
    }

    fn export_native_render_fence(&self) -> Result<OwnedFd, egl::Error> {
        let sync =
            self.egl
                .create_sync_khr(self.display, EGL_SYNC_NATIVE_FENCE_ANDROID, &[egl::NONE])?;
        unsafe { gl::Flush() };
        let fence_fd = self.egl.dup_native_fence_fd_android(self.display, sync);
        let destroy_result = self.egl.destroy_sync_khr(self.display, sync);
        if let Err(error) = destroy_result {
            log::warn!("egl destroy_sync_khr failed after fence export: {:?}", error);
        }
        fence_fd
    }

    pub fn render(
        &self,
        recipe: &OutputFrameRecipe,
        projection: Mat3,
        render_sync_mode: RenderSyncMode,
        damage: &DamageRegion,
        preserve_contents: bool,
        output_transform: OutputPresentationTransform,
        clear_color: Option<[f32; 4]>,
        linear_blend: Option<crate::color::TransferFunction>,
    ) -> RenderCompletion {
        self.clear_damage(damage, preserve_contents, output_transform, clear_color);

        if recipe.is_empty() {
            return self.finish_render(render_sync_mode);
        }

        if let Some(output_tf) = linear_blend {
            if render_linear_blend(
                &self.programs,
                self.device_id,
                &self.linear_blend,
                self.framebuffer_object,
                self.width,
                self.height,
                recipe,
                projection,
                damage,
                output_transform,
                output_tf,
            ) {
                return self.finish_render(render_sync_mode);
            }
        }

        self.draw().render_frame_elements_to_target(
            self.framebuffer_object,
            self.width,
            self.height,
            &recipe.elements,
            projection,
            damage,
            output_transform,
        );

        self.finish_render(render_sync_mode)
    }
}

/// Draw translucent pixels into an FP16 linear buffer, then encode them onto
/// `framebuffer`. Returns false when the intermediate is unused or cannot be
/// created, so the caller draws the frame in the output encoding instead.
pub(super) fn render_linear_blend(
    programs: &GlesPrograms,
    device_id: RenderDeviceId,
    blend_slot: &std::cell::RefCell<Option<super::resources::LinearBlendTarget>>,
    framebuffer: gl::GLuint,
    width: i32,
    height: i32,
    recipe: &OutputFrameRecipe,
    projection: Mat3,
    damage: &DamageRegion,
    output_transform: OutputPresentationTransform,
    output_tf: crate::color::TransferFunction,
) -> bool {
    let Some(bounds) = RectI::new(0, 0, width, height) else {
        return false;
    };
    let regions = crate::render::blend::create_regions(&recipe.elements, bounds, true);
    let blend_damage = crate::render::blend::intersect_damage(&regions.blend, damage);
    if blend_damage.is_empty() {
        return false;
    }
    let Some((blend_fbo, blend_texture)) = linear_blend_target(blend_slot, width, height) else {
        log::warn!("FP16 blend buffer is unavailable; blending in the output encoding");
        return false;
    };
    let draw = |linear_blend| GlesDraw { programs, device_id, width, height, linear_blend };
    unsafe {
        gl::BindFramebuffer(gl::FRAMEBUFFER as _, blend_fbo);
        gl::Viewport(0, 0, width, height);
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Enable(gl::SCISSOR_TEST as _);
        for rect in blend_damage.rects() {
            gl::Scissor(
                rect.x,
                scissor_y(height, *rect, output_transform),
                rect.width,
                rect.height,
            );
            gl::Clear(gl::COLOR_BUFFER_BIT as _);
        }
        gl::Disable(gl::SCISSOR_TEST as _);
    }
    let elements = crate::render::blend::retarget_elements(&recipe.elements, output_tf);
    draw(Some(output_tf)).render_frame_elements_to_target(
        blend_fbo,
        width,
        height,
        &elements,
        projection,
        &blend_damage,
        output_transform,
    );
    let direct = crate::render::blend::intersect_damage(&regions.direct, damage);
    draw(None).render_frame_elements_to_target(
        framebuffer,
        width,
        height,
        &recipe.elements,
        projection,
        &direct,
        output_transform,
    );
    let copy_texture = draw(None).borrowed_texture(blend_texture);
    let copy = RenderItem {
        source: crate::render::TextureSource::Texture,
        texture: copy_texture,
        x: 0,
        y: 0,
        width,
        height,
        tex_transform: Mat3::identity(),
        damage: blend_damage.clone(),
        blend: true,
        alpha: u8::MAX,
        alpha_multiplier: u32::MAX,
        color: Some(std::rc::Rc::new(crate::render::blend::encode_from_linear(output_tf))),
        alpha_mode: 0,
        yuv: None,
        mask: crate::render::TextureMask::None,
    };
    draw(None).render_texture_elements_to_target(
        framebuffer,
        width,
        height,
        std::slice::from_ref(&copy),
        projection,
        &blend_damage,
        output_transform,
    );
    true
}

fn linear_blend_target(
    blend_slot: &std::cell::RefCell<Option<super::resources::LinearBlendTarget>>,
    width: i32,
    height: i32,
) -> Option<(gl::GLuint, gl::GLuint)> {
    let mut slot = blend_slot.borrow_mut();
    let fresh = match slot.as_ref() {
        Some(target) if target.width == width && target.height == height => false,
        _ => true,
    };
    if fresh {
        *slot = crate::render::gles::resources::LinearBlendTarget::new(width, height);
    }
    slot.as_ref()
        .map(|target| (target.framebuffer_object, target.texture))
}

impl GlesDraw<'_> {
    fn texture_program(&self) -> Result<TextureProgram, String> {
        self.texture_program_for(crate::render::color_pipeline::TexturePipelineKey::identity())
    }

    fn texture_program_for(
        &self,
        key: crate::render::color_pipeline::TexturePipelineKey,
    ) -> Result<TextureProgram, String> {
        if !key.is_admitted() {
            return Err("texture pipeline key is outside the finite color space".to_string());
        }
        if let Some(cached) = self.programs.texture.borrow().get(&key) {
            return cached.clone();
        }
        let compiled =
            create_texture_program_from(&crate::render::color_pipeline::gles_fragment_source(key));
        self.programs
            .texture
            .borrow_mut()
            .insert(key, compiled.clone());
        compiled
    }

    fn blur_program(&self) -> Result<&BlurProgram, &str> {
        self.programs
            .blur
            .get_or_init(create_blur_program)
            .as_ref()
            .map_err(String::as_str)
    }

    fn border_program(&self) -> Result<&BorderProgram, &str> {
        self.programs
            .border
            .get_or_init(create_border_program)
            .as_ref()
            .map_err(String::as_str)
    }

    fn shadow_program(&self) -> Result<&ShadowProgram, &str> {
        self.programs
            .shadow
            .get_or_init(create_shadow_program)
            .as_ref()
            .map_err(String::as_str)
    }

    fn dim_texture(&self) -> Option<Rc<dyn Texture>> {
        if let Some(texture) = self.programs.dim.get() {
            return Some(Rc::clone(texture) as Rc<dyn Texture>);
        }
        let texture = GlTexture::from_rgba(&[0, 0, 0, u8::MAX], 1, 1, self.device_id)
            .ok()
            .map(Rc::new)?;
        let _ = self.programs.dim.set(Rc::clone(&texture));
        Some(texture)
    }

    fn borrowed_texture(&self, name: gl::Uint) -> Rc<dyn Texture> {
        Rc::new(GlTexture {
            image: None,
            texture: name,
            delete_on_drop: false,
            device_id: self.device_id,
            samples_as_opaque: false,
            chroma: None,
            yuv_mode: 0,
            yuv_size: [0.0, 0.0],
        })
    }
    fn render_texture_elements_to_target(
        &self,
        framebuffer_object: gl::GLuint,
        target_width: i32,
        target_height: i32,
        items: &[RenderItem],
        projection: Mat3,
        damage: &DamageRegion,
        output_transform: OutputPresentationTransform,
    ) {
        if items.is_empty() {
            return;
        }
        let identity = match self.texture_program() {
            Ok(program) => program,
            Err(error) => {
                log::error!("texture shader is unavailable: {error}");
                return;
            }
        };
        let vertices: [f32; 16] =
            [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        let output_bounds = RectI::new(0, 0, target_width, target_height);

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::Viewport(0, 0, target_width, target_height);
            gl::UseProgram(identity.program);
            gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
            gl::Uniform1i(identity.sampler_location, 0);
            gl::ActiveTexture(gl::TEXTURE0 as _);
            gl::EnableVertexAttribArray(identity.position_location);
            gl::EnableVertexAttribArray(identity.texcoord_location);
        }

        for item in items {
            let mut key = crate::render::color_pipeline::TexturePipelineKey::from_color(
                item.color.as_deref(),
                true,
            );
            key.yuv_mode = item.texture.yuv_mode();
            if !item.texture.samples_as_opaque() {
                key.alpha_mode = item.alpha_mode;
            }
            let program = if key.selects_current_shader() {
                identity
            } else {
                match self.texture_program_for(key) {
                    Ok(program) => program,
                    Err(error) => {
                        log::error!("color texture shader is unavailable: {error}");
                        continue;
                    }
                }
            };
            let Some(item_bounds) = RectI::new(item.x, item.y, item.width, item.height) else {
                continue;
            };
            let mut clipped_damage = damage.clone();
            if let Some(output_bounds) = output_bounds {
                clipped_damage.intersect_rect(output_bounds);
            }
            clipped_damage.intersect_rect(item_bounds);
            clipped_damage.intersect_rect(item.damage.extents().unwrap_or(item_bounds));
            if clipped_damage.is_empty() {
                continue;
            }
            let transform = crate::transform::project_box(
                item.x as f32,
                item.y as f32,
                item.width as f32,
                item.height as f32,
                projection,
            )
            .to_gl_array();
            let tex_transform = item.tex_transform.to_gl_array();
            unsafe {
                if item.blend || item.alpha < u8::MAX || item.alpha_multiplier < u32::MAX {
                    gl::Enable(gl::BLEND as _);
                } else {
                    gl::Disable(gl::BLEND as _);
                }
                let Some(texture_name) = gl_texture_name(item.texture.as_ref(), self.device_id)
                else {
                    continue;
                };
                gl::BindTexture(gl::TEXTURE_2D as _, texture_name);
                if key.yuv_mode != 0 {
                    let Some(chroma) = gl_chroma_name(item.texture.as_ref(), self.device_id) else {
                        log::error!("yuv texture is missing its chroma plane");
                        continue;
                    };
                    gl::ActiveTexture(gl::TEXTURE1 as _);
                    gl::BindTexture(gl::TEXTURE_2D as _, chroma);
                    gl::ActiveTexture(gl::TEXTURE0 as _);
                }
                if program.program != identity.program {
                    gl::UseProgram(program.program);
                    gl::Uniform1i(program.sampler_location, 0);
                    gl::EnableVertexAttribArray(program.position_location);
                    gl::EnableVertexAttribArray(program.texcoord_location);
                    let color =
                        crate::render::color_pipeline::color_uniforms(item.color.as_deref());
                    gl::Uniform4fv(program.color_matrix_0, 1, color.matrix[0].as_ptr());
                    gl::Uniform4fv(program.color_matrix_1, 1, color.matrix[1].as_ptr());
                    gl::Uniform4fv(program.color_matrix_2, 1, color.matrix[2].as_ptr());
                    gl::Uniform1f(program.src_bt1886_lb_over_lw, color.src_bt1886_lb_over_lw);
                    gl::Uniform1f(program.dst_bt1886_lb_over_lw, color.dst_bt1886_lb_over_lw);
                    gl::Uniform1f(program.src_power_exponent, color.src_power_exponent);
                    gl::Uniform1f(program.dst_power_exponent, color.dst_power_exponent);
                    if key.yuv_mode != 0 {
                        let size = item.texture.yuv_size();
                        let yuv = if let Some(yuv) = item.yuv {
                            yuv
                        } else {
                            crate::color::ColorRepresentation::default().yuv_conversion(
                                key.yuv_mode == crate::render::color_pipeline::YUV_BT709_LIMITED_10,
                            )
                        };
                        gl::Uniform1i(program.uv_sampler_location, 1);
                        gl::Uniform2f(program.yuv_size_location, size[0], size[1]);
                        gl::Uniform2f(program.yuv_site_location, yuv.site[0], yuv.site[1]);
                        gl::Uniform4fv(program.yuv_row0, 1, yuv.rows[0].as_ptr());
                        gl::Uniform4fv(program.yuv_row1, 1, yuv.rows[1].as_ptr());
                        gl::Uniform4fv(program.yuv_row2, 1, yuv.rows[2].as_ptr());
                    }
                    gl::Uniform1f(program.tonemap_max, color.tonemap_max);
                    gl::Uniform1f(program.tonemap_dst_max, color.tonemap_dst_max);
                    gl::Uniform1f(program.tonemap_dst_min, color.tonemap_dst_min);
                    gl::Uniform1f(program.tonemap_dst_ref, color.tonemap_dst_ref);
                    gl::Uniform4fv(program.tonemap_to_lms_0, 1, color.tonemap_to_lms[0].as_ptr());
                    gl::Uniform4fv(program.tonemap_to_lms_1, 1, color.tonemap_to_lms[1].as_ptr());
                    gl::Uniform4fv(program.tonemap_to_lms_2, 1, color.tonemap_to_lms[2].as_ptr());
                    gl::Uniform4fv(
                        program.tonemap_from_lms_0,
                        1,
                        color.tonemap_from_lms[0].as_ptr(),
                    );
                    gl::Uniform4fv(
                        program.tonemap_from_lms_1,
                        1,
                        color.tonemap_from_lms[1].as_ptr(),
                    );
                    gl::Uniform4fv(
                        program.tonemap_from_lms_2,
                        1,
                        color.tonemap_from_lms[2].as_ptr(),
                    );
                }
                gl::UniformMatrix3fv(program.transform_location, 1, 0, transform.as_ptr());
                gl::UniformMatrix3fv(program.tex_transform_location, 1, 0, tex_transform.as_ptr());
                gl::Uniform2f(program.item_size_location, item.width as f32, item.height as f32);
                gl::Uniform1f(
                    program.alpha_location,
                    crate::render::effective_alpha(item.alpha, item.alpha_multiplier),
                );
                gl::Uniform1i(
                    program.opaque_sample_location,
                    i32::from(item.texture.samples_as_opaque()),
                );
                match &item.mask {
                    RenderItemMask::None => {
                        gl::Uniform1i(program.clip_mode_location, 0);
                    }
                    RenderItemMask::RoundedClip { clip } => {
                        gl::Uniform1i(program.clip_mode_location, 1);
                        gl::Uniform4f(
                            program.clip_outer_rect_location,
                            clip.outer_rect.x as f32,
                            clip.outer_rect.y as f32,
                            clip.outer_rect.width as f32,
                            clip.outer_rect.height as f32,
                        );
                        gl::Uniform4f(program.clip_inner_rect_location, 0.0, 0.0, 0.0, 0.0);
                        gl::Uniform1f(
                            program.outer_corner_radius_location,
                            clip.corner_radius.max(0) as f32,
                        );
                        gl::Uniform1f(
                            program.outer_rounding_power_location,
                            clip.rounding_power.get().max(0.0001),
                        );
                        gl::Uniform1f(program.inner_corner_radius_location, 0.0);
                        gl::Uniform1f(program.inner_rounding_power_location, 0.0001);
                    }
                    // Corner-only rounding: pixels outside the window box are kept, so
                    // client-drawn CSD shadows survive.
                    RenderItemMask::CornerRoundedClip { clip } => {
                        gl::Uniform1i(program.clip_mode_location, 3);
                        gl::Uniform4f(
                            program.clip_outer_rect_location,
                            clip.outer_rect.x as f32,
                            clip.outer_rect.y as f32,
                            clip.outer_rect.width as f32,
                            clip.outer_rect.height as f32,
                        );
                        gl::Uniform4f(program.clip_inner_rect_location, 0.0, 0.0, 0.0, 0.0);
                        gl::Uniform1f(
                            program.outer_corner_radius_location,
                            clip.corner_radius.max(0) as f32,
                        );
                        gl::Uniform1f(
                            program.outer_rounding_power_location,
                            clip.rounding_power.get().max(0.0001),
                        );
                        gl::Uniform1f(program.inner_corner_radius_location, 0.0);
                        gl::Uniform1f(program.inner_rounding_power_location, 0.0001);
                    }
                    RenderItemMask::FrameRing { ring } => {
                        gl::Uniform1i(program.clip_mode_location, 2);
                        gl::Uniform4f(
                            program.clip_outer_rect_location,
                            ring.outer.outer_rect.x as f32,
                            ring.outer.outer_rect.y as f32,
                            ring.outer.outer_rect.width as f32,
                            ring.outer.outer_rect.height as f32,
                        );
                        gl::Uniform4f(
                            program.clip_inner_rect_location,
                            ring.inner.outer_rect.x as f32,
                            ring.inner.outer_rect.y as f32,
                            ring.inner.outer_rect.width as f32,
                            ring.inner.outer_rect.height as f32,
                        );
                        gl::Uniform1f(
                            program.outer_corner_radius_location,
                            ring.outer.corner_radius.max(0) as f32,
                        );
                        gl::Uniform1f(
                            program.outer_rounding_power_location,
                            ring.outer.rounding_power.get().max(0.0001),
                        );
                        gl::Uniform1f(
                            program.inner_corner_radius_location,
                            ring.inner.corner_radius.max(0) as f32,
                        );
                        gl::Uniform1f(
                            program.inner_rounding_power_location,
                            ring.inner.rounding_power.get().max(0.0001),
                        );
                    }
                }
                gl::VertexAttribPointer(
                    program.position_location,
                    2,
                    gl::FLOAT as _,
                    0,
                    stride,
                    vertices.as_ptr().cast(),
                );
                gl::VertexAttribPointer(
                    program.texcoord_location,
                    2,
                    gl::FLOAT as _,
                    0,
                    stride,
                    vertices.as_ptr().add(2).cast(),
                );
                gl::Enable(gl::SCISSOR_TEST as _);
                for rect in clipped_damage.rects() {
                    gl::Scissor(
                        rect.x,
                        scissor_y(target_height, *rect, output_transform),
                        rect.width,
                        rect.height,
                    );
                    gl::DrawArrays(gl::TRIANGLE_STRIP as _, 0, 4);
                }
                gl::Disable(gl::SCISSOR_TEST as _);
                if program.program != identity.program {
                    gl::DisableVertexAttribArray(program.position_location);
                    gl::DisableVertexAttribArray(program.texcoord_location);
                    gl::UseProgram(identity.program);
                }
            }
        }

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D as _, 0);
            gl::Disable(gl::BLEND as _);
            gl::DisableVertexAttribArray(identity.position_location);
            gl::DisableVertexAttribArray(identity.texcoord_location);
            gl::UseProgram(0);
        }
    }

    fn blur_snapshot_bounds(
        &self,
        clip: crate::state::RootSurfaceClip,
        blur_style: BlurVisualStyle,
    ) -> Option<RectI> {
        let output_bounds = RectI::new(0, 0, self.width, self.height)?;
        let padding = blur_style
            .radius
            .max(1)
            .saturating_mul(i32::from(blur_style.passes.max(1)))
            .saturating_mul(4);
        RectI::from_extents(
            clip.outer_rect.x.saturating_sub(padding),
            clip.outer_rect.y.saturating_sub(padding),
            clip.outer_rect.x2().saturating_add(padding),
            clip.outer_rect.y2().saturating_add(padding),
        )
        .and_then(|bounds| bounds.intersection(output_bounds))
    }

    /// Copy `blur_bounds` out of the framebuffer and blur it. The caller masks
    /// the result: a window passes its padded ring box, a client region passes
    /// `BlurRegion::bounds`.
    fn build_blurred_backdrop(
        &self,
        framebuffer_object: gl::GLuint,
        target_height: i32,
        blur_bounds: RectI,
        blur_style: BlurVisualStyle,
        output_transform: OutputPresentationTransform,
    ) -> Option<(TextureFrameBuffer, RectI)> {
        if blur_bounds.is_empty() {
            return None;
        }
        let copy_rect = framebuffer_rect_for_target(target_height, blur_bounds, output_transform)?;
        let blur_input = TextureFrameBuffer::new(blur_bounds.width, blur_bounds.height)?;
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::BindTexture(gl::TEXTURE_2D as _, blur_input.texture);
            gl::CopyTexSubImage2D(
                gl::TEXTURE_2D as _,
                0,
                0,
                0,
                copy_rect.x,
                copy_rect.y,
                copy_rect.width,
                copy_rect.height,
            );
            gl::BindTexture(gl::TEXTURE_2D as _, 0);
        }
        let mut scratch_a = TextureFrameBuffer::new(blur_bounds.width, blur_bounds.height)?;
        let mut scratch_b = TextureFrameBuffer::new(blur_bounds.width, blur_bounds.height)?;
        self.blur_texture(
            blur_input.texture,
            &mut scratch_a,
            &mut scratch_b,
            blur_bounds.width,
            blur_bounds.height,
            blur_style.radius.max(1) as f32,
            blur_style.passes.max(1),
            crate::transform::output_projection(
                blur_bounds.width as f32,
                blur_bounds.height as f32,
                output_transform,
            ),
        );
        Some((scratch_a, blur_bounds))
    }

    fn blur_texture(
        &self,
        input_texture: gl::GLuint,
        scratch_a: &mut TextureFrameBuffer,
        scratch_b: &mut TextureFrameBuffer,
        width: i32,
        height: i32,
        radius: f32,
        passes: u8,
        projection: Mat3,
    ) {
        let program = match self.blur_program() {
            Ok(program) => program,
            Err(error) => {
                log::error!("blur shader is unavailable: {error}");
                return;
            }
        };
        let vertices: [f32; 16] =
            [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        let transform =
            crate::transform::project_box(0.0, 0.0, width as f32, height as f32, projection)
                .to_gl_array();
        let tex_transform = Mat3::identity().to_gl_array();
        let sanitized_radius = radius.max(1.0);
        let pass_draws =
            blur_pass_draws(input_texture, scratch_a.texture, scratch_b.texture, passes);
        unsafe {
            gl::UseProgram(program.program);
            gl::Uniform1i(program.sampler_location, 0);
            gl::Uniform2f(program.texel_size_location, 1.0 / width as f32, 1.0 / height as f32);
            gl::Uniform1f(program.radius_location, sanitized_radius);
            gl::UniformMatrix3fv(program.transform_location, 1, 0, transform.as_ptr());
            gl::UniformMatrix3fv(program.tex_transform_location, 1, 0, tex_transform.as_ptr());
            gl::Uniform2f(program.item_size_location, width as f32, height as f32);
            gl::EnableVertexAttribArray(program.position_location);
            gl::EnableVertexAttribArray(program.texcoord_location);
            gl::VertexAttribPointer(
                program.position_location,
                2,
                gl::FLOAT as _,
                0,
                stride,
                vertices.as_ptr().cast(),
            );
            gl::VertexAttribPointer(
                program.texcoord_location,
                2,
                gl::FLOAT as _,
                0,
                stride,
                vertices.as_ptr().add(2).cast(),
            );
            gl::ActiveTexture(gl::TEXTURE0 as _);
        }
        for (source_texture, target_texture, direction) in &pass_draws {
            let target = if *target_texture == scratch_a.texture {
                scratch_a.framebuffer_object
            } else {
                scratch_b.framebuffer_object
            };
            unsafe {
                gl::BindFramebuffer(gl::FRAMEBUFFER as _, target);
                gl::Viewport(0, 0, width, height);
                gl::Uniform2f(program.direction_location, direction[0], direction[1]);
                gl::BindTexture(gl::TEXTURE_2D as _, *source_texture);
                gl::DrawArrays(gl::TRIANGLE_STRIP as _, 0, 4);
            }
        }
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D as _, 0);
            gl::DisableVertexAttribArray(program.position_location);
            gl::DisableVertexAttribArray(program.texcoord_location);
            gl::UseProgram(0);
        }
        if pass_draws
            .last()
            .is_some_and(|(_, target_texture, _)| *target_texture == scratch_b.texture)
        {
            std::mem::swap(scratch_a, scratch_b);
        }
    }
}

fn blur_pass_draws(
    input_texture: gl::GLuint,
    scratch_a_texture: gl::GLuint,
    scratch_b_texture: gl::GLuint,
    passes: u8,
) -> Vec<(gl::GLuint, gl::GLuint, [f32; 2])> {
    let mut draws = Vec::with_capacity(usize::from(passes.max(1)) * 2);
    let mut source_texture = input_texture;
    for _ in 0..passes.max(1) {
        for (target_texture, direction) in
            [(scratch_a_texture, [1.0f32, 0.0f32]), (scratch_b_texture, [0.0f32, 1.0f32])]
        {
            draws.push((source_texture, target_texture, direction));
            source_texture = target_texture;
        }
    }
    draws
}

impl EglFrameBuffer {
    /// Draws `elements` into a new transparent texture. Coordinates are local to that texture.
    pub(crate) fn capture_transparent(
        &self,
        elements: &[FrameElement],
        width: i32,
        height: i32,
        presentation_transform: OutputPresentationTransform,
    ) -> Option<GlTexture> {
        self.draw()
            .capture_transparent(elements, width, height, presentation_transform)
    }

    /// Draw a window into another framebuffer and clear that target to transparent first.
    ///
    /// The output framebuffer keeps its own pixels. FlipY matches [`EglFrameBuffer::read_top_left`].
    pub(crate) fn render_into_transparent(
        &self,
        framebuffer: gl::GLuint,
        width: i32,
        height: i32,
        recipe: &OutputFrameRecipe,
    ) {
        self.draw().paint_transparent_target(
            framebuffer,
            width,
            height,
            &recipe.elements,
            OutputPresentationTransform::FlipY,
        );
    }

    pub(crate) fn read_top_left(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        dst_stride: usize,
        dst: &mut [u8],
    ) -> bool {
        read_flip_y_framebuffer(self.framebuffer_object, x, y, width, height, dst_stride, dst)
    }
}

impl GlesDraw<'_> {
    pub(super) fn paint_transparent_target(
        &self,
        framebuffer: gl::GLuint,
        width: i32,
        height: i32,
        elements: &[FrameElement],
        presentation_transform: OutputPresentationTransform,
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer);
            gl::Viewport(0, 0, width, height);
            gl::Disable(gl::SCISSOR_TEST as _);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT as _);
        }
        let Some(damage) = RectI::new(0, 0, width, height).map(DamageRegion::from_rect) else {
            return;
        };
        let projection = crate::transform::output_projection(
            width as f32,
            height as f32,
            presentation_transform,
        );
        self.render_frame_elements_to_target(
            framebuffer,
            width,
            height,
            elements,
            projection,
            &damage,
            presentation_transform,
        );
    }

    pub(super) fn capture_transparent(
        &self,
        elements: &[FrameElement],
        width: i32,
        height: i32,
        presentation_transform: OutputPresentationTransform,
    ) -> Option<GlTexture> {
        let framebuffer = TextureFrameBuffer::new(width, height)?;
        let damage = DamageRegion::from_rect(RectI::new(0, 0, width, height)?);
        let projection = crate::transform::output_projection(
            width as f32,
            height as f32,
            presentation_transform,
        );
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer.framebuffer_object);
            gl::Viewport(0, 0, width, height);
            gl::Disable(gl::SCISSOR_TEST as _);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT as _);
        }
        self.render_frame_elements_to_target(
            framebuffer.framebuffer_object,
            width,
            height,
            elements,
            projection,
            &damage,
            presentation_transform,
        );
        Some(framebuffer.into_texture(self.device_id))
    }

    fn render_frame_elements_to_target(
        &self,
        framebuffer_object: gl::GLuint,
        target_width: i32,
        target_height: i32,
        elements: &[FrameElement],
        projection: Mat3,
        damage: &DamageRegion,
        output_transform: OutputPresentationTransform,
    ) {
        let mut next_index = 0usize;
        while next_index < elements.len() {
            match &elements[next_index] {
                FrameElement::Texture { .. } => {
                    let batch_start = next_index;
                    next_index += 1;
                    while next_index < elements.len()
                        && matches!(elements[next_index], FrameElement::Texture { .. })
                    {
                        next_index += 1;
                    }
                    let batch: Vec<_> = elements[batch_start..next_index]
                        .iter()
                        .filter_map(|element| match element {
                            FrameElement::Texture { element, .. } => Some(element.clone()),
                            FrameElement::Effect { .. } => None,
                        })
                        .collect();
                    self.render_texture_elements_to_target(
                        framebuffer_object,
                        target_width,
                        target_height,
                        &batch,
                        projection,
                        damage,
                        output_transform,
                    );
                }
                FrameElement::Effect { element: effect, .. } => {
                    next_index += 1;
                    match effect {
                        EffectElement::WindowBlurBackdrop { ring, style, alpha } => {
                            let Some(blur_bounds) = self.blur_snapshot_bounds(ring.outer, *style)
                            else {
                                continue;
                            };
                            let Some((texture, blur_bounds)) = self.build_blurred_backdrop(
                                framebuffer_object,
                                target_height,
                                blur_bounds,
                                *style,
                                output_transform,
                            ) else {
                                continue;
                            };
                            let item = RenderItem {
                                source: TextureSource::Texture,
                                texture: self.borrowed_texture(texture.texture),
                                x: blur_bounds.x,
                                y: blur_bounds.y,
                                width: blur_bounds.width,
                                height: blur_bounds.height,
                                tex_transform: Mat3::identity(),
                                damage: damage.clone(),
                                blend: true,
                                alpha: *alpha,
                                alpha_multiplier: u32::MAX,
                                color: None,
                                alpha_mode: 0,
                                yuv: None,
                                mask: RenderItemMask::FrameRing {
                                    ring: crate::render::RoundedRing {
                                        outer: RootSurfaceClip::new(
                                            ring.outer.outer_rect.translate(
                                                blur_bounds.x.saturating_neg(),
                                                blur_bounds.y.saturating_neg(),
                                            ),
                                            ring.outer.corner_radius,
                                            ring.outer.rounding_power,
                                        ),
                                        inner: RootSurfaceClip::new(
                                            ring.inner.outer_rect.translate(
                                                blur_bounds.x.saturating_neg(),
                                                blur_bounds.y.saturating_neg(),
                                            ),
                                            ring.inner.corner_radius,
                                            ring.inner.rounding_power,
                                        ),
                                    },
                                },
                            };
                            self.render_texture_elements_to_target(
                                framebuffer_object,
                                target_width,
                                target_height,
                                &[item],
                                projection,
                                damage,
                                output_transform,
                            );
                        }
                        EffectElement::WindowBorder { ring, gradient, alpha } => {
                            self.render_border_element_to_target(
                                framebuffer_object,
                                target_width,
                                target_height,
                                projection,
                                damage,
                                output_transform,
                                *ring,
                                *gradient,
                                *alpha,
                            );
                        }
                        EffectElement::WindowShadow { clip, style, alpha } => {
                            self.render_shadow_element_to_target(
                                framebuffer_object,
                                target_width,
                                target_height,
                                projection,
                                damage,
                                output_transform,
                                *clip,
                                *style,
                                *alpha,
                            );
                        }
                        EffectElement::SurfaceBlurBackdrop { region, style, alpha } => {
                            let Some(output_bounds) = RectI::new(0, 0, target_width, target_height)
                            else {
                                continue;
                            };
                            let Some(blur_bounds) = region.bounds.intersection(output_bounds)
                            else {
                                continue;
                            };
                            let Some((blurred, blur_bounds)) = self.build_blurred_backdrop(
                                framebuffer_object,
                                target_height,
                                blur_bounds,
                                *style,
                                output_transform,
                            ) else {
                                continue;
                            };
                            let texture = self.borrowed_texture(blurred.texture);
                            let mut items = Vec::new();
                            for rect in region.rects.iter().copied() {
                                let Some(rect) = rect.intersection(blur_bounds) else {
                                    continue;
                                };
                                items.push(RenderItem {
                                    source: TextureSource::Texture,
                                    texture: Rc::clone(&texture),
                                    x: rect.x,
                                    y: rect.y,
                                    width: rect.width,
                                    height: rect.height,
                                    tex_transform: blur_source_transform(blur_bounds, rect),
                                    damage: DamageRegion::from_rect(rect),
                                    blend: true,
                                    alpha: *alpha,
                                    alpha_multiplier: u32::MAX,
                                    color: None,
                                    alpha_mode: 0,
                                    yuv: None,
                                    mask: RenderItemMask::None,
                                });
                            }
                            self.render_texture_elements_to_target(
                                framebuffer_object,
                                target_width,
                                target_height,
                                &items,
                                projection,
                                damage,
                                output_transform,
                            );
                        }
                        EffectElement::DimOverlay { clip, alpha } => {
                            let Some(texture) = self.dim_texture() else {
                                continue;
                            };
                            let item = RenderItem {
                                source: TextureSource::Texture,
                                texture,
                                x: clip.outer_rect.x,
                                y: clip.outer_rect.y,
                                width: clip.outer_rect.width,
                                height: clip.outer_rect.height,
                                tex_transform: Mat3::identity(),
                                damage: damage.clone(),
                                blend: true,
                                alpha: *alpha,
                                alpha_multiplier: u32::MAX,
                                color: None,
                                alpha_mode: 0,
                                yuv: None,
                                mask: RenderItemMask::RoundedClip {
                                    clip: RootSurfaceClip::new(
                                        RectI::new(
                                            0,
                                            0,
                                            clip.outer_rect.width,
                                            clip.outer_rect.height,
                                        )
                                        .expect("dim overlay bounds should stay valid"),
                                        clip.corner_radius,
                                        clip.rounding_power,
                                    ),
                                },
                            };
                            self.render_texture_elements_to_target(
                                framebuffer_object,
                                target_width,
                                target_height,
                                &[item],
                                projection,
                                damage,
                                output_transform,
                            );
                        }
                    }
                }
            }
        }
    }

    fn render_border_element_to_target(
        &self,
        framebuffer_object: gl::GLuint,
        target_width: i32,
        target_height: i32,
        projection: Mat3,
        damage: &DamageRegion,
        output_transform: OutputPresentationTransform,
        ring: crate::render::RoundedRing,
        gradient: crate::render::BorderGradient,
        alpha: u8,
    ) {
        let bounds = ring.outer.outer_rect;
        let mut clipped_damage = damage.clone();
        let Some(output_bounds) = RectI::new(0, 0, target_width, target_height) else {
            return;
        };
        clipped_damage.intersect_rect(output_bounds);
        clipped_damage.intersect_rect(bounds);
        if clipped_damage.is_empty() {
            return;
        }

        let program = match self.border_program() {
            Ok(program) => program,
            Err(error) => {
                log::error!("border shader is unavailable: {error}");
                return;
            }
        };
        let vertices: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let stride = (2 * std::mem::size_of::<f32>()) as i32;
        let transform = crate::transform::project_box(
            bounds.x as f32,
            bounds.y as f32,
            bounds.width as f32,
            bounds.height as f32,
            projection,
        )
        .to_gl_array();
        let start = crate::render::blend::premul_color(gradient.start_color, self.linear_blend);
        let end = crate::render::blend::premul_color(gradient.end_color, self.linear_blend);

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::Viewport(0, 0, target_width, target_height);
            gl::UseProgram(program.program);
            gl::Enable(gl::BLEND as _);
            gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
            gl::EnableVertexAttribArray(program.position_location);
            gl::UniformMatrix3fv(program.transform_location, 1, 0, transform.as_ptr());
            gl::Uniform2f(program.item_size_location, bounds.width as f32, bounds.height as f32);
            gl::Uniform4f(
                program.outer_rect_location,
                0.0,
                0.0,
                bounds.width as f32,
                bounds.height as f32,
            );
            gl::Uniform4f(
                program.inner_rect_location,
                ring.inner.outer_rect.x.saturating_sub(bounds.x) as f32,
                ring.inner.outer_rect.y.saturating_sub(bounds.y) as f32,
                ring.inner.outer_rect.width as f32,
                ring.inner.outer_rect.height as f32,
            );
            gl::Uniform1f(
                program.outer_corner_radius_location,
                ring.outer.corner_radius.max(0) as f32,
            );
            gl::Uniform1f(
                program.outer_rounding_power_location,
                ring.outer.rounding_power.get().max(0.0001),
            );
            gl::Uniform1f(
                program.inner_corner_radius_location,
                ring.inner.corner_radius.max(0) as f32,
            );
            gl::Uniform1f(
                program.inner_rounding_power_location,
                ring.inner.rounding_power.get().max(0.0001),
            );
            gl::Uniform4f(program.start_color_location, start[0], start[1], start[2], start[3]);
            gl::Uniform4f(program.end_color_location, end[0], end[1], end[2], end[3]);
            gl::Uniform1f(program.angle_location, gradient.angle_degrees as f32);
            gl::Uniform1f(program.alpha_location, f32::from(alpha) / 255.0);
            gl::VertexAttribPointer(
                program.position_location,
                2,
                gl::FLOAT as _,
                0,
                stride,
                vertices.as_ptr().cast(),
            );
            gl::Enable(gl::SCISSOR_TEST as _);
            for rect in clipped_damage.rects() {
                gl::Scissor(
                    rect.x,
                    scissor_y(target_height, *rect, output_transform),
                    rect.width,
                    rect.height,
                );
                gl::DrawArrays(gl::TRIANGLE_STRIP as _, 0, 4);
            }
            gl::Disable(gl::SCISSOR_TEST as _);
            gl::DisableVertexAttribArray(program.position_location);
            gl::Disable(gl::BLEND as _);
            gl::UseProgram(0);
        }
    }

    fn render_shadow_element_to_target(
        &self,
        framebuffer_object: gl::GLuint,
        target_width: i32,
        target_height: i32,
        projection: Mat3,
        damage: &DamageRegion,
        output_transform: OutputPresentationTransform,
        clip: crate::state::RootSurfaceClip,
        style: crate::render::ShadowVisualStyle,
        alpha: u8,
    ) {
        let Some(shadow_geometry) = shadow_geometry(clip, style) else {
            return;
        };
        let bounds = shadow_geometry.bounds;

        let mut clipped_damage = damage.clone();
        let Some(output_bounds) = RectI::new(0, 0, target_width, target_height) else {
            return;
        };
        clipped_damage.intersect_rect(output_bounds);
        clipped_damage.intersect_rect(bounds);
        if clipped_damage.is_empty() {
            return;
        }

        let program = match self.shadow_program() {
            Ok(program) => program,
            Err(error) => {
                log::error!("shadow shader is unavailable: {error}");
                return;
            }
        };
        let vertices: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let stride = (2 * std::mem::size_of::<f32>()) as i32;
        let transform = crate::transform::project_box(
            bounds.x as f32,
            bounds.y as f32,
            bounds.width as f32,
            bounds.height as f32,
            projection,
        )
        .to_gl_array();
        let color = crate::render::blend::shadow_coverage(style.color, alpha, self.linear_blend);

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
            gl::Viewport(0, 0, target_width, target_height);
            gl::UseProgram(program.program);
            gl::Enable(gl::BLEND as _);
            gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
            gl::EnableVertexAttribArray(program.position_location);
            gl::UniformMatrix3fv(program.transform_location, 1, 0, transform.as_ptr());
            gl::Uniform2f(program.item_size_location, bounds.width as f32, bounds.height as f32);
            gl::Uniform4f(program.shadow_color_location, color[0], color[1], color[2], color[3]);
            gl::Uniform4f(
                program.inner_rect_location,
                shadow_geometry.inner_rect.x as f32,
                shadow_geometry.inner_rect.y as f32,
                shadow_geometry.inner_rect.width as f32,
                shadow_geometry.inner_rect.height as f32,
            );
            gl::Uniform1f(program.corner_radius_location, clip.corner_radius.max(0) as f32);
            gl::Uniform1f(program.rounding_power_location, clip.rounding_power.get().max(0.0001));
            gl::Uniform1f(program.shadow_range_location, shadow_geometry.shadow_range as f32);
            gl::Uniform1f(program.shadow_power_location, style.power.get());
            gl::VertexAttribPointer(
                program.position_location,
                2,
                gl::FLOAT as _,
                0,
                stride,
                vertices.as_ptr().cast(),
            );
            gl::Enable(gl::SCISSOR_TEST as _);
            for rect in clipped_damage.rects() {
                gl::Scissor(
                    rect.x,
                    scissor_y(target_height, *rect, output_transform),
                    rect.width,
                    rect.height,
                );
                gl::DrawArrays(gl::TRIANGLE_STRIP as _, 0, 4);
            }
            gl::Disable(gl::SCISSOR_TEST as _);
            gl::DisableVertexAttribArray(program.position_location);
            gl::Disable(gl::BLEND as _);
            gl::UseProgram(0);
        }
    }
}

fn create_texture_program_from(fragment_source: &str) -> Result<TextureProgram, String> {
    let vertex_source = include_str!("../../shaders/texture_clip.vert");
    let vertex_shader = compile_shader(gl::VERTEX_SHADER as _, vertex_source)?;
    let fragment_shader = compile_shader(gl::FRAGMENT_SHADER as _, fragment_source)?;
    let program = link_program(vertex_shader, fragment_shader, "texture program")?;
    Ok(TextureProgram {
        program,
        position_location: unsafe {
            gl::GetAttribLocation(program, b"a_position\0".as_ptr().cast()) as gl::Uint
        },
        texcoord_location: unsafe {
            gl::GetAttribLocation(program, b"a_texcoord\0".as_ptr().cast()) as gl::Uint
        },
        sampler_location: unsafe {
            gl::GetUniformLocation(program, b"u_texture\0".as_ptr().cast())
        },
        transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_transform\0".as_ptr().cast())
        },
        tex_transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_tex_transform\0".as_ptr().cast())
        },
        item_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_item_size\0".as_ptr().cast())
        },
        clip_mode_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_mode\0".as_ptr().cast())
        },
        clip_outer_rect_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_outer_rect\0".as_ptr().cast())
        },
        clip_inner_rect_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_inner_rect\0".as_ptr().cast())
        },
        outer_corner_radius_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_outer_corner_radius\0".as_ptr().cast())
        },
        outer_rounding_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_outer_rounding_power\0".as_ptr().cast())
        },
        inner_corner_radius_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_inner_corner_radius\0".as_ptr().cast())
        },
        inner_rounding_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_clip_inner_rounding_power\0".as_ptr().cast())
        },
        alpha_location: unsafe { gl::GetUniformLocation(program, b"u_alpha\0".as_ptr().cast()) },
        opaque_sample_location: unsafe {
            gl::GetUniformLocation(program, b"u_opaque_sample\0".as_ptr().cast())
        },
        color_matrix_0: unsafe {
            gl::GetUniformLocation(program, b"u_color_matrix_0\0".as_ptr().cast())
        },
        color_matrix_1: unsafe {
            gl::GetUniformLocation(program, b"u_color_matrix_1\0".as_ptr().cast())
        },
        color_matrix_2: unsafe {
            gl::GetUniformLocation(program, b"u_color_matrix_2\0".as_ptr().cast())
        },
        src_bt1886_lb_over_lw: unsafe {
            gl::GetUniformLocation(program, b"u_src_bt1886_lb_over_lw\0".as_ptr().cast())
        },
        dst_bt1886_lb_over_lw: unsafe {
            gl::GetUniformLocation(program, b"u_dst_bt1886_lb_over_lw\0".as_ptr().cast())
        },
        src_power_exponent: unsafe {
            gl::GetUniformLocation(program, b"u_src_power_exponent\0".as_ptr().cast())
        },
        dst_power_exponent: unsafe {
            gl::GetUniformLocation(program, b"u_dst_power_exponent\0".as_ptr().cast())
        },
        uv_sampler_location: unsafe {
            gl::GetUniformLocation(program, b"u_texture_uv\0".as_ptr().cast())
        },
        yuv_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_yuv_size\0".as_ptr().cast())
        },
        yuv_site_location: unsafe {
            gl::GetUniformLocation(program, b"u_yuv_site\0".as_ptr().cast())
        },
        yuv_row0: unsafe { gl::GetUniformLocation(program, b"u_yuv_row0\0".as_ptr().cast()) },
        yuv_row1: unsafe { gl::GetUniformLocation(program, b"u_yuv_row1\0".as_ptr().cast()) },
        yuv_row2: unsafe { gl::GetUniformLocation(program, b"u_yuv_row2\0".as_ptr().cast()) },
        tonemap_max: unsafe { gl::GetUniformLocation(program, b"u_tonemap_max\0".as_ptr().cast()) },
        tonemap_dst_max: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_dst_max\0".as_ptr().cast())
        },
        tonemap_dst_min: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_dst_min\0".as_ptr().cast())
        },
        tonemap_dst_ref: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_dst_ref\0".as_ptr().cast())
        },
        tonemap_to_lms_0: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_to_lms_0\0".as_ptr().cast())
        },
        tonemap_to_lms_1: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_to_lms_1\0".as_ptr().cast())
        },
        tonemap_to_lms_2: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_to_lms_2\0".as_ptr().cast())
        },
        tonemap_from_lms_0: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_from_lms_0\0".as_ptr().cast())
        },
        tonemap_from_lms_1: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_from_lms_1\0".as_ptr().cast())
        },
        tonemap_from_lms_2: unsafe {
            gl::GetUniformLocation(program, b"u_tonemap_from_lms_2\0".as_ptr().cast())
        },
    })
}

fn create_border_program() -> Result<BorderProgram, String> {
    let vertex_source = include_str!("../../shaders/shadow.vert");
    let fragment_source = include_str!("../../shaders/border.frag");
    let vertex_shader = compile_shader(gl::VERTEX_SHADER as _, vertex_source)?;
    let fragment_shader = compile_shader(gl::FRAGMENT_SHADER as _, fragment_source)?;
    let program = link_program(vertex_shader, fragment_shader, "border program")?;

    Ok(BorderProgram {
        program,
        position_location: unsafe {
            gl::GetAttribLocation(program, b"a_position\0".as_ptr().cast()) as gl::Uint
        },
        transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_transform\0".as_ptr().cast())
        },
        item_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_item_size\0".as_ptr().cast())
        },
        outer_rect_location: unsafe {
            gl::GetUniformLocation(program, b"u_outer_rect\0".as_ptr().cast())
        },
        inner_rect_location: unsafe {
            gl::GetUniformLocation(program, b"u_inner_rect\0".as_ptr().cast())
        },
        outer_corner_radius_location: unsafe {
            gl::GetUniformLocation(program, b"u_outer_corner_radius\0".as_ptr().cast())
        },
        outer_rounding_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_outer_rounding_power\0".as_ptr().cast())
        },
        inner_corner_radius_location: unsafe {
            gl::GetUniformLocation(program, b"u_inner_corner_radius\0".as_ptr().cast())
        },
        inner_rounding_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_inner_rounding_power\0".as_ptr().cast())
        },
        start_color_location: unsafe {
            gl::GetUniformLocation(program, b"u_gradient_start\0".as_ptr().cast())
        },
        end_color_location: unsafe {
            gl::GetUniformLocation(program, b"u_gradient_end\0".as_ptr().cast())
        },
        angle_location: unsafe {
            gl::GetUniformLocation(program, b"u_gradient_angle_degrees\0".as_ptr().cast())
        },
        alpha_location: unsafe { gl::GetUniformLocation(program, b"u_alpha\0".as_ptr().cast()) },
    })
}

fn create_shadow_program() -> Result<ShadowProgram, String> {
    let vertex_source = include_str!("../../shaders/shadow.vert");
    let fragment_source = include_str!("../../shaders/shadow.frag");
    let vertex_shader = compile_shader(gl::VERTEX_SHADER as _, vertex_source)?;
    let fragment_shader = compile_shader(gl::FRAGMENT_SHADER as _, fragment_source)?;
    let program = link_program(vertex_shader, fragment_shader, "shadow program")?;

    Ok(ShadowProgram {
        program,
        position_location: unsafe {
            gl::GetAttribLocation(program, b"a_position\0".as_ptr().cast()) as gl::Uint
        },
        transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_transform\0".as_ptr().cast())
        },
        item_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_item_size\0".as_ptr().cast())
        },
        shadow_color_location: unsafe {
            gl::GetUniformLocation(program, b"u_shadow_color\0".as_ptr().cast())
        },
        inner_rect_location: unsafe {
            gl::GetUniformLocation(program, b"u_inner_rect\0".as_ptr().cast())
        },
        corner_radius_location: unsafe {
            gl::GetUniformLocation(program, b"u_corner_radius\0".as_ptr().cast())
        },
        rounding_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_rounding_power\0".as_ptr().cast())
        },
        shadow_range_location: unsafe {
            gl::GetUniformLocation(program, b"u_shadow_range\0".as_ptr().cast())
        },
        shadow_power_location: unsafe {
            gl::GetUniformLocation(program, b"u_shadow_power\0".as_ptr().cast())
        },
    })
}

fn set_output_clear_color() {
    let [red, green, blue, alpha] = crate::render::OUTPUT_BACKGROUND;
    unsafe {
        gl::ClearColor(red, green, blue, alpha);
    }
}

fn clear_framebuffer(framebuffer_object: gl::GLuint, width: i32, height: i32) {
    unsafe {
        gl::BindFramebuffer(gl::FRAMEBUFFER as _, framebuffer_object);
        gl::Viewport(0, 0, width, height);
        gl::Disable(gl::SCISSOR_TEST as _);
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT as _);
        gl::BlendFunc(gl::ONE as _, gl::ONE_MINUS_SRC_ALPHA as _);
    }
}

fn create_blur_program() -> Result<BlurProgram, String> {
    let vertex_source = include_str!("../../shaders/blur.vert");
    let fragment_source = include_str!("../../shaders/blur.frag");
    let vertex_shader = compile_shader(gl::VERTEX_SHADER as _, vertex_source)?;
    let fragment_shader = compile_shader(gl::FRAGMENT_SHADER as _, fragment_source)?;
    let program = link_program(vertex_shader, fragment_shader, "blur program")?;
    Ok(BlurProgram {
        program,
        position_location: unsafe {
            gl::GetAttribLocation(program, b"a_position\0".as_ptr().cast()) as gl::Uint
        },
        texcoord_location: unsafe {
            gl::GetAttribLocation(program, b"a_texcoord\0".as_ptr().cast()) as gl::Uint
        },
        sampler_location: unsafe {
            gl::GetUniformLocation(program, b"u_texture\0".as_ptr().cast())
        },
        direction_location: unsafe {
            gl::GetUniformLocation(program, b"u_direction\0".as_ptr().cast())
        },
        texel_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_texel_size\0".as_ptr().cast())
        },
        radius_location: unsafe { gl::GetUniformLocation(program, b"u_radius\0".as_ptr().cast()) },
        transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_transform\0".as_ptr().cast())
        },
        tex_transform_location: unsafe {
            gl::GetUniformLocation(program, b"u_tex_transform\0".as_ptr().cast())
        },
        item_size_location: unsafe {
            gl::GetUniformLocation(program, b"u_item_size\0".as_ptr().cast())
        },
    })
}

fn compile_shader(kind: gl::Enum, source: &str) -> Result<gl::Uint, String> {
    let shader = unsafe { gl::CreateShader(kind) };
    // `string` and `length` are parallel arrays of `count` elements (here count == 1).
    let strings: [*const gl::GLchar; 1] = [source.as_ptr().cast::<gl::GLchar>()];
    let lengths: [gl::GLint; 1] = [source.len() as gl::GLint];

    unsafe {
        gl::ShaderSource(shader, 1, strings.as_ptr(), lengths.as_ptr());
        gl::CompileShader(shader);
    }

    let mut status = 0;
    unsafe { gl::GetShaderiv(shader, gl::COMPILE_STATUS as _, &mut status) };
    if status == 0 {
        let log = shader_info_log(shader, true);
        unsafe { gl::DeleteShader(shader) };
        return Err(format!("could not compile shader: {log}"));
    }

    Ok(shader)
}

fn link_program(
    vertex_shader: gl::Uint,
    fragment_shader: gl::Uint,
    label: &str,
) -> Result<gl::Uint, String> {
    let program = unsafe { gl::CreateProgram() };
    unsafe {
        gl::AttachShader(program, vertex_shader);
        gl::AttachShader(program, fragment_shader);
        gl::LinkProgram(program);
        gl::DeleteShader(vertex_shader);
        gl::DeleteShader(fragment_shader);
    }

    let mut status = 0;
    unsafe { gl::GetProgramiv(program, gl::LINK_STATUS as _, &mut status) };
    if status == 0 {
        let log = shader_info_log(program, false);
        unsafe { gl::DeleteProgram(program) };
        return Err(format!("could not link {label}: {log}"));
    }
    Ok(program)
}

fn shader_info_log(object: gl::Uint, shader: bool) -> String {
    let mut length = 0;
    unsafe {
        if shader {
            gl::GetShaderiv(object, gl::INFO_LOG_LENGTH as _, &mut length);
        } else {
            gl::GetProgramiv(object, gl::INFO_LOG_LENGTH as _, &mut length);
        }
    }
    if length <= 1 {
        return String::new();
    }
    let mut bytes = vec![0_u8; length as usize];
    unsafe {
        if shader {
            gl::GetShaderInfoLog(object, length, std::ptr::null_mut(), bytes.as_mut_ptr().cast());
        } else {
            gl::GetProgramInfoLog(object, length, std::ptr::null_mut(), bytes.as_mut_ptr().cast());
        }
    }
    String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

fn scissor_y(
    target_height: i32,
    rect: RectI,
    output_transform: OutputPresentationTransform,
) -> i32 {
    framebuffer_rect_for_target(target_height, rect, output_transform)
        .map(|framebuffer_rect| framebuffer_rect.y)
        .unwrap_or(0)
}

/// Map the unit square onto `rect` inside the blurred `bounds` texture.
fn blur_source_transform(bounds: RectI, rect: RectI) -> Mat3 {
    let width = bounds.width as f32;
    let height = bounds.height as f32;
    Mat3::translation((rect.x - bounds.x) as f32 / width, (rect.y - bounds.y) as f32 / height)
        .mul(Mat3::scale(rect.width as f32 / width, rect.height as f32 / height))
}

fn framebuffer_rect_for_target(
    target_height: i32,
    rect: RectI,
    output_transform: OutputPresentationTransform,
) -> Option<RectI> {
    RectI::new(
        rect.x,
        match output_transform {
            OutputPresentationTransform::Normal => target_height - rect.y - rect.height,
            OutputPresentationTransform::FlipY => rect.y,
        },
        rect.width,
        rect.height,
    )
}

fn shadow_color_with_alpha(color: crate::config::RgbaColor, alpha: u8) -> [f32; 4] {
    let premultiplied = color.to_premultiplied_rgba_bytes();
    let global_alpha = f32::from(alpha) / 255.0;
    [
        (f32::from(premultiplied[0]) / 255.0) * global_alpha,
        (f32::from(premultiplied[1]) / 255.0) * global_alpha,
        (f32::from(premultiplied[2]) / 255.0) * global_alpha,
        (f32::from(premultiplied[3]) / 255.0) * global_alpha,
    ]
}

#[cfg(test)]
mod tests {
    use {
        super::{
            blur_pass_draws, blur_source_transform, framebuffer_rect_for_target,
            shadow_color_with_alpha,
        },
        crate::{
            config::RgbaColor,
            region::RectI,
            transform::{Mat3, OutputPresentationTransform},
        },
    };

    #[test]
    fn blur_pass_draws_ping_pongs_previous_result_across_multiple_passes() {
        let draws = blur_pass_draws(10, 20, 30, 2);

        assert_eq!(
            draws,
            vec![
                (10, 20, [1.0, 0.0]),
                (20, 30, [0.0, 1.0]),
                (30, 20, [1.0, 0.0]),
                (20, 30, [0.0, 1.0]),
            ]
        );
    }

    #[test]
    fn blur_shader_uses_radius_in_sample_offsets() {
        let source = include_str!("../../shaders/blur.frag");

        assert!(source.contains("1.384615 * radius"));
        assert!(source.contains("3.230769 * radius"));
    }

    #[test]
    fn blur_backdrop_is_built_from_in_stream_framebuffer_snapshot() {
        let source = include_str!("pipeline.rs");

        assert!(source.contains("self.build_blurred_backdrop("));
        assert!(source.contains("gl::CopyTexSubImage2D("));
        assert!(source
            .contains("framebuffer_rect_for_target(target_height, blur_bounds, output_transform)"));
    }

    #[test]
    fn blur_source_transform_selects_the_rect_inside_the_blurred_bounds() {
        let bounds = RectI::new(10, 20, 40, 20).unwrap();
        let whole = blur_source_transform(bounds, bounds);
        assert_eq!(whole.transform_point(0.0, 0.0), (0.0, 0.0));
        assert_eq!(whole.transform_point(1.0, 1.0), (1.0, 1.0));

        let rect = RectI::new(30, 20, 20, 10).unwrap();
        let source = blur_source_transform(bounds, rect);
        assert_eq!(source.transform_point(0.0, 0.0), (0.5, 0.0));
        assert_eq!(source.transform_point(1.0, 1.0), (1.0, 0.5));
        assert_ne!(source, Mat3::identity());
    }

    #[test]
    fn framebuffer_rect_helper_uses_the_active_render_target_height() {
        let rect = RectI::new(10, 20, 30, 40).unwrap();

        assert_eq!(
            framebuffer_rect_for_target(100, rect, OutputPresentationTransform::Normal),
            RectI::new(10, 40, 30, 40)
        );
        assert_eq!(
            framebuffer_rect_for_target(100, rect, OutputPresentationTransform::FlipY),
            RectI::new(10, 20, 30, 40)
        );
    }

    #[test]
    fn texture_and_border_shaders_use_rounded_inner_cutouts_without_texture_dim() {
        let texture_source = include_str!("../../shaders/texture_clip.frag");
        let border_source = include_str!("../../shaders/border.frag");

        assert!(texture_source.contains("pointInRoundedRect(point, u_clip_inner_rect"));
        assert!(!texture_source.contains("u_dim_amount"));
        assert!(!texture_source.contains("color.rgb *= (1.0 - u_dim_amount)"));
        assert!(border_source.contains("pointInRoundedRect(v_local_position, u_inner_rect"));
        assert!(!border_source.contains("pointInRect(v_local_position, u_inner_rect)"));
    }

    #[test]
    fn shadow_alpha_scales_premultiplied_rgb_and_alpha() {
        let color = shadow_color_with_alpha(RgbaColor::new(255, 128, 0, 128), 128);

        assert!((color[0] - 0.25196466).abs() < 0.0001);
        assert!((color[1] - 0.12598233).abs() < 0.0001);
        assert_eq!(color[2], 0.0);
        assert!((color[3] - 0.25196466).abs() < 0.0001);
    }

    #[test]
    fn gl_texture_name_rejects_another_backend_and_another_drm_device() {
        let device_id = crate::render::RenderDeviceId::next();
        let texture = super::super::import::GlTexture {
            image: None,
            texture: 7,
            delete_on_drop: false,
            device_id,
            samples_as_opaque: false,
            chroma: None,
            yuv_mode: 0,
            yuv_size: [0.0, 0.0],
        };

        assert_eq!(super::super::import::gl_texture_name(&texture, device_id), Some(7));
        assert!(super::super::import::gl_texture_name(
            &texture,
            crate::render::RenderDeviceId::next()
        )
        .is_none());
        assert!(super::super::import::gl_texture_name(
            crate::render::inert_texture().as_ref(),
            device_id
        )
        .is_none());
    }
}
