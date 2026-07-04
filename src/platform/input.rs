use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    TogglePause,
    ToggleSubtitles,
    SaveScreenshot,
    SeekRelativeSeconds(i64),
    StepFrameForward,
    StepFrameBackward,
    AdjustVolumeSteps(i16),
    RotateClockwise,
    RotateCounterClockwise,
    ToggleBorderlessFullscreen,
    ZoomAtCursor {
        delta: i16,
        cursor_x: i32,
        cursor_y: i32,
    },
    ResetView,
    SetInPoint,
    ClearInPoint,
    SetOutPoint,
    ClearOutPoint,
    ToggleLoopRange,
    FitWindow,
    HalfSizeWindow,
    ToggleDecodeInfo,
    EscapeKey,
    BackspaceKey,
    StepPlaybackRate(i8),
    ResetPlaybackRate,
    /// One or more files (or a folder) dropped onto the window in a single drop.
    /// The event loop decides whether this becomes a single-file or multi-item
    /// play queue.
    FilesDropped(Vec<PathBuf>),
    /// Manual play-queue navigation (PageUp / PageDown). Handled by the event
    /// loop, which owns the queue; only acts when the queue has >1 item.
    QueuePrevious,
    QueueNext,
    PanDelta {
        dx: i32,
        dy: i32,
    },
    ShowHelp,
    HideHelp,
    OpenFileDialog,
    // Recent-files overlay (handled by the event loop, not the coordinator).
    ToggleRecentOverlay,
    AddMarker,
    ToggleMarkerOverlay,
    EditMarkerNote,
    ExportMarkers,
    BatchMarkerScreenshots,
    SaveReviewQueue,
    ToggleReviewQueueOverlay,
    TextChar(char),
    NavigateUp,
    NavigateDown,
    Confirm,
    RemoveSelected,
}
