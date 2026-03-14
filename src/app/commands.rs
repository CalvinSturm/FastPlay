#![allow(dead_code)]

/// Commands that may be issued to the coordinator.
///
/// M0 keeps this intentionally narrow; playback/open/seek commands arrive in
/// later milestones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Tick,
}
