mod app;
mod audio;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::time::Instant;

use app::commands::SessionCommand;
use app::session::PlaybackSession;
use media::{
    seek::SeekTarget,
    source::MediaSource,
    video::VideoDecodePreference,
};
use platform::input::InputEvent;
use platform::window::NativeWindow;

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
    if let Some(source) = media_path {
        session.open(source, Instant::now())?;
    }

    while session.window().is_open() {
        session.window_mut().pump_messages()?;
        for input in session.window().take_input_events() {
            match input {
                InputEvent::TogglePause => {
                    session.apply_command(SessionCommand::TogglePause, Instant::now())?;
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
            }
        }
        session.tick(Instant::now())?;
    }

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
