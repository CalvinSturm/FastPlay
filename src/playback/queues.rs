#![allow(dead_code)]

/// Latency-oriented queue defaults from the architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueDefaults {
    pub video_packets: usize,
    pub audio_packets: usize,
    pub decoded_video_frames: usize,
    pub decoded_audio_frames: usize,
}

impl Default for QueueDefaults {
    fn default() -> Self {
        Self {
            video_packets: 48,
            audio_packets: 96,
            decoded_video_frames: 4,
            decoded_audio_frames: 12,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueueDefaults;

    #[test]
    fn defaults_match_latency_policy() {
        let defaults = QueueDefaults::default();
        assert_eq!(defaults.video_packets, 48);
        assert_eq!(defaults.audio_packets, 96);
        assert_eq!(defaults.decoded_video_frames, 4);
        assert_eq!(defaults.decoded_audio_frames, 12);
    }
}
