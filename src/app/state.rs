#![allow(dead_code)]

/// Explicit coordinator state machine. M0 only exercises `Idle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Opening,
    Priming,
    Playing,
    Paused,
    Seeking,
    Draining,
    Ended,
    Error,
    Closing,
}
