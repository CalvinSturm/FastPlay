#![allow(dead_code)]

/// Commands that may be issued to the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Tick,
    TogglePause,
}
