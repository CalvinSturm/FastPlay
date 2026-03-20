#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
use render::timeline::{self, TimelineOverlayModel};

const SEEK_OVERLAY_DURATION: std::time::Duration = std::time::Duration::from_millis(800);

struct TimelineUiState {
    was_left_button_down: bool,
    scrubbing: bool,
    scrub_was_paused: bool,
    scrub_origin: Option<std::time::Duration>,
    preview_target: Option<std::time::Duration>,
    last_overlay: Option<TimelineOverlayModel>,
    seek_overlay_until: Option<Instant>,
}

impl TimelineUiState {
    fn new() -> Self {
        Self {
            was_left_button_down: false,
            scrubbing: false,
            scrub_was_paused: false,
            scrub_origin: None,
            preview_target: None,
            last_overlay: None,
            seek_overlay_until: None,
        }
    }

    fn is_scrubbing(&self) -> bool {
        self.scrubbing
    }

    fn cancel_scrub(
        &mut self,
        session: &mut PlaybackSession,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(origin) = self.scrub_origin {
            session.scrub_seek(SeekTarget::new(origin), self.scrub_was_paused, now)?;
            if !self.scrub_was_paused && session.is_paused() {
                session.apply_command(SessionCommand::TogglePause, now)?;
            }
        }
        self.scrubbing = false;
        self.scrub_origin = None;
        self.preview_target = None;
        Ok(())
    }

    fn update(
        &mut self,
        session: &mut PlaybackSession,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(duration) = session.media_duration() else {
            self.scrubbing = false;
            self.preview_target = None;
            self.sync_overlay(session, None)?;
            self.was_left_button_down = session.window().is_left_button_down();
            return Ok(());
        };

        let is_borderless = session.window().is_borderless();
        let (viewport_width, viewport_height) = session.window().client_size()?;
        let cursor = session.window().cursor_client_position()?;
        let left_button_down = session.window().is_left_button_down();
        let hovered = cursor
            .is_some_and(|(x, y)| timeline::activation_hit_test(viewport_width, viewport_height, x, y));

        if is_borderless {
            self.scrubbing = false;
            self.scrub_origin = None;
            self.preview_target = None;
        } else if !self.was_left_button_down && left_button_down {
            if let Some((x, y)) = cursor {
                if timeline::scrub_hit_test(viewport_width, viewport_height, x, y) {
                    self.scrubbing = true;
                    self.scrub_was_paused = session.is_paused();
                    let snapshot = session.snapshot(now);
                    self.scrub_origin = Some(snapshot.position.min(duration));
                    let target = timeline::scrub_target_from_cursor(
                        viewport_width,
                        viewport_height,
                        duration,
                        x,
                    );
                    self.preview_target = Some(target);
                    self.scrub_seek(session, target, now)?;
                }
            }
        } else if self.scrubbing && left_button_down {
            if let Some((x, _)) = cursor {
                let target = timeline::scrub_target_from_cursor(
                    viewport_width,
                    viewport_height,
                    duration,
                    x,
                );
                if self.preview_target != Some(target) {
                    self.preview_target = Some(target);
                    self.scrub_seek(session, target, now)?;
                }
            }
        } else if self.scrubbing && self.was_left_button_down && !left_button_down {
            self.scrub_origin = None;
            self.preview_target = None;
            self.scrubbing = false;
            // Resume playback if it was playing before the scrub started.
            if !self.scrub_was_paused && session.is_paused() {
                session.apply_command(SessionCommand::TogglePause, now)?;
            }
        }

        let replay_indicator_active = session
            .replay_indicator_until()
            .is_some_and(|until| now < until);
        let seek_overlay_active = self
            .seek_overlay_until
            .is_some_and(|until| now < until);
        let visible = !is_borderless
            && duration > std::time::Duration::ZERO
            && (hovered || self.scrubbing || replay_indicator_active || seek_overlay_active);
        let snapshot = session.snapshot(now);
        let display_position = self.scrub_origin.unwrap_or(snapshot.position).min(duration);
        let overlay = if visible {
            timeline::build_overlay_model(
                viewport_width,
                viewport_height,
                display_position,
                self.preview_target,
                duration,
                session.auto_replay(),
            )
        } else {
            None
        };
        self.sync_overlay(session, overlay)?;
        self.was_left_button_down = left_button_down;
        Ok(())
    }

    fn scrub_seek(
        &self,
        session: &mut PlaybackSession,
        target: std::time::Duration,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        session.scrub_seek(SeekTarget::new(target), self.scrub_was_paused, now)
    }

    fn sync_overlay(
        &mut self,
        session: &mut PlaybackSession,
        overlay: Option<TimelineOverlayModel>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.last_overlay == overlay {
            return Ok(());
        }

        session.set_timeline_overlay(overlay)?;
        self.last_overlay = overlay;
        Ok(())
    }
}

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
                    timeline_ui.seek_overlay_until = Some(now + SEEK_OVERLAY_DURATION);
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
                InputEvent::ToggleAutoReplay => {
                    session.apply_command(SessionCommand::ToggleAutoReplay, now)?;
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
                    if timeline_ui.is_scrubbing() {
                        timeline_ui.cancel_scrub(&mut session, now)?;
                    }
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
