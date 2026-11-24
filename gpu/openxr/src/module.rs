//! The GPU module's exports this artifact calls: the module's C ABI
//! (`gpu/src/native.rs`, `gpu/src/vulkan.rs`), looked up by name in the
//! image the host already loaded. Every call is made on the host's thread
//! that renders, as the host's own are, except the provider's hooks, which
//! the module calls itself.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::OnceLock;

/// `exact_gpu::vulkan::VulkanProvider`, in its C layout.
#[repr(C)]
pub(crate) struct VulkanProvider {
    pub context: *mut c_void,
    pub api_version: u32,
    pub multiview_views: u32,
    pub create_instance:
        unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> c_int,
    pub physical_device: unsafe extern "C" fn(*mut c_void, u64, *mut u64) -> c_int,
    pub create_device:
        unsafe extern "C" fn(*mut c_void, *const c_void, u64, *const c_void, *mut u64) -> c_int,
    pub device_made: unsafe extern "C" fn(*mut c_void, u64, u64, u64, *const u8, usize) -> c_int,
}
// SAFETY: the context is null; the hooks are plain functions.
unsafe impl Send for VulkanProvider {}
unsafe impl Sync for VulkanProvider {}

/// `exact_gpu::vulkan::StereoTarget`, in its C layout.
#[repr(C)]
pub(crate) struct StereoTarget {
    pub image: u64,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub usage: u32,
    pub views: [[f32; 16]; 2],
    pub projections: [[f32; 16]; 2],
}

/// `exact_gpu::vulkan::usage`: sampled, a copy's source and destination.
pub(crate) const USAGE_ALL: u32 = 1 | 2 | 4;

macro_rules! exports {
    ($($field:ident: $name:literal => $ty:ty,)*) => {
        pub(crate) struct Module {
            $($field: $ty,)*
        }

        impl Module {
            /// # Safety
            /// `handle` is the GPU module's image, open for the process.
            unsafe fn bind(handle: *mut c_void) -> Result<Self, String> {
                Ok(Self {
                    $($field: {
                        // SAFETY: a NUL-terminated name in a live image.
                        let at = unsafe { dlsym(handle, concat!($name, "\0").as_ptr().cast()) };
                        if at.is_null() {
                            return Err(format!("the GPU module has no {}", $name));
                        }
                        // SAFETY: the module declares this signature (`gpu/src`).
                        unsafe { std::mem::transmute::<*mut c_void, $ty>(at) }
                    },)*
                })
            }
        }
    };
}

exports! {
    vulkan_provider: "gpu_vulkan_provider" => unsafe extern "C" fn(*const VulkanProvider),
    render_stereo: "gpu_render_stereo" => unsafe extern "C" fn(u32, *const StereoTarget, f64) -> u32,
    read_stereo: "gpu_read_stereo" => unsafe extern "C" fn(u32, *mut u8, usize) -> u32,
    input_xr: "gpu_input_xr" => unsafe extern "C" fn(u32, *const f32, usize, f64) -> u32,
    input: "gpu_input" => unsafe extern "C" fn(u32, *const u8, usize) -> u32,
    lifecycle: "gpu_lifecycle" => unsafe extern "C" fn(u32, u32),
    headset: "gpu_headset" => unsafe extern "C" fn(bool),
    debug_switches: "gpu_debug_switches" => unsafe extern "C" fn(*const u8, usize),
    vulkan_format: "gpu_vulkan_format" => unsafe extern "C" fn(u32) -> u32,
    xr_focus: "gpu_xr_focus" => unsafe extern "C" fn(u32, *mut f32) -> u32,
    xr_stereo_mode: "gpu_xr_stereo_mode" => unsafe extern "C" fn(u32) -> u32,
    error: "gpu_error" => unsafe extern "C" fn() -> u32,
    out_ptr: "gpu_out_ptr" => unsafe extern "C" fn() -> *const u8,
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
/// Bionic's `RTLD_NOW` and `RTLD_NOLOAD` on a 64-bit ABI.
const RTLD_NOW: c_int = 2;
const RTLD_NOLOAD: c_int = 4;

static MODULE: OnceLock<Module> = OnceLock::new();

/// Bind the GPU module the host loaded from `path`.
pub(crate) fn bind(path: &str) -> Result<&'static Module, String> {
    if let Some(module) = MODULE.get() {
        return Ok(module);
    }
    let name = CString::new(path).map_err(|_| "a GPU module path with a NUL")?;
    // SAFETY: a NUL-terminated path; NOLOAD only finds an image already open.
    let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_NOLOAD) };
    if handle.is_null() {
        return Err(format!("the GPU module {path} is not loaded"));
    }
    // SAFETY: the host's image, which stays loaded for the process.
    let module = unsafe { Module::bind(handle) }?;
    Ok(MODULE.get_or_init(|| module))
}

