//! Clip-range state and logic: in/out marks, loop range, and auto-replay.
//!
//! This is a behavior-preserving extraction from `PlaybackSession`.
//! `PlaybackSession` remains the single coordinator and owns one concrete
//! `ClipRangeState`; the pure decisions about marks, looping, and where to
//! restart live here so they can be unit-tested in isolation. The session
//! still owns seeking, playback state, and the overlay — it reads the restart
//! target / replay decision from here and acts on it.

use std::time::Duration;

/// In/out marks, loop-range, and auto-replay state for the current clip.
/// Owned by `PlaybackSession`.
pub struct ClipRangeState {
    in_point: Option<Duration>,
    out_point: Option<Duration>,
    loop_range: bool,
    auto_replay: bool,
}

impl Default for ClipRangeState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipRangeState {
    pub fn new() -> Self {
        Self {
            in_point: None,
            out_point: None,
            loop_range: false,
            auto_replay: false,
        }
    }

    pub fn in_point(&self) -> Option<Duration> {
        self.in_point
    }

    pub fn out_point(&self) -> Option<Duration> {
        self.out_point
    }

    pub fn loop_range(&self) -> bool {
        self.loop_range
    }

    pub fn auto_replay(&self) -> bool {
        self.auto_replay
    }

    /// Reset the marks and loop state for a new file open. `auto_replay` is
    /// intentionally left untouched, matching the prior `open()` behavior
    /// (it is a sticky user preference, not per-file state).
    pub fn reset_for_open(&mut self) {
        self.in_point = None;
        self.out_point = None;
        self.loop_range = false;
    }

    /// Mark the in-point at `position`. If the new in-point is at or past the
    /// out-point, clear the out-point (the range would otherwise be invalid).
    pub fn set_in_point(&mut self, position: Duration) {
        self.in_point = Some(position);
        if let (Some(i), Some(o)) = (self.in_point, self.out_point) {
            if i >= o {
                self.out_point = None;
            }
        }
    }

    /// Clear the in-point. Clearing it while looping with no out-point would
    /// leave an empty range, so disable looping in that case.
    pub fn clear_in_point(&mut self) {
        self.in_point = None;
        if self.out_point.is_none() {
            self.loop_range = false;
        }
    }

    /// Mark the out-point at `position`. The out-point must be strictly after
    /// the in-point (or after 0 if no in-point is set).
    pub fn set_out_point(&mut self, position: Duration) {
        if position > self.in_point.unwrap_or(Duration::ZERO) {
            self.out_point = Some(position);
        }
    }

    /// Clear the out-point. Clearing it while looping with no in-point would
    /// leave an empty range, so disable looping in that case.
    pub fn clear_out_point(&mut self) {
        self.out_point = None;
        if self.in_point.is_none() {
            self.loop_range = false;
        }
    }

    /// Handle the loop/replay toggle. When a range is set this toggles
    /// `loop_range`; otherwise it toggles the `auto_replay` preference.
    /// Returns `true` when `auto_replay` was toggled, so the caller can show
    /// the replay indicator overlay (only shown in that branch).
    pub fn toggle_loop_or_replay(&mut self) -> bool {
        if self.in_point.is_some() || self.out_point.is_some() {
            self.loop_range = !self.loop_range;
            false
        } else {
            self.auto_replay = !self.auto_replay;
            true
        }
    }

    /// Whether playback should restart at end-of-stream rather than stopping.
    pub fn should_replay_at_end(&self) -> bool {
        self.auto_replay || self.loop_range
    }

    /// The position a replay/loop restart should seek to: the in-point, or the
    /// start of the file when no in-point is set.
    pub fn replay_position(&self) -> Duration {
        self.in_point.unwrap_or(Duration::ZERO)
    }

    /// If `position` is outside the active clip range (before the in-point or
    /// at/after the out-point), returns the position the range should resume
    /// from. Returns `None` when `position` is already inside the range.
    pub fn resume_target(&self, position: Duration) -> Option<Duration> {
        if self.in_point.is_some_and(|start| position < start)
            || self.out_point.is_some_and(|end| position >= end)
        {
            Some(self.replay_position())
        } else {
            None
        }
    }

