/// Explicit coordinator state machine.
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
}
