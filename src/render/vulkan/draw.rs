//! One textured frame.
//!
//! `BlockingFinishFallback` waits before it returns. `ExplicitInFence` exports a
//! sync file and leaves the command buffer, descriptors, and blur images alive
//! until that fence signals.

use {ash::vk, gpu_alloc::UsageFlags};

use std::{os::fd::AsRawFd, rc::Rc};

use crate::{
    color::ColorTransform,
    region::{DamageRegion, RectI},
    render::{
        color_pipeline::{color_uniforms, TexturePipelineKey},
        shadow_geometry, BlurVisualStyle, BorderGradient, EffectElement, FrameElement,
        OutputFrameRecipe, RenderCompletion, RenderFrame, RenderSyncMode, RendererError,
        RoundedRing, ShadowVisualStyle, Texture, TextureElement, TextureMask, TextureSource,
    },
    state::RootSurfaceClip,
    transform::{self, Mat3, OutputPresentationTransform},
};

use super::{
    allocator::AllocatedBuffer,
    command,
    device::{PendingFrame, RenderedSubmit, VulkanDevice},
    effects::BlurJob,
    pipeline::DrawState,
    resources::PaintedTarget,
    sync::OwnedSemaphores,
    texture,
};

#[repr(C)]
struct DrawUniforms {
    transform_c0: [f32; 4],
    transform_c1: [f32; 4],
    transform_c2: [f32; 4],
    tex_c0: [f32; 4],
    tex_c1: [f32; 4],
    tex_c2: [f32; 4],
    item_size: [f32; 2],
    alpha: f32,
    clip_mode: i32,
    outer_rect: [f32; 4],
    inner_rect: [f32; 4],
    outer_radius: f32,
    outer_power: f32,
    inner_radius: f32,
    inner_power: f32,
    // Tail of the Draw block. std140: three matrix rows, then the source and
    // destination BT.1886 ratios and power exponents.
    color_matrix_0: [f32; 4],
    color_matrix_1: [f32; 4],
    color_matrix_2: [f32; 4],
    src_bt1886_lb_over_lw: f32,
    dst_bt1886_lb_over_lw: f32,
    src_power_exponent: f32,
    dst_power_exponent: f32,
    yuv_size: [f32; 2],
    yuv_site: [f32; 2],
    yuv_row0: [f32; 4],
    yuv_row1: [f32; 4],
    yuv_row2: [f32; 4],
    tonemap_max: f32,
    tonemap_dst_max: f32,
    tonemap_dst_min: f32,
    tonemap_dst_ref: f32,
    tonemap_to_lms_0: [f32; 4],
    tonemap_to_lms_1: [f32; 4],
    tonemap_to_lms_2: [f32; 4],
    tonemap_from_lms_0: [f32; 4],
    tonemap_from_lms_1: [f32; 4],
    tonemap_from_lms_2: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<DrawUniforms>() == 400);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, item_size) == 96);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, outer_rect) == 112);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, color_matrix_0) == 160);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, src_bt1886_lb_over_lw) == 208);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, dst_power_exponent) == 220);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, yuv_size) == 224);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, tonemap_max) == 288);
const _: () = assert!(std::mem::offset_of!(DrawUniforms, tonemap_to_lms_0) == 304);

struct QuadDraw {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set: vk::DescriptorSet,
    _uniform: AllocatedBuffer,
    scissors: Vec<vk::Rect2D>,
}

enum FrameOp {
    Quad(QuadDraw),
    /// One framebuffer blur, then one or more textured composites of that result.
    Blur {
        job: BlurJob,
        results: Vec<QuadDraw>,
    },
}

#[repr(C)]
struct BorderUniforms {
    transform_c0: [f32; 4],
    transform_c1: [f32; 4],
    transform_c2: [f32; 4],
    item_size: [f32; 2],
    outer_radius: f32,
    outer_power: f32,
    inner_radius: f32,
    inner_power: f32,
    angle: f32,
    alpha: f32,
    outer_rect: [f32; 4],
    inner_rect: [f32; 4],
    gradient_start: [f32; 4],
    gradient_end: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<BorderUniforms>() == 144);
const _: () = assert!(std::mem::offset_of!(BorderUniforms, outer_rect) == 80);
const _: () = assert!(std::mem::offset_of!(BorderUniforms, gradient_start) == 112);

#[repr(C)]
struct ShadowUniforms {
    transform_c0: [f32; 4],
    transform_c1: [f32; 4],
    transform_c2: [f32; 4],
    item_size: [f32; 2],
    corner_radius: f32,
    rounding_power: f32,
    shadow_range: f32,
    shadow_power: f32,
    _pad: [f32; 2],
    shadow_color: [f32; 4],
    inner_rect: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<ShadowUniforms>() == 112);
const _: () = assert!(std::mem::offset_of!(ShadowUniforms, shadow_color) == 80);
const _: () = assert!(std::mem::offset_of!(ShadowUniforms, inner_rect) == 96);

struct BlendPass {
    image: vk::Image,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    render_pass: vk::RenderPass,
    ops: Vec<FrameOp>,
    clears: Vec<vk::Rect2D>,
    output_tf: crate::color::TransferFunction,
}

