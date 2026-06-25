use std::time::{Duration, Instant};

use crate::media::subtitle::SubtitleTrack;
use crate::render::presenter::Presenter;

/// Overlay state and the logic that drives the on-screen overlays (subtitles,
/// volume, decode-info window title). Owned by `PlaybackSession`.
///
/// This is a behavior-preserving extraction: `PlaybackSession` remains the
/// single coordinator and still owns the `Presenter`. The overlay-update
/// methods borrow the presenter for the duration of the call and report back
/// whether a present is needed, so the session keeps ownership of the
/// `present_needed` flag and the render loop.
pub struct OverlayManager {
    pub subtitle_track: Option<SubtitleTrack>,
    pub subtitles_enabled: bool,
    pub subtitle_clock_base: Option<Duration>,
    pub active_subtitle_cue: Option<usize>,
    pub active_subtitle_viewport: Option<(u32, u32)>,
    pub volume_overlay_until: Option<Instant>,
    pub replay_indicator_until: Option<Instant>,
    pub show_decode_info: bool,
}

impl Default for OverlayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayManager {
    pub fn new() -> Self {
        Self {
            subtitle_track: None,
            subtitles_enabled: true,
            subtitle_clock_base: None,
            active_subtitle_cue: None,
            active_subtitle_viewport: None,
            volume_overlay_until: None,
            replay_indicator_until: None,
            show_decode_info: false,
        }
    }

    pub fn replay_indicator_until(&self) -> Option<Instant> {
        self.replay_indicator_until
    }

    /// Compose the window title from the current file name, playback rate, and
    /// (when decode info is shown) the active decode mode label. The bracketed
    /// suffix is omitted entirely when there is nothing to show.
    pub fn window_title(
        &self,
        source_name: Option<&str>,
        playback_rate: f64,
        decode_label: Option<&str>,
    ) -> String {
        let base = source_name
            .map(|n| format!("{n} - FastPlay"))
            .unwrap_or_else(|| "FastPlay".to_owned());

        let mut suffixes: Vec<String> = Vec::new();
        if (playback_rate - 1.0).abs() >= 0.01 {
            let rate_str = if playback_rate.fract() == 0.0 {
                format!("{}x", playback_rate as u32)
            } else {
                format!("{playback_rate:.2}")
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_owned()
                    + "x"
            };
            suffixes.push(rate_str);
        }
        if self.show_decode_info {
            if let Some(label) = decode_label {
                suffixes.push(label.to_owned());
            }
        }

        if suffixes.is_empty() {
            base
        } else {
            format!("{base} [{}]", suffixes.join("  "))
        }
    }

    /// Clear the volume overlay once its display window has elapsed. Returns
    /// `true` if the presenter changed (so the caller should schedule a present).
    pub fn refresh_volume(
        &mut self,
        now: Instant,
        presenter: &mut Presenter,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let mut present_needed = false;
        if self.volume_overlay_until.is_some_and(|until| now > until) {
            if presenter.set_volume_overlay(None, 0, 0)? {
                present_needed = true;
            }
            self.volume_overlay_until = None;
        }
        Ok(present_needed)
    }

    /// Update the subtitle overlay for the given playback `position`. Returns
    /// `true` if the displayed cue or viewport changed (so the caller should
    /// schedule a present).
    pub fn update_subtitle(
        &mut self,
        position: Duration,
        presenter: &mut Presenter,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(track) = self.subtitle_track.as_ref() else {
            self.active_subtitle_cue = None;
            self.active_subtitle_viewport = None;
            presenter.clear_subtitle_overlay();
            return Ok(false);
        };
        if !self.subtitles_enabled {
            self.active_subtitle_cue = None;
            self.active_subtitle_viewport = None;
            presenter.clear_subtitle_overlay();
            return Ok(false);
        }

        let viewport = presenter.viewport_size()?;
        if viewport.0 == 0 || viewport.1 == 0 {
            return Ok(false);
        }

        let cue = track.cue_at(position, self.active_subtitle_cue);
        let next_index = cue.map(|(index, _)| index);
        if self.active_subtitle_cue == next_index && self.active_subtitle_viewport == Some(viewport)
        {
            return Ok(false);
        }

        match cue {
            Some((index, cue)) => {
                presenter.set_subtitle_overlay(Some(&cue.text), viewport.0, viewport.1)?;
                self.active_subtitle_cue = Some(index);
                self.active_subtitle_viewport = Some(viewport);
                crate::flog!(
                    "subtitle_cue index={} start_ms={} end_ms={}",
                    index,
                    cue.start.as_millis(),
                    cue.end.as_millis()
                );
            }
            None => {
                if self.active_subtitle_cue.take().is_some() {
                    crate::flog!("subtitle_cue cleared");
                }
                self.active_subtitle_viewport = Some(viewport);
                presenter.clear_subtitle_overlay();
            }
        }

        Ok(true)
    }

