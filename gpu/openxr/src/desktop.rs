//! The PC side of the session (`docs/quest.md`, "PC VR on Windows"): where
//! the debug files are, the debug switches and the log, as `java.rs` has
//! them on a Quest.
//!
//! - The files directory is `EXACT_XR_DIR`, else `xr` beside the executable
//!   (made when first needed): `xr-debug`, `capture`, `xr-hold` and
//!   `xr-input` are read there, and the eye captures written there.
//! - The debug switches are `EXACT_XR_DEBUG`'s words and the `xr-debug`
//!   file's.
//! - The log is `exact-xr.log` in the files directory, begun anew each
//!   launch, and the process's standard error, each line `exact-xr:`.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The log file, opened at the first line.
static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static STARTED: OnceLock<Instant> = OnceLock::new();

/// One line to the log file and standard error.
pub fn log(warn: bool, line: &str) {
    let at = STARTED.get_or_init(Instant::now).elapsed().as_secs_f64();
    let text = format!(
        "exact-xr: {at:9.3} {}{line}",
        if warn { "warning: " } else { "" }
    );
    eprintln!("{text}");
    let mut file = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        if let Some(dir) = files_dir() {
            *file = std::fs::File::create(dir.join("exact-xr.log")).ok();
        }
    }
    if let Some(file) = file.as_mut() {
        let _ = writeln!(file, "{text}");
        let _ = file.flush();
    }
}

/// The directory of the running executable.
pub(crate) fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
}

/// The files directory: `EXACT_XR_DIR`, else `xr` beside the executable.
pub(crate) fn files_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::var_os("EXACT_XR_DIR")
            .map(PathBuf::from)
            .or_else(|| exe_dir().map(|d| d.join("xr")))?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .clone()
}

/// The debug switches (`docs/quest.md`, "Debug switches"): `EXACT_XR_DEBUG`'s
/// words and the `xr-debug` file's, read once, before the device exists.
pub(crate) fn debug_switches() -> &'static str {
    static WORDS: OnceLock<String> = OnceLock::new();
    WORDS.get_or_init(|| {
        let mut words = std::env::var("EXACT_XR_DEBUG").unwrap_or_default();
        if let Some(text) =
            files_dir().and_then(|d| std::fs::read_to_string(d.join("xr-debug")).ok())
        {
            words.push(' ');
            words.push_str(&text);
        }
        words
    })
}

/// Whether debug switch `name` is on.
pub(crate) fn debug_switch(name: &str) -> bool {
    debug_switches().split_whitespace().any(|w| w == name)
}
