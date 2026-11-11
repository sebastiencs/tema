//! Textured-quad, border, shadow, and blur pipelines.

use std::{collections::HashMap, ffi::CString, rc::Rc};

use {ash::vk, gpu_alloc::UsageFlags};

use crate::render::{color_pipeline::TexturePipelineKey, Texture};

use super::{
    allocator::AllocatedBuffer, compiled_shaders, device::VulkanDevice, resources, texture,
};

pub(super) struct DrawState {
    device: Rc<VulkanDevice>,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    vertices: AllocatedBuffer,
    effects: EffectGpu,
    /// Pipelines other than the identity blend-on program. Created on first use.
    texture_pipelines: HashMap<TexturePipelineKey, vk::Pipeline>,
    /// FP16 linear intermediate. Created on the first HDR frame that needs it.
    blend_image: Option<resources::BlendImage>,
    /// Attachment format selected for the frame being recorded.
    active: vk::Format,
    /// Render passes other than `B8G8R8A8`. A 10-bit target needs its own pipelines.
    extra: HashMap<vk::Format, ExtraPass>,
}

impl DrawState {
    pub(super) fn new(device: &Rc<VulkanDevice>) -> Result<Self, String> {
        let sampler = unsafe {
            device.device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
        }
        .map_err(|error| format!("vulkan sampler creation failed: {error}"))?;
        let set_layout = match set_layout(&device.device, sampler) {
            Ok(layout) => layout,
            Err(error) => {
                unsafe { device.device.destroy_sampler(sampler, None) };
                return Err(error);
            }
        };
        let pool = match descriptor_pool(&device.device) {
            Ok(pool) => pool,
            Err(error) => {
                unsafe {
                    device
                        .device
                        .destroy_descriptor_set_layout(set_layout, None);
                    device.device.destroy_sampler(sampler, None);
                }
                return Err(error);
            }
        };
        let pipeline_layout = match pipeline_layout(&device.device, set_layout) {
            Ok(layout) => layout,
            Err(error) => {
                unsafe {
                    device.device.destroy_descriptor_pool(pool, None);
                    device
                        .device
                        .destroy_descriptor_set_layout(set_layout, None);
                    device.device.destroy_sampler(sampler, None);
                }
                return Err(error);
            }
        };
        let render_pass =
            match resources::color_render_pass(&device.device, vk::Format::B8G8R8A8_UNORM) {
                Ok(pass) => pass,
                Err(error) => {
                    unsafe {
                        device.device.destroy_pipeline_layout(pipeline_layout, None);
                        device.device.destroy_descriptor_pool(pool, None);
                        device
                            .device
                            .destroy_descriptor_set_layout(set_layout, None);
                        device.device.destroy_sampler(sampler, None);
                    }
                    return Err(error);
                }
            };
        let pipeline = match graphics_pipeline(
            &device.device,
            pipeline_layout,
            render_pass,
            compiled_shaders::TEXTURE_VERT,
            compiled_shaders::TEXTURE_FRAG,
            false,
            true,
            &[],
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                unsafe {
                    device.device.destroy_render_pass(render_pass, None);
                    device.device.destroy_pipeline_layout(pipeline_layout, None);
                    device.device.destroy_descriptor_pool(pool, None);
                    device
                        .device
                        .destroy_descriptor_set_layout(set_layout, None);
                    device.device.destroy_sampler(sampler, None);
                }
                return Err(error);
            }
        };
        let vertices = match quad_vertices(device) {
            Ok(vertices) => vertices,
            Err(error) => {
                unsafe {
                    device.device.destroy_pipeline(pipeline, None);
                    device.device.destroy_render_pass(render_pass, None);
                    device.device.destroy_pipeline_layout(pipeline_layout, None);
                    device.device.destroy_descriptor_pool(pool, None);
                    device
                        .device
                        .destroy_descriptor_set_layout(set_layout, None);
                    device.device.destroy_sampler(sampler, None);
                }
                return Err(error);
            }
        };
        let effects = match EffectGpu::new(device, render_pass, pipeline_layout) {
            Ok(effects) => effects,
            Err(error) => {
                unsafe {
                    device.device.destroy_pipeline(pipeline, None);
                    device.device.destroy_render_pass(render_pass, None);
                    device.device.destroy_pipeline_layout(pipeline_layout, None);
                    device.device.destroy_descriptor_pool(pool, None);
                    device
                        .device
                        .destroy_descriptor_set_layout(set_layout, None);
                    device.device.destroy_sampler(sampler, None);
                }
                return Err(error);
            }
        };
        Ok(Self {
            device: device.clone(),
            pipeline,
            pipeline_layout,
            render_pass,
            set_layout,
            pool,
            sampler,
            vertices,
            effects,
            texture_pipelines: HashMap::new(),
            blend_image: None,
            active: vk::Format::B8G8R8A8_UNORM,
            extra: HashMap::new(),
        })
    }

    /// Select pipelines whose render pass matches the target. The 8-bit pass is the one
    /// built in [`Self::new`]. A 10-bit attachment builds its pass on first use.
    pub(super) fn use_attachment(&mut self, format: vk::Format) -> Result<(), String> {
        if format == vk::Format::B8G8R8A8_UNORM || format == vk::Format::UNDEFINED {
            self.active = vk::Format::B8G8R8A8_UNORM;
            return Ok(());
        }
        if !self.extra.contains_key(&format) {
            let pass = ExtraPass::new(&self.device, self.pipeline_layout, format)?;
            self.extra.insert(format, pass);
        }
        self.active = format;
        Ok(())
    }

    pub(super) fn active_render_pass(&self) -> vk::RenderPass {
        if self.active == vk::Format::B8G8R8A8_UNORM {
            self.render_pass
        } else {
            self.extra[&self.active].render_pass
        }
    }

    /// RGBA16F target for the linear blend pass. The active render pass must be that format.
    pub(super) fn ensure_blend_image(
        &mut self,
        width: i32,
        height: i32,
    ) -> Result<(vk::Image, vk::ImageView, vk::Framebuffer), String> {
        let stale = self
            .blend_image
            .as_ref()
            .is_none_or(|image| image.width != width || image.height != height);
        if stale {
            self.blend_image = Some(resources::BlendImage::new(
                &self.device,
                width,
                height,
                self.active_render_pass(),
            )?);
        }
        let image = self.blend_image.as_ref().unwrap();
        Ok((image.image(), image.view(), image.framebuffer()))
    }

    pub(super) fn blend_layout(&self) -> Option<&std::cell::Cell<vk::ImageLayout>> {
        self.blend_image.as_ref().map(|image| &image.layout)
    }

    pub(super) fn pipeline(&self) -> vk::Pipeline {
        if self.active == vk::Format::B8G8R8A8_UNORM {
            self.pipeline
        } else {
            self.extra[&self.active].identity
        }
    }

    /// Identity uses the pipeline built for the active attachment. Other keys, including
    /// color transforms, are created on first use from the same shader.
    pub(super) fn pipeline_for(&mut self, key: TexturePipelineKey) -> Result<vk::Pipeline, String> {
        if !key.is_admitted() {
            return Err("texture pipeline key is outside the finite color space".to_string());
        }
        if self.active == vk::Format::B8G8R8A8_UNORM {
            if key == TexturePipelineKey::identity() {
                return Ok(self.pipeline);
            }
            if let Some(pipeline) = self.texture_pipelines.get(&key).copied() {
                return Ok(pipeline);
            }
            let pipeline = graphics_pipeline(
                &self.device.device,
                self.pipeline_layout,
                self.render_pass,
                compiled_shaders::TEXTURE_VERT,
                compiled_shaders::TEXTURE_FRAG,
                false,
                key.blend,
                &key.specialization_constants(),
            )?;
            self.texture_pipelines.insert(key, pipeline);
            return Ok(pipeline);
        }
        if key == TexturePipelineKey::identity() {
            return Ok(self.extra[&self.active].identity);
        }
        if let Some(pipeline) = self.extra[&self.active].textures.get(&key).copied() {
            return Ok(pipeline);
        }
        let render_pass = self.extra[&self.active].render_pass;
        let pipeline = graphics_pipeline(
            &self.device.device,
            self.pipeline_layout,
            render_pass,
            compiled_shaders::TEXTURE_VERT,
            compiled_shaders::TEXTURE_FRAG,
            false,
            key.blend,
            &key.specialization_constants(),
        )?;
        self.extra
            .get_mut(&self.active)
            .unwrap()
            .textures
            .insert(key, pipeline);
        Ok(pipeline)
    }

    pub(super) fn pipeline_layout(&self) -> vk::PipelineLayout {
        self.pipeline_layout
    }

    pub(super) fn set_layout(&self) -> vk::DescriptorSetLayout {
        self.set_layout
    }

    pub(super) fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    pub(super) fn vertices(&self) -> vk::Buffer {
        self.vertices.buffer()
    }

    pub(super) fn allocate_set(&mut self) -> Result<vk::DescriptorSet, String> {
        self.allocate_set_for(self.set_layout)
    }

    pub(super) fn allocate_set_for(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet, String> {
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.pool)
            .set_layouts(&layouts);
        unsafe { self.device.device.allocate_descriptor_sets(&info) }
            .map_err(|error| format!("vulkan descriptor allocation failed: {error}"))?
            .into_iter()
            .next()
            .ok_or_else(|| "vulkan allocated no descriptor set".to_string())
    }

    fn active_effects(&self) -> &EffectGpu {
        if self.active == vk::Format::B8G8R8A8_UNORM {
            &self.effects
        } else {
            &self.extra[&self.active].effects
        }
    }

    pub(super) fn border_pipeline(&self) -> vk::Pipeline {
        self.active_effects().border_pipeline
    }

    pub(super) fn border_layout(&self) -> vk::PipelineLayout {
        self.active_effects().border_layout
    }

    pub(super) fn border_set_layout(&self) -> vk::DescriptorSetLayout {
        self.active_effects().border_set_layout
    }

    pub(super) fn shadow_pipeline(&self) -> vk::Pipeline {
        self.active_effects().shadow_pipeline
    }

    pub(super) fn shadow_layout(&self) -> vk::PipelineLayout {
        self.active_effects().shadow_layout
    }

    pub(super) fn shadow_set_layout(&self) -> vk::DescriptorSetLayout {
        self.active_effects().shadow_set_layout
    }

    pub(super) fn blur_pipeline(&self) -> vk::Pipeline {
        self.active_effects().blur_pipeline
    }

    pub(super) fn dim_texture(&self) -> Rc<dyn Texture> {
        self.active_effects().dim.clone()
    }

    pub(super) fn reset_descriptors(&mut self) -> Result<(), String> {
        if self.pool == vk::DescriptorPool::null() {
            self.pool = descriptor_pool(&self.device.device)?;
        }
        unsafe {
            self.device
                .device
                .reset_descriptor_pool(self.pool, vk::DescriptorPoolResetFlags::empty())
        }
        .map_err(|error| format!("vulkan descriptor reset failed: {error}"))
    }

    /// Hand the pool an in-flight frame still references to that frame.
    ///
    /// The next frame allocates another pool. Resetting this one would free
    /// descriptor sets the GPU is still reading.
    pub(super) fn park_pool(&mut self) -> vk::DescriptorPool {
        std::mem::replace(&mut self.pool, vk::DescriptorPool::null())
    }
}

