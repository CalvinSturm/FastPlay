mod app;
mod ffi;
mod platform;
mod playback;
mod render;

use std::time::Instant;

use app::session::PlaybackSession;
use platform::window::NativeWindow;

fn main() {
    if let Err(error) = run() {
        eprintln!("fastplay failed to start: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;

    while session.window().is_open() {
        session.window_mut().pump_messages()?;
        session.tick(Instant::now())?;
    }

    Ok(())
}
