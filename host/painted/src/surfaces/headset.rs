//! A headset app's OpenXR artifact (`gpu/openxr`, `docs/quest.md`): the APK
//! of a Quest app packages `libexact_openxr.so` beside its GPU module, and
//! no other APK does. Where it is there, the host loads it once, before the
//! primary GPU module makes its device, and hands it that module ([`attach`]);
//! each frame the artifact draws the headset's canvas instead of the module's
//! `gpu_render` ([`render`]).
//!
//! On Windows (`docs/quest.md`, "PC VR on Windows") a PC VR package carries
//! `exact_openxr.dll` beside the executable, and the host loads it only when
//! VR is asked for ([`requested`]: `--vr`, or `EXACT_XR=1`); otherwise the
//! game is the ordinary flat one. When the runtime has no headset the person
//! is told why, and may try again or play in the window. While a session
//! exists the window's event loop drives the headset's frames
//! (`Presenter::headset_frame`), the window shows the page around the
//! canvas, and the canvas's lifecycle follows the headset, not the window.
#![allow(unsafe_code)]
use libloading::Library;
use std::{path::Path, sync::OnceLock};

#[cfg(target_os = "android")]
const ARTIFACT: &str = "libexact_openxr.so";
#[cfg(windows)]
const ARTIFACT: &str = "exact_openxr.dll";

struct Headset {
    /// Kept loaded for the process, as an initialized artifact is.
    _library: Library,
    render: unsafe extern "C" fn(u32, f64) -> i32,
    #[cfg(windows)]
    state: unsafe extern "C" fn() -> u32,
    #[cfg(windows)]
    status: unsafe extern "C" fn(*mut u8, usize) -> usize,
}

/// None where the APK has no artifact, or its runtime did not answer.
static HEADSET: OnceLock<Option<Headset>> = OnceLock::new();

fn log(line: &str) {
    #[cfg(target_os = "android")]
    crate::android::log(line);
    #[cfg(windows)]
    eprintln!("exact-windows: {line}");
}

/// Load the artifact where the APK packages it and give it the primary GPU
/// module, opened and initialized from `module`, before that module makes
/// its device. Once a process; a failure is logged and the app draws to its
/// window.
pub(super) fn attach(module: &Path) {
    #[cfg(windows)]
    if !requested() {
        return;
    }
    HEADSET.get_or_init(|| match open(module) {
        Ok(headset) => headset,
        Err(e) => {
            log(&format!("{ARTIFACT}: {e}; the app draws to its window"));
            None
        }
    });
}

/// Whether this launch asked for VR: `--vr` among the arguments, or
/// `EXACT_XR=1` in the environment.
#[cfg(windows)]
pub(crate) fn requested() -> bool {
    static ASKED: OnceLock<bool> = OnceLock::new();
    *ASKED.get_or_init(|| {
        std::env::args().skip(1).any(|a| a == "--vr")
            || std::env::var("EXACT_XR").is_ok_and(|v| v == "1")
    })
}

#[cfg(target_os = "android")]
fn library() -> Result<Option<Library>, String> {
    // SAFETY: the APK's own artifact, by its name in the app's library
    // directory; its constructors do nothing.
    Ok(match unsafe { Library::new(ARTIFACT) } {
        Ok(library) => Some(library),
        Err(_) => match std::env::var_os("EXACT_NATIVE_LIBS") {
            Some(dir) => unsafe { Library::new(Path::new(&dir).join(ARTIFACT)) }.ok(),
            None => None,
        },
    })
}

#[cfg(windows)]
fn library() -> Result<Option<Library>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe
        .parent()
        .ok_or("the executable has no directory")?
        .join(ARTIFACT);
    if !path.exists() {
        let why = format!(
            "this build has no {ARTIFACT} beside {} (build the PC VR package: app.json \
             \"host\": {{\"windows\": {{\"xr\": \"openxr\"}}}})",
            exe.display()
        );
        tell(&format!(
            "VR could not start: {why}.\n\nThe game opens in its window instead."
        ));
        return Err(why);
    }
    // SAFETY: the package's own artifact beside the executable; its
    // constructors do nothing.
    unsafe { Library::new(&path) }
        .map(Some)
        .map_err(|e| format!("{} did not load: {e}", path.display()))
}