    /// Toggle subtitle visibility. No-op (logged) when no sidecar is loaded.
    /// Does not request a present itself; the next `update_subtitle` call
    /// reconciles the overlay.
    pub fn toggle_subtitles(
        &mut self,
        presenter: &mut Presenter,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.subtitle_track.is_none() {
            crate::flog!("subtitle toggle ignored: no external .srt sidecar was loaded");
            return Ok(());
        }

        self.subtitles_enabled = !self.subtitles_enabled;
        self.subtitle_clock_base = None;
        self.active_subtitle_cue = None;
        self.active_subtitle_viewport = None;
        if !self.subtitles_enabled {
            presenter.clear_subtitle_overlay();
        }
        crate::flog!("subtitles_enabled={}", self.subtitles_enabled);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_with_no_source_is_app_name() {
        let o = OverlayManager::new();
        assert_eq!(o.window_title(None, 1.0, None), "FastPlay");
    }

    #[test]
    fn title_with_source_at_normal_rate_has_no_suffix() {
        let o = OverlayManager::new();
        assert_eq!(
            o.window_title(Some("clip.mp4"), 1.0, None),
            "clip.mp4 - FastPlay"
        );
    }

    #[test]
    fn title_shows_integer_rate_without_decimals() {
        let o = OverlayManager::new();
        assert_eq!(
            o.window_title(Some("clip.mp4"), 2.0, None),
            "clip.mp4 - FastPlay [2x]"
        );
    }

    #[test]
    fn title_trims_trailing_zeros_on_fractional_rate() {
        let o = OverlayManager::new();
        assert_eq!(
            o.window_title(Some("clip.mp4"), 1.5, None),
            "clip.mp4 - FastPlay [1.5x]"
        );
        assert_eq!(
            o.window_title(Some("clip.mp4"), 1.25, None),
            "clip.mp4 - FastPlay [1.25x]"
        );
    }

    #[test]
    fn title_omits_decode_label_when_decode_info_hidden() {
        let o = OverlayManager::new();
        assert_eq!(
            o.window_title(Some("clip.mp4"), 1.0, Some("HW:D3D11")),
            "clip.mp4 - FastPlay"
        );
    }

    #[test]
    fn title_includes_decode_label_when_shown() {
        let mut o = OverlayManager::new();
        o.show_decode_info = true;
        assert_eq!(
            o.window_title(Some("clip.mp4"), 1.0, Some("HW:D3D11")),
            "clip.mp4 - FastPlay [HW:D3D11]"
        );
    }

    #[test]
    fn title_combines_rate_and_decode_label() {
        let mut o = OverlayManager::new();
        o.show_decode_info = true;
        assert_eq!(
            o.window_title(Some("clip.mp4"), 2.0, Some("SW")),
            "clip.mp4 - FastPlay [2x  SW]"
        );
    }

    #[test]
    fn title_near_normal_rate_is_treated_as_normal() {
        // Within the 0.01 epsilon, the rate suffix is suppressed.
        let o = OverlayManager::new();
        assert_eq!(
            o.window_title(Some("c.mp4"), 1.005, None),
            "c.mp4 - FastPlay"
        );
    }

    #[test]
    fn decode_label_requires_both_flag_and_label() {
        let mut o = OverlayManager::new();
        o.show_decode_info = true;
        // Flag on but no active decode mode -> no suffix.
        assert_eq!(o.window_title(Some("c.mp4"), 1.0, None), "c.mp4 - FastPlay");
    }
}
