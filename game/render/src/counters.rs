//! What one encoded view cost the command stream: the passes it began, the
//! pipelines it bound, the draw commands it issued (a merged multi-draw is
//! one) and the CPU time it took to record them. A binning GPU replays a
//! pass's commands in every bin, and a phone or headset CPU pays for each
//! one, so these are the numbers a mobile or XR frame is budgeted in.
//!
//! Thread-local cells, reset when a view begins encoding and read into its
//! [`Stats`](crate::Stats) when it ends: counting costs an add, never a clock
//! read on an unarmed frame (the encode time is a native `Instant`, outside
//! the perf clock `perf::Stamp` counts).
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub passes: u32,
    pub pipelines: u32,
    pub commands: u32,
}

thread_local! {
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { passes: 0, pipelines: 0, commands: 0 }) };
}

fn bump(f: impl FnOnce(&mut Counts)) {
    COUNTS.with(|c| {
        let mut counts = c.get();
        f(&mut counts);
        c.set(counts);
    });
}

/// A render or compute pass began.
pub(crate) fn pass() {
    bump(|c| c.passes += 1);
}
/// A pipeline was bound.
pub(crate) fn pipeline() {
    bump(|c| c.pipelines += 1);
}
/// `n` draw (or dispatch) commands were recorded.
pub(crate) fn commands(n: u32) {
    bump(|c| c.commands += n);
}

/// The counts since the last `take`, and zero from here.
pub(crate) fn take() -> Counts {
    COUNTS.with(|c| c.replace(Counts::default()))
}

/// The start of a view's encoding, for its CPU time.
#[derive(Clone, Copy)]
pub(crate) struct Began {
    #[cfg(not(target_arch = "wasm32"))]
    at: std::time::Instant,
}

/// Reset the counts and start the view's clock.
pub(crate) fn begin() -> Began {
    take();
    Began {
        #[cfg(not(target_arch = "wasm32"))]
        at: std::time::Instant::now(),
    }
}

impl Began {
    /// Milliseconds since `begin`; zero on the web, whose clock the perf
    /// rings own.
    pub fn elapsed_ms(self) -> f32 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.at.elapsed().as_secs_f32() * 1000.
        }
        #[cfg(target_arch = "wasm32")]
        {
            0.
        }
    }
}