/// The module [`bind`] bound, once it has.
pub(crate) fn get() -> Option<&'static Module> {
    MODULE.get()
}

impl Module {
    /// The text in the module's output buffer, `len` bytes.
    fn out(&self, len: u32) -> String {
        // SAFETY: the module's buffer, `len` bytes until its next call.
        let ptr = unsafe { (self.out_ptr)() };
        if ptr.is_null() || len == 0 {
            return String::new();
        }
        // SAFETY: as above.
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
            .into_owned()
    }

    /// The module's last failure.
    pub(crate) fn error(&self) -> String {
        // SAFETY: the module's own export.
        let len = unsafe { (self.error)() };
        self.out(len)
    }

    pub(crate) fn set_provider(&self, provider: &'static VulkanProvider) {
        // SAFETY: a provider that lives for the process.
        unsafe { (self.vulkan_provider)(provider) }
    }

    pub(crate) fn set_debug_switches(&self, words: &str) {
        // SAFETY: `words.len()` readable bytes.
        unsafe { (self.debug_switches)(words.as_ptr(), words.len()) }
    }

    pub(crate) fn set_headset(&self, on: bool) {
        // SAFETY: the module's own export.
        unsafe { (self.headset)(on) }
    }

    pub(crate) fn lifecycle(&self, id: u32, code: u32) {
        // SAFETY: the module's own export.
        unsafe { (self.lifecycle)(id, code) }
    }

    /// The `VkFormat` canvas `id` draws its page in, if any.
    pub(crate) fn vulkan_format(&self, id: u32) -> Option<u32> {
        // SAFETY: the module's own export.
        Some(unsafe { (self.vulkan_format)(id) }).filter(|f| *f != 0)
    }

    pub(crate) fn xr_focus(&self, id: u32) -> Option<[f32; 3]> {
        let mut out = [0f32; 3];
        // SAFETY: three writable numbers.
        (unsafe { (self.xr_focus)(id, out.as_mut_ptr()) } == 1).then_some(out)
    }

    pub(crate) fn xr_stereo_mode(&self, id: u32) -> String {
        // SAFETY: the module's own export.
        let len = unsafe { (self.xr_stereo_mode)(id) };
        self.out(len)
    }

    /// Deliver a headset's input; the module's refusal when it refused.
    pub(crate) fn input_xr(&self, id: u32, floats: &[f32], at_ms: f64) -> Result<(), String> {
        // SAFETY: `floats.len()` readable numbers.
        match unsafe { (self.input_xr)(id, floats.as_ptr(), floats.len(), at_ms) } {
            0 => Ok(()),
            _ => Err(self.error()),
        }
    }

    /// Post `text` to the canvas, as Contract's `postMessage`.
    pub(crate) fn message(&self, id: u32, text: &str, at_ms: f64) -> Result<(), String> {
        let event = format!(r#"{{"t":"message","text":{},"at":{at_ms}}}"#, quoted(text));
        // SAFETY: `event.len()` readable bytes.
        match unsafe { (self.input)(id, event.as_ptr(), event.len()) } {
            0 => Ok(()),
            _ => Err(self.error()),
        }
    }

    /// Draw canvas `id` into `target`: whether it wants another frame.
    pub(crate) fn render_stereo(
        &self,
        id: u32,
        target: &StereoTarget,
        now_ms: f64,
    ) -> Result<bool, String> {
        // SAFETY: one readable target.
        match unsafe { (self.render_stereo)(id, target, now_ms) } {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.error()),
        }
    }

    /// Layer `layer` of the last stereo frame, `len` RGBA bytes.
    pub(crate) fn read_stereo(&self, layer: u32, len: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; len];
        // SAFETY: `len` writable bytes.
        match unsafe { (self.read_stereo)(layer, out.as_mut_ptr(), len) } {
            0 => Ok(out),
            _ => Err(self.error()),
        }
    }
}

/// `text` as a JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
