#![allow(dead_code)]

use std::time::Instant;

/// M0 placeholder for future clock selection policy.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackClock {
    started_at: Instant,
}

impl PlaybackClock {
    pub fn new(started_at: Instant) -> Self {
        Self { started_at }
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }
}
