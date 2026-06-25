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

use std::time::Duration;

use crate::audio::sink::AudioSink;

/// Audio master-clock accounting and persisted volume. Owned by
/// `PlaybackSession`.
pub struct AudioController {
    clock_anchor_pts: Option<Duration>,
    submitted_frames: u64,
    saved_volume: f32,
}

impl AudioController {
    pub fn new(saved_volume: f32) -> Self {
        Self {
            clock_anchor_pts: None,
            submitted_frames: 0,
            saved_volume,
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

    /// Reset the audio clock: drop the anchor and zero the submitted-frame
    /// count. Used on seek/reopen/rate-change and underrun recovery.
    pub fn reset_clock(&mut self) {
        self.clock_anchor_pts = None;
        self.submitted_frames = 0;
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
}