fn open(module: &Path) -> Result<Option<Headset>, String> {
    let Some(library) = library()? else {
        return Ok(None);
    };
    // SAFETY: the artifact's declared signatures (gpu/openxr/src/lib.rs).
    let (open, render) = unsafe {
        (
            std::mem::transmute::<*const (), unsafe extern "C" fn(*const u8, usize) -> u32>(
                symbol(&library, b"xr_open")?,
            ),
            std::mem::transmute::<*const (), unsafe extern "C" fn(u32, f64) -> i32>(symbol(
                &library,
                b"xr_render",
            )?),
        )
    };
    #[cfg(target_os = "android")]
    {
        // SAFETY: as above.
        let init = unsafe {
            std::mem::transmute::<*const (), super::android_init::Entry>(symbol(
                &library,
                b"xr_android_init",
            )?)
        };
        match super::android_init::call(init, || Ok(()))? {
            Ok(0) => {}
            Ok(_) => return Err("its Android initialization failed".into()),
            Err(threw) => return Err(format!("its Android initialization: {threw}")),
        }
    }
    #[cfg(windows)]
    // SAFETY: as above.
    let (state, status) = unsafe {
        (
            std::mem::transmute::<*const (), unsafe extern "C" fn() -> u32>(symbol(
                &library,
                b"xr_state",
            )?),
            std::mem::transmute::<*const (), unsafe extern "C" fn(*mut u8, usize) -> usize>(
                symbol(&library, b"xr_status")?,
            ),
        )
    };
    let path = module.to_string_lossy();
    // SAFETY: `path.len()` readable bytes. The artifact logs why it opened
    // no session.
    #[cfg(target_os = "android")]
    if unsafe { open(path.as_ptr(), path.len()) } != 0 {
        return Ok(None);
    }
    // On a PC, a runtime with no headset yet (Virtual Desktop not
    // connected) is asked again while the person chooses to.
    #[cfg(windows)]
    while unsafe { open(path.as_ptr(), path.len()) } != 0 {
        let why = read_status(status);
        log(&format!("VR: {why}"));
        if !retry(&why) {
            return Err(why);
        }
    }
    Ok(Some(Headset {
        _library: library,
        render,
        #[cfg(windows)]
        state,
        #[cfg(windows)]
        status,
    }))
}

/// # Safety
/// `library` is the artifact, loaded.
unsafe fn symbol(library: &Library, name: &[u8]) -> Result<*const (), String> {
    // SAFETY: the caller's contract.
    unsafe { library.get::<*const ()>(name) }
        .map(|at| *at)
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn read_status(status: unsafe extern "C" fn(*mut u8, usize) -> usize) -> String {
    let mut buffer = vec![0u8; 1024];
    // SAFETY: `buffer.len()` writable bytes.
    let n = unsafe { status(buffer.as_mut_ptr(), buffer.len()) }.min(buffer.len());
    String::from_utf8_lossy(&buffer[..n]).into_owned()
}

#[cfg(windows)]
fn message_box(
    text: &str,
    buttons: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> i32 {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_SETFOREGROUND, MB_TOPMOST};
    let title: Vec<u16> = "VR".encode_utf16().chain([0]).collect();
    let body: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: two NUL-terminated wide strings; no owner window.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            buttons | MB_SETFOREGROUND | MB_TOPMOST,
        )
    }
    .0
}

/// Ask whether to try the runtime again. Under an agent's drive (no person
/// to answer) nothing is asked.
#[cfg(windows)]
fn retry(why: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDRETRY, MB_ICONWARNING, MB_RETRYCANCEL};
    if std::env::var("EXACT_AGENT").as_deref() == Ok("1") {
        return false;
    }
    message_box(
        &format!(
            "VR could not start: {why}.\n\nRetry once the headset is connected, or Cancel to \
             play in the window."
        ),
        MB_RETRYCANCEL | MB_ICONWARNING,
    ) == IDRETRY.0
}

#[cfg(windows)]
fn tell(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK};
    if std::env::var("EXACT_AGENT").as_deref() == Ok("1") {
        return;
    }
    message_box(text, MB_OK | MB_ICONWARNING);
}

/// Canvas `id`'s frame while a headset session draws: `gpu_render`'s result,
/// or None to render it as usual.
pub(crate) fn render(id: u32, now: f64) -> Option<u32> {
    let headset = HEADSET.get()?.as_ref()?;
    // SAFETY: the artifact's export, loaded for the process.
    let code = unsafe { (headset.render)(id, now) };
    u32::try_from(code).ok()
}

/// The runtime made the primary module's device (whether or not its session
/// still runs): the window cannot present that device's frames itself, so
/// it shows the page on the CPU.
#[cfg(windows)]
pub(crate) fn attached() -> bool {
    state() != 0
}

/// After the primary module made its device: where the runtime answered but
/// made no session (its Vulkan device or session refused), the module made
/// its ordinary device and the person is told why the game is flat.
#[cfg(windows)]
pub(super) fn device_made() {
    if matches!(HEADSET.get(), Some(Some(_))) && state() == 0 {
        let why = status()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "the runtime made no session".into());
        log(&format!("VR: {why}; the app draws to its window"));
        tell(&format!(
            "VR could not start: {why}.\n\nThe game opens in its window instead."
        ));
    }
}

/// The session's state (`xr_state`): 0 none, 1 made, 2 running, 3 focused,
/// 4 ended.
#[cfg(windows)]
pub(crate) fn state() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SAID: AtomicU32 = AtomicU32::new(0);
    let state = match HEADSET.get() {
        // SAFETY: the artifact's export, loaded for the process.
        Some(Some(headset)) => unsafe { (headset.state)() },
        _ => 0,
    };
    // Each change once, on standard error, with what it means.
    if SAID.swap(state, Ordering::Relaxed) != state {
        let name = ["none", "made", "running", "focused", "ended"]
            .get(state as usize)
            .copied()
            .unwrap_or("unknown");
        log(&format!(
            "VR session {name}: {}",
            status().unwrap_or_default()
        ));
    }
    state
}

/// A session exists and the headset, not the window, draws the primary
/// module's canvas.
#[cfg(windows)]
pub(crate) fn drawing() -> bool {
    (1..=3).contains(&state())
}

/// What the session last said a person should know.
#[cfg(windows)]
pub(crate) fn status() -> Option<String> {
    match HEADSET.get() {
        Some(Some(headset)) => Some(read_status(headset.status)),
        _ => None,
    }
}