fn prepare_linear_pass(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    ops: &mut Vec<FrameOp>,
    kept_textures: &mut Vec<Rc<dyn Texture>>,
    foreign_images: &mut Vec<(vk::Image, bool)>,
    foreign_textures: &mut Vec<Rc<dyn Texture>>,
) -> Result<Option<BlendPass>, RendererError> {
    let Some(output_tf) = frame.linear_blend else {
        return Ok(None);
    };
    if output_tf == crate::color::TransferFunction::ExtLinear {
        return Ok(None);
    }
    let Some(bounds) = RectI::new(0, 0, target.width(), target.height()) else {
        return Ok(None);
    };
    let regions = crate::render::blend::create_regions(&frame.recipe.elements, bounds, true);
    let blend_damage = crate::render::blend::intersect_damage(&regions.blend, frame.damage);
    if blend_damage.is_empty() {
        return Ok(None);
    }
    let output_format = target.attachment_format();
    if let Err(error) = draw.use_attachment(vk::Format::R16G16B16A16_SFLOAT) {
        log::warn!("FP16 blend buffer is unavailable ({error}); blending in the output encoding");
        let _ = draw.use_attachment(output_format);
        return Ok(None);
    }
    let (image, view, framebuffer) = match draw.ensure_blend_image(target.width(), target.height())
    {
        Ok(handles) => handles,
        Err(error) => {
            log::warn!(
                "FP16 blend buffer is unavailable ({error}); blending in the output encoding"
            );
            let _ = draw.use_attachment(output_format);
            return Ok(None);
        }
    };
    let render_pass = draw.active_render_pass();
    let elements = crate::render::blend::retarget_elements(&frame.recipe.elements, output_tf);
    let recipe = OutputFrameRecipe { elements };
    let blend_frame = RenderFrame {
        recipe: &recipe,
        damage: &blend_damage,
        preserve_contents: true,
        presentation_transform: frame.presentation_transform,
        sync_mode: frame.sync_mode,
        clear_color: Some([0.0, 0.0, 0.0, 0.0]),
        blur_reference: frame.blur_reference,
        linear_blend: Some(output_tf),
    };
    let mut blend_ops = Vec::new();
    append_elements(
        device,
        draw,
        target,
        &blend_frame,
        &mut blend_ops,
        kept_textures,
        foreign_images,
        foreign_textures,
    )?;
    draw.use_attachment(output_format).map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    let direct = crate::render::blend::intersect_damage(&regions.direct, frame.damage);
    let direct_frame = RenderFrame {
        recipe: frame.recipe,
        damage: &direct,
        preserve_contents: frame.preserve_contents,
        presentation_transform: frame.presentation_transform,
        sync_mode: frame.sync_mode,
        clear_color: frame.clear_color,
        blur_reference: frame.blur_reference,
        linear_blend: None,
    };
    append_elements(
        device,
        draw,
        target,
        &direct_frame,
        ops,
        kept_textures,
        foreign_images,
        foreign_textures,
    )?;
    let pass = BlendPass {
        image,
        view,
        framebuffer,
        render_pass,
        ops: blend_ops,
        clears: region_clears(target.height(), &blend_damage, frame.presentation_transform),
        output_tf,
    };
    if let Some(copy) = prepare_blend_copy(
        device,
        draw,
        target,
        &pass,
        &blend_damage,
        frame.presentation_transform,
    )? {
        ops.push(FrameOp::Quad(copy));
    }
    Ok(Some(pass))
}

fn prepare_blend_copy(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    pass: &BlendPass,
    damage: &DamageRegion,
    presentation: OutputPresentationTransform,
) -> Result<Option<QuadDraw>, RendererError> {
    let color = crate::render::blend::encode_from_linear(pass.output_tf);
    let mut key = TexturePipelineKey::from_color(Some(&color), true);
    key.alpha_mode = 0;
    let pipeline = draw.pipeline_for(key).map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    let frame = RenderFrame {
        recipe: &OutputFrameRecipe { elements: Vec::new() },
        damage,
        preserve_contents: true,
        presentation_transform: presentation,
        sync_mode: RenderSyncMode::BlockingFinishFallback,
        clear_color: None,
        blur_reference: None,
        linear_blend: None,
    };
    prepare_sampled(
        device,
        draw,
        target,
        &frame,
        pipeline,
        pass.view,
        pass.view,
        [target.width() as f32, target.height() as f32],
        crate::color::ColorRepresentation::default().yuv_conversion(false),
        0,
        0,
        target.width(),
        target.height(),
        Mat3::identity(),
        1.0,
        &TextureMask::None,
        damage,
        Some(&color),
    )
}

fn append_elements(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    ops: &mut Vec<FrameOp>,
    kept_textures: &mut Vec<Rc<dyn Texture>>,
    foreign_images: &mut Vec<(vk::Image, bool)>,
    foreign_textures: &mut Vec<Rc<dyn Texture>>,
) -> Result<(), RendererError> {
    for element in &frame.recipe.elements {
        match element {
            FrameElement::Texture { element, .. } => {
                if let Some(quad) = prepare_texture(device, draw, target, element, frame)? {
                    if let Some(texture) =
                        texture::sampled_texture(element.texture.as_ref(), device.device_id())
                    {
                        if texture.foreign()
                            && !foreign_images
                                .iter()
                                .any(|(image, _)| *image == texture.image())
                        {
                            foreign_images.push((texture.image(), texture.yuv_mode() != 0));
                            foreign_textures.push(Rc::clone(&element.texture));
                        }
                    }
                    kept_textures.push(Rc::clone(&element.texture));
                    ops.push(FrameOp::Quad(quad));
                }
            }
            FrameElement::Effect { element, .. } => {
                if let Some(op) = prepare_effect(device, draw, target, element, frame) {
                    ops.push(op);
                }
            }
        }
    }
    Ok(())
}

