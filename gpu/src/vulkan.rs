//! A device and a render target from outside the module, on Vulkan
//! (Android, and a PC's OpenXR runtime on Windows): the seams through which a separately loaded artifact that owns
//! the Vulkan objects reaches the module. The OpenXR artifact (`gpu/openxr`,
//! `docs/quest.md`) uses them: the headset's runtime makes the instance and
//! the device, and lends each frame's swapchain image.
//!
//! - [`VulkanProvider`] (`gpu_vulkan_provider`, before `gpu_prepare` or
//!   `gpu_load`): the module makes its device as it always does, with the
//!   extensions and features wgpu wants, but the provider's hooks make the
//!   Vulkan instance, choose the physical device and make the device.
//! - [`StereoTarget`] (`gpu_render_stereo`): a two-layer image of that device
//!   that a canvas draws one stereo frame into; `gpu_read_stereo` reads a
//!   layer of the last one back.
//! - `gpu_input_xr`, `gpu_headset` and `gpu_debug_switches`, and what a
//!   headset host asks of a canvas (`gpu_vulkan_format`, `gpu_xr_focus`,
//!   `gpu_xr_stereo_mode`).
#![allow(unsafe_code)]

mod msaa;
pub use msaa::stereo_msaa_two;

use crate::{native, DeviceFailure, DeviceFailureKind, Gpu, XrStereo};
use ash::vk::{self, Handle as _};
use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void};
#[cfg(target_os = "android")]
use std::ffi::{CStr, CString};
use std::sync::Mutex;

/// The hooks that make the module's Vulkan objects, in C layout. Each
/// returns a `VkResult` (0 for success; the provider logs its own detail).
/// Handles are passed as `u64`.
#[repr(C)]
pub struct VulkanProvider {
    /// Passed back to every hook.
    pub context: *mut c_void,
    /// The Vulkan version the instance is made at (`VK_MAKE_API_VERSION`).
    pub api_version: u32,
    /// The views a multiview render pass draws (2 for stereo), granted with
    /// `MULTIVIEW` where the GPU has it, and `MULTISAMPLE_ARRAY` with it where
    /// the GPU has that; 0 for none.
    pub multiview_views: u32,
    /// Make the instance from `info`, a `VkInstanceCreateInfo*`, with the
    /// module's `vkGetInstanceProcAddr`, and write it to `instance`.
    pub create_instance: unsafe extern "C" fn(
        context: *mut c_void,
        get_instance_proc_addr: *const c_void,
        info: *const c_void,
        instance: *mut u64,
    ) -> c_int,
    /// Write the physical device the module uses on `instance`.
    pub physical_device:
        unsafe extern "C" fn(context: *mut c_void, instance: u64, physical: *mut u64) -> c_int,
    /// Make the device from `info`, a `VkDeviceCreateInfo*` asking for one
    /// queue of family 0, and write it to `device`.
    pub create_device: unsafe extern "C" fn(
        context: *mut c_void,
        get_instance_proc_addr: *const c_void,
        physical: u64,
        info: *const c_void,
        device: *mut u64,
    ) -> c_int,
    /// The module wrapped the device, whose queue is family 0, index 0:
    /// `summary` (UTF-8, `summary_len` bytes) names it and what was granted.
    /// Anything but 0 refuses the device.
    pub device_made: unsafe extern "C" fn(
        context: *mut c_void,
        instance: u64,
        physical: u64,
        device: u64,
        summary: *const u8,
        summary_len: usize,
    ) -> c_int,
}

struct Provider(*const VulkanProvider);
// SAFETY: the provider's contract makes it valid for the process, on any thread.
unsafe impl Send for Provider {}
static PROVIDER: Mutex<Option<Provider>> = Mutex::new(None);

/// Make the next device through `provider`'s hooks; null makes it as usual.
///
/// # Safety
/// `provider`, when non-null, and its hooks stay valid for the process and
/// may be called from any thread.
pub unsafe fn set_provider(provider: *const VulkanProvider) {
    *PROVIDER.lock().unwrap_or_else(|e| e.into_inner()) =
        (!provider.is_null()).then_some(Provider(provider));
}