impl Drop for DrawState {
    fn drop(&mut self) {
        unsafe {
            if let Err(error) = self.device.device.device_wait_idle() {
                log::error!("vulkan wait before destroying the pipeline failed: {error}");
            }
            for pipeline in self.texture_pipelines.values().copied() {
                self.device.device.destroy_pipeline(pipeline, None);
            }
            self.texture_pipelines.clear();
            self.effects.destroy();
            self.device.device.destroy_pipeline(self.pipeline, None);
            self.device
                .device
                .destroy_render_pass(self.render_pass, None);
            self.device
                .device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            if self.pool != vk::DescriptorPool::null() {
                self.device.device.destroy_descriptor_pool(self.pool, None);
            }
            self.device
                .device
                .destroy_descriptor_set_layout(self.set_layout, None);
            self.device.device.destroy_sampler(self.sampler, None);
        }
    }
}

fn set_layout(
    device: &ash::Device,
    sampler: vk::Sampler,
) -> Result<vk::DescriptorSetLayout, String> {
    let sampler_binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .immutable_samplers(std::slice::from_ref(&sampler));
    let uniform_binding = vk::DescriptorSetLayoutBinding::default()
        .binding(1)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT);
    let chroma_binding = vk::DescriptorSetLayoutBinding::default()
        .binding(2)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .immutable_samplers(std::slice::from_ref(&sampler));
    let bindings = [sampler_binding, uniform_binding, chroma_binding];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|error| format!("vulkan descriptor layout failed: {error}"))
}

