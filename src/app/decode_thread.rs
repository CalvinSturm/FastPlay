//! Lifecycle bookkeeping for the persistent decode worker thread.
//!
//! This is a conservative, behavior-preserving extraction from
//! `PlaybackSession`. The worker *body* (the long FFmpeg/D3D11 decode loop) is
//! still spawned by the coordinator — only the handle that tracks the running
//! thread moves here: its command channel, the preference it was spawned with,
//! the live-worker counter, and the join handle, plus the careful teardown that
//! guarantees the worker has released its D3D11 device clone before the caller
//! rebuilds or destroys the shared device.
//!
//! `PlaybackSession` remains the single coordinator: it builds the worker
//! closure, calls [`DecodeThreadHandle::prepare_spawn`] to register it, then
//! [`DecodeThreadHandle::set_join`] with the spawned handle.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::media::video::VideoDecodePreference;
use crate::playback::decode_control::DecodeControl;

/// Upper bound on how long a blocking teardown waits for the decode worker to
/// release its resources after cancellation. The FFmpeg interrupt callback
/// aborts in-flight I/O on shutdown, so a healthy worker exits in milliseconds;
/// this only caps the pathological case so the UI thread can never hang.
const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Handle to the current persistent decode thread (if any). Owned by
/// `PlaybackSession`.
pub struct DecodeThreadHandle {
    /// Command channel to the current persistent decode thread (if any). A
    /// same-preference position seek is delivered to it as a command (seek
    /// within the open file) instead of spawning a fresh worker that reopens
    /// the file. `None` when no decode thread is running.
    control: Option<Arc<DecodeControl>>,
    /// Decode preference the running thread was spawned with. A seek that keeps
    /// this preference reuses the thread; a change (HW↔SW) respawns it.
    preference: Option<VideoDecodePreference>,
    active_worker_count: Arc<AtomicU32>,
    /// Join handle for the current persistent decode thread. Kept so a
    /// blocking teardown (`wait = true`) can *join* the worker — guaranteeing
    /// its entire D3D11 teardown (decoder resources plus the captured device
    /// clone it releases on exit) has completed before the caller destroys or
    /// touches the shared device. Waiting on `active_worker_count` alone is
    /// insufficient: the count is decremented before the device clone is
    /// released, leaving a window where the worker is still inside d3d11.dll.
    join: Option<JoinHandle<()>>,
}

impl Default for DecodeThreadHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeThreadHandle {
    pub fn new() -> Self {
        Self {
            control: None,
            preference: None,
            active_worker_count: Arc::new(AtomicU32::new(0)),
            join: None,
        }
    }

    /// Register a worker that is about to be spawned: record its control channel
    /// and preference, bump the live-worker counter, and return a clone of that
    /// counter for the worker closure's exit guard. Call [`set_join`] with the
    /// spawned handle immediately afterward.
    ///
    /// [`set_join`]: Self::set_join
    pub fn prepare_spawn(
        &mut self,
        control: Arc<DecodeControl>,
        preference: VideoDecodePreference,
    ) -> Arc<AtomicU32> {
        self.control = Some(control);
        self.preference = Some(preference);
        self.active_worker_count.fetch_add(1, Ordering::Release);
        self.active_worker_count.clone()
    }

    /// Store the join handle for the worker just spawned.
    ///
    /// Every spawn is preceded by `teardown(false)`, which has already signalled
    /// the previous worker to shut down; replacing the handle here detaches that
    /// already-exiting thread (it finishes on its own).
    pub fn set_join(&mut self, handle: JoinHandle<()>) {
        self.join = Some(handle);
    }

    /// The current worker's command channel, if one is running.
    pub fn control(&self) -> Option<&Arc<DecodeControl>> {
        self.control.as_ref()
    }

    /// Live worker count (for diagnostics/logging).
    pub fn worker_count(&self) -> u32 {
        self.active_worker_count.load(Ordering::Acquire)
    }

    /// Whether a persistent decode thread is running that can serve a seek of
    /// `current_preference` as an in-place command (rather than a reopen).
    /// False after a preference change (HW↔SW) until respawned.
    ///
    /// The `worker_count` term is a liveness check: `control` is an `Arc` that
    /// outlives the thread holding the other end, so on its own it cannot tell a
    /// running worker from one that has exited. A seek delivered to an exited
    /// worker's channel is never served, and nothing ever notices — the caller
    /// must respawn instead. The worker bodies also retry a cancelled open
    /// rather than exiting (see `PlaybackSession::spawn_decode_thread`); this is
    /// the second line of defence for the paths that do exit, such as a decoder
    /// open error. It cannot produce a false negative: `prepare_spawn` bumps the
    /// count before the thread is spawned, and only the worker's exit guard
    /// decrements it.
    pub fn serves(&self, current_preference: VideoDecodePreference) -> bool {
        self.control.is_some()
            && self.preference == Some(current_preference)
            && self.worker_count() > 0
    }

