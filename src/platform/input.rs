//! Platform-independent input vocabulary and the keyboard mapping.
//!
//! [`InputEvent`] is the crate's typed input-command enum: the Win32 seam
//! produces them, the event loop in `main` consumes them, and
//! `app::input_dispatch` converts the context-free ones into `SessionCommand`s.
//!
//! [`command_for_key`] and [`command_for_key_release`] are the whole keyboard
//! shortcut policy, as pure functions. They were extracted out of `window_proc`
//! in `ffi/dxgi.rs` (see `docs/audits/codebase-review.md` §10 Stage 2): the
//! mapping needs nothing from Win32 beyond a virtual-key code, the Ctrl/Shift
//! state, and the auto-repeat flag, so keeping it in the unsafe seam made the
//! single largest untested surface in the program untestable. It had already
//! shipped one user-visible bug that way.

use std::path::PathBuf;

/// Windows virtual-key codes for the keys FastPlay binds, so the mapping below
/// reads as shortcuts rather than magic numbers. Values are the Win32 `VK_*`
/// constants; they are spelled out here rather than imported from `windows` so
/// this module stays free of platform dependencies and testable anywhere.
mod vk {
    pub const BACK: u32 = 0x08;
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    /// Page Up (`VK_PRIOR`).
    pub const PRIOR: u32 = 0x21;
    /// Page Down (`VK_NEXT`).
    pub const NEXT: u32 = 0x22;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const DELETE: u32 = 0x2E;
    pub const DIGIT_0: u32 = 0x30;
    pub const B: u32 = 0x42;
    pub const E: u32 = 0x45;
    pub const F: u32 = 0x46;
    pub const H: u32 = 0x48;
    pub const I: u32 = 0x49;
    pub const O: u32 = 0x4F;
    pub const Q: u32 = 0x51;
    pub const R: u32 = 0x52;
    pub const S: u32 = 0x53;
    pub const W: u32 = 0x57;
    /// `` ` `` on a US layout (`VK_OEM_3`).
    pub const BACKTICK: u32 = 0xC0;
    /// `[` (`VK_OEM_4`).
    pub const LEFT_BRACKET: u32 = 0xDB;
    /// `\` (`VK_OEM_5`).
    pub const BACKSLASH: u32 = 0xDC;
    /// `]` (`VK_OEM_6`).
    pub const RIGHT_BRACKET: u32 = 0xDD;
}

/// Seconds an arrow-key seek moves on the initial press.
const SEEK_STEP_SECONDS: i64 = 5;
/// Seconds an arrow-key seek moves once the key is auto-repeating, so holding
/// the key scans faster than tapping it.
const SEEK_STEP_REPEAT_SECONDS: i64 = 15;

