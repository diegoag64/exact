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
//! The pieces that need no headset (each eye's matrices, the capture's
//! trigger grammar and PNG writer, the controllers' debug script) build and
//! test on every host; the rest is Android's.
#![allow(unsafe_code)]

pub mod pose;
pub mod script;

#[cfg(target_os = "android")]
mod capture;
#[cfg(target_os = "android")]
mod controls;
#[cfg(target_os = "android")]
mod device;
#[cfg(target_os = "android")]
mod java;
#[cfg(target_os = "android")]
mod module;
#[cfg(target_os = "android")]
mod session;

#[cfg(target_os = "android")]
mod exports {
    use crate::java::log;
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Keep the Java VM and the application `Context`. 0 on success.
    ///
    /// # Safety
    /// `env` is the calling thread's live `JNIEnv*`, `vm` the process's
    /// `JavaVM*` and `context` a live reference to the application `Context`.
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
            module.set_debug_switches(crate::java::debug_switches());
            module.set_provider(provider);
            Ok::<_, String>(())
        });
        match opened {
            Ok(Ok(())) => 0,
            Ok(Err(e)) => {
                log(true, &format!("{e}: the app draws to its window"));
                1
            }
            Err(_) => {
                log(true, "xr_open panicked: the app draws to its window");
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
}