fn record_blend_pass(
    vulkan: &ash::Device,
    command: vk::CommandBuffer,
    draw: &DrawState,
    target: &dyn PaintedTarget,
    pass: &BlendPass,
) {
    let old = draw
        .blend_layout()
        .map(|layout| layout.get())
        .unwrap_or(vk::ImageLayout::UNDEFINED);
    let (src_stage, src_access) = if old == vk::ImageLayout::UNDEFINED {
        (vk::PipelineStageFlags::TOP_OF_PIPE, vk::AccessFlags::empty())
    } else {
        (vk::PipelineStageFlags::FRAGMENT_SHADER, vk::AccessFlags::SHADER_READ)
    };
    command::image_barrier(
        vulkan,
        command,
        pass.image,
        old,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        src_stage,
        vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        src_access,
        vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
    );
    let mut cleared = false;
    begin_pass(
        vulkan,
        command,
        draw,
        pass.render_pass,
        pass.framebuffer,
        target.width(),
        target.height(),
        &pass.clears,
        [0.0, 0.0, 0.0, 0.0],
        &mut cleared,
    );
    for op in &pass.ops {
        match op {
            FrameOp::Quad(quad) => draw_quad(vulkan, command, quad),
            FrameOp::Blur { job, results } => {
                unsafe { vulkan.cmd_end_render_pass(command) };
                let mut layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
                job.record(vulkan, command, draw, pass.image, &mut layout);
                cleared = true;
                begin_pass(
                    vulkan,
                    command,
                    draw,
                    pass.render_pass,
                    pass.framebuffer,
                    target.width(),
                    target.height(),
                    &pass.clears,
                    [0.0, 0.0, 0.0, 0.0],
                    &mut cleared,
                );
                for result in results {
                    draw_quad(vulkan, command, result);
                }
            }
        }
    }
    unsafe { vulkan.cmd_end_render_pass(command) };
    command::image_barrier(
        vulkan,
        command,
        pass.image,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        vk::PipelineStageFlags::FRAGMENT_SHADER,
        vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        vk::AccessFlags::SHADER_READ,
    );
    if let Some(layout) = draw.blend_layout() {
        layout.set(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    }
}

fn region_clears(
    height: i32,
    damage: &DamageRegion,
    presentation: OutputPresentationTransform,
) -> Vec<vk::Rect2D> {
    damage
        .rects()
        .iter()
        .copied()
        .filter_map(|rect| framebuffer_rect(height, rect, presentation))
        .filter_map(vulkan_scissor)
        .collect()
}

pub(super) fn paint(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: RenderFrame<'_>,
    pending: &mut Vec<PendingFrame>,
    acquires: Vec<super::sync::StashedSemaphore>,
) -> Result<RenderCompletion, RendererError> {
    if target.width() <= 0 || target.height() <= 0 {
        log::error!("vulkan render target has no positive size");
        return Err(RendererError::RenderFailed);
    }
    draw.reset_descriptors().map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    draw.use_attachment(target.attachment_format())
        .map_err(|error| {
            log::error!("{error}");
            RendererError::RenderFailed
        })?;
    let mut ops = Vec::new();
    let mut kept_textures = Vec::new();
    let mut foreign_images: Vec<(vk::Image, bool)> = Vec::new();
    let mut foreign_textures = Vec::new();
    let blend_pass = prepare_linear_pass(
        device,
        draw,
        target,
        &frame,
        &mut ops,
        &mut kept_textures,
        &mut foreign_images,
        &mut foreign_textures,
    )?;
    if blend_pass.is_none() {
        append_elements(
            device,
            draw,
            target,
            &frame,
            &mut ops,
            &mut kept_textures,
            &mut foreign_images,
            &mut foreign_textures,
        )?;
    }
    let clear_rects =
        clear_rects(target, frame.damage, frame.preserve_contents, frame.presentation_transform);
    let clear_color = frame.clear_color.unwrap_or_else(|| target.clear_color());
    let explicit = frame.sync_mode == RenderSyncMode::ExplicitInFence;
    let foreign_planes = foreign_plane_fds(&foreign_textures, device.device_id());
    let implicit = super::sync::implicit_read_waits(device, &foreign_planes)?;
    let mut acquires = acquires;
    acquires.extend(implicit);
    let waits = OwnedSemaphores::from_stashed(device, acquires);
    let external = target.external();
    let graphics_family = device.graphics_queue_family;
    let submitted = device.submit_rendered(explicit, waits.as_slice(), |vulkan, command| {
        let old_layout = target.layout().get();
        if external {
            command::record_barrier(
                vulkan,
                command,
                target.image(),
                command::foreign_acquire(
                    old_layout == vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    graphics_family,
                ),
            );
        } else {
            let (src_stage, src_access) = if old_layout == vk::ImageLayout::UNDEFINED {
                (vk::PipelineStageFlags::TOP_OF_PIPE, vk::AccessFlags::empty())
            } else {
                (
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                )
            };
            command::image_barrier(
                vulkan,
                command,
                target.image(),
                old_layout,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_stage,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                src_access,
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
        }
        for (image, yuv) in &foreign_images {
            let spec = command::foreign_acquire(
                false,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::AccessFlags::SHADER_READ,
                graphics_family,
            );
            if *yuv {
                command::record_plane_barriers(vulkan, command, *image, spec);
            } else {
                command::record_barrier(vulkan, command, *image, spec);
            }
        }
        if let Some(pass) = &blend_pass {
            record_blend_pass(vulkan, command, draw, target, pass);
        }
        let mut layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
        let mut pass_open = false;
        let mut cleared = false;
        for op in &ops {
            match op {
                FrameOp::Quad(quad) => {
                    if !pass_open {
                        begin_output_pass(
                            vulkan,
                            command,
                            draw,
                            target,
                            &clear_rects,
                            clear_color,
                            &mut cleared,
                        );
                        pass_open = true;
                    }
                    draw_quad(vulkan, command, quad);
                }
                FrameOp::Blur { job, results } => {
                    if !pass_open {
                        begin_output_pass(
                            vulkan,
                            command,
                            draw,
                            target,
                            &clear_rects,
                            clear_color,
                            &mut cleared,
                        );
                    }
                    unsafe { vulkan.cmd_end_render_pass(command) };
                    job.record(vulkan, command, draw, target.image(), &mut layout);
                    begin_output_pass(
                        vulkan,
                        command,
                        draw,
                        target,
                        &clear_rects,
                        clear_color,
                        &mut cleared,
                    );
                    pass_open = true;
                    for result in results {
                        draw_quad(vulkan, command, result);
                    }
                }
            }
        }
        if !pass_open {
            begin_output_pass(
                vulkan,
                command,
                draw,
                target,
                &clear_rects,
                clear_color,
                &mut cleared,
            );
        }
        unsafe { vulkan.cmd_end_render_pass(command) };
        for (image, yuv) in &foreign_images {
            let spec = command::foreign_release(
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::AccessFlags::SHADER_READ,
                graphics_family,
            );
            if *yuv {
                command::record_plane_barriers(vulkan, command, *image, spec);
            } else {
                command::record_barrier(vulkan, command, *image, spec);
            }
        }
        if external {
            // Jay's final_barriers releases a direct framebuffer from
            // COLOR_ATTACHMENT_OPTIMAL back to the foreign queue.
            command::record_barrier(
                vulkan,
                command,
                target.image(),
                command::foreign_release(
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    graphics_family,
                ),
            );
        }
        Ok(())
    })?;
    target.layout().set(if external {
        vk::ImageLayout::GENERAL
    } else {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
    });
    match submitted {
        RenderedSubmit::Blocked => Ok(RenderCompletion::blocking_fallback()),
        RenderedSubmit::Explicit(detached) => {
            super::sync::attach_implicit_read_fence(device, &foreign_planes, detached.sync_fd());
            let pool = draw.park_pool();
            let (sync_fd, parked) = detached.into_frame(
                device,
                waits.into_handles(),
                pool,
                Box::new((ops, kept_textures)),
            );
            if crate::logger::ENABLE_NOISY_LOG {
                log::debug!("vulkan exported explicit render fence fd={}", sync_fd.as_raw_fd());
            }
            pending.push(parked);
            Ok(RenderCompletion::explicit(sync_fd))
        }
    }
}

fn foreign_plane_fds(
    textures: &[Rc<dyn crate::render::Texture>],
    device_id: crate::render::RenderDeviceId,
) -> Vec<&std::os::fd::OwnedFd> {
    textures
        .iter()
        .filter_map(|texture| texture::sampled_texture(texture.as_ref(), device_id))
        .flat_map(|texture| texture.plane_fds().iter())
        .collect()
}

fn yuv_draw(element: &TextureElement, yuv_mode: u32) -> crate::color::YuvConversion {
    if let Some(yuv) = element.yuv {
        return yuv;
    }
    crate::color::ColorRepresentation::default()
        .yuv_conversion(yuv_mode == crate::render::color_pipeline::YUV_BT709_LIMITED_10)
}

fn prepare_texture(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    element: &TextureElement,
    frame: &RenderFrame<'_>,
) -> Result<Option<QuadDraw>, RendererError> {
    let Some(texture) = texture::sampled_texture(element.texture.as_ref(), device.device_id())
    else {
        return Ok(None);
    };
    let mut key = TexturePipelineKey::from_color(element.color.as_deref(), true);
    key.yuv_mode = texture.yuv_mode();
    if !texture.samples_as_opaque() {
        key.alpha_mode = element.alpha_mode;
    }
    let pipeline = draw.pipeline_for(key).map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    let chroma =
        if texture.uv_view() == vk::ImageView::null() { texture.view() } else { texture.uv_view() };
    let yuv = yuv_draw(element, texture.yuv_mode());
    prepare_sampled(
        device,
        draw,
        target,
        frame,
        pipeline,
        texture.view(),
        chroma,
        texture.yuv_size(),
        yuv,
        element.x,
        element.y,
        element.width,
        element.height,
        element.tex_transform,
        crate::render::effective_alpha(element.alpha, element.alpha_multiplier),
        &element.mask,
        &element.damage,
        element.color.as_deref(),
    )
}

fn prepare_sampled(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    pipeline: vk::Pipeline,
    view: vk::ImageView,
    chroma: vk::ImageView,
    yuv_size: [f32; 2],
    yuv: crate::color::YuvConversion,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    tex_transform: Mat3,
    alpha: f32,
    mask: &TextureMask,
    damage: &DamageRegion,
    color: Option<&ColorTransform>,
) -> Result<Option<QuadDraw>, RendererError> {
    let Some(item_bounds) = RectI::new(x, y, width, height) else {
        return Ok(None);
    };
    let Some(output_bounds) = RectI::new(0, 0, target.width(), target.height()) else {
        return Ok(None);
    };
    let mut clipped = frame.damage.clone();
    clipped.intersect_rect(output_bounds);
    clipped.intersect_rect(item_bounds);
    clipped.intersect_rect(damage.extents().unwrap_or(item_bounds));
    if clipped.is_empty() {
        return Ok(None);
    }
    let scissors = clipped
        .rects()
        .iter()
        .filter_map(|rect| framebuffer_rect(target.height(), *rect, frame.presentation_transform))
        .filter_map(vulkan_scissor)
        .collect::<Vec<_>>();
    if scissors.is_empty() {
        return Ok(None);
    }
    let projection = transform::output_projection(
        target.width() as f32,
        target.height() as f32,
        frame.presentation_transform,
    );
    let transform = columns(transform::project_box(
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        projection,
    ));
    let tex = columns(tex_transform);
    let clip = clip_uniforms(mask);
    let color = color_uniforms(color);
    let uniforms = DrawUniforms {
        transform_c0: transform[0],
        transform_c1: transform[1],
        transform_c2: transform[2],
        tex_c0: tex[0],
        tex_c1: tex[1],
        tex_c2: tex[2],
        item_size: [width as f32, height as f32],
        alpha,
        clip_mode: clip.mode,
        outer_rect: clip.outer,
        inner_rect: clip.inner,
        outer_radius: clip.outer_radius,
        outer_power: clip.outer_power,
        inner_radius: clip.inner_radius,
        inner_power: clip.inner_power,
        color_matrix_0: color.matrix[0],
        color_matrix_1: color.matrix[1],
        color_matrix_2: color.matrix[2],
        src_bt1886_lb_over_lw: color.src_bt1886_lb_over_lw,
        dst_bt1886_lb_over_lw: color.dst_bt1886_lb_over_lw,
        src_power_exponent: color.src_power_exponent,
        dst_power_exponent: color.dst_power_exponent,
        yuv_size,
        yuv_site: yuv.site,
        yuv_row0: yuv.rows[0],
        yuv_row1: yuv.rows[1],
        yuv_row2: yuv.rows[2],
        tonemap_max: color.tonemap_max,
        tonemap_dst_max: color.tonemap_dst_max,
        tonemap_dst_min: color.tonemap_dst_min,
        tonemap_dst_ref: color.tonemap_dst_ref,
        tonemap_to_lms_0: color.tonemap_to_lms[0],
        tonemap_to_lms_1: color.tonemap_to_lms[1],
        tonemap_to_lms_2: color.tonemap_to_lms[2],
        tonemap_from_lms_0: color.tonemap_from_lms[0],
        tonemap_from_lms_1: color.tonemap_from_lms[1],
        tonemap_from_lms_2: color.tonemap_from_lms[2],
    };
    let uniform = uniform_buffer(device, &uniforms).map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    let set = draw.allocate_set().map_err(|error| {
        log::error!("{error}");
        RendererError::RenderFailed
    })?;
    write_set(
        &device.device,
        set,
        draw.sampler(),
        view,
        chroma,
        uniform.buffer(),
        std::mem::size_of::<DrawUniforms>() as u64,
    );
    Ok(Some(QuadDraw {
        pipeline,
        layout: draw.pipeline_layout(),
        set,
        _uniform: uniform,
        scissors,
    }))
}

fn write_set(
    device: &ash::Device,
    set: vk::DescriptorSet,
    sampler: vk::Sampler,
    view: vk::ImageView,
    chroma: vk::ImageView,
    buffer: vk::Buffer,
    range: u64,
) {
    let image = vk::DescriptorImageInfo {
        sampler,
        image_view: view,
        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    };
    let chroma_image = vk::DescriptorImageInfo {
        sampler,
        image_view: chroma,
        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    };
    let uniform = vk::DescriptorBufferInfo { buffer, offset: 0, range };
    let writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image)),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(std::slice::from_ref(&uniform)),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&chroma_image)),
    ];
    unsafe { device.update_descriptor_sets(&writes, &[]) };
}

