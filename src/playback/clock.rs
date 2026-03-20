use std::time::{Duration, Instant};

/// Video-only playback clock with configurable playback rate.
///
/// The first presented video frame establishes the anchor: that frame's PTS
/// is mapped to the UI-thread `Instant` when it was selected for present.
/// Later frames are due relative to that anchor, scaled by `rate`.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackClock {
    anchor_instant: Instant,
    anchor_pts: Duration,
    rate: f64,
}

impl PlaybackClock {
    pub fn new(anchor_instant: Instant, anchor_pts: Duration, rate: f64) -> Self {
        Self {
            anchor_instant,
            anchor_pts,
            rate: rate.max(0.01),
        }
    }

    pub fn deadline_for(&self, pts: Duration) -> Instant {
        if pts <= self.anchor_pts {
            return self.anchor_instant;
        }

        let delta_pts = pts.saturating_sub(self.anchor_pts).as_secs_f64();
        let delta_real = delta_pts / self.rate;
        self.anchor_instant + Duration::from_secs_f64(delta_real)
    }

    pub fn position_at(&self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.anchor_instant).as_secs_f64();
        self.anchor_pts + Duration::from_secs_f64(elapsed * self.rate)
    }
}