    /// Signal the persistent decode thread to stop. When `wait` is set, block
    /// briefly until it has exited (releasing its device clone and codecs)
    /// before the caller rebuilds the device or drops the session.
    pub fn teardown(&mut self, wait: bool) {
        if let Some(control) = self.control.take() {
            control.send_shutdown();
        }
        self.preference = None;
        if wait {
            let handle = self.join.take();
            // Wait — bounded — for every live worker (the current one plus any
            // detached straggler from a rapid reopen) to release its resources.
            // `active_worker_count` only reaches zero once each worker's
            // `WorkerGuard` drops, which runs after its D3D11 codec/session
            // teardown. The interrupt callback aborts any in-flight FFmpeg I/O
            // on `is_shutdown`, and every other worker wait point checks the
            // same flag, so a healthy worker reaches zero within milliseconds.
            let deadline = Instant::now() + WORKER_JOIN_TIMEOUT;
            while self.active_worker_count.load(Ordering::Acquire) > 0 && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }

            if let Some(handle) = handle {
                if self.active_worker_count.load(Ordering::Acquire) == 0 {
                    // The worker has released its device clone; join is now
                    // effectively instant and guarantees the thread (and the
                    // captured device clone it drops last) is fully gone before
                    // the caller rebuilds or destroys the shared device.
                    let _ = handle.join();
                } else {
                    // A worker did not exit within the timeout (e.g. wedged
                    // beyond the interrupt callback's reach). Detach it rather
                    // than freezing the UI thread on an unbounded join; log so
                    // the stall is diagnosable.
                    crate::flog!(
                        "[teardown] decode worker still alive {}ms after cancel; detaching to avoid UI hang",
                        WORKER_JOIN_TIMEOUT.as_millis()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_idle() {
        let h = DecodeThreadHandle::new();
        assert_eq!(h.worker_count(), 0);
        assert!(h.control().is_none());
        assert!(!h.serves(VideoDecodePreference::Auto));
        assert!(!h.serves(VideoDecodePreference::ForceSoftware));
    }

    #[test]
    fn prepare_spawn_registers_and_counts() {
        let mut h = DecodeThreadHandle::new();
        let control = Arc::new(DecodeControl::new());
        let count = h.prepare_spawn(control, VideoDecodePreference::Auto);
        assert_eq!(h.worker_count(), 1);
        assert_eq!(count.load(Ordering::Acquire), 1);
        assert!(h.control().is_some());
        assert!(h.serves(VideoDecodePreference::Auto));
    }

    #[test]
    fn serves_false_after_preference_change() {
        let mut h = DecodeThreadHandle::new();
        h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        // A seek that switches HW<->SW must not reuse the running thread.
        assert!(!h.serves(VideoDecodePreference::ForceSoftware));
    }

    #[test]
    fn does_not_serve_once_the_worker_has_exited() {
        // Regression: `control` is an Arc that outlives the worker thread, so a
        // handle whose worker exited (cancelled open, decoder open error) used
        // to still report `serves() == true`. The coordinator then delivered the
        // seek to a channel nobody was reading and video never came back. The
        // live-worker count is what distinguishes the two.
        let mut h = DecodeThreadHandle::new();
        let count = h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        assert!(h.serves(VideoDecodePreference::Auto));

        // Simulate the worker's exit guard running (WorkerGuard::drop).
        count.fetch_sub(1, Ordering::Release);

        assert_eq!(h.worker_count(), 0);
        assert!(
            h.control().is_some(),
            "the control channel deliberately outlives the thread"
        );
        assert!(
            !h.serves(VideoDecodePreference::Auto),
            "a dead worker must not be sent an in-place seek"
        );
    }

    #[test]
    fn serves_again_after_a_respawn_replaces_the_dead_worker() {
        let mut h = DecodeThreadHandle::new();
        let count = h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        count.fetch_sub(1, Ordering::Release);
        assert!(!h.serves(VideoDecodePreference::Auto));

        h.teardown(false);
        h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        assert!(h.serves(VideoDecodePreference::Auto));
    }

    #[test]
    fn teardown_without_wait_clears_control_and_preference() {
        let mut h = DecodeThreadHandle::new();
        h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        h.teardown(false);
        assert!(h.control().is_none());
        assert!(!h.serves(VideoDecodePreference::Auto));
    }
}
