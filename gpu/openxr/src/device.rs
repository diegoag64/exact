//! The GPU module's device, made by the headset's runtime
//! (`XR_KHR_vulkan_enable2`), and the session made on it.
//!
//! [`open`] loads the Khronos loader, makes the OpenXR instance and finds the
//! headset; the GPU module then makes its device as it always does, calling
//! the [`VulkanProvider`] hooks here where the runtime must make or choose
//! the Vulkan objects (`gpu/src/vulkan.rs`), and [`device_made`] makes the
//! session on the device it wrapped.

use crate::java::{self, log};
use crate::module::{self, VulkanProvider};
use crate::session::{Foveation, Headset, HEADSET};
use openxr as xr;
use std::ffi::{c_int, c_void};
use std::sync::OnceLock;

/// The runtime's instance and the headset, made by [`open`].
struct Runtime {
    instance: xr::Instance,
    system: xr::SystemId,
}
// SAFETY: OpenXR handles are usable from any thread; the hooks that use
// them run one at a time, on the thread making the module's device.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static PROVIDER: OnceLock<VulkanProvider> = OnceLock::new();

/// What a hook returns for an OpenXR failure (logged with its detail):
/// `VK_ERROR_INITIALIZATION_FAILED`.
const FAILED: c_int = -3;

/// `VK_MAKE_API_VERSION(0, major, minor, 0)`.
fn vk_version(major: u32, minor: u32) -> u32 {
    (major << 22) | (minor << 12)
}

/// Make the OpenXR instance and find the headset, and the provider the GPU
/// module makes its device with. An error where there is no loader or no
/// runtime (the app then draws to its window) or the runtime refused.
pub(crate) fn open() -> Result<&'static VulkanProvider, String> {
    if let Some(provider) = PROVIDER.get() {
        return Ok(provider);
    }
    let java = java::JAVA
        .get()
        .ok_or("no Java VM (xr_android_init did not run)")?;
    let attached = java::Attached::new(java.vm)?;
    let activity = java::activity(java, attached.env);
    // SAFETY: a live VM and a global reference to an Activity (or Context).
    let platform = unsafe { xr::AndroidPlatformInfo::new(java.vm.cast(), activity.cast()) };
    let entry = load_entry(&platform).map_err(|e| format!("no OpenXR loader or runtime ({e})"))?;
    let available = entry
        .enumerate_extensions()
        .map_err(|e| format!("extensions: {e}"))?;
    if !available.khr_vulkan_enable2 {
        return Err("the runtime has no XR_KHR_vulkan_enable2".into());
    }
    let mut wanted = xr::ExtensionSet::default();
    wanted.khr_vulkan_enable2 = true;
    wanted.khr_android_create_instance = available.khr_android_create_instance;
    wanted.ext_local_floor = available.ext_local_floor;
    wanted.fb_display_refresh_rate = available.fb_display_refresh_rate;
    wanted.fb_foveation = available.fb_foveation;
    wanted.fb_foveation_configuration = available.fb_foveation_configuration;
    wanted.fb_swapchain_update_state = available.fb_swapchain_update_state;
    wanted.ext_performance_settings = available.ext_performance_settings;
    let instance = entry
        .create_instance(
            &xr::ApplicationInfo {
                application_name: "exact",
                application_version: 1,
                engine_name: "exact",
                engine_version: 1,
                api_version: xr::Version::new(1, 0, 34),
            },
            &wanted,
            &[],
            &platform,
        )
        .map_err(|e| format!("instance: {e}"))?;
    drop(attached);
    if let Ok(props) = instance.properties() {
        log(
            false,
            &format!("runtime {} {}", props.runtime_name, props.runtime_version),
        );
    }
    let system = instance
        .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
        .map_err(|e| format!("system: {e}"))?;
    let reqs = instance
        .graphics_requirements::<xr::Vulkan>(system)
        .map_err(|e| format!("graphics requirements: {e}"))?;
    let switches = java::debug_switches();
    if !switches.trim().is_empty() {
        log(
            true,
            &format!(
                "debug switches: {}",
                switches.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        );
    }
    // Vulkan 1.1: multiview is core there.
    let api = if java::debug_switch("vk13") {
        vk_version(1, 3)
    } else {
        vk_version(1, 1)
    };
    let min = reqs.min_api_version_supported;
    let api = api.max(vk_version(min.major().into(), min.minor().into()));
    let _ = RUNTIME.set(Runtime { instance, system });
    Ok(PROVIDER.get_or_init(|| VulkanProvider {
        context: std::ptr::null_mut(),
        api_version: api,
        // Both eyes in one pass where the GPU can.
        multiview_views: if java::debug_switch("no-multiview") {
            0
        } else {
            2
        },
        create_instance,
        physical_device,
        create_device,
        device_made,
    }))
}

fn load_entry(platform: &xr::AndroidPlatformInfo) -> Result<xr::Entry, String> {
    // SAFETY: the Khronos loader the APK packages beside this artifact.
    match unsafe { xr::Entry::load_from(std::path::Path::new("libopenxr_loader.so"), platform) } {
        Ok(entry) => Ok(entry),
        Err(first) => {
            let Some(dir) = std::env::var_os("EXACT_NATIVE_LIBS") else {
                return Err(first.to_string());
            };
            let path = std::path::Path::new(&dir).join("libopenxr_loader.so");
            // SAFETY: as above.
            unsafe { xr::Entry::load_from(&path, platform) }.map_err(|e| format!("{first}; {e}"))
        }
    }
}

