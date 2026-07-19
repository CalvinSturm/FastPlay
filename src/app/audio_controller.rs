//! Audio coordination state: the audio master-clock anchor, submitted-frame
//! accounting, and the persisted user volume.
//!
//! This is a deliberately conservative, behavior-preserving extraction.
//! `PlaybackSession` remains the single coordinator and still owns the WASAPI
//! [`AudioSink`], the decoded-audio queue, the audio event channel, and the
//! end-of-stream/scheduler state machine. Only the cohesive, side-effect-free
//! accounting moves here:
//!
//! - `clock_anchor_pts` — the PTS the audio master clock is anchored to, set
//!   once the first decoded audio is handed to a started sink.
//! - `submitted_frames` — total frames written to the sink since the last
//!   anchor reset; combined with the sink's buffered count to derive the
//!   played position.
//! - `saved_volume` — the user volume, persisted across sessions and reapplied
//!   to freshly created sinks.
//!
//! The clock *policy* (audio-is-master, video fallback, drift handling) stays
//! in `PlaybackSession`; this type only holds the numbers that policy reads.

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use crate::audio::sink::AudioSink;

/// Wall-clock extrapolation state for the staircase raw played position.
#[derive(Clone, Copy)]
struct ClockSmoothing {
    /// The raw played position the last time it advanced.
    base: Duration,
    /// When that advance was observed.
    at: Instant,
    /// The last value returned, enforcing monotonicity across the moment
    /// a raw advance lands below a clamped extrapolation.
    last: Duration,
}

/// Audio master-clock accounting and persisted volume. Owned by
/// `PlaybackSession`.
pub struct AudioController {
    clock_anchor_pts: Option<Duration>,
    submitted_frames: u64,
    saved_volume: f32,
    /// Interior-mutable so the read-only master-clock query can update the
    /// extrapolation state without threading `&mut` through every caller.
    smoothing: Cell<Option<ClockSmoothing>>,
}

impl AudioController {
    pub fn new(saved_volume: f32) -> Self {
        Self {
            clock_anchor_pts: None,
            submitted_frames: 0,
            saved_volume,
            smoothing: Cell::new(None),
        }
    }

    /// The PTS the audio master clock is anchored to, if any.
    pub fn clock_anchor_pts(&self) -> Option<Duration> {
        self.clock_anchor_pts
    }

    /// Whether the audio master clock is currently anchored.
    pub fn is_clock_anchored(&self) -> bool {
        self.clock_anchor_pts.is_some()
    }

    /// Anchor (or clear) the audio master clock to `pts`. Does not affect the
    /// submitted-frame count, which has already been accumulated for the frames
    /// that established the anchor.
    pub fn set_clock_anchor(&mut self, pts: Option<Duration>) {
        self.clock_anchor_pts = pts;
    }

    /// Reset the audio clock: drop the anchor, zero the submitted-frame
    /// count, and clear the smoothing state. Used on seek/reopen/rate-change
    /// and underrun recovery.
    pub fn reset_clock(&mut self) {
        self.clock_anchor_pts = None;
        self.submitted_frames = 0;
        self.smoothing.set(None);
    }

    /// Smooth the staircase raw played position into a continuously
    /// advancing clock.
    ///
    /// The raw position is derived from WASAPI's `GetCurrentPadding`, which
    /// in shared mode only updates once per audio-engine period (~10 ms) —
    /// a 10 ms staircase. Video frames shorter than a tread (anything above
    /// 100 fps) then become due two at a time on each step, and the
    /// scheduler's catch-up path drops one of them: a structural ~17% drop
    /// rate at 120 fps with no real lateness anywhere.
    ///
    /// Between raw advances this extrapolates from the last advance with
    /// wall time (audio hardware consumes in real time at 1.0× — the only
    /// rate the audio clock masters), clamped to `MAX_EXTRAPOLATION` past
    /// the last raw observation so a stalled or glitching device can never
    /// run the clock ahead unboundedly. The returned value is monotonic
    /// non-decreasing; any residual lead over raw is bounded by the clamp
    /// and converges as raw catches up.
    pub fn smooth_played(&self, raw: Duration, now: Instant) -> Duration {
        /// Slightly over one WASAPI engine period: covers normal staircase
        /// treads while capping how far a dead device can lead.
        const MAX_EXTRAPOLATION: Duration = Duration::from_millis(12);

        let state = match self.smoothing.get() {
            // Raw advanced: it becomes the new extrapolation base.
            Some(state) if raw > state.base => ClockSmoothing {
                base: raw,
                at: now,
                last: state.last,
            },
            Some(state) => state,
            None => ClockSmoothing {
                base: raw,
                at: now,
                last: raw,
            },
        };
        let extrapolated = state.base.saturating_add(
            now.saturating_duration_since(state.at)
                .min(MAX_EXTRAPOLATION),
        );
        let value = extrapolated.max(state.last);
        self.smoothing.set(Some(ClockSmoothing {
            last: value,
            ..state
        }));
        value
    }

    /// Record that `frames` more audio frames were written to the sink.
    pub fn record_submitted(&mut self, frames: u64) {
        self.submitted_frames = self.submitted_frames.saturating_add(frames);
    }

