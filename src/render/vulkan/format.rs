use std::collections::HashMap;

use {ash::vk, drm::buffer::DrmFourcc};

use crate::{
    drm::{client_format, output_format, renderer_query_fourccs, FormatKind},
    render::{FormatSupport, RendererCapabilities},
};

use super::instance::VulkanInstance;

const INVALID_MODIFIER: u64 = 0x00ff_ffff_ffff_ffff;

#[derive(Clone, Copy, Debug)]
pub(super) struct MaxExtent {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ModifierSupport {
    pub planes: u32,
    pub disjoint: bool,
    pub sample_max: Option<MaxExtent>,
    pub render_max: Option<MaxExtent>,
}

#[derive(Clone, Debug)]
pub(super) struct FormatSet {
    pub by_fourcc: HashMap<DrmFourcc, HashMap<u64, ModifierSupport>>,
    pub capabilities: RendererCapabilities,
}

pub(super) fn query_formats(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
) -> Result<FormatSet, String> {
    let ycbcr = sampler_ycbcr_conversion(instance, physical_device);
    let mut by_fourcc = HashMap::new();
    for fourcc in renderer_query_fourccs() {
        if !ycbcr
            && client_format(fourcc)
                .is_some_and(|format| matches!(format.kind, FormatKind::Yuv { .. }))
        {
            continue;
        }
        let modifiers = query_fourcc(instance, physical_device, fourcc)?;
        if !modifiers.is_empty() {
            by_fourcc.insert(fourcc, modifiers);
        }
    }
    let capabilities = capabilities_from(&by_fourcc);
    Ok(FormatSet { by_fourcc, capabilities })
}

fn query_fourcc(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
    fourcc: DrmFourcc,
) -> Result<HashMap<u64, ModifierSupport>, String> {
    let vk_format = vk_format(fourcc);
    let count = modifier_count(instance, physical_device, vk_format);
    if count == 0 {
        return Ok(HashMap::new());
    }
    let listed = modifier_properties(instance, physical_device, vk_format, count)?;
    let mut modifiers = HashMap::new();
    for modifier in listed {
        if modifier.drm_format_modifier == INVALID_MODIFIER
            || modifier.drm_format_modifier_plane_count == 0
        {
            continue;
        }
        let sample_max = image_extent(
            instance,
            physical_device,
            vk_format,
            &modifier,
            vk::FormatFeatureFlags::SAMPLED_IMAGE,
            vk::ImageUsageFlags::SAMPLED,
        )?;
        // Jay advertises a modifier as a framebuffer only when it can also be copied.
        let render_max = image_extent(
            instance,
            physical_device,
            vk_format,
            &modifier,
            vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::TRANSFER_SRC,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        if sample_max.is_none() && render_max.is_none() {
            continue;
        }
        modifiers.insert(
            modifier.drm_format_modifier,
            ModifierSupport {
                planes: modifier.drm_format_modifier_plane_count,
                disjoint: modifier
                    .drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::DISJOINT),
                sample_max,
                render_max,
            },
        );
    }
    Ok(modifiers)
}

fn modifier_count(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
) -> u32 {
    let mut modifiers = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut properties = vk::FormatProperties2::default().push_next(&mut modifiers);
    unsafe {
        instance.instance.get_physical_device_format_properties2(
            physical_device,
            format,
            &mut properties,
        );
    }
    modifiers.drm_format_modifier_count
}

fn modifier_properties(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
    count: u32,
) -> Result<Vec<vk::DrmFormatModifierPropertiesEXT>, String> {
    let mut listed = vec![vk::DrmFormatModifierPropertiesEXT::default(); count as usize];
    let reported = {
        let mut modifiers = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut listed);
        let mut properties = vk::FormatProperties2::default().push_next(&mut modifiers);
        unsafe {
            instance.instance.get_physical_device_format_properties2(
                physical_device,
                format,
                &mut properties,
            );
        }
        modifiers.drm_format_modifier_count
    };
    if reported as usize > listed.len() {
        return Err(format!("vulkan reported {count} DRM modifiers and then {reported}"));
    }
    listed.truncate(reported as usize);
    Ok(listed)
}

