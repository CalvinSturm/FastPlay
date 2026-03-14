use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekTarget {
    position: Duration,
}

impl SeekTarget {
    pub fn new(position: Duration) -> Self {
        Self { position }
    }

    pub fn position(self) -> Duration {
        self.position
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionKind {
    SettledPlaybackClock,
    PendingSeekTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackSnapshot {
    pub position: Duration,
    pub kind: PositionKind,
}
