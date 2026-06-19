use std::time::{Duration, Instant};

use crate::app::commands::SessionCommand;
use crate::app::session::PlaybackSession;
use crate::media::seek::SeekTarget;
use crate::render::timeline::{self, TimelineOverlayModel};

pub const SEEK_OVERLAY_DURATION: Duration = Duration::from_millis(800);

pub struct TimelineUiState {
    was_left_button_down: bool,
    scrubbing: bool,
    scrub_was_paused: bool,
    scrub_origin: Option<Duration>,
    preview_target: Option<Duration>,
    last_overlay: Option<TimelineOverlayModel>,
    pub seek_overlay_until: Option<Instant>,
}

impl TimelineUiState {
    pub fn new() -> Self {
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

    pub fn is_scrubbing(&self) -> bool {
        self.scrubbing
    }

    pub fn cancel_scrub(
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

    pub fn update(
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
        let hovered = cursor.is_some_and(|(x, y)| {
            timeline::activation_hit_test(viewport_width, viewport_height, x, y)
        });

        if is_borderless {
            self.scrubbing = false;
            self.scrub_origin = None;
            self.preview_target = None;
        } else if !self.was_left_button_down && left_button_down && !session.window().is_ctrl_held()
        {
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
            // Land deterministically. Intermediate previews pause-after-seek.
            // Resume only when the target is inside the active I/O range; an
            // outside-range target is an inspection position and must remain
            // paused. Resuming beyond O immediately collides with out-point
            // enforcement and creates a redundant seek/stop cycle that presents
            // as lag and jitter.
            if !self.scrub_was_paused {
                if let Some(target) = self.preview_target {
                    if session.position_is_in_active_range(target) {
                        session.scrub_seek(SeekTarget::new(target), false, now)?;
                    }
                }
            }
            self.scrub_origin = None;
            self.preview_target = None;
            self.scrubbing = false;
        }

        let replay_indicator_active = session
            .replay_indicator_until()
            .is_some_and(|until| now < until);
        let seek_overlay_active = self.seek_overlay_until.is_some_and(|until| now < until);
        let visible = !is_borderless
            && duration > Duration::ZERO
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
                session.auto_replay() || session.loop_range(),
                session.in_point(),
                session.out_point(),
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
        target: Duration,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Pause-preview every intermediate scrub target, regardless of whether
        // playback was running when the drag began. This routes the seek through
        // the `pause_after_seek` path, which only pauses the audio sink instead
        // of doing a full `sink.reset()` per target — rapid resets churn WASAPI
        // (0x88890005) and stall the audio clock, slowing playback after the
        // scrub. The mouse-up handler resumes playback if `!scrub_was_paused`.
        session.scrub_seek(SeekTarget::new(target), true, now)
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