fn descriptor_pool(device: &ash::Device) -> Result<vk::DescriptorPool, String> {
    let sizes = [
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 2048,
        },
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1024,
        },
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1024)
        .pool_sizes(&sizes);
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|error| format!("vulkan descriptor pool failed: {error}"))
}

fn pipeline_layout(
    device: &ash::Device,
    set_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, String> {
    let info =
        vk::PipelineLayoutCreateInfo::default().set_layouts(std::slice::from_ref(&set_layout));
    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|error| format!("vulkan pipeline layout failed: {error}"))
}

fn graphics_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    vertex_spirv: &[u8],
    fragment_spirv: &[u8],
    position_only: bool,
    blend: bool,
    specialization: &[(u32, u32)],
) -> Result<vk::Pipeline, String> {
    let vertex = shader_module(device, vertex_spirv)?;
    let fragment = match shader_module(device, fragment_spirv) {
        Ok(module) => module,
        Err(error) => {
            unsafe { device.destroy_shader_module(vertex, None) };
            return Err(error);
        }
    };
    let entry = CString::new("main").expect("shader entry point");
    let spec_bytes: Vec<u8> = specialization
        .iter()
        .flat_map(|(_, value)| value.to_le_bytes())
        .collect();
    let spec_entries: Vec<vk::SpecializationMapEntry> = specialization
        .iter()
        .enumerate()
        .map(|(index, (constant_id, _))| vk::SpecializationMapEntry {
            constant_id: *constant_id,
            offset: (index * 4) as u32,
            size: 4,
        })
        .collect();
    let spec = vk::SpecializationInfo::default()
        .map_entries(&spec_entries)
        .data(&spec_bytes);
    let mut fragment_stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::FRAGMENT)
        .module(fragment)
        .name(&entry);
    if !specialization.is_empty() {
        fragment_stage = fragment_stage.specialization_info(&spec);
    }
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex)
            .name(&entry),
        fragment_stage,
    ];
    let binding = vk::VertexInputBindingDescription {
        binding: 0,
        stride: 16,
        input_rate: vk::VertexInputRate::VERTEX,
    };
    let position = vk::VertexInputAttributeDescription {
        location: 0,
        binding: 0,
        format: vk::Format::R32G32_SFLOAT,
        offset: 0,
    };
    let texcoord = vk::VertexInputAttributeDescription {
        location: 1,
        binding: 0,
        format: vk::Format::R32G32_SFLOAT,
        offset: 8,
    };
    let attributes = [position, texcoord];
    let position_attributes = [position];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(std::slice::from_ref(&binding))
        .vertex_attribute_descriptions(if position_only {
            &position_attributes
        } else {
            &attributes
        });
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(blend)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    let blend = vk::PipelineColorBlendStateCreateInfo::default()
        .attachments(std::slice::from_ref(&blend_attachment));
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0);
    let pipelines = unsafe {
        device.create_graphics_pipelines(
            vk::PipelineCache::null(),
            std::slice::from_ref(&info),
            None,
        )
    };
    unsafe {
        device.destroy_shader_module(vertex, None);
        device.destroy_shader_module(fragment, None);
    }
    match pipelines {
        Ok(pipelines) => pipelines
            .into_iter()
            .next()
            .ok_or_else(|| "vulkan created no graphics pipeline".to_string()),
        Err((_, error)) => Err(format!("vulkan graphics pipeline failed: {error}")),
    }
}

