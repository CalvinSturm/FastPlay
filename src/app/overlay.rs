use std::time::{Duration, Instant};

use crate::media::subtitle::SubtitleTrack;

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
}
