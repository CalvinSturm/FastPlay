//! Viewport state and math: zoom, pan, and rotation applied during
//! presentation.
//!
//! This is a behavior-preserving extraction from `PlaybackSession`.
//! `PlaybackSession` remains the single coordinator and owns one concrete
//! `ViewportState`; the pure math that does not require live presenter/window
//! I/O lives here so it can be unit-tested in isolation. The session passes in
//! the current viewport size (in pixels) where the math needs it, and reads
//! back a [`ViewTransform`] for the presenter.

use crate::render::ViewTransform;

/// Minimum and maximum zoom factors. Zoom of 1.0 means "no zoom".
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 8.0;
/// Multiplicative zoom step per wheel notch.
const ZOOM_STEP: f32 = 1.125;
/// Max pan, as a fraction of the zoomed content size, allowed on each axis.
/// At 0.75 at least 25% of the content stays visible (see [`ViewportState::clamp_pan`]).
const MAX_PAN_FRACTION: f32 = 0.75;

/// Zoom, pan, and rotation for the displayed video. Owned by `PlaybackSession`.
pub struct ViewportState {
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    /// Current view rotation in quarter turns (0..=3), clockwise.
    rotation_quarter_turns: u8,
    /// Rotation baked into the stream's display matrix. [`reset`](Self::reset)
    /// returns the view to this so "reset" matches the file's intended
    /// orientation rather than an unrotated frame.
    stream_rotation_quarter_turns: u8,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewportState {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            rotation_quarter_turns: 0,
            stream_rotation_quarter_turns: 0,
        }
    }

    /// The render transform for the current viewport state.
    pub fn transform(&self) -> ViewTransform {
        ViewTransform {
            zoom: self.zoom,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
            rotation_quarter_turns: self.rotation_quarter_turns,
        }
    }

    /// Reset rotation state for a new file open. Zoom and pan are intentionally
    /// left untouched, matching the prior `open()` behavior.
    pub fn reset_for_open(&mut self) {
        self.rotation_quarter_turns = 0;
        self.stream_rotation_quarter_turns = 0;
    }

    /// Apply stream-metadata rotation. Only takes effect when the stream
    /// rotation actually changes, so a manual rotation survives mid-stream
    /// re-inits (HW→SW fallback, scrub seeks) that re-report the same rotation.
    pub fn apply_stream_rotation(&mut self, rotation_quarter_turns: u8) {
        if self.stream_rotation_quarter_turns != rotation_quarter_turns {
            self.stream_rotation_quarter_turns = rotation_quarter_turns;
            self.rotation_quarter_turns = rotation_quarter_turns;
        }
    }

    /// Rotate the view by `delta_quarter_turns` clockwise, wrapping at 4.
    pub fn rotate(&mut self, delta_quarter_turns: u8) {
        self.rotation_quarter_turns = self
            .rotation_quarter_turns
            .wrapping_add(delta_quarter_turns)
            % 4;
    }

    /// Reset zoom, pan, and rotation. Rotation returns to the stream's rotation.
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.rotation_quarter_turns = self.stream_rotation_quarter_turns;
    }

    /// Zoom toward the cursor. `viewport` is the presenter viewport size in
    /// pixels. Returns `true` if the zoom changed (so the caller should
    /// schedule a present).
    pub fn zoom_at_cursor(
        &mut self,
        delta: i16,
        cursor_x: i32,
        cursor_y: i32,
        viewport: (u32, u32),
    ) -> bool {
        let factor = if delta > 0 {
            ZOOM_STEP
        } else {
            1.0 / ZOOM_STEP
        };
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);

        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return false;
        }

        // Compute the viewport size for cursor-centered zoom.
        let (vw, vh) = viewport;
        let cx = vw as f32 * 0.5;
        let cy = vh as f32 * 0.5;

        // Pixel under cursor in content space: content_pt = (cursor - center - pan) / zoom
        // New pan keeps that content point under the cursor.
        let dx = cursor_x as f32 - cx;
        let dy = cursor_y as f32 - cy;
        let content_x = (dx - self.pan_x) / self.zoom;
        let content_y = (dy - self.pan_y) / self.zoom;
        let new_pan_x = dx - content_x * new_zoom;
        let new_pan_y = dy - content_y * new_zoom;

        self.zoom = new_zoom;

        // Clamp at zoom == 1.0: no pan drift.
        if new_zoom <= 1.0 {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
        } else {
            self.pan_x = new_pan_x;
            self.pan_y = new_pan_y;
            self.clamp_pan(viewport);
        }

        true
    }

    /// Pan by a delta, but only when zoomed in. `viewport` is the presenter
    /// viewport size in pixels. Returns `true` if pan was applied.
    pub fn pan_by(&mut self, dx: f32, dy: f32, viewport: (u32, u32)) -> bool {
        if self.zoom > 1.0 {
            self.pan_x += dx;
            self.pan_y += dy;
            self.clamp_pan(viewport);
            true
        } else {
            false
        }
    }

    /// Clamp pan so that at least 25% of the content remains visible on each
    /// axis. Without this the user can drag the video entirely off-screen.
    pub fn clamp_pan(&mut self, viewport: (u32, u32)) {
        let (vw, vh) = viewport;
        let max_pan_x = vw as f32 * self.zoom * MAX_PAN_FRACTION;
        let max_pan_y = vh as f32 * self.zoom * MAX_PAN_FRACTION;
        self.pan_x = self.pan_x.clamp(-max_pan_x, max_pan_x);
        self.pan_y = self.pan_y.clamp(-max_pan_y, max_pan_y);
    }

    /// Orient content dimensions for the current rotation: odd quarter-turns
    /// swap width and height. Used for initial-open, fit, and half-size window
    /// sizing.
    pub fn orient_dimensions(&self, width: u32, height: u32) -> (u32, u32) {
        if !self.rotation_quarter_turns.is_multiple_of(2) {
            (height, width)
        } else {
            (width, height)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: (u32, u32) = (1000, 500);

    #[test]
    fn new_is_identity_transform() {
        let v = ViewportState::new();
        let t = v.transform();
        assert_eq!(t.zoom, 1.0);
        assert_eq!(t.pan_x, 0.0);
        assert_eq!(t.pan_y, 0.0);
        assert_eq!(t.rotation_quarter_turns, 0);
    }

    #[test]
    fn zoom_in_increases_zoom_and_requests_present() {
        let mut v = ViewportState::new();
        // Zoom centered at the viewport center: pan stays at zero.
        let changed = v.zoom_at_cursor(1, 500, 250, VIEWPORT);
        assert!(changed);
        assert!((v.transform().zoom - 1.125).abs() < 1e-4);
    }

    #[test]
    fn zoom_clamps_at_max() {
        let mut v = ViewportState::new();
        // Many zoom-in steps must saturate at MAX_ZOOM and eventually stop changing.
        let mut last_changed = true;
        for _ in 0..100 {
            last_changed = v.zoom_at_cursor(1, 500, 250, VIEWPORT);
        }
        assert!(!last_changed, "zoom should stop changing at the max");
        assert_eq!(v.transform().zoom, MAX_ZOOM);
    }

    #[test]
    fn zoom_clamps_at_min_and_clears_pan() {
        let mut v = ViewportState::new();
        // Force a zoomed, panned state, then zoom all the way back out.
        v.zoom_at_cursor(1, 900, 400, VIEWPORT);
        v.zoom_at_cursor(1, 900, 400, VIEWPORT);
        assert!(v.transform().zoom > 1.0);
        for _ in 0..100 {
            v.zoom_at_cursor(-1, 900, 400, VIEWPORT);
        }
        let t = v.transform();
        assert_eq!(t.zoom, MIN_ZOOM);
        assert_eq!(t.pan_x, 0.0);
        assert_eq!(t.pan_y, 0.0);
    }

    #[test]
    fn pan_ignored_when_not_zoomed() {
        let mut v = ViewportState::new();
        let changed = v.pan_by(100.0, 100.0, VIEWPORT);
        assert!(!changed);
        assert_eq!(v.transform().pan_x, 0.0);
        assert_eq!(v.transform().pan_y, 0.0);
    }

    #[test]
    fn pan_clamps_to_visible_fraction() {
        let mut v = ViewportState::new();
        v.zoom_at_cursor(1, 500, 250, VIEWPORT); // zoom to 1.125, pan stays 0
                                                 // Pan far past the limit; it must clamp to size * zoom * 0.75.
        v.pan_by(100_000.0, 100_000.0, VIEWPORT);
        let zoom = v.transform().zoom;
        let max_pan_x = VIEWPORT.0 as f32 * zoom * MAX_PAN_FRACTION;
        let max_pan_y = VIEWPORT.1 as f32 * zoom * MAX_PAN_FRACTION;
        assert!((v.transform().pan_x - max_pan_x).abs() < 1e-3);
        assert!((v.transform().pan_y - max_pan_y).abs() < 1e-3);
    }

    #[test]
    fn rotate_wraps_at_four() {
        let mut v = ViewportState::new();
        v.rotate(3);
        assert_eq!(v.transform().rotation_quarter_turns, 3);
        v.rotate(2); // 3 + 2 = 5 -> 1
        assert_eq!(v.transform().rotation_quarter_turns, 1);
    }

    #[test]
    fn orient_dimensions_swaps_on_odd_rotation() {
        let mut v = ViewportState::new();
        assert_eq!(v.orient_dimensions(1920, 1080), (1920, 1080));
        v.rotate(1);
        assert_eq!(v.orient_dimensions(1920, 1080), (1080, 1920));
        v.rotate(1); // now 2 quarter turns (even)
        assert_eq!(v.orient_dimensions(1920, 1080), (1920, 1080));
    }

    #[test]
    fn stream_rotation_orients_initial_window_dimensions() {
        let mut v = ViewportState::new();
        v.apply_stream_rotation(1);
        assert_eq!(
            v.orient_dimensions(1920, 1080),
            (1080, 1920),
            "90-degree stream metadata must open a portrait window"
        );

        v.apply_stream_rotation(3);
        assert_eq!(
            v.orient_dimensions(1920, 1080),
            (1080, 1920),
            "270-degree stream metadata must open a portrait window"
        );
    }

    #[test]
    fn reset_returns_rotation_to_stream() {
        let mut v = ViewportState::new();
        v.apply_stream_rotation(1);
        v.rotate(1); // user rotates to 2
        v.zoom_at_cursor(1, 900, 400, VIEWPORT);
        v.reset();
        let t = v.transform();
        assert_eq!(t.zoom, 1.0);
        assert_eq!(t.pan_x, 0.0);
        assert_eq!(t.pan_y, 0.0);
        assert_eq!(
            t.rotation_quarter_turns, 1,
            "reset returns to stream rotation"
        );
    }

    #[test]
    fn apply_stream_rotation_preserves_manual_rotation_on_repeat() {
        let mut v = ViewportState::new();
        v.apply_stream_rotation(1); // initial open: view follows stream
        assert_eq!(v.transform().rotation_quarter_turns, 1);
        v.rotate(1); // user manually rotates to 2
                     // Same stream rotation re-reported (HW→SW fallback / scrub): keep user's choice.
        v.apply_stream_rotation(1);
        assert_eq!(v.transform().rotation_quarter_turns, 2);
        // A genuinely new stream rotation does take effect.
        v.apply_stream_rotation(3);
        assert_eq!(v.transform().rotation_quarter_turns, 3);
    }

    #[test]
    fn reset_for_open_clears_rotation_only() {
        let mut v = ViewportState::new();
        v.apply_stream_rotation(2);
        v.zoom_at_cursor(1, 900, 400, VIEWPORT);
        v.reset_for_open();
        let t = v.transform();
        assert_eq!(t.rotation_quarter_turns, 0);
        // Zoom/pan are intentionally untouched by open().
        assert!(t.zoom > 1.0);
    }
}