/// The device made through the provider, or None when there is none.
pub(crate) fn create_gpu(
    requirements: &[crate::grants::Requirements],
) -> Option<Result<Gpu, DeviceFailure>> {
    let provider = PROVIDER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .0;
    // SAFETY: `set_provider`'s contract.
    Some(make(unsafe { &*provider }, requirements))
}

fn failure(stage: &str, e: impl std::fmt::Display) -> DeviceFailure {
    DeviceFailure::new(
        DeviceFailureKind::NoDevice,
        format!("Vulkan provider, {stage}: {e}"),
    )
}

fn hook(stage: &str, code: c_int) -> Result<(), DeviceFailure> {
    match code {
        0 => Ok(()),
        code => Err(failure(stage, vk::Result::from_raw(code))),
    }
}

fn make(
    p: &VulkanProvider,
    requirements: &[crate::grants::Requirements],
) -> Result<Gpu, DeviceFailure> {
    let api = p.api_version;
    // SAFETY: the system's libvulkan.
    let vk_entry = unsafe { ash::Entry::load() }.map_err(|e| failure("libvulkan", e))?;
    let flags = crate::device::instance_flags()
        - wgpu::InstanceFlags::DEBUG
        - wgpu::InstanceFlags::VALIDATION;
    let extensions = wgpu::hal::vulkan::Instance::desired_extensions(&vk_entry, api, flags)
        .map_err(|e| failure("instance extensions", e))?;
    let names: Vec<*const c_char> = extensions.iter().map(|e| e.as_ptr()).collect();
    let app = vk::ApplicationInfo::default()
        .application_name(c"exact")
        .engine_name(c"exact")
        .api_version(api);
    let create = vk::InstanceCreateInfo::default()
        .application_info(&app)
        .enabled_extension_names(&names);
    let get_proc = vk_entry.static_fn().get_instance_proc_addr as *const c_void;
    let mut raw_instance = 0u64;
    // SAFETY: the hook's contract; `create` and what it points at outlive the call.
    hook("instance", unsafe {
        (p.create_instance)(
            p.context,
            get_proc,
            (&raw const create).cast(),
            &mut raw_instance,
        )
    })?;
    let vk_handle = vk::Instance::from_raw(raw_instance);
    // SAFETY: the instance the provider just made with this entry.
    let ash_instance = unsafe { ash::Instance::load(vk_entry.static_fn(), vk_handle) };
    // SAFETY: made from `vk_entry` with `extensions` at `api`; never
    // destroyed (the provider holds it for the process).
    let hal_instance = unsafe {
        wgpu::hal::vulkan::Instance::from_raw(
            vk_entry,
            ash_instance.clone(),
            api,
            sdk_version(),
            None,
            extensions,
            flags,
            Default::default(),
            false,
            Some(Box::new(|| {})),
        )
    }
    .map_err(|e| failure("wgpu instance", e))?;
    // Exact's Android shaders opt into Adreno's f16 arithmetic
    // (`device.rs`); a PC's device keeps the window's f32.
    #[cfg(target_os = "android")]
    hal_instance.enable_f16_arithmetic();

    let mut raw_physical = 0u64;
    // SAFETY: the hook's contract, on the provider's own instance.
    hook("physical device", unsafe {
        (p.physical_device)(p.context, raw_instance, &mut raw_physical)
    })?;
    let exposed = hal_instance
        .expose_adapter(vk::PhysicalDevice::from_raw(raw_physical))
        .ok_or_else(|| failure("adapter", "wgpu cannot use the provider's GPU"))?;
    crate::tier::choose(&exposed.info);
    let grant = crate::grants::negotiate(
        exposed.capabilities.limits.clone(),
        exposed.features,
        requirements,
    );
    let mut features = grant.features;
    let mut limits = grant.limits;
    if crate::debug_switch("no-first-instance") {
        features.remove(wgpu::Features::INDIRECT_FIRST_INSTANCE);
    }
    // wgpu's default view count is 0, which refuses a multiview pass.
    let views = exposed.capabilities.limits.max_multiview_view_count;
    if p.multiview_views > 0
        && exposed.features.contains(wgpu::Features::MULTIVIEW)
        && views >= p.multiview_views
    {
        features |= wgpu::Features::MULTIVIEW;
        limits.max_multiview_view_count = p.multiview_views;
        if exposed.features.contains(wgpu::Features::MULTISAMPLE_ARRAY) {
            features |= wgpu::Features::MULTISAMPLE_ARRAY;
        }
    }
    if features.contains(wgpu::Features::MULTISAMPLE_ARRAY) {
        features |= exposed.features & wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    }
    // A phone's small first blocks (`device.rs`, `memory_hints`); a PC's
    // device takes the desktop's.
    let memory_hints = if cfg!(target_os = "android") {
        wgpu::MemoryHints::MemoryUsage
    } else {
        wgpu::MemoryHints::Performance
    };
    let device_extensions = exposed.adapter.required_device_extensions(features);
    let mut device_features = exposed
        .adapter
        .physical_device_features(&device_extensions, features);
    let extension_names: Vec<*const c_char> =
        device_extensions.iter().map(|e| e.as_ptr()).collect();
    // wgpu-hal opens queue family 0, index 0.
    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(0)
        .queue_priorities(&priorities)];
    let create = device_features.add_to_device_create(
        vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extension_names),
    );
    let mut raw_device = 0u64;
    // SAFETY: the hook's contract; `create` and what it points at outlive the call.
    hook("device", unsafe {
        (p.create_device)(
            p.context,
            get_proc,
            raw_physical,
            (&raw const create).cast(),
            &mut raw_device,
        )
    })?;
    // SAFETY: the device the provider just made from this instance.
    let ash_device =
        unsafe { ash::Device::load(ash_instance.fp_v1_0(), vk::Device::from_raw(raw_device)) };
    // SAFETY: made from this adapter with these extensions and features, on
    // family 0 queue 0; never destroyed.
    let open = unsafe {
        exposed.adapter.device_from_raw(
            ash_device,
            Some(Box::new(|| {})),
            &device_extensions,
            features,
            &limits,
            &memory_hints,
            0,
            0,
        )
    }
    .map_err(|e| failure("wgpu device", e))?;
    // The wgpu instance carries the module's instance flags, as every other
    // host's does (`native.rs`). `wgpu::Instance::from_hal` would give it
    // wgpu-core's defaults instead, among them `VALIDATION_INDIRECT_CALL`:
    // every indirect draw then goes through wgpu's validation pass, and on a
    // Quest 3 that pass zeroed each of the game renderer's culled draws
    // (`docs/quest.md`, "What the headset drew white"). So the instance is
    // made with no backend of its own (no second VkInstance) and given the
    // provider's adapter, which keeps the provider's VkInstance alive.
    let wgpu_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::empty(),
        flags,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    drop(hal_instance);
    // SAFETY: the adapter and device above, made from the provider's instance.
    let adapter = unsafe { wgpu_instance.create_adapter_from_hal(exposed) };
    let (device, queue) = unsafe {
        adapter.create_device_from_hal(
            open,
            &wgpu::DeviceDescriptor {
                label: Some("exact external vulkan"),
                required_features: features,
                required_limits: limits.clone(),
                memory_hints,
                ..Default::default()
            },
        )
    }
    .map_err(|e| failure("wgpu device", e))?;
    let summary = format!(
        "device {} (Vulkan {}.{}, driver {}), multiview {} (views {} of {views}), multisample array {}, indirect first instance {}, unclipped depth {}",
        adapter.get_info().name,
        vk::api_version_major(api),
        vk::api_version_minor(api),
        adapter.get_info().driver_info,
        features.contains(wgpu::Features::MULTIVIEW),
        limits.max_multiview_view_count,
        features.contains(wgpu::Features::MULTISAMPLE_ARRAY),
        features.contains(wgpu::Features::INDIRECT_FIRST_INSTANCE),
        features.contains(wgpu::Features::DEPTH_CLIP_CONTROL)
    );
    // SAFETY: the hook's contract; the summary outlives the call.
    hook("device made", unsafe {
        (p.device_made)(
            p.context,
            raw_instance,
            raw_physical,
            raw_device,
            summary.as_ptr(),
            summary.len(),
        )
    })?;
    Ok(Gpu {
        instance: wgpu_instance,
        adapter,
        device,
        queue,
    })
}

