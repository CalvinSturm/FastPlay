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

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::media::video::VideoDecodePreference;
use crate::playback::decode_control::DecodeControl;

/// How a seek should reach the worker behind a [`DecodeThreadHandle`].
///
/// Three-way rather than a boolean because "no worker is running" has two very
/// different causes, and treating them alike costs either correctness or
/// performance:
///
/// - the worker died on an error or a cancelled open, and the file still has a
///   stream for it — it must be respawned, or that stream is dead for the rest
///   of the file (the bug fixed for audio in `b603f6f` and for video alongside
///   this type); versus
/// - the worker exited because the file has nothing for it to decode at all —
///   an audio-only file has no video worker to run. Respawning there re-opens
///   and re-demuxes the file on *every* seek just to rediscover the same
///   absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekDelivery {
    /// A live worker is serving the current preference: send it an in-place
    /// seek command rather than reopening the file.
    InPlace,
    /// No usable worker: tear down whatever is registered and spawn a fresh one.
    Respawn,
    /// The worker retired because this file has no stream of its kind. Do
    /// nothing — there is nothing to seek and nothing worth respawning.
    Retired,
}

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
    /// Set by a worker that is exiting because the file has no stream of its
    /// kind (`NoVideoStream` / `NoAudioStream`) — a permanent condition for this
    /// open, as opposed to the transient exits that warrant a respawn. Cleared
    /// by [`prepare_spawn`], so every new worker starts un-retired.
    ///
    /// [`prepare_spawn`]: Self::prepare_spawn
    retired: Arc<AtomicBool>,
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
            retired: Arc::new(AtomicBool::new(false)),
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
        // A fresh worker is not retired, whatever the last one concluded: this
        // may be a different file.
        self.retired.store(false, Ordering::Release);
        self.active_worker_count.fetch_add(1, Ordering::Release);
        self.active_worker_count.clone()
    }

    /// The flag a worker sets just before exiting because the file has no
    /// stream of its kind. See [`SeekDelivery::Retired`].
    pub fn retirement_flag(&self) -> Arc<AtomicBool> {
        self.retired.clone()
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

    /// How a seek should reach this handle's worker, given the decode
    /// preference the session now wants. See [`SeekDelivery`].
    ///
    /// The liveness term matters because `control` is an `Arc` that outlives the
    /// thread holding the other end, so possession of a control channel is not
    /// evidence that anyone is reading it. A seek delivered to an exited
    /// worker's channel is never served and nothing notices. The worker bodies
    /// also retry a cancelled open rather than exiting (see
    /// `PlaybackSession::spawn_decode_thread`); this is the second line of
    /// defence for the paths that legitimately do exit.
    ///
    /// Liveness cannot produce a false negative: `prepare_spawn` increments the
    /// count before the thread is spawned, and only the worker's exit guard
    /// decrements it.
    ///
    /// Pass [`VideoDecodePreference::Auto`] for the audio handle — audio has no
    /// hardware path, so it is always registered with `Auto` and the preference
    /// term is a tautology there.
    pub fn seek_delivery(&self, current_preference: VideoDecodePreference) -> SeekDelivery {
        // Never spawned, or torn down: nothing to reuse.
        if self.control.is_none() {
            return SeekDelivery::Respawn;
        }
        // Preference changed (HW↔SW): the running worker cannot serve it.
        if self.preference != Some(current_preference) {
            return SeekDelivery::Respawn;
        }
        if self.worker_count() > 0 {
            return SeekDelivery::InPlace;
        }
        if self.retired.load(Ordering::Acquire) {
            return SeekDelivery::Retired;
        }
        SeekDelivery::Respawn
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

    fn spawned(preference: VideoDecodePreference) -> (DecodeThreadHandle, Arc<AtomicU32>) {
        let mut handle = DecodeThreadHandle::new();
        let count = handle.prepare_spawn(Arc::new(DecodeControl::new()), preference);
        (handle, count)
    }

    /// Simulate a worker's exit guard (`WorkerGuard::drop`) running.
    fn worker_exits(count: &Arc<AtomicU32>) {
        count.fetch_sub(1, Ordering::Release);
    }

    #[test]
    fn new_is_idle() {
        let h = DecodeThreadHandle::new();
        assert_eq!(h.worker_count(), 0);
        assert!(h.control().is_none());
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Respawn
        );
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::ForceSoftware),
            SeekDelivery::Respawn
        );
    }

    #[test]
    fn prepare_spawn_registers_and_counts() {
        let (h, count) = spawned(VideoDecodePreference::Auto);
        assert_eq!(h.worker_count(), 1);
        assert_eq!(count.load(Ordering::Acquire), 1);
        assert!(h.control().is_some());
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::InPlace
        );
    }

    #[test]
    fn preference_change_forces_a_respawn() {
        let (h, _count) = spawned(VideoDecodePreference::Auto);
        // A seek that switches HW<->SW must not reuse the running thread.
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::ForceSoftware),
            SeekDelivery::Respawn
        );
    }

    #[test]
    fn dead_worker_is_respawned_not_sent_an_in_place_seek() {
        // Regression: `control` is an Arc that outlives the worker thread, so a
        // handle whose worker exited (cancelled open, decoder open error) used
        // to still look reusable. The coordinator then delivered the seek to a
        // channel nobody was reading and the stream never came back. The
        // live-worker count is what distinguishes the two.
        let (h, count) = spawned(VideoDecodePreference::Auto);
        worker_exits(&count);

        assert_eq!(h.worker_count(), 0);
        assert!(
            h.control().is_some(),
            "the control channel deliberately outlives the thread"
        );
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Respawn,
            "a dead worker must not be sent an in-place seek"
        );
    }

    #[test]
    fn retired_worker_is_left_alone_rather_than_respawned() {
        // An audio-only file retires the video worker (NoVideoStream), and a
        // video-only file retires the audio worker (NoAudioStream). Respawning
        // there would reopen and re-demux the file on every single seek only to
        // rediscover the same absence, so the handle must report Retired rather
        // than Respawn.
        let (h, count) = spawned(VideoDecodePreference::Auto);
        h.retirement_flag().store(true, Ordering::Release);
        worker_exits(&count);

        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Retired
        );
    }

    #[test]
    fn a_live_worker_outranks_the_retirement_flag() {
        // Retirement is only consulted once the worker is actually gone; a live
        // worker always takes the in-place seek.
        let (h, _count) = spawned(VideoDecodePreference::Auto);
        h.retirement_flag().store(true, Ordering::Release);
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::InPlace
        );
    }

    #[test]
    fn respawning_clears_retirement_from_the_previous_file() {
        // The flag describes one open, not the handle forever: opening a file
        // that does have the stream must not inherit the last file's verdict.
        let (mut h, count) = spawned(VideoDecodePreference::Auto);
        h.retirement_flag().store(true, Ordering::Release);
        worker_exits(&count);
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Retired
        );

        h.teardown(false);
        h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::InPlace
        );
    }

    #[test]
    fn serves_again_after_a_respawn_replaces_the_dead_worker() {
        let (mut h, count) = spawned(VideoDecodePreference::Auto);
        worker_exits(&count);
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Respawn
        );

        h.teardown(false);
        h.prepare_spawn(Arc::new(DecodeControl::new()), VideoDecodePreference::Auto);
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::InPlace
        );
    }

    #[test]
    fn teardown_without_wait_clears_control_and_preference() {
        let (mut h, _count) = spawned(VideoDecodePreference::Auto);
        h.teardown(false);
        assert!(h.control().is_none());
        assert_eq!(
            h.seek_delivery(VideoDecodePreference::Auto),
            SeekDelivery::Respawn
        );
    }
}
