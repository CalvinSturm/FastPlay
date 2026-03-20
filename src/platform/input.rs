use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    TogglePause,
    ToggleSubtitles,
    SeekRelativeSeconds(i64),
    AdjustVolumeSteps(i16),
    RotateClockwise,
    RotateCounterClockwise,
    ToggleBorderlessFullscreen,
    ZoomAtCursor { delta: i16, cursor_x: i32, cursor_y: i32 },
    ResetView,
    ToggleAutoReplay,
    FitWindow,
    HalfSizeWindow,
    ToggleDecodeInfo,
    EscapeKey,
    BackspaceKey,
    StepPlaybackRate(i8),
    FileDropped(PathBuf),
}
