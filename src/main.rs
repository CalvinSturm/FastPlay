mod app;
mod audio;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::{ffi::c_void, time::Instant};

use app::commands::SessionCommand;
use app::session::PlaybackSession;
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
    let media_path = parse_media_source_from_args()?;
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;

    // SAFETY: `session` lives on this stack frame for the entire main loop.
    // The callback is cleared before `session` is dropped.
    unsafe {
        let ctx = &mut session as *mut PlaybackSession as *mut c_void;
        session.window().install_modal_tick(ctx, modal_tick_trampoline);
    }

    if let Some(source) = media_path {
        session.open(source, Instant::now())?;
    }

    while session.window().is_open() {
        session.window().pump_messages()?;
        for input in session.window().take_input_events() {
            match input {
                InputEvent::TogglePause => {
                    session.apply_command(SessionCommand::TogglePause, Instant::now())?;
                }
                InputEvent::ToggleSubtitles => {
                    session.apply_command(SessionCommand::ToggleSubtitles, Instant::now())?;
                }
                InputEvent::SeekRelativeSeconds(offset_seconds) => {
                    let snapshot = session.snapshot(Instant::now());
                    let next_position = if offset_seconds >= 0 {
                        snapshot
                            .position
                            .saturating_add(std::time::Duration::from_secs(offset_seconds as u64))
                    } else {
                        snapshot
                            .position
                            .saturating_sub(std::time::Duration::from_secs((-offset_seconds) as u64))
                    };
                    session.apply_command(SessionCommand::Seek(SeekTarget::new(next_position)), Instant::now())?;
                }
                InputEvent::ToggleBorderlessFullscreen => {
                    session.apply_command(SessionCommand::ToggleBorderlessFullscreen, Instant::now())?;
                }
                InputEvent::ZoomAtCursor { delta, cursor_x, cursor_y } => {
                    session.apply_command(SessionCommand::ZoomAtCursor { delta, cursor_x, cursor_y }, Instant::now())?;
                }
                InputEvent::ResetView => {
                    session.apply_command(SessionCommand::ResetView, Instant::now())?;
                }
            }
        }
        session.tick(Instant::now())?;
    }

    session.window().clear_modal_tick();
    Ok(())
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