fn columns(transform: Mat3) -> [[f32; 4]; 3] {
    let packed = transform.to_gl_array();
    [
        [packed[0], packed[1], packed[2], 0.0],
        [packed[3], packed[4], packed[5], 0.0],
        [packed[6], packed[7], packed[8], 0.0],
    ]
}

struct ClipUniforms {
    mode: i32,
    outer: [f32; 4],
    inner: [f32; 4],
    outer_radius: f32,
    outer_power: f32,
    inner_radius: f32,
    inner_power: f32,
}

fn clip_uniforms(mask: &TextureMask) -> ClipUniforms {
    match mask {
        TextureMask::None => ClipUniforms {
            mode: 0,
            outer: [0.0; 4],
            inner: [0.0; 4],
            outer_radius: 0.0,
            outer_power: 0.0001,
            inner_radius: 0.0,
            inner_power: 0.0001,
        },
        TextureMask::RoundedClip { clip } => ClipUniforms {
            mode: 1,
            outer: rect_vec(clip.outer_rect),
            inner: [0.0; 4],
            outer_radius: clip.corner_radius.max(0) as f32,
            outer_power: clip.rounding_power.get().max(0.0001),
            inner_radius: 0.0,
            inner_power: 0.0001,
        },
        TextureMask::RoundedClipUnderBorder { clip } => ClipUniforms {
            mode: 4,
            outer: rect_vec(clip.outer_rect),
            inner: [0.0; 4],
            outer_radius: clip.corner_radius.max(0) as f32,
            outer_power: clip.rounding_power.get().max(0.0001),
            inner_radius: 0.0,
            inner_power: 0.0001,
        },
        // Corner-only rounding: pixels outside the window box are kept, so
        // client-drawn CSD shadows survive.
        TextureMask::CornerRoundedClip { clip } => ClipUniforms {
            mode: 3,
            outer: rect_vec(clip.outer_rect),
            inner: [0.0; 4],
            outer_radius: clip.corner_radius.max(0) as f32,
            outer_power: clip.rounding_power.get().max(0.0001),
            inner_radius: 0.0,
            inner_power: 0.0001,
        },
        TextureMask::FrameRing { ring } => ClipUniforms {
            mode: 2,
            outer: rect_vec(ring.outer.outer_rect),
            inner: rect_vec(ring.inner.outer_rect),
            outer_radius: ring.outer.corner_radius.max(0) as f32,
            outer_power: ring.outer.rounding_power.get().max(0.0001),
            inner_radius: ring.inner.corner_radius.max(0) as f32,
            inner_power: ring.inner.rounding_power.get().max(0.0001),
        },
    }
}

