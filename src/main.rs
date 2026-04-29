#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Baseline allow-list for CI: these categories are pervasive in the Win32/FFI
// shims (self-transmutes through typed HANDLEs, Win32 naming conventions,
// high-arity GPU render calls) or represent stylistic debt we haven't paid
// down yet. New violations in other categories still fail `-D warnings`.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::useless_transmute)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::type_complexity)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::manual_dangling_ptr)]
#![allow(clippy::cmp_null)]

mod app;
mod audio;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::time::{Duration, Instant};

use app::commands::SessionCommand;
use app::session::PlaybackSession;
use app::timeline_ui::TimelineUiState;
use media::{
    seek::SeekTarget,
    source::MediaSource,
    video::VideoDecodePreference,
};
use platform::input::InputEvent;
use platform::window::NativeWindow;

fn main() {
    // ── Persistent stderr log ──────────────────────────────────────────
    // Redirect stderr (fd 2) to a log file so that ALL eprintln! tracing
    // is captured even when the process is killed by an access violation.
    ffi::runtime::redirect_stderr_to_appdata_log();

    // ── Vectored Exception Handler ─────────────────────────────────────
    // Access violations from d3d11.dll kill the process instantly —
    // Rust's panic handler never fires.  A VEH runs BEFORE the default
    // handler, giving us a chance to write crash context to disk.
    ffi::runtime::install_crash_handler();

    std::panic::set_hook(Box::new(|info| {
        let msg = format!("panic: {info}");
        eprintln!("{msg}");
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = std::path::PathBuf::from(appdata).join("FastPlay");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.log"), &msg);
        }
    }));
    if let Err(error) = run() {
        let msg = format!("fatal: {error}");
        eprintln!("{msg}");
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = std::path::PathBuf::from(appdata).join("FastPlay");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.log"), &msg);
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Raise the Windows multimedia timer resolution to 1 ms so that
    // thread::sleep(1ms) wakes up on time.  Without this the default
    // resolution is ~15 ms, which causes the main loop to miss video frame
    // deadlines and produces stuttering / A-V desync.
    let _timer_resolution = ffi::runtime::MultimediaTimerResolution::begin_1ms();

    let media_path = parse_media_source_from_args()?;
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;
    let mut timeline_ui = TimelineUiState::new();

    ffi::dxgi::install_modal_tick(&mut session);

    if let Some(source) = media_path {
        let title = window_title_for(&source);
        session.open(source, Instant::now())?;
        session.window().set_title(&title);
    }

    let mut input_events: Vec<InputEvent> = Vec::new();
    while session.window().is_open() {
        session.window().pump_messages()?;
        // WM_CLOSE may have destroyed the window during pump_messages.
        if !session.window().is_open() {
            break;
        }
        let now = Instant::now();
        session.window().take_input_events(&mut input_events);
        for input in input_events.drain(..) {
            match input {
                InputEvent::TogglePause => {
                    session.apply_command(SessionCommand::TogglePause, now)?;
                }
                InputEvent::ToggleSubtitles => {
                    session.apply_command(SessionCommand::ToggleSubtitles, now)?;
                }
                InputEvent::SeekRelativeSeconds(offset_seconds) => {
                    let snapshot = session.snapshot(now);
                    let next_position = if offset_seconds >= 0 {
                        snapshot
                            .position
                            .saturating_add(std::time::Duration::from_secs(offset_seconds as u64))
                    } else {
                        snapshot
                            .position
                            .saturating_sub(std::time::Duration::from_secs((-offset_seconds) as u64))
                    };
                    session.apply_command(SessionCommand::Seek(SeekTarget::new(next_position)), now)?;
                    timeline_ui.seek_overlay_until = Some(now + app::timeline_ui::SEEK_OVERLAY_DURATION);
                }
                InputEvent::AdjustVolumeSteps(steps) => {
                    session.apply_command(SessionCommand::AdjustVolumeSteps(steps), now)?;
                }
                InputEvent::RotateClockwise => {
                    session.apply_command(SessionCommand::RotateClockwise, now)?;
                }
                InputEvent::RotateCounterClockwise => {
                    session.apply_command(SessionCommand::RotateCounterClockwise, now)?;
                }
                InputEvent::ToggleBorderlessFullscreen => {
                    session.apply_command(SessionCommand::ToggleBorderlessFullscreen, now)?;
                }
                InputEvent::ZoomAtCursor { delta, cursor_x, cursor_y } => {
                    session.apply_command(SessionCommand::ZoomAtCursor { delta, cursor_x, cursor_y }, now)?;
                }
                InputEvent::ResetView => {
                    session.apply_command(SessionCommand::ResetView, now)?;
                }
                InputEvent::SetInPoint => {
                    session.apply_command(SessionCommand::SetInPoint, now)?;
                }
                InputEvent::ClearInPoint => {
                    session.apply_command(SessionCommand::ClearInPoint, now)?;
                }
                InputEvent::SetOutPoint => {
                    session.apply_command(SessionCommand::SetOutPoint, now)?;
                }
                InputEvent::ClearOutPoint => {
                    session.apply_command(SessionCommand::ClearOutPoint, now)?;
                }
                InputEvent::ToggleLoopRange => {
                    session.apply_command(SessionCommand::ToggleLoopRange, now)?;
                }
                InputEvent::FitWindow => {
                    session.apply_command(SessionCommand::FitWindow, now)?;
                }
                InputEvent::HalfSizeWindow => {
                    session.apply_command(SessionCommand::HalfSizeWindow, now)?;
                }
                InputEvent::ToggleDecodeInfo => {
                    session.apply_command(SessionCommand::ToggleDecodeInfo, now)?;
                }
                InputEvent::EscapeKey => {
                    if session.window().is_borderless() {
                        session.apply_command(SessionCommand::ToggleBorderlessFullscreen, now)?;
                    }
                }
                InputEvent::BackspaceKey => {
                    if timeline_ui.is_scrubbing() {
                        timeline_ui.cancel_scrub(&mut session, now)?;
                    }
                }
                InputEvent::StepPlaybackRate(step) => {
                    session.apply_command(SessionCommand::StepPlaybackRate(step), now)?;
                }
                InputEvent::ResetPlaybackRate => {
                    session.apply_command(SessionCommand::ResetPlaybackRate, now)?;
                }
                InputEvent::PanDelta { dx, dy } => {
                    session.apply_command(
                        SessionCommand::PanBy { dx: dx as f32, dy: dy as f32 },
                        now,
                    )?;
                }
                InputEvent::ShowHelp => {
                    session.apply_command(SessionCommand::ShowHelp, now)?;
                }
                InputEvent::HideHelp => {
                    session.apply_command(SessionCommand::HideHelp, now)?;
                }
                InputEvent::FileDropped(path) => {
                    let source = MediaSource::new(path);
                    let source = if session.decode_preference() == VideoDecodePreference::ForceSoftware {
                        source.with_decode_preference(VideoDecodePreference::ForceSoftware)
                    } else {
                        source
                    };
                    let title = window_title_for(&source);
                    session.open(source, now)?;
                    session.window().set_title(&title);
                }
            }
        }
        timeline_ui.update(&mut session, now)?;
        session.tick(now)?;
        if session.take_idle_pace_request() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    session.window().clear_modal_tick();
    Ok(())
}

fn window_title_for(source: &MediaSource) -> String {
    let name = source.path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("FastPlay");
    format!("{name} - FastPlay")
}

fn parse_media_source_from_args() -> Result<Option<MediaSource>, Box<dyn std::error::Error>> {
    let mut force_software = false;
    let mut media_path = None;

    for argument in std::env::args_os().skip(1) {
        if media_path.is_none() && argument == "--force-sw" {
            force_software = true;
            continue;
        }

        if media_path.is_some() {
            return Err("usage: fastplay [--force-sw] <media-path>".into());
        }

        media_path = Some(argument);
    }

    Ok(media_path.map(|path| {
        let source = MediaSource::new(path);
        if force_software {
            source.with_decode_preference(VideoDecodePreference::ForceSoftware)
        } else {
            source
        }
    }))
}
