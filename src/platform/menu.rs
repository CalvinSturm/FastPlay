//! Native menu command vocabulary.
//!
//! Menu selections deliberately resolve to the same [`InputEvent`] values as
//! keyboard and mouse input. This keeps the Win32 menu a thin presentation
//! layer: context-dependent actions still go through the main event loop, and
//! playback actions still become `SessionCommand`s through `input_dispatch`.

use super::input::InputEvent;

pub(crate) const MEDIA_OPEN: u16 = 1001;
pub(crate) const MEDIA_RECENT: u16 = 1002;
pub(crate) const MEDIA_PREVIOUS: u16 = 1003;
pub(crate) const MEDIA_NEXT: u16 = 1004;
pub(crate) const MEDIA_SCREENSHOT: u16 = 1005;
pub(crate) const MEDIA_EXIT: u16 = 1006;
pub(crate) const MEDIA_EXPORT_RANGE: u16 = 1007;

pub(crate) const PLAYBACK_TOGGLE_PAUSE: u16 = 1101;
pub(crate) const PLAYBACK_SEEK_BACK: u16 = 1102;
pub(crate) const PLAYBACK_SEEK_FORWARD: u16 = 1103;
pub(crate) const PLAYBACK_FRAME_BACK: u16 = 1104;
pub(crate) const PLAYBACK_FRAME_FORWARD: u16 = 1105;
pub(crate) const PLAYBACK_SLOWER: u16 = 1106;
pub(crate) const PLAYBACK_FASTER: u16 = 1107;
pub(crate) const PLAYBACK_NORMAL_SPEED: u16 = 1108;
pub(crate) const PLAYBACK_SET_IN: u16 = 1109;
pub(crate) const PLAYBACK_CLEAR_IN: u16 = 1110;
pub(crate) const PLAYBACK_SET_OUT: u16 = 1111;
pub(crate) const PLAYBACK_CLEAR_OUT: u16 = 1112;
pub(crate) const PLAYBACK_TOGGLE_LOOP: u16 = 1113;

pub(crate) const AUDIO_VOLUME_UP: u16 = 1201;
pub(crate) const AUDIO_VOLUME_DOWN: u16 = 1202;

pub(crate) const VIDEO_ROTATE_CW: u16 = 1301;
pub(crate) const VIDEO_ROTATE_CCW: u16 = 1302;
pub(crate) const VIDEO_RESET_VIEW: u16 = 1303;
pub(crate) const VIDEO_DECODE_INFO: u16 = 1304;

pub(crate) const SUBTITLES_TOGGLE: u16 = 1401;

pub(crate) const VIEW_FULLSCREEN: u16 = 1501;
pub(crate) const VIEW_FRAMELESS: u16 = 1502;
pub(crate) const VIEW_FIT: u16 = 1503;
pub(crate) const VIEW_HALF_SIZE: u16 = 1504;
pub(crate) const VIEW_MENU_BAR: u16 = 1505;

pub(crate) const HELP_SHORTCUTS: u16 = 1601;

/// Whether the menu bar is shown in the framed window. Frameless and fullscreen
/// never dock it; there it is only revealed while Alt is held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuBarMode {
    #[default]
    Visible,
    Hidden,
}

impl MenuBarMode {
    /// Ctrl+Shift+M toggles the menu bar on and off.
    pub fn next(self) -> Self {
        match self {
            Self::Visible => Self::Hidden,
            Self::Hidden => Self::Visible,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        }
    }

