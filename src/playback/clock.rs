use std::time::{Duration, Instant};

/// M2 video-only playback clock.
///
/// Until audio exists, the first presented video frame establishes the anchor:
/// that frame's PTS is mapped to the UI-thread `Instant` when it was selected
/// for present. Later frames are due relative to that anchor.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackClock {
    anchor_instant: Instant,
    anchor_pts: Duration,
}

impl PlaybackClock {
    pub fn new(anchor_instant: Instant, anchor_pts: Duration) -> Self {
        Self {
            anchor_instant,
            anchor_pts,
        }
    }

    pub fn deadline_for(&self, pts: Duration) -> Instant {
        if pts <= self.anchor_pts {
            return self.anchor_instant;
        }

        self.anchor_instant + pts.saturating_sub(self.anchor_pts)
    }

    pub fn position_at(&self, now: Instant) -> Duration {
        self.anchor_pts
            .saturating_add(now.saturating_duration_since(self.anchor_instant))
    }
}
