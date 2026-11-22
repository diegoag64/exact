//! Background workers that compile pipeline specializations: one per key,
//! retried a bounded number of times, all joined before the pipelines drop.

use super::MODEL_GENERAL;

#[derive(Clone, Copy, Default)]
struct Attempt {
    running: bool,
    tries: u8,
    retry_frames: u8,
}

pub(super) struct Specializations(std::sync::Mutex<Workers>);
struct Workers {
    attempts: Box<[Attempt]>,
    handles: Vec<(usize, std::thread::JoinHandle<()>)>,
}
impl Default for Specializations {
    fn default() -> Self {
        Self(std::sync::Mutex::new(Workers {
            attempts: vec![Attempt::default(); MODEL_GENERAL].into_boxed_slice(),
            handles: Vec::new(),
        }))
    }
}
impl Specializations {
    pub(super) fn start(&self, key: usize, work: impl FnOnce() + Send + 'static) {
        self.start_with(key, || {
            std::thread::Builder::new()
                .name("exact specialization".into())
                .spawn(work)
        });
    }

    fn start_with(
        &self,
        key: usize,
        spawn: impl FnOnce() -> std::io::Result<std::thread::JoinHandle<()>>,
    ) {
        let mut workers = self.0.lock().unwrap();
        let attempt = &mut workers.attempts[key];
        if attempt.running || attempt.tries == 3 || attempt.retry_frames != 0 {
            return;
        }
        attempt.tries += 1;
        match spawn() {
            Ok(handle) => {
                attempt.running = true;
                workers.handles.push((key, handle));
            }
            Err(error) => {
                attempt.running = false;
                attempt.retry_frames = 60;
                eprintln!("game specialization spawn failed: {error}");
            }
        }
    }

    pub(super) fn reap(&self, wait: bool) {
        let mut workers = self.0.lock().unwrap();
        for attempt in &mut workers.attempts {
            attempt.retry_frames = attempt.retry_frames.saturating_sub(1);
        }
        let mut i = 0;
        while i < workers.handles.len() {
            if wait || workers.handles[i].1.is_finished() {
                let (key, worker) = workers.handles.swap_remove(i);
                let attempt = &mut workers.attempts[key];
                attempt.running = false;
                if worker.join().is_err() {
                    attempt.retry_frames = 60;
                    eprintln!("game specialization worker panicked for key {key}");
                }
            } else {
                i += 1;
            }
        }
    }
}
impl Drop for Specializations {
    fn drop(&mut self) {
        // No worker may execute artifact code after gpu_unload returns.
        self.reap(true);
    }
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use std::sync::OnceLock;

    #[test]
    fn spawn_failure_and_panic_clear_running_and_retry_with_a_bound() {
        let workers = Specializations::default();
        workers.start_with(17, || Err(std::io::Error::other("injected spawn failure")));
        assert!(!workers.0.lock().unwrap().attempts[17].running);
        workers.start_with(17, || panic!("retry must wait for later frames"));
        for _ in 0..60 {
            workers.reap(false);
        }
        workers.start(17, || panic!("injected worker panic"));
        workers.reap(true);
        assert!(!workers.0.lock().unwrap().attempts[17].running);
        for _ in 0..60 {
            workers.reap(false);
        }
        let result = std::sync::Arc::new(OnceLock::new());
        let output = result.clone();
        workers.start(17, move || {
            output.set(42).unwrap();
        });
        workers.reap(true);
        assert_eq!(result.get(), Some(&42));
        assert!(workers.0.lock().unwrap().handles.is_empty());
        workers.start_with(17, || panic!("three attempts exhaust the key's budget"));
        // A different key still compiles after one key exhausts its budget.
        workers.start(18, || {});
        workers.reap(true);
        assert_eq!(workers.0.lock().unwrap().attempts[18].tries, 1);
    }

    #[test]
    fn frames_reap_completed_workers_without_waiting_for_live_ones() {
        let workers = Specializations::default();
        let (release, wait) = std::sync::mpsc::channel();
        workers.start(1, move || {
            wait.recv().unwrap();
        });
        workers.start_with(1, || panic!("only one worker per key"));
        workers.reap(false);
        assert_eq!(workers.0.lock().unwrap().handles.len(), 1);
        workers.start(2, || {});
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !workers.0.lock().unwrap().handles[1].1.is_finished() {
            assert!(std::time::Instant::now() < end);
            std::thread::yield_now();
        }
        workers.reap(false);
        assert_eq!(workers.0.lock().unwrap().handles.len(), 1);
        assert!(!workers.0.lock().unwrap().attempts[2].running);
        release.send(()).unwrap();
        drop(workers);
    }
}