fn rect_vec(rect: RectI) -> [f32; 4] {
    [rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32]
}

fn clear_rects(
    target: &dyn PaintedTarget,
    damage: &DamageRegion,
    preserve_contents: bool,
    presentation: OutputPresentationTransform,
) -> Vec<vk::Rect2D> {
    let Some(bounds) = RectI::new(0, 0, target.width(), target.height()) else {
        return Vec::new();
    };
    let mut clipped = damage.clone();
    clipped.intersect_rect(bounds);
    if !preserve_contents || clipped.extents() == Some(bounds) {
        return vec![vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D { width: target.width() as u32, height: target.height() as u32 },
        }];
    }
    clipped
        .rects()
        .iter()
        .filter_map(|rect| framebuffer_rect(target.height(), *rect, presentation))
        .filter_map(vulkan_scissor)
        .collect()
}

fn framebuffer_rect(
    target_height: i32,
    rect: RectI,
    presentation: OutputPresentationTransform,
) -> Option<RectI> {
    RectI::new(
        rect.x,
        match presentation {
            OutputPresentationTransform::Normal => target_height - rect.y - rect.height,
            OutputPresentationTransform::FlipY => rect.y,
        },
        rect.width,
        rect.height,
    )
}

fn vulkan_scissor(rect: RectI) -> Option<vk::Rect2D> {
    if rect.width <= 0 || rect.height <= 0 {
        return None;
    }
    Some(vk::Rect2D {
        offset: vk::Offset2D { x: rect.x, y: rect.y },
        extent: vk::Extent2D { width: rect.width as u32, height: rect.height as u32 },
    })
}