/// The device's Android API level, which wgpu-hal's Vulkan instance takes;
/// none on a PC.
#[cfg(not(target_os = "android"))]
fn sdk_version() -> u32 {
    0
}

/// One line to the PC host's standard error, as `exact-xr:` (the OpenXR
/// artifact keeps its own log file, `docs/quest.md`, "PC VR on Windows").
#[cfg(not(target_os = "android"))]
pub(crate) fn log(line: &str) {
    eprintln!("exact-xr: {line}");
}

/// The device's Android API level, which wgpu-hal's Vulkan instance takes.
#[cfg(target_os = "android")]
fn sdk_version() -> u32 {
    let mut value = [0 as c_char; 92];
    // SAFETY: PROP_VALUE_MAX is 92 bytes, NUL included.
    let n = unsafe { __system_property_get(c"ro.build.version.sdk".as_ptr(), value.as_mut_ptr()) };
    if n <= 0 {
        return 0;
    }
    // SAFETY: the property call NUL-terminated it.
    unsafe { CStr::from_ptr(value.as_ptr()) }
        .to_str()
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(target_os = "android")]
#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

/// One line to logcat under the headset's tag, `exact-xr` (info).
#[cfg(target_os = "android")]
pub(crate) fn log(line: &str) {
    let text = CString::new(line.replace('\0', " ")).unwrap_or_default();
    // SAFETY: two NUL-terminated strings.
    unsafe { __android_log_write(4, c"exact-xr".as_ptr(), text.as_ptr()) };
}

