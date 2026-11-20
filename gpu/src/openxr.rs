//! A native headset: OpenXR on Android (Meta Quest), behind the `openxr`
//! feature. See `docs/quest.md`.
//!
//! The module's device is the headset's: the OpenXR runtime makes the Vulkan
//! instance and device (`XR_KHR_vulkan_enable2`) and wgpu wraps them, so the
//! world renders straight into the session's swapchain. One colour swapchain
//! of two array layers (left, right) serves both eyes; each frame the canvas
//! the host renders first draws a stereo frame into it
//! ([`crate::Module::render_xr_stereo`], which falls back to one pass per
//! eye) instead of into its window, and the frame is handed to the
//! compositor as one projection layer.
//!
//! The frame loop rides the host's own: the presenter calls `gpu_render`
//! each display frame, and that call waits for the headset's next frame
//! (`xrWaitFrame`), so the host is paced by the headset. The world ticks
//! once per headset frame, on its predicted display time.
//!
//! Where no runtime answers (a phone), the device is made as it always is
//! and nothing here runs: the app draws to its window.
#![allow(unsafe_code)]

use crate::xr_capture::{self, Request};
use crate::{wgpu, DeviceFailure, DeviceFailureKind, Gpu, Module, XrStereo};
use ash::vk::{self, Handle as _};
use openxr as xr;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// The near and far planes of each eye, metres (the stage rescales them to
/// world units).
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
/// The display rate asked for (`XR_FB_display_refresh_rate`).
const REFRESH_HZ: f32 = 72.0;

// ---------------------------------------------------------------- logging

#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

/// One line to logcat under the tag `exact-xr` (info), or warn.
pub(crate) fn log(warn: bool, line: &str) {
    let text = CString::new(line.replace('\0', " ")).unwrap_or_default();
    // SAFETY: two NUL-terminated strings.
    unsafe { __android_log_write(if warn { 5 } else { 4 }, c"exact-xr".as_ptr(), text.as_ptr()) };
}

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

// ---------------------------------------------------------------- Java

/// The process's Java VM and a global reference to the application
/// `Context`, which [`android_init`] keeps.
struct Java {
    vm: *mut jni_sys::JavaVM,
    context: jni_sys::jobject,
}
// SAFETY: a JavaVM pointer and a global reference are valid on every thread.
unsafe impl Send for Java {}
unsafe impl Sync for Java {}
static JAVA: OnceLock<Java> = OnceLock::new();

/// The artifact's Android initialization (`gpu_android_init`): keep the VM
/// and the application's `Context` for the session made later.
pub fn android_init(android: crate::android::Android) -> Result<(), String> {
    let env = android.env as *mut jni_sys::JNIEnv;
    // SAFETY: the host's contract: a live env on this thread and a live
    // reference to the application Context.
    let context = unsafe { ((**env).NewGlobalRef.ok_or("no NewGlobalRef")?)(env, android.context as jni_sys::jobject) };
    if context.is_null() {
        return Err("no global reference to the application Context".into());
    }
    let _ = JAVA.set(Java {
        vm: android.vm as *mut jni_sys::JavaVM,
        context,
    });
    Ok(())
}

/// This thread attached to the VM for the guard's life (detached after, if
/// it was not attached before).
struct Attached {
    vm: *mut jni_sys::JavaVM,
    env: *mut jni_sys::JNIEnv,
    detach: bool,
}

impl Attached {
    fn new(vm: *mut jni_sys::JavaVM) -> Result<Self, String> {
        let mut env: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live VM.
        unsafe {
            let invoke = &**vm;
            if (invoke.GetEnv.unwrap())(vm, &mut env, jni_sys::JNI_VERSION_1_6) == jni_sys::JNI_OK {
                return Ok(Self { vm, env: env.cast(), detach: false });
            }
            if (invoke.AttachCurrentThread.unwrap())(vm, &mut env, std::ptr::null_mut()) != jni_sys::JNI_OK {
                return Err("could not attach this thread to the Java VM".into());
            }
        }
        Ok(Self { vm, env: env.cast(), detach: true })
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        if self.detach {
            // SAFETY: attached by `new` on this thread.
            unsafe { ((**self.vm).DetachCurrentThread.unwrap())(self.vm) };
        }
    }
}