fn uniform_buffer<T>(device: &Rc<VulkanDevice>, value: &T) -> Result<AllocatedBuffer, String> {
    let bytes = unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    };
    let buffer = AllocatedBuffer::create(
        device,
        bytes.len() as u64,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        UsageFlags::UPLOAD | UsageFlags::TRANSIENT,
    )?;
    buffer.write(bytes)?;
    Ok(buffer)
}

fn prepare_effect(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    element: &EffectElement,
    frame: &RenderFrame<'_>,
) -> Option<FrameOp> {
    match element {
        EffectElement::WindowBorder { ring, gradient, alpha } => {
            prepare_border(device, draw, target, frame, *ring, *gradient, *alpha).map(FrameOp::Quad)
        }
        EffectElement::WindowShadow { clip, style, alpha } => {
            prepare_shadow(device, draw, target, frame, *clip, *style, *alpha).map(FrameOp::Quad)
        }
        EffectElement::DimOverlay { clip, alpha } => {
            prepare_dim(device, draw, target, frame, *clip, *alpha)
        }
        EffectElement::WindowBlurBackdrop { ring, style, alpha } => {
            prepare_blur(device, draw, target, frame, *ring, *style, *alpha)
        }
        EffectElement::SurfaceBlurBackdrop { region, style, alpha } => {
            prepare_surface_blur(device, draw, target, frame, region, *style, *alpha)
        }
    }
}

fn prepare_border(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    ring: RoundedRing,
    gradient: BorderGradient,
    alpha: u8,
) -> Option<QuadDraw> {
    let bounds = ring.outer.outer_rect;
    let scissors = effect_scissors(target, frame, bounds)?;
    let projection = transform::output_projection(
        target.width() as f32,
        target.height() as f32,
        frame.presentation_transform,
    );
    let transform = columns(transform::project_box(
        bounds.x as f32,
        bounds.y as f32,
        bounds.width as f32,
        bounds.height as f32,
        projection,
    ));
    let uniforms = BorderUniforms {
        transform_c0: transform[0],
        transform_c1: transform[1],
        transform_c2: transform[2],
        item_size: [bounds.width as f32, bounds.height as f32],
        outer_radius: ring.outer.corner_radius.max(0) as f32,
        outer_power: ring.outer.rounding_power.get().max(0.0001),
        inner_radius: ring.inner.corner_radius.max(0) as f32,
        inner_power: ring.inner.rounding_power.get().max(0.0001),
        angle: gradient.angle_degrees as f32,
        alpha: f32::from(alpha) / 255.0,
        outer_rect: [0.0, 0.0, bounds.width as f32, bounds.height as f32],
        inner_rect: [
            ring.inner.outer_rect.x.saturating_sub(bounds.x) as f32,
            ring.inner.outer_rect.y.saturating_sub(bounds.y) as f32,
            ring.inner.outer_rect.width as f32,
            ring.inner.outer_rect.height as f32,
        ],
        gradient_start: crate::render::blend::premul_color(
            gradient.start_color,
            frame.linear_blend,
        ),
        gradient_end: crate::render::blend::premul_color(gradient.end_color, frame.linear_blend),
    };
    solid_quad(
        device,
        draw,
        draw.border_pipeline(),
        draw.border_layout(),
        draw.border_set_layout(),
        &uniforms,
        scissors,
        "border",
    )
}

