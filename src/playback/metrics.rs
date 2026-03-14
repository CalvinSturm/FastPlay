use std::time::Instant;

#[derive(Debug, Default)]
pub struct PlaybackMetrics {
    last_present_at: Option<Instant>,
    last_resize_at: Option<Instant>,
    open_requested_at: Option<Instant>,
    first_frame_presented_at: Option<Instant>,
}

impl PlaybackMetrics {
    pub fn note_open_requested(&mut self, now: Instant) {
        self.open_requested_at = Some(now);
        self.first_frame_presented_at = None;
    }

    pub fn note_present(&mut self, now: Instant) {
        self.last_present_at = Some(now);
    }

    pub fn note_resize(&mut self, now: Instant) {
        self.last_resize_at = Some(now);
    }

    pub fn note_first_frame_presented(&mut self, now: Instant) -> Option<std::time::Duration> {
        let open_started = self.open_requested_at?;
        self.first_frame_presented_at = Some(now);
        Some(now.saturating_duration_since(open_started))
    }
}
