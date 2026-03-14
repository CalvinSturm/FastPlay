#![allow(dead_code)]

use crate::media::seek::SeekTarget;

/// Commands that may be issued to the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Tick,
    TogglePause,
    ToggleSubtitles,
    Seek(SeekTarget),
}