fn prepare_shadow(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    clip: RootSurfaceClip,
    style: ShadowVisualStyle,
    alpha: u8,
) -> Option<QuadDraw> {
    let geometry = shadow_geometry(clip, style)?;
    let bounds = geometry.bounds;
    let scissors = effect_scissors(target, frame, bounds)?;
    let projection = transform::output_projection(
        target.width() as f32,
        target.height() as f32,
        frame.presentation_transform,
    );
    let transform = columns(transform::project_box(
        bounds.x as f32,
        bounds.y as f32,
        bounds.width as f32,
        bounds.height as f32,
        projection,
    ));
    let uniforms = ShadowUniforms {
        transform_c0: transform[0],
        transform_c1: transform[1],
        transform_c2: transform[2],
        item_size: [bounds.width as f32, bounds.height as f32],
        corner_radius: clip.corner_radius.max(0) as f32,
        rounding_power: clip.rounding_power.get().max(0.0001),
        shadow_range: geometry.shadow_range as f32,
        shadow_power: style.power.get(),
        _pad: [0.0; 2],
        shadow_color: crate::render::blend::shadow_coverage(style.color, alpha, frame.linear_blend),
        inner_rect: [
            geometry.inner_rect.x as f32,
            geometry.inner_rect.y as f32,
            geometry.inner_rect.width as f32,
            geometry.inner_rect.height as f32,
        ],
    };
    solid_quad(
        device,
        draw,
        draw.shadow_pipeline(),
        draw.shadow_layout(),
        draw.shadow_set_layout(),
        &uniforms,
        scissors,
        "shadow",
    )
}

fn prepare_dim(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    clip: RootSurfaceClip,
    alpha: u8,
) -> Option<FrameOp> {
    let bounds = clip.outer_rect;
    let Some(local) = RectI::new(0, 0, bounds.width, bounds.height) else {
        return None;
    };
    let element = TextureElement {
        source: TextureSource::Texture,
        texture: draw.dim_texture(),
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
        tex_transform: Mat3::identity(),
        damage: frame.damage.clone(),
        blend: true,
        alpha,
        alpha_multiplier: u32::MAX,
        color: None,
        alpha_mode: 0,
        yuv: None,
        mask: TextureMask::RoundedClip {
            clip: RootSurfaceClip::new(local, clip.corner_radius, clip.rounding_power),
        },
    };
    match prepare_texture(device, draw, target, &element, frame) {
        Ok(Some(quad)) => Some(FrameOp::Quad(quad)),
        Ok(None) => None,
        Err(_) => {
            log::error!("vulkan skipped dim overlay");
            None
        }
    }
}

fn prepare_blur(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    ring: RoundedRing,
    style: BlurVisualStyle,
    alpha: u8,
) -> Option<FrameOp> {
    let bounds = blur_snapshot_bounds(target, frame.blur_reference, ring.outer, style)?;
    let mask = TextureMask::FrameRing {
        ring: RoundedRing {
            outer: RootSurfaceClip::new(
                ring.outer
                    .outer_rect
                    .translate(bounds.x.saturating_neg(), bounds.y.saturating_neg()),
                ring.outer.corner_radius,
                ring.outer.rounding_power,
            ),
            inner: RootSurfaceClip::new(
                ring.inner
                    .outer_rect
                    .translate(bounds.x.saturating_neg(), bounds.y.saturating_neg()),
                ring.inner.corner_radius,
                ring.inner.rounding_power,
            ),
        },
    };
    let job = blur_framebuffer_rect(device, draw, target, frame, bounds, style)?;
    let result = blur_quad(
        device,
        draw,
        target,
        frame,
        job.result_view(),
        bounds,
        Mat3::identity(),
        &mask,
        frame.damage,
        alpha,
    )?;
    Some(FrameOp::Blur { job, results: vec![result] })
}

fn prepare_surface_blur(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    region: &crate::render::BlurRegion,
    style: BlurVisualStyle,
    alpha: u8,
) -> Option<FrameOp> {
    let output = RectI::new(0, 0, target.width(), target.height())?;
    let bounds = region.bounds.intersection(output)?;
    let job = blur_framebuffer_rect(device, draw, target, frame, bounds, style)?;
    let view = job.result_view();
    let mut results = Vec::new();
    for rect in region.rects.iter().copied() {
        let Some(rect) = rect.intersection(bounds) else {
            continue;
        };
        let Some(scissors) = effect_scissors(target, frame, rect) else {
            continue;
        };
        let Some(mut quad) = blur_quad(
            device,
            draw,
            target,
            frame,
            view,
            rect,
            crate::render::blur_source_transform(bounds, rect),
            &TextureMask::None,
            &DamageRegion::from_rect(rect),
            alpha,
        ) else {
            continue;
        };
        quad.scissors = scissors;
        results.push(quad);
    }
    if results.is_empty() {
        return None;
    }
    Some(FrameOp::Blur { job, results })
}

/// Copy `bounds` out of the framebuffer and blur it. A window passes its padded
/// ring box; a client region passes `BlurRegion::bounds`.
fn blur_framebuffer_rect(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    bounds: RectI,
    style: BlurVisualStyle,
) -> Option<BlurJob> {
    if bounds.is_empty() {
        return None;
    }
    let copy = framebuffer_rect(target.height(), bounds, frame.presentation_transform)
        .and_then(vulkan_scissor)?;
    BlurJob::new(
        device,
        draw,
        bounds.width,
        bounds.height,
        style.radius.max(1) as f32,
        style.passes.max(1),
        frame.presentation_transform,
        copy,
    )
}

fn blur_quad(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    view: vk::ImageView,
    bounds: RectI,
    tex_transform: Mat3,
    mask: &TextureMask,
    damage: &DamageRegion,
    alpha: u8,
) -> Option<QuadDraw> {
    match prepare_sampled(
        device,
        draw,
        target,
        frame,
        draw.pipeline(),
        view,
        view,
        [0.0, 0.0],
        crate::color::ColorRepresentation::default().yuv_conversion(false),
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
        tex_transform,
        f32::from(alpha) / 255.0,
        mask,
        damage,
        None,
    ) {
        Ok(quad) => quad,
        Err(_) => {
            log::error!("vulkan skipped blur");
            None
        }
    }
}

