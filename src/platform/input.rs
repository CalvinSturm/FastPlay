#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    TogglePause,
    ToggleSubtitles,
    SeekRelativeSeconds(i64),
}
