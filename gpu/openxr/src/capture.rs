//! The headset's debug eye capture and its per-second log line
//! (`docs/quest.md`).

use crate::module::Module;
use crate::platform::{self, log};
use crate::pose::{self, Request};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

/// The debug eye capture (`docs/quest.md`, "Seeing what the headset shows"):
/// off until the file `capture` appears in the app's external files
/// directory; then the next stereo frame's two layers are read back and
/// written beside it as `eye-<n>-left.png` and `eye-<n>-right.png`.
#[derive(Default)]
pub(super) struct Capture {
    /// The external files directory, once looked up (None inside: none).
    dir: Option<Option<PathBuf>>,
    /// Frames since the trigger was last looked for.
    since_poll: u32,
    /// The trigger's modification time and length when it could not be
    /// removed: the same file is not taken again.
    seen: Option<(SystemTime, u64)>,
    /// A capture the next drawn frame takes.
    pub(super) pending: Option<Request>,
    /// Captures taken in this process.
    taken: u32,
    /// A fixed head pose every frame draws from (the `xr-hold` file), for
    /// measurements that must see the same picture run after run.
    pub(super) hold: Option<pose::StagePose>,
    /// The hold file's last stamp, as `seen` for the trigger.
    hold_seen: Option<(SystemTime, u64)>,
}

/// Frames between looks for the trigger: about twice a second.
const CAPTURE_POLL_FRAMES: u32 = 36;
/// The trigger's file name in the external files directory.
const CAPTURE_TRIGGER: &str = "capture";
/// The held pose's file name (`docs/quest.md`, "Holding the head still"):
/// `stage …` or `look …` as the trigger takes them holds that pose for
/// every frame; `head` or `release` gives the frames back to the tracked head.
const HOLD_FILE: &str = "xr-hold";

impl Capture {
    /// Look for the trigger every [`CAPTURE_POLL_FRAMES`] frames.
    pub(super) fn poll(&mut self) {
        self.since_poll += 1;
        if self.pending.is_some() || self.since_poll < CAPTURE_POLL_FRAMES {
            return;
        }
        self.since_poll = 0;
        let dir = self.dir.get_or_insert_with(platform::files_dir).clone();
        let Some(dir) = dir else { return };
        self.poll_hold(&dir);
        let trigger = dir.join(CAPTURE_TRIGGER);
        let Ok(meta) = std::fs::metadata(&trigger) else {
            self.seen = None;
            return;
        };
        let stamp = (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        );
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

    /// Take a new `xr-hold` file: the pose every frame draws from until the
    /// next one releases it.
    fn poll_hold(&mut self, dir: &std::path::Path) {
        let file = dir.join(HOLD_FILE);
        let Ok(meta) = std::fs::metadata(&file) else {
            self.hold_seen = None;
            return;
        };
        let stamp = (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        );
        if self.hold_seen == Some(stamp) {
            return;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if std::fs::remove_file(&file).is_err() {
            self.hold_seen = Some(stamp);
        }
        let text = if text.trim() == "release" {
            "head"
        } else {
            text.as_str()
        };
        match Request::parse(text) {
            Ok(request) => {
                self.hold = request.pose;
                log(false, &format!("hold: {:?}", self.hold));
            }
            Err(e) => log(true, &format!("hold: {e}")),
        }
    }

    /// Read both layers of the frame just drawn back through the module and
    /// write them as PNGs beside the trigger, on a thread of their own.
    pub(super) fn take(&mut self, module: &Module, size: (u32, u32), what: &str) {
        let Some(Some(dir)) = self.dir.clone() else {
            return;
        };
        let (w, h) = size;
        let len = w as usize * h as usize * 4;
        let mut layers = Vec::with_capacity(2);
        for layer in 0..2 {
            match module.read_stereo(layer, len) {
                Ok(rgba) => layers.push(rgba),
                Err(e) => {
                    log(true, &format!("capture: the read back failed: {e}"));
                    return;
                }
            }
        }
        self.taken += 1;
        let n = self.taken;
        let what = what.to_owned();
        std::thread::spawn(move || {
            let mut written = Vec::new();
            for (rgba, eye) in layers.iter().zip(["left", "right"]) {
                let path = dir.join(format!("eye-{n}-{eye}.png"));
                match std::fs::write(&path, pose::png_rgb(w, h, &pose::rgb(rgba))) {
                    Ok(()) => written.push(path.display().to_string()),
                    Err(e) => log(true, &format!("capture: {}: {e}", path.display())),
                }
            }
            log(
                false,
                &format!("capture {n} ({what}): {}", written.join(" ")),
            );
        });
    }
}

/// Per-second numbers for the log line.
#[derive(Default)]
pub(super) struct Perf {
    since: Option<Instant>,
    frames: u32,
    skipped: u32,
    cpu_ms: f64,
    encode_ms: f64,
    wait_ms: f64,
    worst_ms: f64,
    /// How the last frame drew (the module's `gpu_xr_stereo_mode`).
    mode: String,
}

impl Perf {
    pub(super) fn frame(
        &mut self,
        wait_ms: f64,
        cpu_ms: f64,
        encode_ms: f64,
        rendered: bool,
        mode: Option<String>,
    ) {
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
                    if self.mode.is_empty() { "none" } else { &self.mode },
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
                mode: std::mem::take(&mut self.mode),
                ..Default::default()
            };
        }
    }
}
