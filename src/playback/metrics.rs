use std::time::Instant;

#[derive(Debug, Default)]
pub struct PlaybackMetrics {
    last_present_at: Option<Instant>,
    last_resize_at: Option<Instant>,
}

impl PlaybackMetrics {
    pub fn note_present(&mut self, now: Instant) {
        self.last_present_at = Some(now);
    }

    pub fn note_resize(&mut self, now: Instant) {
        self.last_resize_at = Some(now);
    }
}
