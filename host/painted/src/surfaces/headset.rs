//! A headset app's OpenXR artifact (`gpu/openxr`, `docs/quest.md`): the APK
//! of a Quest app packages `libexact_openxr.so` beside its GPU module, and
//! no other APK does. Where it is there, the host loads it once, before the
//! primary GPU module makes its device, and hands it that module ([`attach`]);
//! each frame the artifact draws the headset's canvas instead of the module's
//! `gpu_render` ([`render`]).
#![allow(unsafe_code)]
use libloading::Library;
use std::{path::Path, sync::OnceLock};

const ARTIFACT: &str = "libexact_openxr.so";

struct Headset {
    /// Kept loaded for the process, as an initialized artifact is.
    _library: Library,
    render: unsafe extern "C" fn(u32, f64) -> i32,
}

/// None where the APK has no artifact, or its runtime did not answer.
static HEADSET: OnceLock<Option<Headset>> = OnceLock::new();

/// Load the artifact where the APK packages it and give it the primary GPU
/// module, opened and initialized from `module`, before that module makes
/// its device. Once a process; a failure is logged and the app draws to its
/// window.
pub(super) fn attach(module: &Path) {
    HEADSET.get_or_init(|| match open(module) {
        Ok(headset) => headset,
        Err(e) => {
            crate::android::log(&format!("{ARTIFACT}: {e}; the app draws to its window"));
            None
        }
    });
}

fn open(module: &Path) -> Result<Option<Headset>, String> {
    // SAFETY: the APK's own artifact, by its name in the app's library
    // directory; its constructors do nothing.
    let library = match unsafe { Library::new(ARTIFACT) } {
        Ok(library) => library,
        Err(_) => match std::env::var_os("EXACT_NATIVE_LIBS") {
            Some(dir) => match unsafe { Library::new(Path::new(&dir).join(ARTIFACT)) } {
                Ok(library) => library,
                Err(_) => return Ok(None),
            },
            None => return Ok(None),
        },
    };
    // SAFETY: the artifact's declared signatures (gpu/openxr/src/lib.rs).
    let (init, open, render) = unsafe {
        let symbol = |name: &[u8]| {
            library
                .get::<*const ()>(name)
                .map(|at| *at)
                .map_err(|e| e.to_string())
        };
        (
            std::mem::transmute::<*const (), super::android_init::Entry>(symbol(
                b"xr_android_init",
            )?),
            std::mem::transmute::<*const (), unsafe extern "C" fn(*const u8, usize) -> u32>(
                symbol(b"xr_open")?,
            ),
            std::mem::transmute::<*const (), unsafe extern "C" fn(u32, f64) -> i32>(symbol(
                b"xr_render",
            )?),
        )
    };
    match super::android_init::call(init, || Ok(()))? {
        Ok(0) => {}
        Ok(_) => return Err("its Android initialization failed".into()),
        Err(threw) => return Err(format!("its Android initialization: {threw}")),
    }
    let path = module.to_string_lossy();
    // SAFETY: `path.len()` readable bytes. The artifact logs why it opened
    // no session.
    if unsafe { open(path.as_ptr(), path.len()) } != 0 {
        return Ok(None);
    }
    Ok(Some(Headset {
        _library: library,
        render,
    }))
}

/// Canvas `id`'s frame while a headset session draws: `gpu_render`'s result,
/// or None to render it as usual.
pub(super) fn render(id: u32, now: f64) -> Option<u32> {
    let headset = HEADSET.get()?.as_ref()?;
    // SAFETY: the artifact's export, loaded for the process.
    let code = unsafe { (headset.render)(id, now) };
    u32::try_from(code).ok()
}