    pub fn from_setting(value: &str) -> Option<Self> {
        match value {
            "visible" => Some(Self::Visible),
            "hidden" => Some(Self::Hidden),
            // Migrate the removed hover option to Off.
            "autohide" => Some(Self::Hidden),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MenuAction {
    Input(InputEvent),
    Close,
}

/// Translate one Win32 menu item ID into the existing input vocabulary.
pub(crate) fn action_for_id(id: u16) -> Option<MenuAction> {
    let input = match id {
        MEDIA_OPEN => InputEvent::OpenFileDialog,
        MEDIA_RECENT => InputEvent::ToggleRecentOverlay,
        MEDIA_PREVIOUS => InputEvent::QueuePrevious,
        MEDIA_NEXT => InputEvent::QueueNext,
        MEDIA_SCREENSHOT => InputEvent::SaveScreenshot,
        MEDIA_EXIT => return Some(MenuAction::Close),
        MEDIA_EXPORT_RANGE => InputEvent::ExportRange,
        PLAYBACK_TOGGLE_PAUSE => InputEvent::TogglePause,
        PLAYBACK_SEEK_BACK => InputEvent::SeekRelativeSeconds(-5),
        PLAYBACK_SEEK_FORWARD => InputEvent::SeekRelativeSeconds(5),
        PLAYBACK_FRAME_BACK => InputEvent::StepFrameBackward,
        PLAYBACK_FRAME_FORWARD => InputEvent::StepFrameForward,
        PLAYBACK_SLOWER => InputEvent::StepPlaybackRate(-1),
        PLAYBACK_FASTER => InputEvent::StepPlaybackRate(1),
        PLAYBACK_NORMAL_SPEED => InputEvent::ResetPlaybackRate,
        PLAYBACK_SET_IN => InputEvent::SetInPoint,
        PLAYBACK_CLEAR_IN => InputEvent::ClearInPoint,
        PLAYBACK_SET_OUT => InputEvent::SetOutPoint,
        PLAYBACK_CLEAR_OUT => InputEvent::ClearOutPoint,
        PLAYBACK_TOGGLE_LOOP => InputEvent::ToggleLoopRange,
        AUDIO_VOLUME_UP => InputEvent::AdjustVolumeSteps(1),
        AUDIO_VOLUME_DOWN => InputEvent::AdjustVolumeSteps(-1),
        VIDEO_ROTATE_CW => InputEvent::RotateClockwise,
        VIDEO_ROTATE_CCW => InputEvent::RotateCounterClockwise,
        VIDEO_RESET_VIEW => InputEvent::ResetView,
        VIDEO_DECODE_INFO => InputEvent::ToggleDecodeInfo,
        SUBTITLES_TOGGLE => InputEvent::ToggleSubtitles,
        VIEW_FULLSCREEN => InputEvent::ToggleBorderlessFullscreen,
        VIEW_FRAMELESS => InputEvent::ToggleFramelessWindowed,
        VIEW_FIT => InputEvent::FitWindow,
        VIEW_HALF_SIZE => InputEvent::HalfSizeWindow,
        VIEW_MENU_BAR => InputEvent::CycleMenuBarMode,
        HELP_SHORTCUTS => InputEvent::ShowHelp,
        _ => return None,
    };
    Some(MenuAction::Input(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_items_reuse_existing_input_actions() {
        assert_eq!(
            action_for_id(MEDIA_OPEN),
            Some(MenuAction::Input(InputEvent::OpenFileDialog))
        );
        assert_eq!(
            action_for_id(PLAYBACK_SEEK_BACK),
            Some(MenuAction::Input(InputEvent::SeekRelativeSeconds(-5)))
        );
        assert_eq!(
            action_for_id(VIEW_FRAMELESS),
            Some(MenuAction::Input(InputEvent::ToggleFramelessWindowed))
        );
        assert_eq!(action_for_id(MEDIA_EXIT), Some(MenuAction::Close));
        assert_eq!(
            action_for_id(MEDIA_EXPORT_RANGE),
            Some(MenuAction::Input(InputEvent::ExportRange))
        );
        assert_eq!(action_for_id(u16::MAX), None);
    }

    #[test]
    fn menu_bar_mode_toggles_and_round_trips() {
        let mut mode = MenuBarMode::Visible;
        let mut seen = Vec::new();
        for _ in 0..2 {
            mode = mode.next();
            seen.push(mode);
            assert_eq!(MenuBarMode::from_setting(mode.as_setting()), Some(mode));
        }
        assert_eq!(seen, [MenuBarMode::Hidden, MenuBarMode::Visible]);
        assert_eq!(
            MenuBarMode::from_setting("autohide"),
            Some(MenuBarMode::Hidden)
        );
        assert_eq!(MenuBarMode::from_setting("sometimes"), None);
    }
}
