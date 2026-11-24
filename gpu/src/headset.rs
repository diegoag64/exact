//! Whether this process draws into a native headset, and the host's debug
//! switches (`docs/quest.md`).

use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_SWITCHES: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static HEADSET: AtomicBool = AtomicBool::new(false);

/// Set a native host's debug switches once, at start: whitespace-separated
/// words (the Quest host's `xr-debug` file, `docs/quest.md`).
pub fn set_debug_switches(words: String) {
    let _ = DEBUG_SWITCHES.set(words);
}
/// Whether the host turned debug switch `name` on ([`set_debug_switches`]);
/// always false where none were set.
pub fn debug_switch(name: &str) -> bool {
    DEBUG_SWITCHES
        .get()
        .is_some_and(|words| words.split_whitespace().any(|w| w == name))
}
/// One line to the headset's log (`exact-xr`, logcat) where a debug switch
/// took effect, so a measurement can confirm what the engine applied
/// (`docs/quest.md`, "Debug switches"); nothing elsewhere.
pub fn debug_applied(line: &str) {
    #[cfg(target_os = "android")]
    if headset() {
        crate::vulkan::log(&format!("applied: {line}"));
    }
    #[cfg(not(target_os = "android"))]
    let _ = line;
}
/// Set by a headset host while its session draws this module's canvas
/// (`gpu_headset`).
pub fn set_headset(on: bool) {
    HEADSET.store(on, Ordering::Relaxed);
}
/// Whether this process draws into a native headset ([`set_headset`]).
pub fn headset() -> bool {
    HEADSET.load(Ordering::Relaxed)
}
