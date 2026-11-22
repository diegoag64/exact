//! Whether this process draws into a native headset, and the host's debug
//! switches (`docs/quest.md`).

pub(crate) static DEBUG_SWITCHES: std::sync::OnceLock<String> = std::sync::OnceLock::new();
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
    #[cfg(all(target_os = "android", feature = "openxr"))]
    crate::openxr::log(false, &format!("applied: {line}"));
    #[cfg(not(all(target_os = "android", feature = "openxr")))]
    let _ = line;
}
/// Whether this process draws into a native headset: an OpenXR session
/// made with the device (the `openxr` feature on Android, `docs/quest.md`).
pub fn headset() -> bool {
    #[cfg(all(target_os = "android", feature = "openxr"))]
    return crate::openxr::active();
    #[cfg(not(all(target_os = "android", feature = "openxr")))]
    false
}
/// The module's Android initialization for a headset session, which
/// [`crate::module!`] defines: with the `openxr` feature on Android, a
/// `gpu_android_init` that keeps the Java VM and Context for the session.
#[cfg(all(target_os = "android", feature = "openxr"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __openxr_android_init {
    () => {
        $crate::android_init!(|android| $crate::openxr::android_init(android));
    };
}
/// Nothing without a headset session.
#[cfg(not(all(target_os = "android", feature = "openxr")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __openxr_android_init {
    () => {};
}