    /// Whether `position` is inside the currently active clip range.
    pub fn position_is_in_active_range(&self, position: Duration) -> bool {
        self.resume_target(position).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn new_is_empty() {
        let c = ClipRangeState::new();
        assert_eq!(c.in_point(), None);
        assert_eq!(c.out_point(), None);
        assert!(!c.loop_range());
        assert!(!c.auto_replay());
    }

    #[test]
    fn mark_in_only() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(5));
        assert_eq!(c.in_point(), Some(secs(5)));
        assert_eq!(c.out_point(), None);
    }

    #[test]
    fn mark_out_only() {
        let mut c = ClipRangeState::new();
        c.set_out_point(secs(10));
        assert_eq!(c.out_point(), Some(secs(10)));
        assert_eq!(c.in_point(), None);
    }

    #[test]
    fn valid_in_out_range() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(5));
        c.set_out_point(secs(10));
        assert_eq!(c.in_point(), Some(secs(5)));
        assert_eq!(c.out_point(), Some(secs(10)));
    }

    #[test]
    fn out_point_at_or_before_in_point_is_rejected() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(10));
        c.set_out_point(secs(10)); // not strictly after in-point
        assert_eq!(c.out_point(), None);
        c.set_out_point(secs(5)); // before in-point
        assert_eq!(c.out_point(), None);
    }

    #[test]
    fn in_point_at_or_after_out_point_clears_out_point() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(2));
        c.set_out_point(secs(8));
        c.set_in_point(secs(8)); // at out-point
        assert_eq!(c.in_point(), Some(secs(8)));
        assert_eq!(c.out_point(), None);
    }

    #[test]
    fn out_point_after_0_when_no_in_point() {
        let mut c = ClipRangeState::new();
        c.set_out_point(Duration::ZERO); // not strictly after 0
        assert_eq!(c.out_point(), None);
        c.set_out_point(Duration::from_millis(1));
        assert_eq!(c.out_point(), Some(Duration::from_millis(1)));
    }

    #[test]
    fn clear_in_point_disables_loop_when_no_out_point() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(5));
        c.toggle_loop_or_replay(); // range set -> loop on
        assert!(c.loop_range());
        c.clear_in_point();
        assert_eq!(c.in_point(), None);
        assert!(!c.loop_range());
    }

    #[test]
    fn clear_in_point_keeps_loop_when_out_point_remains() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(5));
        c.set_out_point(secs(10));
        c.toggle_loop_or_replay();
        assert!(c.loop_range());
        c.clear_in_point();
        assert!(c.loop_range(), "out-point still defines a range");
    }

    #[test]
    fn clear_out_point_disables_loop_when_no_in_point() {
        let mut c = ClipRangeState::new();
        c.set_out_point(secs(10));
        c.toggle_loop_or_replay();
        assert!(c.loop_range());
        c.clear_out_point();
        assert!(!c.loop_range());
    }

    #[test]
    fn toggle_loops_when_range_set_and_replays_otherwise() {
        let mut c = ClipRangeState::new();
        // No range: toggles auto-replay, signals indicator.
        assert!(c.toggle_loop_or_replay());
        assert!(c.auto_replay());
        assert!(!c.loop_range());
        // With a range: toggles loop, no indicator.
        c.set_in_point(secs(1));
        assert!(!c.toggle_loop_or_replay());
        assert!(c.loop_range());
    }

    #[test]
    fn should_replay_at_end_reflects_loop_or_auto_replay() {
        let mut c = ClipRangeState::new();
        assert!(!c.should_replay_at_end());
        c.toggle_loop_or_replay(); // auto-replay on
        assert!(c.should_replay_at_end());
    }

    #[test]
    fn replay_position_is_in_point_or_zero() {
        let mut c = ClipRangeState::new();
        assert_eq!(c.replay_position(), Duration::ZERO);
        c.set_in_point(secs(7));
        assert_eq!(c.replay_position(), secs(7));
    }

    #[test]
    fn resume_before_in_point_restarts_at_in_point() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(10));
        c.set_out_point(secs(20));
        assert_eq!(c.resume_target(secs(5)), Some(secs(10)));
    }

    #[test]
    fn resume_at_or_after_out_point_restarts_at_range_start() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(10));
        c.set_out_point(secs(20));
        assert_eq!(c.resume_target(secs(20)), Some(secs(10)));
    }

    #[test]
    fn resume_inside_range_keeps_current_position() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(10));
        c.set_out_point(secs(20));
        assert_eq!(c.resume_target(secs(15)), None);
    }

    #[test]
    fn resume_with_no_marks_is_always_inside() {
        let c = ClipRangeState::new();
        assert_eq!(c.resume_target(Duration::ZERO), None);
        assert_eq!(c.resume_target(secs(1000)), None);
        assert!(c.position_is_in_active_range(secs(42)));
    }

    #[test]
    fn boundary_exactly_at_in_point_is_inside() {
        let mut c = ClipRangeState::new();
        c.set_in_point(secs(10));
        // position == in_point is not "< in_point", so it is inside.
        assert_eq!(c.resume_target(secs(10)), None);
    }
}