fn shader_module(device: &ash::Device, bytes: &[u8]) -> Result<vk::ShaderModule, String> {
    let mut words = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let mut word = [0; 4];
        word.copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    if words.is_empty() {
        return Err("vulkan shader spir-v is empty".to_string());
    }
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    unsafe { device.create_shader_module(&info, None) }
        .map_err(|error| format!("vulkan shader module failed: {error}"))
}

fn quad_vertices(device: &Rc<VulkanDevice>) -> Result<AllocatedBuffer, String> {
    let vertices: [f32; 16] =
        [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    let bytes = unsafe {
        std::slice::from_raw_parts(vertices.as_ptr().cast::<u8>(), std::mem::size_of_val(&vertices))
    };
    let buffer = AllocatedBuffer::create(
        device,
        bytes.len() as u64,
        vk::BufferUsageFlags::VERTEX_BUFFER,
        UsageFlags::UPLOAD,
    )?;
    buffer.write(bytes)?;
    Ok(buffer)
}

struct ExtraPass {
    device: Rc<VulkanDevice>,
    render_pass: vk::RenderPass,
    identity: vk::Pipeline,
    textures: HashMap<TexturePipelineKey, vk::Pipeline>,
    effects: EffectGpu,
}

impl ExtraPass {
    fn new(
        device: &Rc<VulkanDevice>,
        pipeline_layout: vk::PipelineLayout,
        format: vk::Format,
    ) -> Result<Self, String> {
        let render_pass = resources::color_render_pass(&device.device, format)?;
        let identity = match graphics_pipeline(
            &device.device,
            pipeline_layout,
            render_pass,
            compiled_shaders::TEXTURE_VERT,
            compiled_shaders::TEXTURE_FRAG,
            false,
            true,
            &[],
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                unsafe { device.device.destroy_render_pass(render_pass, None) };
                return Err(error);
            }
        };
        let effects = match EffectGpu::new(device, render_pass, pipeline_layout) {
            Ok(effects) => effects,
            Err(error) => {
                unsafe {
                    device.device.destroy_pipeline(identity, None);
                    device.device.destroy_render_pass(render_pass, None);
                }
                return Err(error);
            }
        };
        Ok(Self {
            device: device.clone(),
            render_pass,
            identity,
            textures: HashMap::new(),
            effects,
        })
    }
}