/// The activity the reader registered (`dev.exact.canvas.Immersive.activity`),
/// as a global reference, else the application Context.
fn activity(java: &Java, env: *mut jni_sys::JNIEnv) -> jni_sys::jobject {
    // SAFETY: JNI calls on an attached thread with live references; every
    // exception is cleared before the next call.
    unsafe {
        let e = &**env;
        let clear = || {
            if (e.ExceptionCheck.unwrap())(env) != 0 {
                (e.ExceptionClear.unwrap())(env);
                true
            } else {
                false
            }
        };
        let class = (e.GetObjectClass.unwrap())(env, java.context);
        let get_loader = (e.GetMethodID.unwrap())(env, class, c"getClassLoader".as_ptr(), c"()Ljava/lang/ClassLoader;".as_ptr());
        let loader = (e.CallObjectMethodA.unwrap())(env, java.context, get_loader, std::ptr::null());
        if clear() || loader.is_null() {
            return java.context;
        }
        let loader_class = (e.GetObjectClass.unwrap())(env, loader);
        let load = (e.GetMethodID.unwrap())(env, loader_class, c"loadClass".as_ptr(), c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr());
        let name = (e.NewStringUTF.unwrap())(env, c"dev.exact.canvas.Immersive".as_ptr());
        let args = [jni_sys::jvalue { l: name }];
        let immersive = (e.CallObjectMethodA.unwrap())(env, loader, load, args.as_ptr());
        if clear() || immersive.is_null() {
            log(true, "no dev.exact.canvas.Immersive: the session is given the application Context");
            return java.context;
        }
        let field = (e.GetStaticFieldID.unwrap())(env, immersive, c"activity".as_ptr(), c"Landroid/app/Activity;".as_ptr());
        if clear() || field.is_null() {
            return java.context;
        }
        // The activity is made before the host boots; give it a moment.
        for _ in 0..200 {
            let activity = (e.GetStaticObjectField.unwrap())(env, immersive, field);
            if !activity.is_null() {
                return (e.NewGlobalRef.unwrap())(env, activity);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        log(true, "no activity registered: the session is given the application Context");
        java.context
    }
}

/// The application's external files directory
/// (`Context.getExternalFilesDir(null)`, made if missing): where `adb` reads
/// and writes a debug build's and a release build's files alike.
fn external_files_dir(java: &Java) -> Option<PathBuf> {
    let attached = Attached::new(java.vm).ok()?;
    let env = attached.env;
    // SAFETY: JNI calls on an attached thread with live references; every
    // exception is cleared before the next call.
    unsafe {
        let e = &**env;
        let failed = || {
            if (e.ExceptionCheck.unwrap())(env) != 0 {
                (e.ExceptionClear.unwrap())(env);
                true
            } else {
                false
            }
        };
        let class = (e.GetObjectClass.unwrap())(env, java.context);
        let method = (e.GetMethodID.unwrap())(env, class, c"getExternalFilesDir".as_ptr(), c"(Ljava/lang/String;)Ljava/io/File;".as_ptr());
        if failed() || method.is_null() {
            return None;
        }
        let args = [jni_sys::jvalue { l: std::ptr::null_mut() }];
        let file = (e.CallObjectMethodA.unwrap())(env, java.context, method, args.as_ptr());
        if failed() || file.is_null() {
            return None;
        }
        let file_class = (e.GetObjectClass.unwrap())(env, file);
        let path = (e.GetMethodID.unwrap())(env, file_class, c"getAbsolutePath".as_ptr(), c"()Ljava/lang/String;".as_ptr());
        let text = (e.CallObjectMethodA.unwrap())(env, file, path, std::ptr::null());
        if failed() || text.is_null() {
            return None;
        }
        let chars = (e.GetStringUTFChars.unwrap())(env, text, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let dir = CStr::from_ptr(chars).to_string_lossy().into_owned();
        (e.ReleaseStringUTFChars.unwrap())(env, text, chars);
        Some(PathBuf::from(dir))
    }
}

/// Debug switch `name` from the `xr-debug` file (`docs/quest.md`), read
/// before the device exists.
fn early_switch(name: &str) -> bool {
    JAVA.get()
        .and_then(external_files_dir)
        .and_then(|dir| std::fs::read_to_string(dir.join("xr-debug")).ok())
        .is_some_and(|text| text.split_whitespace().any(|w| w == name))
}

/// The debug eye capture (`docs/quest.md`, "Seeing what the headset shows"):
/// off until the file `capture` appears in the app's external files
/// directory; then the next stereo frame's two layers are read back and
/// written beside it as `eye-<n>-left.png` and `eye-<n>-right.png`.
#[derive(Default)]
struct Capture {
    /// The external files directory, once looked up (None inside: none).
    dir: Option<Option<PathBuf>>,
    /// Frames since the trigger was last looked for.
    since_poll: u32,
    /// The trigger's modification time and length when it could not be
    /// removed: the same file is not taken again.
    seen: Option<(SystemTime, u64)>,
    /// A capture the next drawn frame takes.
    pending: Option<Request>,
    /// Captures taken in this process.
    taken: u32,
}

/// Frames between looks for the trigger: about twice a second.
const CAPTURE_POLL_FRAMES: u32 = 36;
/// The trigger's file name in the external files directory.
const CAPTURE_TRIGGER: &str = "capture";

impl Capture {
    /// Look for the trigger every [`CAPTURE_POLL_FRAMES`] frames.
    fn poll(&mut self) {
        self.since_poll += 1;
        if self.pending.is_some() || self.since_poll < CAPTURE_POLL_FRAMES {
            return;
        }
        self.since_poll = 0;
        let dir = self
            .dir
            .get_or_insert_with(|| JAVA.get().and_then(external_files_dir))
            .clone();
        let Some(dir) = dir else { return };
        let trigger = dir.join(CAPTURE_TRIGGER);
        let Ok(meta) = std::fs::metadata(&trigger) else {
            self.seen = None;
            return;
        };
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        if self.seen == Some(stamp) {
            return;
        }
        let text = std::fs::read_to_string(&trigger).unwrap_or_default();
        if std::fs::remove_file(&trigger).is_err() {
            self.seen = Some(stamp);
        }
        match Request::parse(&text) {
            Ok(request) => self.pending = Some(request),
            Err(e) => log(true, &format!("capture: {e}")),
        }
    }

    /// Copy both layers of `target` into a buffer and write them as PNGs
    /// (on another thread, so the frame loop waits only for the copy).
    fn take(&mut self, gpu: &Gpu, target: &wgpu::Texture, format: wgpu::TextureFormat, size: (u32, u32), what: &str) {
        let Some(Some(dir)) = self.dir.clone() else { return };
        let bgra = matches!(format, wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm);
        let (w, h) = size;
        let bytes_per_row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let layer_bytes = u64::from(bytes_per_row) * u64::from(h);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("exact xr capture"),
            size: layer_bytes * 2,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact xr capture"),
        });
        for layer in 0..2u32 {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: target,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: layer_bytes * u64::from(layer),
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        gpu.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        if !matches!(rx.recv(), Ok(Ok(()))) {
            log(true, "capture: the read back failed");
            return;
        }
        let data = match slice.get_mapped_range() {
            Ok(view) => view.to_vec(),
            Err(e) => {
                log(true, &format!("capture: the read back failed: {e:?}"));
                return;
            }
        };
        buffer.unmap();
        self.taken += 1;
        let n = self.taken;
        let what = what.to_owned();
        std::thread::spawn(move || {
            let mut written = Vec::new();
            for (layer, eye) in ["left", "right"].into_iter().enumerate() {
                let start = layer * layer_bytes as usize;
                let rows = &data[start..start + layer_bytes as usize];
                let rgb = xr_capture::rgb_rows(rows, bytes_per_row as usize, w as usize, h as usize, bgra);
                let path = dir.join(format!("eye-{n}-{eye}.png"));
                match std::fs::write(&path, xr_capture::png_rgb(w, h, &rgb)) {
                    Ok(()) => written.push(path.display().to_string()),
                    Err(e) => log(true, &format!("capture: {}: {e}", path.display())),
                }
            }
            log(false, &format!("capture {n} ({what}): {}", written.join(" ")));
        });
    }
}

// ---------------------------------------------------------------- the headset

/// Per-second numbers for the log line.
#[derive(Default)]
struct Perf {
    since: Option<Instant>,
    frames: u32,
    skipped: u32,
    cpu_ms: f64,
    encode_ms: f64,
    wait_ms: f64,
    worst_ms: f64,
    /// How the last frame drew (`Module::xr_stereo_mode`).
    mode: &'static str,
}

impl Perf {
    fn frame(&mut self, wait_ms: f64, cpu_ms: f64, encode_ms: f64, rendered: bool, mode: Option<&'static str>) {
        if let Some(mode) = mode {
            if mode != self.mode {
                log(false, &format!("stereo frames draw {mode}"));
            }
            self.mode = mode;
        }
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        if rendered {
            self.frames += 1;
            self.cpu_ms += cpu_ms;
            self.encode_ms += encode_ms;
            self.worst_ms = self.worst_ms.max(cpu_ms);
        } else {
            self.skipped += 1;
        }
        self.wait_ms += wait_ms;
        let elapsed = now.duration_since(since).as_secs_f64();
        if elapsed >= 1.0 {
            let n = f64::from(self.frames.max(1));
            let all = f64::from((self.frames + self.skipped).max(1));
            log(
                false,
                &format!(
                    "perf mode={} fps={:.1} cpu_ms={:.2} encode_ms={:.2} worst_ms={:.2} wait_ms={:.2} skipped={}",
                    if self.mode.is_empty() { "none" } else { self.mode },
                    f64::from(self.frames) / elapsed,
                    self.cpu_ms / n,
                    self.encode_ms / n,
                    self.worst_ms,
                    self.wait_ms / all,
                    self.skipped
                ),
            );
            *self = Perf {
                since: Some(now),
                mode: self.mode,
                ..Default::default()
            };
        }
    }
}

/// The session and what it draws into, made with the device.
struct Headset {
    instance: xr::Instance,
    system: xr::SystemId,
    session: xr::Session<xr::Vulkan>,
    waiter: xr::FrameWaiter,
    stream: xr::FrameStream<xr::Vulkan>,
    space: Option<xr::Space>,
    swapchain: Option<Swapchain>,
    /// `xrBeginSession` was called and `xrEndSession` not since.
    running: bool,
    focused: bool,
    exiting: bool,
    /// The last frame had no valid head orientation.
    untracked: bool,
    /// The canvas drawn into the headset: the first the host renders.
    canvas: Option<u32>,
    /// Host clock − headset clock, milliseconds, fixed at the first frame.
    clock_offset: Option<f64>,
    perf: Perf,
    frames: u64,
    refresh: Option<xr::raw::DisplayRefreshRateFB>,
    foveation: Option<Foveation>,
    vk_instance: ash::Instance,
    hal_device_ready: bool,
    capture: Capture,
}

struct Swapchain {
    handle: xr::Swapchain<xr::Vulkan>,
    textures: Vec<wgpu::Texture>,
    format: wgpu::TextureFormat,
    size: (u32, u32),
}

/// `XR_FB_foveation` and its swapchain update, where the runtime has them.
struct Foveation {
    fov: xr::raw::FoveationFB,
    config: xr::raw::FoveationConfigurationFB,
    update: xr::raw::SwapchainUpdateStateFB,
}

// SAFETY: OpenXR handles are usable from any thread with external
// synchronisation, which the mutex gives; the Vulkan instance is a handle
// table.
unsafe impl Send for Headset {}

static HEADSET: Mutex<Option<Headset>> = Mutex::new(None);

fn failure(stage: &str, e: impl std::fmt::Display) -> DeviceFailure {
    DeviceFailure::new(DeviceFailureKind::NoDevice, format!("OpenXR {stage}: {e}"))
}

/// Make the module's device through the headset's runtime, and its session.
/// None where there is no runtime to ask (the caller then makes the device
/// as it always does); an error where one answered and failed.
pub(crate) fn create_gpu(
    requirements: &[crate::grants::Requirements],
) -> Option<Result<Gpu, DeviceFailure>> {
    let Some(java) = JAVA.get() else {
        log(true, "no Java VM (gpu_android_init did not run): no headset");
        return None;
    };
    let attached = match Attached::new(java.vm) {
        Ok(a) => a,
        Err(e) => {
            log(true, &e);
            return None;
        }
    };
    let activity = activity(java, attached.env);
    // SAFETY: a live VM and a global reference to an Activity (or Context).
    let platform = unsafe { xr::AndroidPlatformInfo::new(java.vm.cast(), activity.cast()) };
    let entry = match load_entry(&platform) {
        Ok(entry) => entry,
        Err(e) => {
            log(true, &format!("no OpenXR loader or runtime ({e}): the app draws to its window"));
            return None;
        }
    };
    let made = make(entry, &platform, requirements);
    drop(attached);
    Some(made)
}

fn load_entry(platform: &xr::AndroidPlatformInfo) -> Result<xr::Entry, String> {
    // SAFETY: the Khronos loader the APK packages beside this module.
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

fn make(
    entry: xr::Entry,
    platform: &xr::AndroidPlatformInfo,
    requirements: &[crate::grants::Requirements],
) -> Result<Gpu, DeviceFailure> {
    let available = entry
        .enumerate_extensions()
        .map_err(|e| failure("extensions", e))?;
    if !available.khr_vulkan_enable2 {
        return Err(failure("extensions", "the runtime has no XR_KHR_vulkan_enable2"));
    }
    let mut wanted = xr::ExtensionSet::default();
    wanted.khr_vulkan_enable2 = true;
    wanted.khr_android_create_instance = available.khr_android_create_instance;
    wanted.ext_local_floor = available.ext_local_floor;
    wanted.fb_display_refresh_rate = available.fb_display_refresh_rate;
    wanted.fb_foveation = available.fb_foveation;
    wanted.fb_foveation_configuration = available.fb_foveation_configuration;
    wanted.fb_swapchain_update_state = available.fb_swapchain_update_state;
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
            platform,
        )
        .map_err(|e| failure("instance", e))?;
    if let Ok(props) = instance.properties() {
        log(
            false,
            &format!("runtime {} {}", props.runtime_name, props.runtime_version),
        );
    }
    let system = instance
        .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
        .map_err(|e| failure("system", e))?;
    let reqs = instance
        .graphics_requirements::<xr::Vulkan>(system)
        .map_err(|e| failure("graphics requirements", e))?;
    // Vulkan 1.1: multiview is core there.
    let api = if early_switch("vk13") {
        vk::make_api_version(0, 1, 3, 0)
    } else {
        vk::make_api_version(0, 1, 1, 0)
    };
    let min = vk::make_api_version(
        0,
        reqs.min_api_version_supported.major().into(),
        reqs.min_api_version_supported.minor().into(),
        0,
    );
    let api = api.max(min);

    // SAFETY: the system's libvulkan.
    let vk_entry = unsafe { ash::Entry::load() }.map_err(|e| failure("libvulkan", e))?;
    let flags = crate::device::instance_flags() - wgpu::InstanceFlags::DEBUG - wgpu::InstanceFlags::VALIDATION;
    let extensions = wgpu::hal::vulkan::Instance::desired_extensions(&vk_entry, api, flags)
        .map_err(|e| failure("Vulkan extensions", e))?;
    let names: Vec<*const c_char> = extensions.iter().map(|e| e.as_ptr()).collect();
    let app = vk::ApplicationInfo::default()
        .application_name(c"exact")
        .engine_name(c"exact")
        .api_version(api);
    let create = vk::InstanceCreateInfo::default()
        .application_info(&app)
        .enabled_extension_names(&names);
    let get_proc = vk_entry.static_fn().get_instance_proc_addr;
    // SAFETY: a valid create info whose pointers outlive the call.
    let raw_instance = unsafe {
        instance.create_vulkan_instance(
            system,
            std::mem::transmute::<vk::PFN_vkGetInstanceProcAddr, xr::sys::platform::VkGetInstanceProcAddr>(get_proc),
            &create as *const _ as *const _,
        )
    }
    .map_err(|e| failure("Vulkan instance", e))?
    .map_err(|e| failure("Vulkan instance", vk::Result::from_raw(e)))?;
    let vk_handle = vk::Instance::from_raw(raw_instance as u64);
    // SAFETY: the instance the runtime just made from this entry.
    let ash_instance = unsafe { ash::Instance::load(vk_entry.static_fn(), vk_handle) };
    // SAFETY: made from `vk_entry` with `extensions` at `api`; never
    // destroyed (the runtime holds it for the process).
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
    hal_instance.enable_f16_arithmetic();

    // SAFETY: the instance above.
    let raw_physical = unsafe { instance.vulkan_graphics_device(system, raw_instance) }
        .map_err(|e| failure("Vulkan physical device", e))?;
    let physical = vk::PhysicalDevice::from_raw(raw_physical as u64);
    let exposed = hal_instance
        .expose_adapter(physical)
        .ok_or_else(|| failure("adapter", "wgpu cannot use the headset's GPU"))?;
    crate::tier::choose(&exposed.info);
    let grant = crate::grants::negotiate(
        exposed.capabilities.limits.clone(),
        exposed.features,
        requirements,
    );
    let mut features = grant.features;
    let mut limits = grant.limits;
    // Debug switches read once at start (`docs/quest.md`, "Debug switches"):
    // the file `xr-debug` in the app's external files directory.
    let switches = JAVA
        .get()
        .and_then(external_files_dir)
        .and_then(|dir| std::fs::read_to_string(dir.join("xr-debug")).ok())
        .unwrap_or_default();
    crate::set_debug_switches(switches.clone());
    let switch = crate::debug_switch;
    if !switches.trim().is_empty() {
        log(true, &format!("debug switches: {}", switches.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    if switch("no-first-instance") {
        features.remove(wgpu::Features::INDIRECT_FIRST_INSTANCE);
    }
    // For a single-pass stereo renderer: multiview of both eyes (wgpu's
    // default view count is 0, so two are asked for), and multisampled
    // two-layer attachments for its 4x frames.
    let views = exposed.capabilities.limits.max_multiview_view_count;
    if exposed.features.contains(wgpu::Features::MULTIVIEW) && views >= 2 && !switch("no-multiview") {
        features |= wgpu::Features::MULTIVIEW;
        limits.max_multiview_view_count = 2;
        if exposed.features.contains(wgpu::Features::MULTISAMPLE_ARRAY) {
            features |= wgpu::Features::MULTISAMPLE_ARRAY;
        }
    }
    let memory_hints = wgpu::MemoryHints::MemoryUsage;
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
    // SAFETY: a valid create info whose pointers outlive the call.
    let raw_device = unsafe {
        instance.create_vulkan_device(
            system,
            std::mem::transmute::<vk::PFN_vkGetInstanceProcAddr, xr::sys::platform::VkGetInstanceProcAddr>(get_proc),
            raw_physical,
            &create as *const _ as *const _,
        )
    }
    .map_err(|e| failure("Vulkan device", e))?
    .map_err(|e| failure("Vulkan device", vk::Result::from_raw(e)))?;
    let device_handle = vk::Device::from_raw(raw_device as u64);
    // SAFETY: the device the runtime just made from this instance.
    let ash_device = unsafe { ash::Device::load(ash_instance.fp_v1_0(), device_handle) };
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
    // Quest 3 that pass zeroed each of the game renderer's culled draws (the
    // island drew nothing opaque: `docs/quest.md`, "What the headset drew
    // white"). So the instance is made with no backend of its own (no second
    // VkInstance) and given the runtime's adapter; the adapter keeps the
    // runtime's VkInstance alive.
    let wgpu_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::empty(),
        flags,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    drop(hal_instance);
    // SAFETY: the adapter and device above, made from the runtime's instance.
    let adapter = unsafe { wgpu_instance.create_adapter_from_hal(exposed) };
    let (device, queue) = unsafe {
        adapter.create_device_from_hal(
            open,
            &wgpu::DeviceDescriptor {
                label: Some("exact xr"),
                required_features: features,
                required_limits: limits.clone(),
                memory_hints,
                ..Default::default()
            },
        )
    }
    .map_err(|e| failure("wgpu device", e))?;
    log(
        false,
        &format!(
            "device {} (Vulkan {}.{}, driver {}), multiview {} (views {} of {views}), multisample array {}, indirect first instance {}",
            adapter.get_info().name,
            vk::api_version_major(api),
            vk::api_version_minor(api),
            adapter.get_info().driver_info,
            features.contains(wgpu::Features::MULTIVIEW),
            limits.max_multiview_view_count,
            features.contains(wgpu::Features::MULTISAMPLE_ARRAY),
            features.contains(wgpu::Features::INDIRECT_FIRST_INSTANCE)
        ),
    );

    // SAFETY: the Vulkan objects above, made for this system.
    let (session, waiter, stream) = unsafe {
        instance.create_session::<xr::Vulkan>(
            system,
            &xr::vulkan::SessionCreateInfo {
                instance: raw_instance,
                physical_device: raw_physical,
                device: raw_device,
                queue_family_index: 0,
                queue_index: 0,
            },
        )
    }
    .map_err(|e| failure("session", e))?;
    let refresh = instance.exts().fb_display_refresh_rate;
    let foveation = match (
        instance.exts().fb_foveation,
        instance.exts().fb_foveation_configuration,
        instance.exts().fb_swapchain_update_state,
    ) {
        (Some(fov), Some(config), Some(update)) => Some(Foveation { fov, config, update }),
        _ => None,
    };
    *HEADSET.lock().unwrap_or_else(|e| e.into_inner()) = Some(Headset {
        instance,
        system,
        session,
        waiter,
        stream,
        space: None,
        swapchain: None,
        running: false,
        focused: false,
        exiting: false,
        untracked: false,
        canvas: None,
        clock_offset: None,
        perf: Perf::default(),
        frames: 0,
        refresh,
        foveation,
        vk_instance: ash_instance,
        hal_device_ready: true,
        capture: Capture::default(),
    });
    log(false, "session made; waiting for READY");
    Ok(Gpu {
        instance: wgpu_instance,
        adapter,
        device,
        queue,
    })
}

/// Whether a headset session exists (the device is the headset's).
pub fn active() -> bool {
    HEADSET
        .lock()
        .map(|h| h.as_ref().is_some_and(|h| !h.exiting))
        .unwrap_or(false)
}

impl Headset {
    /// Handle the runtime's events; whether the session runs.
    fn poll(&mut self, module: &mut Module) -> xr::Result<()> {
        let mut buffer = xr::EventDataBuffer::new();
        while let Some(event) = self.instance.poll_event(&mut buffer)? {
            use xr::Event;
            match event {
                Event::SessionStateChanged(change) => {
                    let state = change.state();
                    log(false, &format!("session state {state:?}"));
                    match state {
                        xr::SessionState::READY => {
                            self.session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            self.running = true;
                            self.request_refresh_rate();
                        }
                        xr::SessionState::STOPPING => {
                            self.session.end()?;
                            self.running = false;
                        }
                        xr::SessionState::FOCUSED => {
                            self.focused = true;
                            if let Some(id) = self.canvas {
                                module.lifecycle(id, 3);
                            }
                        }
                        xr::SessionState::VISIBLE => {
                            if self.focused {
                                if let Some(id) = self.canvas {
                                    module.lifecycle(id, 2);
                                }
                            }
                            self.focused = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            self.running = false;
                            self.exiting = true;
                        }
                        _ => {}
                    }
                }
                Event::InstanceLossPending(_) => {
                    self.running = false;
                    self.exiting = true;
                }
                Event::DisplayRefreshRateChangedFB(change) => log(
                    false,
                    &format!(
                        "display {} Hz → {} Hz",
                        change.from_display_refresh_rate(),
                        change.to_display_refresh_rate()
                    ),
                ),
                _ => {}
            }
        }
        Ok(())
    }

    fn request_refresh_rate(&self) {
        let Some(refresh) = self.refresh else { return };
        let mut count = 0u32;
        // SAFETY: the extension's functions on this session.
        unsafe {
            let session = self.session.as_raw();
            if (refresh.enumerate_display_refresh_rates)(session, 0, &mut count, std::ptr::null_mut()).into_raw() < 0 {
                return;
            }
            let mut rates = vec![0f32; count as usize];
            (refresh.enumerate_display_refresh_rates)(session, count, &mut count, rates.as_mut_ptr());
            let rate = if rates.iter().any(|r| (r - REFRESH_HZ).abs() < 0.5) {
                REFRESH_HZ
            } else {
                rates.iter().copied().fold(0.0, f32::max)
            };
            let result = (refresh.request_display_refresh_rate)(session, rate);
            log(false, &format!("display rates {rates:?}: asked for {rate} Hz ({result:?})"));
        }
    }

    fn space(&mut self) -> xr::Result<&xr::Space> {
        if self.space.is_none() {
            let floor = self
                .session
                .enumerate_reference_spaces()?
                .contains(&xr::ReferenceSpaceType::LOCAL_FLOOR);
            let space = if floor {
                log(false, "reference space LOCAL_FLOOR");
                self.session
                    .create_reference_space(xr::ReferenceSpaceType::LOCAL_FLOOR, xr::Posef::IDENTITY)?
            } else {
                // The floor 1.6 m under where the head started.
                log(false, "reference space LOCAL, the floor 1.6 m under the head");
                self.session.create_reference_space(
                    xr::ReferenceSpaceType::LOCAL,
                    xr::Posef {
                        orientation: xr::Quaternionf::IDENTITY,
                        position: xr::Vector3f { x: 0.0, y: -1.6, z: 0.0 },
                    },
                )?
            };
            self.space = Some(space);
        }
        Ok(self.space.as_ref().unwrap())
    }

    fn swapchain(&mut self, module: &Module, id: u32) -> Result<(), String> {
        if self.swapchain.is_some() {
            return Ok(());
        }
        let gpu = module.gpu().ok_or("no device")?;
        let views = self
            .instance
            .enumerate_view_configuration_views(self.system, xr::ViewConfigurationType::PRIMARY_STEREO)
            .map_err(|e| format!("views: {e}"))?;
        let view = views.first().ok_or("no views")?;
        let size = (view.recommended_image_rect_width, view.recommended_image_rect_height);
        let formats = self
            .session
            .enumerate_swapchain_formats()
            .map_err(|e| format!("formats: {e}"))?;
        // The canvas's own format when it is sRGB and offered (its renderer
        // is kept), else sRGB RGBA, else sRGB BGRA.
        let page = module.render_format(id);
        let candidates = page
            .into_iter()
            .chain([wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureFormat::Bgra8UnormSrgb]);
        let (format, vk_format) = candidates
            .filter(|f| f.is_srgb())
            .filter_map(|f| Some((f, vk_format(f)?)))
            .find(|(_, vk)| formats.contains(&(vk.as_raw() as u32)))
            .ok_or_else(|| format!("no sRGB colour format among {formats:?}"))?;
        let usage_full = xr::SwapchainUsageFlags::COLOR_ATTACHMENT
            | xr::SwapchainUsageFlags::SAMPLED
            | xr::SwapchainUsageFlags::TRANSFER_SRC
            | xr::SwapchainUsageFlags::TRANSFER_DST;
        let info = |usage| xr::SwapchainCreateInfo::<xr::Vulkan> {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: usage,
            format: vk_format.as_raw() as u32,
            sample_count: 1,
            width: size.0,
            height: size.1,
            face_count: 1,
            array_size: 2,
            mip_count: 1,
        };
        let (handle, full) = match self.session.create_swapchain(&info(usage_full)) {
            Ok(s) => (s, true),
            Err(_) => (
                self.session
                    .create_swapchain(&info(xr::SwapchainUsageFlags::COLOR_ATTACHMENT))
                    .map_err(|e| format!("swapchain: {e}"))?,
                false,
            ),
        };
        let usage = if full {
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        };
        let images = handle.enumerate_images().map_err(|e| format!("images: {e}"))?;
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 2,
        };
        let textures = images
            .iter()
            .map(|&image| {
                // SAFETY: the runtime's image for this device, made as the
                // descriptor says; the swapchain owns it and outlives the
                // texture (the module drops textures before the session).
                // SAFETY: the device's own HAL, read only to wrap the image.
                let device = unsafe { gpu.device.as_hal::<wgpu::hal::api::Vulkan>() };
                let hal = device.map(|device| unsafe {
                    device.texture_from_raw(
                        vk::Image::from_raw(image),
                        &wgpu::hal::TextureDescriptor {
                            label: Some("exact xr swapchain"),
                            size: extent,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: hal_uses(usage),
                            memory_flags: wgpu::hal::MemoryFlags::empty(),
                            view_formats: vec![],
                        },
                        Some(Box::new(|| {})),
                        wgpu::hal::vulkan::TextureMemory::External,
                    )
                });
                let hal = hal.ok_or("the device is not Vulkan")?;
                // SAFETY: as above; acquired images arrive in
                // COLOR_ATTACHMENT_OPTIMAL (XR_KHR_vulkan_enable2).
                Ok(unsafe {
                    gpu.device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                        hal,
                        &wgpu::TextureDescriptor {
                            label: Some("exact xr swapchain"),
                            size: extent,
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
            })
            .collect::<Result<Vec<_>, String>>()?;
        log(
            false,
            &format!(
                "swapchain {}×{}×2 {format:?}, {} images, usage {}",
                size.0,
                size.1,
                textures.len(),
                if full { "colour+sampled+copy" } else { "colour" }
            ),
        );
        self.swapchain = Some(Swapchain {
            handle,
            textures,
            format,
            size,
        });
        self.foveate();
        Ok(())
    }

    /// Fixed foveation, HIGH and dynamic, on the colour swapchain
    /// (`XR_FB_foveation`). On Vulkan the runtime applies it through a
    /// fragment density map the render pass must use, which wgpu's passes
    /// do not yet: until then this is asked for and has no effect.
    fn foveate(&self) {
        let (Some(f), Some(swapchain)) = (self.foveation.as_ref(), self.swapchain.as_ref()) else {
            log(false, "foveation: not offered by the runtime");
            return;
        };
        // SAFETY: the extensions' functions with valid structures.
        unsafe {
            let level = xr::sys::FoveationLevelProfileCreateInfoFB {
                ty: xr::sys::FoveationLevelProfileCreateInfoFB::TYPE,
                next: std::ptr::null_mut(),
                level: xr::sys::FoveationLevelFB::HIGH,
                vertical_offset: 0.0,
                dynamic: xr::sys::FoveationDynamicFB::LEVEL_ENABLED,
            };
            let info = xr::sys::FoveationProfileCreateInfoFB {
                ty: xr::sys::FoveationProfileCreateInfoFB::TYPE,
                next: &level as *const _ as *mut _,
            };
            let mut profile = <xr::sys::FoveationProfileFB as xr::sys::Handle>::NULL;
            let made = (f.fov.create_foveation_profile)(self.session.as_raw(), &info, &mut profile);
            if made.into_raw() < 0 {
                log(true, &format!("foveation profile: {made:?}"));
                return;
            }
            let state = xr::sys::SwapchainStateFoveationFB {
                ty: xr::sys::SwapchainStateFoveationFB::TYPE,
                next: std::ptr::null_mut(),
                flags: xr::sys::SwapchainStateFoveationFlagsFB::EMPTY,
                profile,
            };
            let updated = (f.update.update_swapchain)(swapchain.handle.as_raw(), &state as *const _ as *const _);
            log(false, &format!("foveation HIGH, dynamic: {updated:?} (no effect without a fragment density map)"));
            let _ = (f.fov.destroy_foveation_profile)(profile);
            let _ = &f.config;
        }
    }
}

fn vk_format(format: wgpu::TextureFormat) -> Option<vk::Format> {
    Some(match format {
        wgpu::TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        wgpu::TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        wgpu::TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        wgpu::TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        _ => return None,
    })
}

fn hal_uses(usage: wgpu::TextureUsages) -> wgpu::TextureUses {
    let mut uses = wgpu::TextureUses::empty();
    if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        uses |= wgpu::TextureUses::COLOR_TARGET;
    }
    if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
        uses |= wgpu::TextureUses::RESOURCE;
    }
    if usage.contains(wgpu::TextureUsages::COPY_SRC) {
        uses |= wgpu::TextureUses::COPY_SRC;
    }
    if usage.contains(wgpu::TextureUsages::COPY_DST) {
        uses |= wgpu::TextureUses::COPY_DST;
    }
    uses
}

/// Eye → clip from an OpenXR view's field of view (0–1 depth).
fn projection(fov: xr::Fovf, near: f32, far: f32) -> [f32; 16] {
    crate::xr::fov_projection(
        [fov.angle_left, fov.angle_right, fov.angle_up, fov.angle_down],
        near,
        far,
    )
}

/// Reference space → eye from an OpenXR view's pose.
fn view(pose: xr::Posef) -> [f32; 16] {
    let (q, p) = (pose.orientation, pose.position);
    crate::xr::pose_view([q.x, q.y, q.z, q.w], [p.x, p.y, p.z])
}

/// Canvas `id`'s frame while a headset session exists: Some(the render
/// result for the host: 1 another frame, 2 a failure), or None to render
/// it as usual (another canvas, or no session).
pub(crate) fn render(module: &mut Module, id: u32, host_now_ms: f64) -> Option<u32> {
    let mut slot = HEADSET.lock().unwrap_or_else(|e| e.into_inner());
    let headset = slot.as_mut()?;
    if headset.exiting {
        return None;
    }
    if headset.canvas.is_some_and(|c| c != id) {
        // Not drawn while the headset shows another.
        return Some(0);
    }
    if let Err(e) = headset.poll(module) {
        log(true, &format!("events: {e}"));
    }
    if !headset.running {
        // Not shown until the session runs; rest a little so a host that
        // keeps asking does not spin.
        std::thread::sleep(Duration::from_millis(5));
        return Some(1);
    }
    match frame(headset, module, id, host_now_ms) {
        Ok(()) => Some(1),
        Err(e) => {
            log(true, &format!("frame: {e}"));
            Some(1)
        }
    }
}

fn frame(headset: &mut Headset, module: &mut Module, id: u32, host_now_ms: f64) -> Result<(), String> {
    let waited = Instant::now();
    let state = headset.waiter.wait().map_err(|e| format!("wait: {e}"))?;
    let wait_ms = waited.elapsed().as_secs_f64() * 1e3;
    let started = Instant::now();
    headset.capture.poll();
    headset.stream.begin().map_err(|e| format!("begin: {e}"))?;
    let time = state.predicted_display_time;
    if !state.should_render {
        headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[])
            .map_err(|e| format!("end: {e}"))?;
        headset.perf.frame(wait_ms, 0.0, 0.0, false, None);
        return Ok(());
    }
    if let Err(e) = headset.swapchain(module, id) {
        let _ = headset.stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(e);
    }
    headset.space().map_err(|e| format!("space: {e}"))?;
    let space = headset.space.as_ref().unwrap();
    let (located, mut views) = headset
        .session
        .locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, space)
        .map_err(|e| format!("views: {e}"))?;
    if views.len() < 2 {
        let _ = headset.stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(format!("{} views", views.len()));
    }
    // A capture from a fixed pose: both eyes look at the stage from it, for
    // this frame only, whether or not anyone wears the headset.
    let capture = headset.capture.pending.take();
    let fixed = capture.and_then(|c| c.pose).map(|pose| {
        let focus = module.xr_focus(id).unwrap_or([0.0, 1.0, -0.8]);
        for (view, (q, p)) in views.iter_mut().zip(pose.eyes(focus)) {
            view.pose = xr::Posef {
                orientation: xr::Quaternionf { x: q[0], y: q[1], z: q[2], w: q[3] },
                position: xr::Vector3f { x: p[0], y: p[1], z: p[2] },
            };
            let f = view.fov;
            if [f.angle_left, f.angle_right, f.angle_up, f.angle_down].iter().all(|a| *a == 0.0) {
                let a = 45f32.to_radians();
                view.fov = xr::Fovf { angle_left: -a, angle_right: a, angle_up: a, angle_down: -a };
            }
        }
        format!(
            "stage pose {:.2} m, {:.0}° down, {:.0}° turned, looking at {focus:?}",
            pose.distance, pose.pitch_deg, pose.yaw_deg
        )
    });
    // Tracking lost (a headset nobody wears, a dark room): the compositor
    // refuses a layer without a valid orientation, so the frame shows
    // nothing and the world waits.
    if fixed.is_none() && !located.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
        // A capture of the tracked head waits for tracking.
        headset.capture.pending = capture;
        headset
            .stream
            .end(time, xr::EnvironmentBlendMode::OPAQUE, &[])
            .map_err(|e| format!("end: {e}"))?;
        if !headset.untracked {
            log(true, "tracking lost: frames show nothing until the head is tracked again");
        }
        headset.untracked = true;
        headset.perf.frame(wait_ms, 0.0, 0.0, false, None);
        return Ok(());
    }
    if std::mem::take(&mut headset.untracked) {
        log(false, "tracking again");
    }
    let swapchain = headset.swapchain.as_mut().unwrap();
    let image = swapchain
        .handle
        .acquire_image()
        .map_err(|e| format!("acquire: {e}"))?;
    swapchain
        .handle
        .wait_image(xr::Duration::INFINITE)
        .map_err(|e| format!("wait image: {e}"))?;
    let stereo = XrStereo {
        target: swapchain.textures[image as usize].clone(),
        size: swapchain.size,
        views: [view(views[0].pose), view(views[1].pose)],
        projections: [
            projection(views[0].fov, NEAR, FAR),
            projection(views[1].fov, NEAR, FAR),
        ],
    };
    // The world's clock: the frame's predicted display time on the host's.
    let xr_ms = time.as_nanos() as f64 / 1e6;
    let offset = *headset.clock_offset.get_or_insert_with(|| {
        let offset = host_now_ms - xr_ms;
        log(false, &format!("clocks: host − headset = {offset:.1} ms"));
        if offset.abs() < 1000.0 { 0.0 } else { offset }
    });
    if headset.canvas.is_none() {
        log(false, &format!("canvas {id} draws in the headset"));
        headset.canvas = Some(id);
    }
    let encode = Instant::now();
    let drawn = module.render_xr_stereo(id, xr_ms + offset, swapchain.format, &stereo);
    let encode_ms = encode.elapsed().as_secs_f64() * 1e3;
    if capture.is_some() && drawn.is_some() {
        if let Some(gpu) = module.gpu() {
            let what = fixed.as_deref().unwrap_or("the tracked head");
            headset
                .capture
                .take(gpu, &stereo.target, swapchain.format, swapchain.size, what);
        }
    }
    // Hand the image back in the layout the runtime expects.
    if let Some(gpu) = module.gpu() {
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("exact xr release"),
        });
        encoder.transition_resources(
            std::iter::empty::<wgpu::BufferTransition<&wgpu::Buffer>>(),
            std::iter::once(wgpu::TextureTransition {
                texture: &stereo.target,
                selector: None,
                state: wgpu::TextureUses::COLOR_TARGET,
            }),
        );
        gpu.queue.submit([encoder.finish()]);
    }
    swapchain
        .handle
        .release_image()
        .map_err(|e| format!("release: {e}"))?;
    if drawn.is_none() {
        let why = module.error.clone();
        let _ = headset.stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &[]);
        return Err(format!("canvas {id}: {why}"));
    }
    let (w, h) = swapchain.size;
    let rect = xr::Rect2Di {
        offset: xr::Offset2Di { x: 0, y: 0 },
        extent: xr::Extent2Di {
            width: w as i32,
            height: h as i32,
        },
    };
    let projection_views = [0, 1].map(|i| {
        xr::CompositionLayerProjectionView::new()
            .pose(views[i].pose)
            .fov(views[i].fov)
            .sub_image(
                xr::SwapchainSubImage::new()
                    .swapchain(&swapchain.handle)
                    .image_array_index(i as u32)
                    .image_rect(rect),
            )
    });
    let layer = xr::CompositionLayerProjection::new()
        .space(space)
        .views(&projection_views);
    headset
        .stream
        .end(time, xr::EnvironmentBlendMode::OPAQUE, &[&layer])
        .map_err(|e| format!("end: {e}"))?;
    headset.frames += 1;
    if headset.frames == 1 {
        log(false, "first frame submitted");
    }
    let cpu_ms = started.elapsed().as_secs_f64() * 1e3;
    let mode = module.xr_stereo_mode(id);
    headset.perf.frame(wait_ms, cpu_ms, encode_ms, true, Some(mode));
    let _ = (&headset.vk_instance, headset.hal_device_ready);
    Ok(())
}