fn image_extent(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
    modifier: &vk::DrmFormatModifierPropertiesEXT,
    features: vk::FormatFeatureFlags,
    usage: vk::ImageUsageFlags,
) -> Result<Option<MaxExtent>, String> {
    if !modifier
        .drm_format_modifier_tiling_features
        .contains(features)
    {
        return Ok(None);
    }
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier.drm_format_modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let plane_views = yuv_view_formats_vk(format);
    let listed = plane_views.unwrap_or([format, format, format]);
    let mut format_list = vk::ImageFormatListCreateInfo::default().view_formats(&listed);
    let mut format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .ty(vk::ImageType::TYPE_2D)
        .format(format)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .push_next(&mut external_info)
        .push_next(&mut modifier_info);
    if plane_views.is_some() {
        format_info = format_info
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE)
            .push_next(&mut format_list);
    }
    let mut external_properties = vk::ExternalImageFormatProperties::default();
    let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external_properties);
    let result = unsafe {
        instance
            .instance
            .get_physical_device_image_format_properties2(
                physical_device,
                &format_info,
                &mut properties,
            )
    };
    if let Err(error) = result {
        return match error {
            vk::Result::ERROR_FORMAT_NOT_SUPPORTED => Ok(None),
            other => Err(format!("failed to query vulkan image format properties: {other}")),
        };
    }
    let extent = properties.image_format_properties.max_extent;
    let importable = external_properties
        .external_memory_properties
        .external_memory_features
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE);
    if !importable {
        return Ok(None);
    }
    if extent.width == 0 || extent.height == 0 {
        return Ok(None);
    }
    Ok(Some(MaxExtent { width: extent.width, height: extent.height }))
}

fn capabilities_from(
    formats: &HashMap<DrmFourcc, HashMap<u64, ModifierSupport>>,
) -> RendererCapabilities {
    let mut fourccs: Vec<DrmFourcc> = formats.keys().copied().collect();
    fourccs.sort_by_key(|fourcc| format!("{fourcc:?}"));
    let sample_fourccs: Vec<_> = fourccs
        .iter()
        .copied()
        .filter(|fourcc| client_format(*fourcc).is_some())
        .collect();
    let render_fourccs: Vec<_> = fourccs
        .iter()
        .copied()
        .filter(|fourcc| output_format(*fourcc).is_some())
        .collect();
    let sample_formats = listed(formats, &sample_fourccs, |modifier| modifier.sample_max.is_some());
    let render_formats = listed(formats, &render_fourccs, |modifier| modifier.render_max.is_some());
    RendererCapabilities { sample_formats, render_formats, explicit_sync: true }
}

fn listed(
    formats: &HashMap<DrmFourcc, HashMap<u64, ModifierSupport>>,
    fourccs: &[DrmFourcc],
    accept: impl Fn(&ModifierSupport) -> bool,
) -> Vec<FormatSupport> {
    fourccs
        .iter()
        .filter_map(|fourcc| {
            let mut modifiers: Vec<u64> = formats[fourcc]
                .iter()
                .filter(|(_, modifier)| accept(modifier))
                .map(|(modifier, _)| *modifier)
                .collect();
            if modifiers.is_empty() {
                return None;
            }
            modifiers.sort_unstable();
            Some(FormatSupport { fourcc: *fourcc, modifiers })
        })
        .collect()
}

/// Expand a packed 10-bit readback row into the 8-bit BGRA bytes capture already writes.
pub(super) fn expand_packed_readback(format: vk::Format, opaque: bool, bytes: &mut [u8]) {
    let low_is_blue = match format {
        vk::Format::A2R10G10B10_UNORM_PACK32 => true,
        vk::Format::A2B10G10R10_UNORM_PACK32 => false,
        _ => return,
    };
    for chunk in bytes.chunks_exact_mut(4) {
        let pixel = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let c0 = pixel & 0x3ff;
        let c1 = (pixel >> 10) & 0x3ff;
        let c2 = (pixel >> 20) & 0x3ff;
        let alpha = (pixel >> 30) & 0x3;
        let (blue, green, red) = if low_is_blue { (c0, c1, c2) } else { (c2, c1, c0) };
        chunk[0] = scale_10bit(blue);
        chunk[1] = scale_10bit(green);
        chunk[2] = scale_10bit(red);
        chunk[3] = if opaque { 255 } else { scale_2bit(alpha) };
    }
}

fn scale_10bit(value: u32) -> u8 {
    ((value * 255 + 511) / 1023) as u8
}

fn scale_2bit(value: u32) -> u8 {
    ((value * 255 + 1) / 3) as u8
}

