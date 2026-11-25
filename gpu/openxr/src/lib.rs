//! The OpenXR host for a Meta Quest app (`docs/quest.md`): a separate
//! artifact, `libexact_openxr.so`, that only a Quest APK packages (app.json
//! `"host": {"android": {"xr": "quest"}}`) and that the Android host loads
//! where it finds it. It makes the headset's session, reads its controllers
//! and takes the debug eye captures, and reaches the GPU module only through
//! the module's C exports (`gpu/src/vulkan.rs`): the runtime makes the
//! module's Vulkan device, and each headset frame the module draws a canvas
//! into the session's swapchain image.
//!
//! The host calls, on Android:
//!
//! 1. `xr_android_init(vm, env, context)`, once, as it calls a GPU artifact's
//!    `gpu_android_init` (LLP 1009 D11): the process's Java VM, the calling
//!    thread's environment and the application `Context`. 0 on success.
//! 2. `xr_open(path, len)` with the primary GPU module's path, once it is
//!    open and initialized and before it makes its device. 0 when the module
//!    will make its device through the headset's runtime; 1 where there is
//!    no loader or runtime (a phone) or it refused, logged, and the app draws
//!    to its window as usual.
//! 3. `xr_render(id, now_ms)` where it would call the module's `gpu_render`
//!    for canvas `id`: -1 to render it as usual (no session, or one that
//!    ended); else `gpu_render`'s result, the first canvas asked for having
//!    drawn into the headset (paced by the headset's frame) and any other
//!    drawn nowhere.
//!
//! On Windows (`docs/quest.md`, "PC VR on Windows") the same artifact is
//! `exact_openxr.dll`, which a PC VR package carries beside the Khronos
//! loader (`openxr_loader.dll`) and the Windows host loads only when VR is
//! asked for (`--vr`, `EXACT_XR=1`). The host calls `xr_open` and
//! `xr_render` as above (no `xr_android_init`), and `xr_state` and
//! `xr_status` to say what happened: whether a session draws, and why not.
//!
//! The pieces that need no headset (each eye's matrices, the capture's
//! trigger grammar and PNG writer, the controllers' debug script) build and
//! test on every host; the rest is Android's and Windows'.
#![allow(unsafe_code)]

pub mod pose;
pub mod script;

#[cfg(any(target_os = "android", windows))]
mod capture;
#[cfg(any(target_os = "android", windows))]
mod controls;
#[cfg(any(target_os = "android", windows))]
mod device;
#[cfg(target_os = "android")]
mod java;
#[cfg(any(target_os = "android", windows))]
mod module;
#[cfg(any(target_os = "android", windows))]
mod session;

/// Where the debug files are, the switches and the log: the app's Java
/// side on a Quest, the executable's directory on a PC.
#[cfg(target_os = "android")]
use java as platform;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
use desktop as platform;

#[cfg(any(target_os = "android", windows))]
mod exports {
    use crate::platform::log;
    #[cfg(target_os = "android")]
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Keep the Java VM and the application `Context`. 0 on success.
    ///
    /// # Safety
    /// `env` is the calling thread's live `JNIEnv*`, `vm` the process's
    /// `JavaVM*` and `context` a live reference to the application `Context`.
    #[cfg(target_os = "android")]
    #[no_mangle]
    pub unsafe extern "C" fn xr_android_init(
        vm: *mut c_void,
        env: *mut c_void,
        context: *mut c_void,
    ) -> u32 {
        // SAFETY: the caller's contract.
        match unsafe { crate::java::android_init(vm, env, context) } {
            Ok(()) => 0,
            Err(e) => {
                log(true, &format!("android init: {e}"));
                1
            }
        }
    }

    /// Open the headset's runtime and give the GPU module at `path` its
    /// device provider. 0 when the module makes its device through the
    /// runtime, 1 when it makes it as usual.
    ///
    /// # Safety
    /// `path` is `len` readable bytes.
    #[no_mangle]
    pub unsafe extern "C" fn xr_open(path: *const u8, len: usize) -> u32 {
        if path.is_null() {
            return 1;
        }
        // SAFETY: the caller's contract.
        let path = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(path, len) });
        let opened = catch_unwind(|| {
            let module = crate::module::bind(&path)?;
            let provider = crate::device::open()?;
            module.set_debug_switches(crate::platform::debug_switches());
            module.set_provider(provider);
            Ok::<_, String>(())
        });
        match opened {
            Ok(Ok(())) => 0,
            Ok(Err(e)) => {
                log(true, &format!("{e}: the app draws to its window"));
                crate::session::set_status(&e);
                1
            }
            Err(_) => {
                log(true, "xr_open panicked: the app draws to its window");
                crate::session::set_status("the OpenXR artifact failed while opening the runtime");
                1
            }
        }
    }

    /// Canvas `id`'s frame: -1 to render it as usual, else `gpu_render`'s
    /// result.
    #[no_mangle]
    pub extern "C" fn xr_render(id: u32, now_ms: f64) -> i32 {
        let Some(module) = crate::module::get() else {
            return -1;
        };
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            crate::session::render(module, id, now_ms)
        }));
        match rendered {
            Ok(Some(code)) => code as i32,
            Ok(None) => -1,
            Err(_) => {
                log(true, "xr_render panicked");
                -1
            }
        }
    }

    /// Whether a headset session draws: 0 none was made (or `xr_open` was
    /// not called or refused), 1 one was made and does not run yet (or
    /// stopped), 2 it runs, 3 it runs with the input focus, 4 it ended or
    /// was lost (the window draws the canvas again).
    #[cfg(windows)]
    #[no_mangle]
    pub extern "C" fn xr_state() -> u32 {
        catch_unwind(crate::session::state).unwrap_or(0)
    }

    /// The last thing the session says a person should know (why no
    /// session was made, or what it does now), UTF-8, copied into `out` up
    /// to `cap` bytes; its whole length.
    ///
    /// # Safety
    /// `out` is `cap` writable bytes, or null with `cap` 0.
    #[cfg(windows)]
    #[no_mangle]
    pub unsafe extern "C" fn xr_status(out: *mut u8, cap: usize) -> usize {
        let text = crate::session::status();
        let n = text.len().min(cap);
        if n > 0 && !out.is_null() {
            // SAFETY: the caller's contract.
            unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, n) };
        }
        text.len()
    }
}
