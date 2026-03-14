use std::time::Instant;

#[derive(Debug, Default)]
pub struct PlaybackMetrics {
    last_present_at: Option<Instant>,
    last_resize_at: Option<Instant>,
    open_requested_at: Option<Instant>,
    first_frame_presented_at: Option<Instant>,
    first_audio_started_at: Option<Instant>,
    seek_requested_at: Option<Instant>,
    seek_first_frame_presented_at: Option<Instant>,
    seek_av_settled_at: Option<Instant>,
    resize_recovery_started_at: Option<Instant>,
    device_recovery_started_at: Option<Instant>,
    presented_video_frames: u64,
    dropped_video_frames: u64,
    audio_underruns: u64,
    ended_at: Option<Instant>,
}

impl PlaybackMetrics {
    pub fn note_open_requested(&mut self, now: Instant) {
        self.open_requested_at = Some(now);
        self.first_frame_presented_at = None;
        self.first_audio_started_at = None;
        self.seek_requested_at = None;
        self.seek_first_frame_presented_at = None;
        self.seek_av_settled_at = None;
        self.resize_recovery_started_at = None;
        self.device_recovery_started_at = None;
        self.presented_video_frames = 0;
        self.dropped_video_frames = 0;
        self.audio_underruns = 0;
        self.ended_at = None;
    }

    pub fn note_present(&mut self, now: Instant) {
        self.last_present_at = Some(now);
    }

    pub fn note_resize(&mut self, now: Instant) {
        self.last_resize_at = Some(now);
    }

    pub fn note_seek_requested(&mut self, now: Instant) {
        self.seek_requested_at = Some(now);
        self.seek_first_frame_presented_at = None;
        self.seek_av_settled_at = None;
    }

    pub fn note_first_frame_presented(&mut self, now: Instant) -> Option<std::time::Duration> {
        let open_started = self.open_requested_at?;
        self.first_frame_presented_at = Some(now);
        Some(now.saturating_duration_since(open_started))
    }

    pub fn note_first_audio_started(&mut self, now: Instant) -> Option<std::time::Duration> {
        let open_started = self.open_requested_at?;
        self.first_audio_started_at = Some(now);
        Some(now.saturating_duration_since(open_started))
    }

    pub fn note_seek_first_frame_presented(
        &mut self,
        now: Instant,
    ) -> Option<std::time::Duration> {
        let seek_started = self.seek_requested_at?;
        if self.seek_first_frame_presented_at.is_some() {
            return None;
        }
        self.seek_first_frame_presented_at = Some(now);
        Some(now.saturating_duration_since(seek_started))
    }

    pub fn note_seek_av_settled(&mut self, now: Instant) -> Option<std::time::Duration> {
        let seek_started = self.seek_requested_at?;
        if self.seek_av_settled_at.is_some() {
            return None;
        }
        self.seek_av_settled_at = Some(now);
        Some(now.saturating_duration_since(seek_started))
    }

    pub fn note_resize_recovery_started(&mut self, now: Instant) {
        self.resize_recovery_started_at = Some(now);
    }

    pub fn note_resize_recovered(&mut self, now: Instant) -> Option<std::time::Duration> {
        let started = self.resize_recovery_started_at.take()?;
        Some(now.saturating_duration_since(started))
    }

    pub fn note_device_recovery_started(&mut self, now: Instant) {
        self.device_recovery_started_at = Some(now);
    }

    pub fn note_device_recovered(&mut self, now: Instant) -> Option<std::time::Duration> {
        let started = self.device_recovery_started_at.take()?;
        Some(now.saturating_duration_since(started))
    }

    pub fn note_video_frame_presented(&mut self) {
        self.presented_video_frames = self.presented_video_frames.saturating_add(1);
    }

    pub fn note_video_frame_dropped(&mut self) {
        self.dropped_video_frames = self.dropped_video_frames.saturating_add(1);
    }

    pub fn note_audio_underrun(&mut self) {
        self.audio_underruns = self.audio_underruns.saturating_add(1);
    }

    pub fn note_ended(&mut self, now: Instant) {
        self.ended_at = Some(now);
    }

    pub fn presented_video_frames(&self) -> u64 {
        self.presented_video_frames
    }

    pub fn dropped_video_frames(&self) -> u64 {
        self.dropped_video_frames
    }

    pub fn audio_underruns(&self) -> u64 {
        self.audio_underruns
    }
}