// ---------------------------------------------------------------- the target

/// One frame's stereo target as a provider lends it, in C layout: a `D2`
/// image of the provider's device with two array layers (left, right), in
/// `COLOR_ATTACHMENT_OPTIMAL` and handed back so, and each eye's matrices.
#[repr(C)]
pub struct StereoTarget {
    /// The `VkImage`. It outlives every frame drawn into it: the module keeps
    /// a wrapper of it until it unloads.
    pub image: u64,
    /// Its `VkFormat`: an 8-bit RGBA or BGRA format.
    pub format: u32,
    /// Each layer's width in pixels.
    pub width: u32,
    /// Each layer's height in pixels.
    pub height: u32,
    /// Bits of [`usage`] the image was made with besides colour attachment.
    pub usage: u32,
    /// Reference space → eye, column-major, per layer.
    pub views: [[f32; 16]; 2],
    /// Eye → clip, column-major, 0–1 depth (not reversed), per layer.
    pub projections: [[f32; 16]; 2],
}

/// [`StereoTarget::usage`]'s bits.
pub mod usage {
    /// Sampled (`VK_IMAGE_USAGE_SAMPLED_BIT`).
    pub const SAMPLED: u32 = 1;
    /// A copy's source: `gpu_read_stereo` needs it.
    pub const COPY_SRC: u32 = 2;
    /// A copy's destination.
    pub const COPY_DST: u32 = 4;
}

/// The provider's images as wgpu wraps them, by handle, and the last drawn.
#[derive(Default)]
struct Targets {
    msaa_eye: Option<(u32, u32, wgpu::TextureFormat)>,
    wrapped: Vec<(u64, wgpu::Texture)>,
    last: Option<wgpu::Texture>,
}

thread_local! {
    static TARGETS: RefCell<Targets> = RefCell::default();
}

