#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::{ffi::c_void, time::Instant};

use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

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

/// Trampoline called by the Win32 modal move/resize timer.
///
/// # Safety
///
/// `ctx` must point to a live `PlaybackSession` on the current thread.
unsafe fn modal_tick_trampoline(ctx: *mut c_void) {
    let session = &mut *(ctx as *mut PlaybackSession);
    let _ = session.tick(Instant::now());
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fastplay failed to start: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Raise the Windows multimedia timer resolution to 1 ms so that
    // thread::sleep(1ms) wakes up on time.  Without this the default
    // resolution is ~15 ms, which causes the main loop to miss video frame
    // deadlines and produces stuttering / A-V desync.
    // SAFETY: balanced by timeEndPeriod(1) at the end of this function.
    unsafe { timeBeginPeriod(1) };

    let media_path = parse_media_source_from_args()?;
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;
    let mut timeline_ui = TimelineUiState::new();

    // SAFETY: `session` lives on this stack frame for the entire main loop.
    // The callback is cleared before `session` is dropped.
    unsafe {
        let ctx = &mut session as *mut PlaybackSession as *mut c_void;
        session.window().install_modal_tick(ctx, modal_tick_trampoline);
    }

    if let Some(source) = media_path {
        let title = window_title_for(&source);
        session.open(source, Instant::now())?;
        session.window().set_title(&title);
    }

    let mut input_events: Vec<InputEvent> = Vec::new();
    while session.window().is_open() {
        session.window().pump_messages()?;
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
    }

    session.window().clear_modal_tick();
    unsafe { timeEndPeriod(1) };
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
