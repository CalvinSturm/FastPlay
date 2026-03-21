#![allow(dead_code)]

use crate::media::seek::SeekTarget;

/// Commands that may be issued to the coordinator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SessionCommand {
    Tick,
    TogglePause,
    ToggleSubtitles,
    Seek(SeekTarget),
    AdjustVolumeSteps(i16),
    RotateClockwise,
    RotateCounterClockwise,
    ToggleBorderlessFullscreen,
    ZoomAtCursor { delta: i16, cursor_x: i32, cursor_y: i32 },
    ResetView,
    ToggleAutoReplay,
    SetInPoint,
    ClearInPoint,
    SetOutPoint,
    ClearOutPoint,
    ToggleLoopRange,
    FitWindow,
    HalfSizeWindow,
    ToggleDecodeInfo,
    StepPlaybackRate(i8),
    ResetPlaybackRate,
    PanBy { dx: f32, dy: f32 },
    ShowHelp,
    HideHelp,
}