fn runtime() -> Result<&'static Runtime, c_int> {
    RUNTIME.get().ok_or_else(|| {
        log(true, "the GPU module asked for a device before xr_open");
        FAILED
    })
}

/// An OpenXR call that yields a Vulkan result, as a hook's code.
fn vulkan<T>(stage: &str, made: xr::Result<Result<T, i32>>) -> Result<T, c_int> {
    match made {
        Ok(Ok(made)) => Ok(made),
        Ok(Err(code)) => {
            log(true, &format!("Vulkan {stage}: VkResult {code}"));
            Err(code)
        }
        Err(e) => {
            log(true, &format!("Vulkan {stage}: {e}"));
            Err(FAILED)
        }
    }
}

unsafe extern "C" fn create_instance(
    _: *mut c_void,
    get_instance_proc_addr: *const c_void,
    info: *const c_void,
    out: *mut u64,
) -> c_int {
    let made = runtime().and_then(|rt| {
        // SAFETY: the module's contract: its loader's `vkGetInstanceProcAddr`
        // and a valid `VkInstanceCreateInfo`.
        vulkan("instance", unsafe {
            rt.instance.create_vulkan_instance(
                rt.system,
                std::mem::transmute::<*const c_void, xr::sys::platform::VkGetInstanceProcAddr>(
                    get_instance_proc_addr,
                ),
                info.cast(),
            )
        })
    });
    match made {
        Ok(instance) => {
            // SAFETY: the module's out pointer.
            unsafe { *out = instance as usize as u64 };
            0
        }
        Err(code) => code,
    }
}

unsafe extern "C" fn physical_device(_: *mut c_void, instance: u64, out: *mut u64) -> c_int {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    // SAFETY: the instance `create_instance` made.
    match unsafe {
        rt.instance
            .vulkan_graphics_device(rt.system, instance as usize as _)
    } {
        Ok(physical) => {
            // SAFETY: the module's out pointer.
            unsafe { *out = physical as usize as u64 };
            0
        }
        Err(e) => {
            log(true, &format!("Vulkan physical device: {e}"));
            FAILED
        }
    }
}

unsafe extern "C" fn create_device(
    _: *mut c_void,
    get_instance_proc_addr: *const c_void,
    physical: u64,
    info: *const c_void,
    out: *mut u64,
) -> c_int {
    let made = runtime().and_then(|rt| {
        // SAFETY: the module's contract: as for the instance, with a valid
        // `VkDeviceCreateInfo` for the physical device the runtime chose.
        vulkan("device", unsafe {
            rt.instance.create_vulkan_device(
                rt.system,
                std::mem::transmute::<*const c_void, xr::sys::platform::VkGetInstanceProcAddr>(
                    get_instance_proc_addr,
                ),
                physical as usize as _,
                info.cast(),
            )
        })
    });
    match made {
        Ok(device) => {
            // SAFETY: the module's out pointer.
            unsafe { *out = device as usize as u64 };
            0
        }
        Err(code) => code,
    }
}

/// The module wrapped the device: make the session on it.
unsafe extern "C" fn device_made(
    _: *mut c_void,
    instance: u64,
    physical: u64,
    device: u64,
    summary: *const u8,
    summary_len: usize,
) -> c_int {
    if !summary.is_null() {
        // SAFETY: the module's `summary_len` bytes.
        let text = unsafe { std::slice::from_raw_parts(summary, summary_len) };
        log(false, &String::from_utf8_lossy(text));
    }
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    // SAFETY: the Vulkan objects the hooks above made, for this system.
    let made = unsafe {
        rt.instance.create_session::<xr::Vulkan>(
            rt.system,
            &xr::vulkan::SessionCreateInfo {
                instance: instance as usize as _,
                physical_device: physical as usize as _,
                device: device as usize as _,
                queue_family_index: 0,
                queue_index: 0,
            },
        )
    };
    let (session, waiter, stream) = match made {
        Ok(made) => made,
        Err(e) => {
            log(true, &format!("session: {e}"));
            return FAILED;
        }
    };
    let exts = rt.instance.exts();
    let foveation = match (
        exts.fb_foveation,
        exts.fb_foveation_configuration,
        exts.fb_swapchain_update_state,
    ) {
        (Some(fov), Some(config), Some(update)) => Some(Foveation {
            fov,
            config,
            update,
        }),
        _ => None,
    };
    let controls = match crate::controls::Controls::new(&rt.instance, &session) {
        Ok(controls) => Some(controls),
        Err(e) => {
            log(true, &format!("controllers: {e}; the canvas hears none"));
            None
        }
    };
    *HEADSET.lock().unwrap_or_else(|e| e.into_inner()) = Some(Headset::new(
        rt.instance.clone(),
        rt.system,
        session,
        waiter,
        stream,
        exts.fb_display_refresh_rate,
        foveation,
        controls,
    ));
    if let Some(module) = module::get() {
        module.set_headset(true);
    }
    log(false, "session made; waiting for READY");
    0
}