pub(super) fn vk_format(fourcc: DrmFourcc) -> vk::Format {
    match fourcc {
        DrmFourcc::Argb8888 | DrmFourcc::Xrgb8888 => vk::Format::B8G8R8A8_UNORM,
        // Low 10 bits are blue. The X variants swizzle alpha to ONE on the view.
        DrmFourcc::Argb2101010 | DrmFourcc::Xrgb2101010 => vk::Format::A2R10G10B10_UNORM_PACK32,
        // Low 10 bits are red.
        DrmFourcc::Abgr2101010 | DrmFourcc::Xbgr2101010 => vk::Format::A2B10G10R10_UNORM_PACK32,
        // Low 16 bits are red. The X variant swizzles alpha to ONE on the view.
        DrmFourcc::Abgr16161616f | DrmFourcc::Xbgr16161616f => vk::Format::R16G16B16A16_SFLOAT,
        DrmFourcc::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
        DrmFourcc::P010 => vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
        _ => vk::Format::UNDEFINED,
    }
}

/// Image format plus the two single-plane view formats. Plane 0 is luma.
pub(super) fn yuv_view_formats(fourcc: DrmFourcc) -> Option<(vk::Format, vk::Format, vk::Format)> {
    match fourcc {
        DrmFourcc::Nv12 => Some((
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
            vk::Format::R8_UNORM,
            vk::Format::R8G8_UNORM,
        )),
        DrmFourcc::P010 => Some((
            vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
            vk::Format::R16_UNORM,
            vk::Format::R16G16_UNORM,
        )),
        _ => None,
    }
}

fn yuv_view_formats_vk(format: vk::Format) -> Option<[vk::Format; 3]> {
    match format {
        vk::Format::G8_B8R8_2PLANE_420_UNORM => {
            Some([format, vk::Format::R8_UNORM, vk::Format::R8G8_UNORM])
        }
        vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16 => {
            Some([format, vk::Format::R16_UNORM, vk::Format::R16G16_UNORM])
        }
        _ => None,
    }
}

pub(super) fn sampler_ycbcr_conversion(
    instance: &VulkanInstance,
    physical_device: vk::PhysicalDevice,
) -> bool {
    let mut ycbcr = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut ycbcr);
    unsafe {
        instance
            .instance
            .get_physical_device_features2(physical_device, &mut features);
    }
    ycbcr.sampler_ycbcr_conversion != 0
}

#[cfg(test)]
mod tests {
    use {ash::vk, drm::buffer::DrmFourcc};

    use super::{expand_packed_readback, vk_format};

    #[test]
    fn ten_bit_formats_use_packed_unorm_with_the_drm_channel_order() {
        assert_eq!(vk_format(DrmFourcc::Argb2101010), vk::Format::A2R10G10B10_UNORM_PACK32);
        assert_eq!(vk_format(DrmFourcc::Xrgb2101010), vk_format(DrmFourcc::Argb2101010));
        assert_eq!(vk_format(DrmFourcc::Abgr2101010), vk::Format::A2B10G10R10_UNORM_PACK32);
        assert_eq!(vk_format(DrmFourcc::Xbgr2101010), vk_format(DrmFourcc::Abgr2101010));
        assert_ne!(vk_format(DrmFourcc::Argb2101010), vk_format(DrmFourcc::Abgr2101010));
        assert_eq!(vk_format(DrmFourcc::Abgr16161616f), vk::Format::R16G16B16A16_SFLOAT);
        assert_eq!(vk_format(DrmFourcc::Xbgr16161616f), vk_format(DrmFourcc::Abgr16161616f));
        assert_eq!(vk_format(DrmFourcc::Nv12), vk::Format::G8_B8R8_2PLANE_420_UNORM);
        assert_eq!(
            vk_format(DrmFourcc::P010),
            vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
        );
    }

    #[test]
    fn packed_ten_bit_readback_becomes_bgra8888() {
        let blue = 1023u32 | (3 << 30);
        let mut xrgb = blue.to_le_bytes();
        expand_packed_readback(vk::Format::A2R10G10B10_UNORM_PACK32, true, &mut xrgb);
        assert_eq!(xrgb, [255, 0, 0, 255]);

        let red = 1023u32;
        let mut xbgr = red.to_le_bytes();
        expand_packed_readback(vk::Format::A2B10G10R10_UNORM_PACK32, true, &mut xbgr);
        assert_eq!(xbgr, [0, 0, 255, 255]);

        let mut straight = (1u32 << 30).to_le_bytes();
        expand_packed_readback(vk::Format::A2R10G10B10_UNORM_PACK32, false, &mut straight);
        assert_eq!(straight[3], 85);

        let mut eight = [4u8, 5, 6, 7];
        expand_packed_readback(vk::Format::B8G8R8A8_UNORM, true, &mut eight);
        assert_eq!(eight, [4, 5, 6, 7]);
    }
}
