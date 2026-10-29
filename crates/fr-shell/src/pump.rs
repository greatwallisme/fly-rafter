// Rust guideline compliant 2026-10-29
//! The registry-owned pull pump (dsh `jobs-local/src/pump.ts`).
//!
//! The pump copies a job's output sources into its ring at a bounded
//! cadence and drains them once more after the producer settles, so the
//! ring holds every byte before settlement trims and closes it. One
//! plugin-owned thread per pumped job parks in bounded slices and exits
//! after the final drain.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::jobs_types::JobAppendOptions;
use crate::jobs_types::JobOutputSource;

/// Where a pump delivers what it reads (the registry sink).
pub trait PumpSink: Send + Sync {
    /// Appends copied text to the ring.
    fn append(&self, text: &str, options: JobAppendOptions);
    /// Records the spill file source `index` advertised on its latest
    /// read; called on every read, with `None` when the source keeps or
    /// withdrew it.
    fn spill(&self, index: usize, path: Option<String>);
}

/// The pump thread's stop latch: the registry sets it when the producer
/// settles (a rejection counts as settlement).
#[derive(Debug, Default)]
pub struct PumpStop(Arc<AtomicBool>);

impl PumpStop {
    /// Creates a clear latch.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Releases the wait and triggers the final drain.
    pub fn release(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the producer settled.
    #[must_use]
    pub fn released(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Clone for PumpStop {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Spawns the pump thread: drains every source in array order, sleeps
/// `poll_ms` (bounded), repeats, then drains one final time after the
/// stop latch releases.
///
/// A lossy source read lands as a gap chunk; the spill file a source
/// keeps is reported to the sink on every read. Release with
/// `stop.release()` followed by `handle.thread().unpark()` so the final
/// drain runs at once (the dsh wake shape).
///
/// # Panics
///
/// Panics when `poll_ms` is zero (the dsh validation refuses a
/// non-positive interval; the registry config schema rejects it first).
pub fn start_pump(
    sources: Vec<(Option<crate::jobs_types::JobChannel>, JobOutputSource)>,
    sink: Arc<dyn PumpSink>,
    poll_ms: u64,
    stop: PumpStop,
) -> thread::JoinHandle<()> {
    assert!(
        poll_ms > 0,
        "invalid pump pollMs: expected a positive finite number of milliseconds"
    );
    thread::Builder::new()
        .name("fr-shell-job-pump".to_owned())
        .spawn(move || {
            let mut cursors = vec![0_u64; sources.len()];
            let drain = |cursors: &mut [u64]| {
                for (index, (channel, source)) in sources.iter().enumerate() {
                    let read = source(cursors[index]);
                    cursors[index] = read.next_offset;
                    sink.spill(index, read.spill_path);
                    if read.text.is_empty() {
                        continue;
                    }
                    let options = JobAppendOptions {
                        channel: *channel,
                        gap_before: read.lossy,
                    };
                    sink.append(&read.text, options);
                }
            };
            while !stop.released() {
                drain(&mut cursors);
                if stop.released() {
                    break;
                }
                thread::park_timeout(Duration::from_millis(poll_ms));
            }
            drain(&mut cursors);
        })
        .expect("spawning the job pump thread never fails under the plugin contract")
}