/// The keyboard shortcut for `vk` pressed with the given modifier and
/// auto-repeat state, or `None` if the combination is unbound.
///
/// `is_repeat` is Win32's "previous key state" (lparam bit 30): false on the
/// initial press, true for every auto-repeat while the key is held.
///
/// This matches on the virtual key *first* and resolves modifiers inside each
/// arm, which is deliberate. The previous implementation used one flat match
/// with guard clauses, so a guarded arm (`S if ctrl && !is_repeat`) could fall
/// silently through to a later unguarded arm for the same key (`S`) — which is
/// exactly the bug that made a held Ctrl+S toggle subtitles. One arm per key
/// makes that class of mistake unrepresentable: every arm must account for its
/// own modifier combinations.
///
/// Alt is not consulted. Alt combinations arrive as `WM_SYSKEYDOWN`, which the
/// window procedure does not handle, so they never reach this function.
pub(crate) fn command_for_key(
    vk: u32,
    ctrl: bool,
    shift: bool,
    is_repeat: bool,
) -> Option<InputEvent> {
    match vk {
        // Ctrl+H → borderless fullscreen (repeats, as it always has).
        // H alone → show the help overlay while held, on first press only.
        vk::H => {
            if ctrl {
                Some(InputEvent::ToggleBorderlessFullscreen)
            } else if is_repeat {
                None
            } else {
                Some(InputEvent::ShowHelp)
            }
        }
        // Ctrl+S → screenshot, first press only (holding must not spray files).
        // S alone → toggle subtitles.
        vk::S => {
            if ctrl {
                if is_repeat {
                    None
                } else {
                    Some(InputEvent::SaveScreenshot)
                }
            } else {
                Some(InputEvent::ToggleSubtitles)
            }
        }
        // I → set in-point, Shift+I → clear it.
        // Ctrl+I is deliberately unbound: it used to clear the in-point, and is
        // reserved rather than reassigned so the old muscle memory does nothing
        // instead of doing something else.
        vk::I => {
            if ctrl {
                None
            } else if shift {
                Some(InputEvent::ClearInPoint)
            } else {
                Some(InputEvent::SetInPoint)
            }
        }
        // Ctrl+Shift+O → recent files, Ctrl+O → open dialog,
        // Shift+O → clear out-point, O → set out-point.
        vk::O => match (ctrl, shift) {
            (true, true) => Some(InputEvent::ToggleRecentOverlay),
            (true, false) => Some(InputEvent::OpenFileDialog),
            (false, true) => Some(InputEvent::ClearOutPoint),
            (false, false) => Some(InputEvent::SetOutPoint),
        },
        // Ctrl+R → rotate clockwise, R → toggle loop/auto-replay.
        vk::R => {
            if ctrl {
                Some(InputEvent::RotateClockwise)
            } else {
                Some(InputEvent::ToggleLoopRange)
            }
        }
        // Ctrl+E → rotate counter-clockwise.
        vk::E => ctrl.then_some(InputEvent::RotateCounterClockwise),
        // Ctrl+F / Ctrl+B → step one frame forward / backward.
        vk::F => ctrl.then_some(InputEvent::StepFrameForward),
        vk::B => ctrl.then_some(InputEvent::StepFrameBackward),
        // Ctrl+W → fit the window to the video (no black padding).
        vk::W => ctrl.then_some(InputEvent::FitWindow),
        // Ctrl+Q → half the video's native resolution.
        vk::Q => ctrl.then_some(InputEvent::HalfSizeWindow),
        // Ctrl+0 → reset zoom/pan/rotation.
        vk::DIGIT_0 => ctrl.then_some(InputEvent::ResetView),
        // ` → toggle decode info in the title bar.
        vk::BACKTICK => Some(InputEvent::ToggleDecodeInfo),
        // Esc → leave borderless fullscreen / close the recent overlay.
        vk::ESCAPE => Some(InputEvent::EscapeKey),
        // Backspace → cancel an in-progress scrub.
        vk::BACK => Some(InputEvent::BackspaceKey),
        // [ slower, ] faster, \ back to 1x.
        vk::LEFT_BRACKET => Some(InputEvent::StepPlaybackRate(-1)),
        vk::RIGHT_BRACKET => Some(InputEvent::StepPlaybackRate(1)),
        vk::BACKSLASH => Some(InputEvent::ResetPlaybackRate),
        // Left / Right → relative seek, accelerating while held.
        vk::LEFT => Some(InputEvent::SeekRelativeSeconds(-seek_step(is_repeat))),
        vk::RIGHT => Some(InputEvent::SeekRelativeSeconds(seek_step(is_repeat))),
        // Up / Down → move the recent-overlay selection; repeats are wanted here.
        vk::UP => Some(InputEvent::NavigateUp),
        vk::DOWN => Some(InputEvent::NavigateDown),
        // Enter / Delete → confirm / remove the selection, first press only.
        vk::RETURN => (!is_repeat).then_some(InputEvent::Confirm),
        vk::DELETE => (!is_repeat).then_some(InputEvent::RemoveSelected),
        // PageUp / PageDown → previous / next queue item, first press only:
        // holding must not blast through the whole queue.
        vk::PRIOR => (!is_repeat).then_some(InputEvent::QueuePrevious),
        vk::NEXT => (!is_repeat).then_some(InputEvent::QueueNext),
        _ => None,
    }
}

/// The shortcut for *releasing* `vk`, or `None`. Only the help overlay is
/// hold-to-show, so this is the one key release the player reacts to. Modifier
/// state is deliberately not consulted: releasing H after Ctrl+H also emits
/// `HideHelp`, which is a no-op because the overlay was never shown.
pub(crate) fn command_for_key_release(vk: u32) -> Option<InputEvent> {
    match vk {
        vk::H => Some(InputEvent::HideHelp),
        _ => None,
    }
}

