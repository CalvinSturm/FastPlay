#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    TogglePause,
    SeekRelativeSeconds(i64),
}
