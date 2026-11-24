//! Two-sample stereo uses native format features beyond WebGPU's 1 and 4.
use super::{log, Gpu, StereoTarget};
use ash::vk;
use std::sync::atomic::{AtomicBool, Ordering};

static TWO: AtomicBool = AtomicBool::new(false);

/// True while the current OpenXR swapchain can resolve a two-sample scene.
pub fn stereo_msaa_two() -> bool {
    TWO.load(Ordering::Relaxed)
}

pub(super) fn reset() {
    TWO.store(false, Ordering::Relaxed);
}

pub(super) fn select(gpu: &Gpu, target: &StereoTarget, format: wgpu::TextureFormat) {
    let required = wgpu::Features::MULTIVIEW
        | wgpu::Features::MULTISAMPLE_ARRAY
        | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    let features = gpu.device.features();
    let mut supported = features.contains(required)
        && gpu.device.limits().max_multiview_view_count >= 2
        && format.is_srgb();
    log(&format!(
        "msaa-2: multiview={} multisample_array={} adapter_specific_formats={} views={}",
        features.contains(wgpu::Features::MULTIVIEW),
        features.contains(wgpu::Features::MULTISAMPLE_ARRAY),
        features.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
        gpu.device.limits().max_multiview_view_count,
    ));
    // SAFETY: read-only physical-device queries through the live provider device.
    let Some(hal) = (unsafe { gpu.device.as_hal::<wgpu::hal::api::Vulkan>() }) else {
        reset();
        return;
    };
    let color = vk::ImageUsageFlags::COLOR_ATTACHMENT;
    let depth = vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
    let transient = vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
    let sampled = vk::ImageUsageFlags::SAMPLED;
    // The renderer also constructs HDR scratch targets and mono pipelines
    // before drawing direct stereo. A retained depth target can be sampled.
    for (wformat, vformat, uses) in [
        (format, vk::Format::from_raw(target.format as i32), color),
        (
            wgpu::TextureFormat::Rgba16Float,
            vk::Format::R16G16B16A16_SFLOAT,
            color,
        ),
        (
            wgpu::TextureFormat::Depth32Float,
            vk::Format::D32_SFLOAT,
            depth,
        ),
    ] {
        let flags = gpu.adapter.get_texture_format_features(wformat);
        let mut needed = wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2;
        if uses == color {
            needed |= wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE;
        }
        supported &= flags
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            && flags.flags.contains(needed);
        log(&format!(
            "msaa-2: wgpu {wformat:?} samples={:?} flags={:?} usages={:?}",
            flags.flags.supported_sample_counts(),
            flags.flags,
            flags.allowed_usages,
        ));
        for (usage, layers) in [(uses, 2), (uses | transient, 1), (uses | sampled, 2)] {
            if usage.contains(sampled) && uses != depth {
                continue;
            }
            // SAFETY: known 2D attachment parameters and the device's own handles.
            let props = unsafe {
                hal.shared_instance()
                    .raw_instance()
                    .get_physical_device_image_format_properties(
                        hal.raw_physical_device(),
                        vformat,
                        vk::ImageType::TYPE_2D,
                        vk::ImageTiling::OPTIMAL,
                        usage,
                        vk::ImageCreateFlags::empty(),
                    )
            };
            let ok = props.as_ref().is_ok_and(|p| {
                p.sample_counts.contains(vk::SampleCountFlags::TYPE_2)
                    && p.max_array_layers >= layers
                    && p.max_extent.width >= target.width
                    && p.max_extent.height >= target.height
            });
            supported &= ok;
            log(&format!(
                "msaa-2: Vulkan {wformat:?} usage={:#x} {}x{}x{layers} sample_mask={:#x} supported={ok}",
                usage.as_raw(), target.width, target.height,
                props.map_or(0, |p| p.sample_counts.as_raw()),
            ));
        }
    }
    TWO.store(supported, Ordering::Relaxed);
    log(if supported {
        "msaa-2: native two-sample stereo available"
    } else {
        "msaa-2: unsupported eye target or device feature; keeping the game's samples"
    });
}