fn blur_snapshot_bounds(
    target: &dyn PaintedTarget,
    reference: Option<(i32, i32)>,
    clip: RootSurfaceClip,
    style: BlurVisualStyle,
) -> Option<RectI> {
    let (reference_width, reference_height) =
        reference.unwrap_or((target.width(), target.height()));
    let reference_bounds = RectI::new(0, 0, reference_width, reference_height)?;
    let target_bounds = RectI::new(0, 0, target.width(), target.height())?;
    let padding = style
        .radius
        .max(1)
        .saturating_mul(i32::from(style.passes.max(1)))
        .saturating_mul(4);
    RectI::from_extents(
        clip.outer_rect.x.saturating_sub(padding),
        clip.outer_rect.y.saturating_sub(padding),
        clip.outer_rect.x2().saturating_add(padding),
        clip.outer_rect.y2().saturating_add(padding),
    )?
    .intersection(reference_bounds)?
    .intersection(target_bounds)
}

fn effect_scissors(
    target: &dyn PaintedTarget,
    frame: &RenderFrame<'_>,
    bounds: RectI,
) -> Option<Vec<vk::Rect2D>> {
    let output_bounds = RectI::new(0, 0, target.width(), target.height())?;
    let mut clipped = frame.damage.clone();
    clipped.intersect_rect(output_bounds);
    clipped.intersect_rect(bounds);
    if clipped.is_empty() {
        return None;
    }
    let scissors = clipped
        .rects()
        .iter()
        .filter_map(|rect| framebuffer_rect(target.height(), *rect, frame.presentation_transform))
        .filter_map(vulkan_scissor)
        .collect::<Vec<_>>();
    if scissors.is_empty() {
        None
    } else {
        Some(scissors)
    }
}

fn solid_quad<T>(
    device: &Rc<VulkanDevice>,
    draw: &mut DrawState,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    uniforms: &T,
    scissors: Vec<vk::Rect2D>,
    label: &str,
) -> Option<QuadDraw> {
    let uniform = match uniform_buffer(device, uniforms) {
        Ok(uniform) => uniform,
        Err(error) => {
            log::error!("vulkan skipped {label}: {error}");
            return None;
        }
    };
    let set = match draw.allocate_set_for(set_layout) {
        Ok(set) => set,
        Err(error) => {
            log::error!("vulkan skipped {label}: {error}");
            return None;
        }
    };
    let info = vk::DescriptorBufferInfo {
        buffer: uniform.buffer(),
        offset: 0,
        range: std::mem::size_of::<T>() as u64,
    };
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(0)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .buffer_info(std::slice::from_ref(&info));
    unsafe {
        device
            .device
            .update_descriptor_sets(std::slice::from_ref(&write), &[])
    };
    Some(QuadDraw { pipeline, layout, set, _uniform: uniform, scissors })
}

fn begin_output_pass(
    vulkan: &ash::Device,
    command: vk::CommandBuffer,
    draw: &DrawState,
    target: &dyn PaintedTarget,
    clear_rects: &[vk::Rect2D],
    clear_color: [f32; 4],
    cleared: &mut bool,
) {
    begin_pass(
        vulkan,
        command,
        draw,
        target.render_pass(),
        target.framebuffer(),
        target.width(),
        target.height(),
        clear_rects,
        clear_color,
        cleared,
    );
}

fn begin_pass(
    vulkan: &ash::Device,
    command: vk::CommandBuffer,
    draw: &DrawState,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    width: i32,
    height: i32,
    clear_rects: &[vk::Rect2D],
    clear_color: [f32; 4],
    cleared: &mut bool,
) {
    let begin = vk::RenderPassBeginInfo::default()
        .render_pass(render_pass)
        .framebuffer(framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D { width: width as u32, height: height as u32 },
        });
    unsafe {
        vulkan.cmd_begin_render_pass(command, &begin, vk::SubpassContents::INLINE);
        if !*cleared {
            if !clear_rects.is_empty() {
                let attachment = vk::ClearAttachment::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .color_attachment(0)
                    .clear_value(vk::ClearValue {
                        color: vk::ClearColorValue { float32: clear_color },
                    });
                let regions: Vec<vk::ClearRect> = clear_rects
                    .iter()
                    .copied()
                    .map(|rect| vk::ClearRect { rect, base_array_layer: 0, layer_count: 1 })
                    .collect();
                vulkan.cmd_clear_attachments(command, std::slice::from_ref(&attachment), &regions);
            }
            *cleared = true;
        }
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        vulkan.cmd_set_viewport(command, 0, std::slice::from_ref(&viewport));
        vulkan.cmd_bind_vertex_buffers(command, 0, std::slice::from_ref(&draw.vertices()), &[0]);
    }
}

fn draw_quad(vulkan: &ash::Device, command: vk::CommandBuffer, quad: &QuadDraw) {
    unsafe {
        vulkan.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, quad.pipeline);
        vulkan.cmd_bind_descriptor_sets(
            command,
            vk::PipelineBindPoint::GRAPHICS,
            quad.layout,
            0,
            std::slice::from_ref(&quad.set),
            &[],
        );
        for scissor in &quad.scissors {
            vulkan.cmd_set_scissor(command, 0, std::slice::from_ref(scissor));
            vulkan.cmd_draw(command, 4, 1, 0, 0);
        }
    }
}