    /// Frames the sink has actually played: submitted minus still-buffered.
    /// Saturating so a transiently larger buffered count never underflows.
    pub fn played_frames(&self, buffered: u64) -> u64 {
        self.submitted_frames.saturating_sub(buffered)
    }

    /// The persisted user volume, reapplied to newly created sinks.
    pub fn saved_volume(&self) -> f32 {
        self.saved_volume
    }

    /// Apply a volume step change to `sink`, persist the new volume, and return
    /// the resulting volume as a percentage (for the overlay). The step math
    /// and clamping live in the WASAPI sink and are unchanged.
    pub fn adjust_volume(&mut self, steps: i16, sink: &mut AudioSink) -> u32 {
        sink.adjust_volume_steps(steps);
        self.saved_volume = sink.volume();
        super::settings::save_volume(self.saved_volume);
        sink.volume_percent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn new_starts_unanchored_with_zero_frames() {
        let a = AudioController::new(0.8);
        assert!(!a.is_clock_anchored());
        assert_eq!(a.clock_anchor_pts(), None);
        assert_eq!(a.played_frames(0), 0);
        assert_eq!(a.saved_volume(), 0.8);
    }

    #[test]
    fn set_clock_anchor_marks_anchored_without_touching_frames() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(1000);
        a.set_clock_anchor(Some(ms(500)));
        assert!(a.is_clock_anchored());
        assert_eq!(a.clock_anchor_pts(), Some(ms(500)));
        assert_eq!(a.played_frames(0), 1000, "anchoring must not reset frames");
    }

    #[test]
    fn reset_clock_clears_anchor_and_frames() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(4096);
        a.set_clock_anchor(Some(ms(250)));
        a.reset_clock();
        assert!(!a.is_clock_anchored());
        assert_eq!(a.played_frames(0), 0);
    }

    #[test]
    fn record_submitted_accumulates() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(100);
        a.record_submitted(50);
        assert_eq!(a.played_frames(0), 150);
    }

    #[test]
    fn record_submitted_saturates_at_u64_max() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(u64::MAX);
        a.record_submitted(10);
        assert_eq!(a.played_frames(0), u64::MAX);
    }

    #[test]
    fn played_frames_is_submitted_minus_buffered() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(1000);
        assert_eq!(a.played_frames(200), 800);
    }

    #[test]
    fn played_frames_saturates_when_buffered_exceeds_submitted() {
        let mut a = AudioController::new(1.0);
        a.record_submitted(100);
        assert_eq!(a.played_frames(500), 0);
    }

    #[test]
    fn set_clock_anchor_none_unanchors() {
        let mut a = AudioController::new(1.0);
        a.set_clock_anchor(Some(ms(10)));
        a.set_clock_anchor(None);
        assert!(!a.is_clock_anchored());
    }

    #[test]
    fn smooth_played_fills_the_staircase_between_raw_advances() {
        // Raw position frozen (WASAPI padding not yet updated): the clock
        // must keep advancing with wall time instead of stalling on the
        // tread and then jumping a whole engine period at once.
        let a = AudioController::new(1.0);
        let t0 = Instant::now();
        assert_eq!(a.smooth_played(ms(100), t0), ms(100));
        let mid = a.smooth_played(ms(100), t0 + ms(4));
        assert_eq!(mid, ms(104));
        // Raw then advances a full 10 ms tread; the smoothed clock lands on
        // it without ever having stalled.
        assert_eq!(a.smooth_played(ms(110), t0 + ms(10)), ms(110));
    }

    #[test]
    fn smooth_played_clamps_extrapolation_on_a_stalled_device() {
        // Raw frozen far beyond one engine period (glitching endpoint): the
        // clock may lead by at most the clamp, never unboundedly.
        let a = AudioController::new(1.0);
        let t0 = Instant::now();
        a.smooth_played(ms(100), t0);
        assert_eq!(a.smooth_played(ms(100), t0 + ms(50)), ms(112));
        assert_eq!(a.smooth_played(ms(100), t0 + ms(500)), ms(112));
    }

    #[test]
    fn smooth_played_is_monotonic_when_raw_lands_below_the_clamp() {
        // Extrapolation reached the clamp (112), then raw advances to only
        // 110: the returned clock must not step backward; it holds at 112
        // until raw passes it.
        let a = AudioController::new(1.0);
        let t0 = Instant::now();
        a.smooth_played(ms(100), t0);
        assert_eq!(a.smooth_played(ms(100), t0 + ms(20)), ms(112));
        assert_eq!(a.smooth_played(ms(110), t0 + ms(20)), ms(112));
        // Raw catches up past the held value and the clock follows it again.
        assert_eq!(a.smooth_played(ms(120), t0 + ms(21)), ms(120));
    }

    #[test]
    fn reset_clock_clears_smoothing_state() {
        // After a seek the raw position restarts near zero; stale smoothing
        // state must not hold the clock at the pre-seek value.
        let mut a = AudioController::new(1.0);
        let t0 = Instant::now();
        a.smooth_played(ms(5000), t0);
        a.reset_clock();
        assert_eq!(a.smooth_played(ms(0), t0 + ms(1)), ms(0));
    }
}