/// The provider's `VkFormat` as wgpu's, for the formats a target may have.
fn wgpu_format(format: u32) -> Option<wgpu::TextureFormat> {
    Some(match vk::Format::from_raw(format as i32) {
        vk::Format::R8G8B8A8_SRGB => wgpu::TextureFormat::Rgba8UnormSrgb,
        vk::Format::B8G8R8A8_SRGB => wgpu::TextureFormat::Bgra8UnormSrgb,
        vk::Format::R8G8B8A8_UNORM => wgpu::TextureFormat::Rgba8Unorm,
        vk::Format::B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
        _ => return None,
    })
}

/// The `VkFormat` canvas `id` draws its page frames in (0 when it has none
/// a target may have): a target in it keeps the surface's renderer as it is.
pub fn vulkan_format(id: u32) -> u32 {
    let format = native::with(|m| m.render_format(id)).flatten();
    let vk = match format {
        Some(wgpu::TextureFormat::Rgba8UnormSrgb) => vk::Format::R8G8B8A8_SRGB,
        Some(wgpu::TextureFormat::Bgra8UnormSrgb) => vk::Format::B8G8R8A8_SRGB,
        Some(wgpu::TextureFormat::Rgba8Unorm) => vk::Format::R8G8B8A8_UNORM,
        Some(wgpu::TextureFormat::Bgra8Unorm) => vk::Format::B8G8R8A8_UNORM,
        _ => return 0,
    };
    vk.as_raw() as u32
}

fn wrap(gpu: &Gpu, t: &StereoTarget, format: wgpu::TextureFormat) -> Result<wgpu::Texture, String> {
    let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
    let mut uses = wgpu::TextureUses::COLOR_TARGET;
    for (bit, u, hal) in [
        (
            usage::SAMPLED,
            wgpu::TextureUsages::TEXTURE_BINDING,
            wgpu::TextureUses::RESOURCE,
        ),
        (
            usage::COPY_SRC,
            wgpu::TextureUsages::COPY_SRC,
            wgpu::TextureUses::COPY_SRC,
        ),
        (
            usage::COPY_DST,
            wgpu::TextureUsages::COPY_DST,
            wgpu::TextureUses::COPY_DST,
        ),
    ] {
        if t.usage & bit != 0 {
            usage |= u;
            uses |= hal;
        }
    }
    let size = wgpu::Extent3d {
        width: t.width,
        height: t.height,
        depth_or_array_layers: 2,
    };
    // SAFETY: the device's own HAL, read only to wrap the image.
    let device = unsafe { gpu.device.as_hal::<wgpu::hal::api::Vulkan>() }
        .ok_or("the device is not Vulkan")?;
    // SAFETY: the provider's image of this device, made as `t` says; it
    // outlives the wrapper (`StereoTarget::image`), which never frees it.
    let hal = unsafe {
        device.texture_from_raw(
            vk::Image::from_raw(t.image),
            &wgpu::hal::TextureDescriptor {
                label: Some("exact stereo target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: uses,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            Some(Box::new(|| {})),
            wgpu::hal::vulkan::TextureMemory::External,
        )
    };
    drop(device);
    // SAFETY: as above; the image arrives in COLOR_ATTACHMENT_OPTIMAL.
    Ok(unsafe {
        gpu.device
            .create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("exact stereo target"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                },
                wgpu::TextureUses::COLOR_TARGET,
            )
    })
}

/// `gpu_render_stereo`: canvas `id` ticks once and draws a stereo frame into
/// `target`, submitted, the image left in `COLOR_ATTACHMENT_OPTIMAL`. 1 when
/// the canvas wants another frame, 0 when not, 2 on failure (`gpu_error`).
pub fn render_stereo(id: u32, target: &StereoTarget, now_ms: f64) -> u32 {
    let Some(format) = wgpu_format(target.format) else {
        native::refuse(&format!(
            "a stereo target's format {} is not 8-bit RGBA or BGRA",
            target.format
        ));
        return 2;
    };
    let texture = native::with(|m| {
        let gpu = m.gpu().ok_or("no device")?;
        TARGETS.with(|t| {
            let mut t = t.borrow_mut();
            let fits = |(image, texture): &&(u64, wgpu::Texture)| {
                *image == target.image
                    && texture.format() == format
                    && texture.width() == target.width
                    && texture.height() == target.height
            };
            if let Some((_, texture)) = t.wrapped.iter().find(fits) {
                return Ok(texture.clone());
            }
            let texture = wrap(gpu, target, format)?;
            let eye = (target.width, target.height, format);
            if t.msaa_eye != Some(eye) {
                msaa::select(gpu, target, format);
                t.msaa_eye = Some(eye);
            }
            t.wrapped.retain(|(image, _)| *image != target.image);
            t.wrapped.push((target.image, texture.clone()));
            Ok::<_, String>(texture)
        })
    });
    let texture = match texture {
        Some(Ok(texture)) => texture,
        Some(Err(e)) => {
            native::refuse(&format!("a stereo target: {e}"));
            return 2;
        }
        None => {
            native::refuse("GPU module is not loaded");
            return 2;
        }
    };
    let stereo = XrStereo {
        target: texture.clone(),
        size: (target.width, target.height),
        views: target.views,
        projections: target.projections,
    };
    let drawn = native::with(|m| {
        let drawn = m.render_xr_stereo(id, now_ms, format, &stereo);
        release(m.gpu(), &texture);
        drawn
    })
    .flatten();
    TARGETS.with(|t| t.borrow_mut().last = Some(texture));
    match drawn {
        Some(true) => 1,
        Some(false) => 0,
        None => 2,
    }
}

/// Hand the image back in the layout the provider expects.
fn release(gpu: Option<&Gpu>, texture: &wgpu::Texture) {
    let Some(gpu) = gpu else { return };
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact stereo target release"),
        });
    encoder.transition_resources(
        std::iter::empty::<wgpu::BufferTransition<&wgpu::Buffer>>(),
        std::iter::once(wgpu::TextureTransition {
            texture,
            selector: None,
            state: wgpu::TextureUses::COLOR_TARGET,
        }),
    );
    gpu.queue.submit([encoder.finish()]);
}