impl Drop for ExtraPass {
    fn drop(&mut self) {
        unsafe {
            for pipeline in self.textures.values().copied() {
                self.device.device.destroy_pipeline(pipeline, None);
            }
            self.textures.clear();
            self.effects.destroy();
            if self.identity != vk::Pipeline::null() {
                self.device.device.destroy_pipeline(self.identity, None);
            }
            if self.render_pass != vk::RenderPass::null() {
                self.device
                    .device
                    .destroy_render_pass(self.render_pass, None);
            }
        }
    }
}

struct EffectGpu {
    device: Rc<VulkanDevice>,
    border_pipeline: vk::Pipeline,
    border_layout: vk::PipelineLayout,
    border_set_layout: vk::DescriptorSetLayout,
    shadow_pipeline: vk::Pipeline,
    shadow_layout: vk::PipelineLayout,
    shadow_set_layout: vk::DescriptorSetLayout,
    blur_pipeline: vk::Pipeline,
    dim: Rc<dyn Texture>,
}

impl EffectGpu {
    fn new(
        device: &Rc<VulkanDevice>,
        render_pass: vk::RenderPass,
        texture_layout: vk::PipelineLayout,
    ) -> Result<Self, String> {
        let (border_pipeline, border_layout, border_set_layout) = uniform_pipeline(
            &device.device,
            render_pass,
            compiled_shaders::BORDER_VERT,
            compiled_shaders::BORDER_FRAG,
        )?;
        let (shadow_pipeline, shadow_layout, shadow_set_layout) = match uniform_pipeline(
            &device.device,
            render_pass,
            compiled_shaders::SHADOW_VERT,
            compiled_shaders::SHADOW_FRAG,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                destroy_uniform(&device.device, border_pipeline, border_layout, border_set_layout);
                return Err(error);
            }
        };
        let blur_pipeline = match graphics_pipeline(
            &device.device,
            texture_layout,
            render_pass,
            compiled_shaders::BLUR_VERT,
            compiled_shaders::BLUR_FRAG,
            false,
            false,
            &[],
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                destroy_uniform(&device.device, shadow_pipeline, shadow_layout, shadow_set_layout);
                destroy_uniform(&device.device, border_pipeline, border_layout, border_set_layout);
                return Err(error);
            }
        };
        let dim = match texture::VulkanTexture::upload_rgba(device, &[0, 0, 0, 255], 1, 1) {
            Ok(texture) => Rc::new(texture) as Rc<dyn Texture>,
            Err(_) => {
                unsafe { device.device.destroy_pipeline(blur_pipeline, None) };
                destroy_uniform(&device.device, shadow_pipeline, shadow_layout, shadow_set_layout);
                destroy_uniform(&device.device, border_pipeline, border_layout, border_set_layout);
                return Err("vulkan dim texture failed".to_string());
            }
        };
        Ok(Self {
            device: device.clone(),
            border_pipeline,
            border_layout,
            border_set_layout,
            shadow_pipeline,
            shadow_layout,
            shadow_set_layout,
            blur_pipeline,
            dim,
        })
    }

    fn destroy(&mut self) {
        unsafe {
            let device = &self.device.device;
            if self.blur_pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(self.blur_pipeline, None);
                self.blur_pipeline = vk::Pipeline::null();
            }
            if self.shadow_pipeline != vk::Pipeline::null() {
                destroy_uniform(
                    device,
                    self.shadow_pipeline,
                    self.shadow_layout,
                    self.shadow_set_layout,
                );
                self.shadow_pipeline = vk::Pipeline::null();
                self.shadow_layout = vk::PipelineLayout::null();
                self.shadow_set_layout = vk::DescriptorSetLayout::null();
            }
            if self.border_pipeline != vk::Pipeline::null() {
                destroy_uniform(
                    device,
                    self.border_pipeline,
                    self.border_layout,
                    self.border_set_layout,
                );
                self.border_pipeline = vk::Pipeline::null();
                self.border_layout = vk::PipelineLayout::null();
                self.border_set_layout = vk::DescriptorSetLayout::null();
            }
        }
    }
}

