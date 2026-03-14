mod app;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::time::Instant;

use app::session::PlaybackSession;
use media::source::MediaSource;
use platform::window::NativeWindow;

fn main() {
    if let Err(error) = run() {
        eprintln!("fastplay failed to start: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let media_path = std::env::args_os().nth(1).map(MediaSource::new);
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;
    if let Some(source) = media_path {
        session.open(source, Instant::now())?;
    }

    while session.window().is_open() {
        session.window_mut().pump_messages()?;
        session.tick(Instant::now())?;
    }

    Ok(())
}