/// Seconds one arrow-key seek covers, larger while the key auto-repeats.
fn seek_step(is_repeat: bool) -> i64 {
    if is_repeat {
        SEEK_STEP_REPEAT_SECONDS
    } else {
        SEEK_STEP_SECONDS
    }
}

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
    NavigateUp,
    NavigateDown,
    Confirm,
    RemoveSelected,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the characterization table: the exact shortcut behavior the
    /// Win32 `window_proc` match arms had before this mapping was extracted.
    /// Written against the pre-extraction implementation, arm by arm, so the
    /// move is provably behavior-preserving.
    struct Case {
        /// Human-readable shortcut, for assertion failure messages.
        name: &'static str,
        vk: u32,
        ctrl: bool,
        shift: bool,
        is_repeat: bool,
        expected: Option<InputEvent>,
    }

    const fn case(
        name: &'static str,
        vk: u32,
        ctrl: bool,
        shift: bool,
        is_repeat: bool,
        expected: Option<InputEvent>,
    ) -> Case {
        Case {
            name,
            vk,
            ctrl,
            shift,
            is_repeat,
            expected,
        }
    }

    /// Every mapped shortcut, plus the modifier and repeat combinations that
    /// must produce nothing. Ordering mirrors the original match arms.
    fn characterization_table() -> Vec<Case> {
        use InputEvent::*;
        vec![
            // ── H: Ctrl+H fullscreen (repeats), H help (first press only) ──
            case(
                "Ctrl+H",
                vk::H,
                true,
                false,
                false,
                Some(ToggleBorderlessFullscreen),
            ),
            case(
                "Ctrl+H (repeat)",
                vk::H,
                true,
                false,
                true,
                Some(ToggleBorderlessFullscreen),
            ),
            case(
                "Ctrl+Shift+H",
                vk::H,
                true,
                true,
                false,
                Some(ToggleBorderlessFullscreen),
            ),
            case("H", vk::H, false, false, false, Some(ShowHelp)),
            case("H (repeat)", vk::H, false, false, true, None),
            case("Shift+H", vk::H, false, true, false, Some(ShowHelp)),
            // ── S: the Ctrl+S / subtitles collision this suite exists for ──
            case("Ctrl+S", vk::S, true, false, false, Some(SaveScreenshot)),
            case("Ctrl+S (repeat)", vk::S, true, false, true, None),
            case(
                "Ctrl+Shift+S",
                vk::S,
                true,
                true,
                false,
                Some(SaveScreenshot),
            ),
            case("Ctrl+Shift+S (repeat)", vk::S, true, true, true, None),
            case("S", vk::S, false, false, false, Some(ToggleSubtitles)),
            case(
                "S (repeat)",
                vk::S,
                false,
                false,
                true,
                Some(ToggleSubtitles),
            ),
            case("Shift+S", vk::S, false, true, false, Some(ToggleSubtitles)),
            // ── I: in-point, with Ctrl+I reserved as an explicit no-op ──
            case("I", vk::I, false, false, false, Some(SetInPoint)),
            case("I (repeat)", vk::I, false, false, true, Some(SetInPoint)),
            case("Shift+I", vk::I, false, true, false, Some(ClearInPoint)),
            case("Ctrl+I (reserved)", vk::I, true, false, false, None),
            case("Ctrl+Shift+I (reserved)", vk::I, true, true, false, None),
            // ── O: all four modifier combinations are bound ──
            case("O", vk::O, false, false, false, Some(SetOutPoint)),
            case("Shift+O", vk::O, false, true, false, Some(ClearOutPoint)),
            case("Ctrl+O", vk::O, true, false, false, Some(OpenFileDialog)),
            case(
                "Ctrl+Shift+O",
                vk::O,
                true,
                true,
                false,
                Some(ToggleRecentOverlay),
            ),
            case(
                "Ctrl+O (repeat)",
                vk::O,
                true,
                false,
                true,
                Some(OpenFileDialog),
            ),
            // ── R: Ctrl+R rotate, R loop ──
            case("Ctrl+R", vk::R, true, false, false, Some(RotateClockwise)),
            case(
                "Ctrl+R (repeat)",
                vk::R,
                true,
                false,
                true,
                Some(RotateClockwise),
            ),
            case("R", vk::R, false, false, false, Some(ToggleLoopRange)),
            case("Shift+R", vk::R, false, true, false, Some(ToggleLoopRange)),
            // ── Ctrl-only shortcuts: unmodified key is unbound ──
            case(
                "Ctrl+E",
                vk::E,
                true,
                false,
                false,
                Some(RotateCounterClockwise),
            ),
            case("E", vk::E, false, false, false, None),
            case("Ctrl+F", vk::F, true, false, false, Some(StepFrameForward)),
            case(
                "Ctrl+F (repeat)",
                vk::F,
                true,
                false,
                true,
                Some(StepFrameForward),
            ),
            case("F", vk::F, false, false, false, None),
            case("Ctrl+B", vk::B, true, false, false, Some(StepFrameBackward)),
            case("B", vk::B, false, false, false, None),
            case("Ctrl+W", vk::W, true, false, false, Some(FitWindow)),
            case("W", vk::W, false, false, false, None),
            case("Ctrl+Q", vk::Q, true, false, false, Some(HalfSizeWindow)),
            case("Q", vk::Q, false, false, false, None),
            case("Ctrl+0", vk::DIGIT_0, true, false, false, Some(ResetView)),
            case("0", vk::DIGIT_0, false, false, false, None),
            // ── Modifier-insensitive keys: bound regardless of Ctrl/Shift ──
            case(
                "`",
                vk::BACKTICK,
                false,
                false,
                false,
                Some(ToggleDecodeInfo),
            ),
            case(
                "Ctrl+`",
                vk::BACKTICK,
                true,
                false,
                false,
                Some(ToggleDecodeInfo),
            ),
            case("Esc", vk::ESCAPE, false, false, false, Some(EscapeKey)),
            case("Ctrl+Esc", vk::ESCAPE, true, false, false, Some(EscapeKey)),
            case(
                "Backspace",
                vk::BACK,
                false,
                false,
                false,
                Some(BackspaceKey),
            ),
            case(
                "Ctrl+Backspace",
                vk::BACK,
                true,
                false,
                false,
                Some(BackspaceKey),
            ),
            case(
                "[",
                vk::LEFT_BRACKET,
                false,
                false,
                false,
                Some(StepPlaybackRate(-1)),
            ),
            case(
                "]",
                vk::RIGHT_BRACKET,
                false,
                false,
                false,
                Some(StepPlaybackRate(1)),
            ),
            case(
                "\\",
                vk::BACKSLASH,
                false,
                false,
                false,
                Some(ResetPlaybackRate),
            ),
            case(
                "] (repeat)",
                vk::RIGHT_BRACKET,
                false,
                false,
                true,
                Some(StepPlaybackRate(1)),
            ),
            // ── Arrows: repeat accelerates the seek rather than gating it ──
            case(
                "Left",
                vk::LEFT,
                false,
                false,
                false,
                Some(SeekRelativeSeconds(-5)),
            ),
            case(
                "Left (repeat)",
                vk::LEFT,
                false,
                false,
                true,
                Some(SeekRelativeSeconds(-15)),
            ),
            case(
                "Right",
                vk::RIGHT,
                false,
                false,
                false,
                Some(SeekRelativeSeconds(5)),
            ),
            case(
                "Right (repeat)",
                vk::RIGHT,
                false,
                false,
                true,
                Some(SeekRelativeSeconds(15)),
            ),
            case(
                "Ctrl+Right",
                vk::RIGHT,
                true,
                false,
                false,
                Some(SeekRelativeSeconds(5)),
            ),
            case(
                "Shift+Left",
                vk::LEFT,
                false,
                true,
                false,
                Some(SeekRelativeSeconds(-5)),
            ),
            // ── Overlay navigation: repeats wanted ──
            case("Up", vk::UP, false, false, false, Some(NavigateUp)),
            case("Up (repeat)", vk::UP, false, false, true, Some(NavigateUp)),
            case("Down", vk::DOWN, false, false, false, Some(NavigateDown)),
            case(
                "Down (repeat)",
                vk::DOWN,
                false,
                false,
                true,
                Some(NavigateDown),
            ),
            // ── First-press-only keys ──
            case("Enter", vk::RETURN, false, false, false, Some(Confirm)),
            case("Enter (repeat)", vk::RETURN, false, false, true, None),
            case(
                "Delete",
                vk::DELETE,
                false,
                false,
                false,
                Some(RemoveSelected),
            ),
            case("Delete (repeat)", vk::DELETE, false, false, true, None),
            case(
                "PageUp",
                vk::PRIOR,
                false,
                false,
                false,
                Some(QueuePrevious),
            ),
            case("PageUp (repeat)", vk::PRIOR, false, false, true, None),
            case("PageDown", vk::NEXT, false, false, false, Some(QueueNext)),
            case("PageDown (repeat)", vk::NEXT, false, false, true, None),
            // ── Unmapped keys ──
            case("A", 0x41, false, false, false, None),
            case("Ctrl+A", 0x41, true, false, false, None),
            case("Z", 0x5A, false, false, false, None),
            case("F1", 0x70, false, false, false, None),
            case(
                "Space (handled by WM_CHAR)",
                0x20,
                false,
                false,
                false,
                None,
            ),
            case("Tab", 0x09, false, false, false, None),
            case("VK 0x00", 0x00, false, false, false, None),
            case("VK 0xFF", 0xFF, true, true, true, None),
        ]
    }

    #[test]
    fn keymap_matches_the_characterization_table() {
        for Case {
            name,
            vk,
            ctrl,
            shift,
            is_repeat,
            expected,
        } in characterization_table()
        {
            assert_eq!(
                command_for_key(vk, ctrl, shift, is_repeat),
                expected,
                "{name} (vk={vk:#04X} ctrl={ctrl} shift={shift} repeat={is_repeat})"
            );
        }
    }

    #[test]
    fn characterization_table_covers_every_bound_key() {
        // Guards against a shortcut being added to the mapping without a row
        // here. Every virtual key the mapping can respond to must appear.
        let covered: std::collections::BTreeSet<u32> =
            characterization_table().iter().map(|c| c.vk).collect();
        for vk in 0u32..=0xFF {
            let bound = [
                (false, false, false),
                (false, false, true),
                (false, true, false),
                (true, false, false),
                (true, true, false),
            ]
            .iter()
            .any(|&(ctrl, shift, repeat)| command_for_key(vk, ctrl, shift, repeat).is_some());
            if bound {
                assert!(
                    covered.contains(&vk),
                    "virtual key {vk:#04X} is bound but has no characterization row"
                );
            }
        }
    }

    /// The regression this extraction exists to make impossible.
    ///
    /// `Ctrl+S` is guarded to the initial press so holding it cannot spray
    /// screenshots. In the old flat match that guard failing meant the arm did
    /// not match, and the *next* arm for the same virtual key — bare `S` —
    /// matched instead, so a held Ctrl+S toggled subtitles at the keyboard
    /// repeat rate. A repeat must produce nothing at all.
    #[test]
    fn held_ctrl_s_does_not_fall_through_to_toggle_subtitles() {
        assert_eq!(
            command_for_key(vk::S, true, false, false),
            Some(InputEvent::SaveScreenshot),
            "initial Ctrl+S still saves a screenshot"
        );
        for shift in [false, true] {
            assert_eq!(
                command_for_key(vk::S, true, shift, true),
                None,
                "a held Ctrl+S must emit nothing, not ToggleSubtitles"
            );
        }
    }

    /// Generalizes the case above. For every key that means one thing with
    /// Ctrl and something else without it, holding the Ctrl combination must
    /// never start producing the unmodified key's command.
    #[test]
    fn no_ctrl_shortcut_falls_through_to_its_unmodified_command() {
        // (key, name) pairs where both Ctrl+key and key are meaningful.
        let dual_meaning = [
            (vk::H, "H"),
            (vk::S, "S"),
            (vk::I, "I"),
            (vk::O, "O"),
            (vk::R, "R"),
        ];
        for (key, name) in dual_meaning {
            for shift in [false, true] {
                let unmodified = command_for_key(key, false, shift, false);
                let unmodified_repeat = command_for_key(key, false, shift, true);
                for is_repeat in [false, true] {
                    let with_ctrl = command_for_key(key, true, shift, is_repeat);
                    assert!(
                        with_ctrl.is_none()
                            || (with_ctrl != unmodified && with_ctrl != unmodified_repeat),
                        "Ctrl+{name} (shift={shift} repeat={is_repeat}) produced the \
                         unmodified key's command {with_ctrl:?} — a guard fell through"
                    );
                }
            }
        }
    }

    #[test]
    fn key_release_only_hides_the_help_overlay() {
        assert_eq!(command_for_key_release(vk::H), Some(InputEvent::HideHelp));
        // Every other key release is inert, including the ones with press bindings.
        for vk in [
            vk::S,
            vk::I,
            vk::O,
            vk::R,
            vk::ESCAPE,
            vk::LEFT,
            vk::RETURN,
            0x41,
        ] {
            assert_eq!(
                command_for_key_release(vk),
                None,
                "releasing {vk:#04X} must emit nothing"
            );
        }
    }

    #[test]
    fn seek_accelerates_only_on_repeat() {
        assert_eq!(seek_step(false), SEEK_STEP_SECONDS);
        assert_eq!(seek_step(true), SEEK_STEP_REPEAT_SECONDS);
        const {
            assert!(
                SEEK_STEP_REPEAT_SECONDS > SEEK_STEP_SECONDS,
                "holding an arrow key must scan faster than tapping it"
            )
        };
    }
}