/// `gpu_read_stereo`: layer `layer` of the last stereo target drawn, as
/// `width * height` RGBA pixels in `out`, waiting for the GPU. 0 on success,
/// 1 on failure (`gpu_error`).
pub fn read_stereo(layer: u32, out: &mut [u8]) -> u32 {
    let Some(texture) = TARGETS.with(|t| t.borrow().last.clone()) else {
        native::refuse("no stereo frame has been drawn");
        return 1;
    };
    let read = native::with(|m| {
        let read = crate::fixture::read_layer(m.gpu().ok_or("no device")?, &texture, layer);
        release(m.gpu(), &texture);
        read
    });
    match read {
        Some(Ok(pixels)) if pixels.data.len() == out.len() => {
            out.copy_from_slice(&pixels.data);
            0
        }
        Some(Ok(pixels)) => {
            native::refuse(&format!(
                "a stereo layer is {} bytes, not {}",
                pixels.data.len(),
                out.len()
            ));
            1
        }
        Some(Err(e)) => {
            native::refuse(&format!("reading a stereo layer: {e}"));
            1
        }
        None => {
            native::refuse("GPU module is not loaded");
            1
        }
    }
}

/// `gpu_input_xr`: a headset's head and hands for one frame, as
/// [`crate::XR_INPUT_FLOATS`] numbers ([`crate::XrInput::from_floats`]), to
/// canvas `id`. 0 on success, 1 on failure (`gpu_error`).
pub fn input_xr(id: u32, floats: &[f32], at_ms: f64) -> u32 {
    let Some(input) = crate::XrInput::from_floats(floats) else {
        native::refuse(&format!(
            "gpu_input_xr: {} numbers that are not one headset input",
            floats.len()
        ));
        return 1;
    };
    let event = crate::InputEvent::Xr { input, at_ms };
    match native::with(|m| m.input(id, &event)) {
        Some(true) => 0,
        _ => 1,
    }
}