impl Drop for EffectGpu {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn uniform_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    vertex_spirv: &[u8],
    fragment_spirv: &[u8],
) -> Result<(vk::Pipeline, vk::PipelineLayout, vk::DescriptorSetLayout), String> {
    let set_layout = uniform_set_layout(device)?;
    let layout = match pipeline_layout(device, set_layout) {
        Ok(layout) => layout,
        Err(error) => {
            unsafe { device.destroy_descriptor_set_layout(set_layout, None) };
            return Err(error);
        }
    };
    match graphics_pipeline(
        device,
        layout,
        render_pass,
        vertex_spirv,
        fragment_spirv,
        true,
        true,
        &[],
    ) {
        Ok(pipeline) => Ok((pipeline, layout, set_layout)),
        Err(error) => {
            unsafe {
                device.destroy_pipeline_layout(layout, None);
                device.destroy_descriptor_set_layout(set_layout, None);
            }
            Err(error)
        }
    }
}

fn uniform_set_layout(device: &ash::Device) -> Result<vk::DescriptorSetLayout, String> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT);
    let info =
        vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding));
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|error| format!("vulkan uniform layout failed: {error}"))
}

fn destroy_uniform(
    device: &ash::Device,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
) {
    unsafe {
        device.destroy_pipeline(pipeline, None);
        device.destroy_pipeline_layout(layout, None);
        device.destroy_descriptor_set_layout(set_layout, None);
    }
}
