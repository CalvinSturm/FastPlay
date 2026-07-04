//! Pure translation from low-level [`InputEvent`]s (produced by the Win32
//! window proc) to the [`SessionCommand`]s the coordinator understands.
//!
//! This is a behavior-preserving extraction of the input-routing match that
//! previously lived inline in the `main` event loop. Keeping the mapping in one
//! pure function makes it unit-testable and keeps the event loop focused on the
//! handful of events that genuinely need event-loop context.

use crate::app::commands::SessionCommand;
use crate::platform::input::InputEvent;

/// Map a context-free input event to the session command it triggers.
///
/// Returns `None` for events that need state only the event loop has — the
/// current playback position ([`InputEvent::SeekRelativeSeconds`]), the
/// timeline-scrub UI ([`InputEvent::BackspaceKey`]), window state
/// ([`InputEvent::EscapeKey`]), file I/O ([`InputEvent::FilesDropped`],
/// [`InputEvent::OpenFileDialog`]), play-queue navigation
/// ([`InputEvent::QueuePrevious`]/[`InputEvent::QueueNext`]), or an accompanying
/// overlay side effect
/// ([`InputEvent::StepFrameForward`]/[`InputEvent::StepFrameBackward`]). Those
/// are handled directly in the loop.
pub fn command_for(event: &InputEvent) -> Option<SessionCommand> {
    let command = match event {
        InputEvent::TogglePause => SessionCommand::TogglePause,
        InputEvent::ToggleSubtitles => SessionCommand::ToggleSubtitles,
        InputEvent::SaveScreenshot => SessionCommand::SaveScreenshot,
        InputEvent::AdjustVolumeSteps(steps) => SessionCommand::AdjustVolumeSteps(*steps),
        InputEvent::RotateClockwise => SessionCommand::RotateClockwise,
        InputEvent::RotateCounterClockwise => SessionCommand::RotateCounterClockwise,
        InputEvent::ToggleBorderlessFullscreen => SessionCommand::ToggleBorderlessFullscreen,
        InputEvent::ZoomAtCursor {
            delta,
            cursor_x,
            cursor_y,
        } => SessionCommand::ZoomAtCursor {
            delta: *delta,
            cursor_x: *cursor_x,
            cursor_y: *cursor_y,
        },
        InputEvent::ResetView => SessionCommand::ResetView,
        InputEvent::SetInPoint => SessionCommand::SetInPoint,
        InputEvent::ClearInPoint => SessionCommand::ClearInPoint,
        InputEvent::SetOutPoint => SessionCommand::SetOutPoint,
        InputEvent::ClearOutPoint => SessionCommand::ClearOutPoint,
        InputEvent::ToggleLoopRange => SessionCommand::ToggleLoopRange,
        InputEvent::FitWindow => SessionCommand::FitWindow,
        InputEvent::HalfSizeWindow => SessionCommand::HalfSizeWindow,
        InputEvent::ToggleDecodeInfo => SessionCommand::ToggleDecodeInfo,
        InputEvent::StepPlaybackRate(step) => SessionCommand::StepPlaybackRate(*step),
        InputEvent::ResetPlaybackRate => SessionCommand::ResetPlaybackRate,
        InputEvent::PanDelta { dx, dy } => SessionCommand::PanBy {
            dx: *dx as f32,
            dy: *dy as f32,
        },
        InputEvent::ShowHelp => SessionCommand::ShowHelp,
        InputEvent::HideHelp => SessionCommand::HideHelp,

        // Context-dependent events are handled directly in the main loop.
        InputEvent::SeekRelativeSeconds(_)
        | InputEvent::StepFrameForward
        | InputEvent::StepFrameBackward
        | InputEvent::EscapeKey
        | InputEvent::BackspaceKey
        | InputEvent::FilesDropped(_)
        | InputEvent::OpenFileDialog
        // Play-queue navigation is handled by the event loop, which owns the queue.
        | InputEvent::QueuePrevious
        | InputEvent::QueueNext
        // Recent-files overlay events are handled by the event loop.
        | InputEvent::ToggleRecentOverlay
        // Review marker workflow is owned by the event loop beside Recent/queue state.
        | InputEvent::AddMarker
        | InputEvent::ToggleMarkerOverlay
        | InputEvent::ExportMarkers
        | InputEvent::BatchMarkerScreenshots
        | InputEvent::NavigateUp
        | InputEvent::NavigateDown
        | InputEvent::Confirm
        | InputEvent::RemoveSelected => return None,
    };
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn direct_mappings_translate_one_to_one() {
        assert_eq!(
            command_for(&InputEvent::TogglePause),
            Some(SessionCommand::TogglePause)
        );
        assert_eq!(
            command_for(&InputEvent::ToggleSubtitles),
            Some(SessionCommand::ToggleSubtitles)
        );
        assert_eq!(
            command_for(&InputEvent::SaveScreenshot),
            Some(SessionCommand::SaveScreenshot)
        );
        assert_eq!(
            command_for(&InputEvent::RotateClockwise),
            Some(SessionCommand::RotateClockwise)
        );
        assert_eq!(
            command_for(&InputEvent::RotateCounterClockwise),
            Some(SessionCommand::RotateCounterClockwise)
        );
        assert_eq!(
            command_for(&InputEvent::ToggleBorderlessFullscreen),
            Some(SessionCommand::ToggleBorderlessFullscreen)
        );
        assert_eq!(
            command_for(&InputEvent::ResetView),
            Some(SessionCommand::ResetView)
        );
        assert_eq!(
            command_for(&InputEvent::SetInPoint),
            Some(SessionCommand::SetInPoint)
        );
        assert_eq!(
            command_for(&InputEvent::ClearInPoint),
            Some(SessionCommand::ClearInPoint)
        );
        assert_eq!(
            command_for(&InputEvent::SetOutPoint),
            Some(SessionCommand::SetOutPoint)
        );
        assert_eq!(
            command_for(&InputEvent::ClearOutPoint),
            Some(SessionCommand::ClearOutPoint)
        );
        assert_eq!(
            command_for(&InputEvent::ToggleLoopRange),
            Some(SessionCommand::ToggleLoopRange)
        );
        assert_eq!(
            command_for(&InputEvent::FitWindow),
            Some(SessionCommand::FitWindow)
        );
        assert_eq!(
            command_for(&InputEvent::HalfSizeWindow),
            Some(SessionCommand::HalfSizeWindow)
        );
        assert_eq!(
            command_for(&InputEvent::ToggleDecodeInfo),
            Some(SessionCommand::ToggleDecodeInfo)
        );
        assert_eq!(
            command_for(&InputEvent::ResetPlaybackRate),
            Some(SessionCommand::ResetPlaybackRate)
        );
        assert_eq!(
            command_for(&InputEvent::ShowHelp),
            Some(SessionCommand::ShowHelp)
        );
        assert_eq!(
            command_for(&InputEvent::HideHelp),
            Some(SessionCommand::HideHelp)
        );
    }

    #[test]
    fn payloads_are_preserved() {
        assert_eq!(
            command_for(&InputEvent::AdjustVolumeSteps(-3)),
            Some(SessionCommand::AdjustVolumeSteps(-3))
        );
        assert_eq!(
            command_for(&InputEvent::StepPlaybackRate(2)),
            Some(SessionCommand::StepPlaybackRate(2))
        );
        assert_eq!(
            command_for(&InputEvent::ZoomAtCursor {
                delta: 120,
                cursor_x: 640,
                cursor_y: 360,
            }),
            Some(SessionCommand::ZoomAtCursor {
                delta: 120,
                cursor_x: 640,
                cursor_y: 360,
            })
        );
    }

    #[test]
    fn pan_delta_converts_to_float_pan() {
        assert_eq!(
            command_for(&InputEvent::PanDelta { dx: -5, dy: 7 }),
            Some(SessionCommand::PanBy { dx: -5.0, dy: 7.0 })
        );
    }

    #[test]
    fn context_dependent_events_return_none() {
        assert_eq!(command_for(&InputEvent::SeekRelativeSeconds(5)), None);
        assert_eq!(command_for(&InputEvent::StepFrameForward), None);
        assert_eq!(command_for(&InputEvent::StepFrameBackward), None);
        assert_eq!(command_for(&InputEvent::EscapeKey), None);
        assert_eq!(command_for(&InputEvent::BackspaceKey), None);
        assert_eq!(
            command_for(&InputEvent::FilesDropped(vec![PathBuf::from("x.mp4")])),
            None
        );
        assert_eq!(command_for(&InputEvent::OpenFileDialog), None);
        assert_eq!(command_for(&InputEvent::QueuePrevious), None);
        assert_eq!(command_for(&InputEvent::QueueNext), None);
        assert_eq!(command_for(&InputEvent::AddMarker), None);
        assert_eq!(command_for(&InputEvent::ToggleMarkerOverlay), None);
        assert_eq!(command_for(&InputEvent::ExportMarkers), None);
        assert_eq!(command_for(&InputEvent::BatchMarkerScreenshots), None);
    }
}