/// The C exports of these seams, which [`crate::module!`] defines.
#[doc(hidden)]
#[macro_export]
macro_rules! __vulkan_exports {
    () => {
        /// Make the device through a provider's hooks
        /// (`exact_gpu::vulkan::VulkanProvider`), before `gpu_prepare` or
        /// `gpu_load`; null makes it as usual.
        /// # Safety
        /// `provider`, when non-null, stays valid for the process.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_vulkan_provider(
            provider: *const $crate::vulkan::VulkanProvider,
        ) {
            $crate::vulkan::set_provider(provider)
        }
        /// Draw canvas `id`'s stereo frame into a provider's image. 1 another
        /// frame wanted, 0 not, 2 failure.
        /// # Safety
        /// `target` is null or one readable `exact_gpu::vulkan::StereoTarget`.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_render_stereo(
            id: u32,
            target: *const $crate::vulkan::StereoTarget,
            now_ms: f64,
        ) -> u32 {
            match target.as_ref() {
                Some(target) => $crate::vulkan::render_stereo(id, target, now_ms),
                None => {
                    $crate::native::refuse("gpu_render_stereo: a null target");
                    2
                }
            }
        }
        /// A layer of the last stereo frame as RGBA pixels in `out`. 0 on success.
        /// # Safety
        /// `out` is `len` writable bytes.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_read_stereo(layer: u32, out: *mut u8, len: usize) -> u32 {
            match $crate::native::bytes_mut("gpu_read_stereo", out, len) {
                Some(out) => $crate::vulkan::read_stereo(layer, out),
                None => 1,
            }
        }
        /// A headset's input for one frame, as numbers. 0 on success.
        /// # Safety
        /// `floats` is `len` readable numbers.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_input_xr(
            id: u32,
            floats: *const f32,
            len: usize,
            at_ms: f64,
        ) -> u32 {
            if floats.is_null() {
                $crate::native::refuse("gpu_input_xr: a null pointer");
                return 1;
            }
            $crate::vulkan::input_xr(id, ::std::slice::from_raw_parts(floats, len), at_ms)
        }
        /// Whether a headset session draws this module's canvas.
        #[no_mangle]
        pub extern "C" fn gpu_headset(on: bool) {
            $crate::set_headset(on)
        }
        /// The host's debug switches, once: whitespace-separated words.
        /// # Safety
        /// `words` is `len` readable bytes.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_debug_switches(words: *const u8, len: usize) {
            if let Some(words) = $crate::native::bytes("gpu_debug_switches", words, len) {
                $crate::set_debug_switches(String::from_utf8_lossy(words).into_owned());
            }
        }
        /// The `VkFormat` canvas `id` draws its page in; 0 for none.
        #[no_mangle]
        pub extern "C" fn gpu_vulkan_format(id: u32) -> u32 {
            $crate::vulkan::vulkan_format(id)
        }
        /// Where canvas `id`'s immersive subject stands: 1 with three numbers
        /// written to `out`, 0 when it does not say.
        /// # Safety
        /// `out` is three writable numbers.
        #[no_mangle]
        pub unsafe extern "C" fn gpu_xr_focus(id: u32, out: *mut f32) -> u32 {
            match $crate::native::xr_focus(id) {
                Some(focus) if !out.is_null() => {
                    ::std::ptr::copy_nonoverlapping(focus.as_ptr(), out, 3);
                    1
                }
                _ => 0,
            }
        }
        /// How canvas `id` drew its last stereo frame, in gpu_out_ptr; its length.
        #[no_mangle]
        pub extern "C" fn gpu_xr_stereo_mode(id: u32) -> u32 {
            let bytes = $crate::native::xr_stereo_mode(id).as_bytes().to_vec();
            let len = bytes.len() as u32;
            EXACT_GPU_OUT.with(|out| *out.borrow_mut() = bytes);
            len
        }
    };
}
