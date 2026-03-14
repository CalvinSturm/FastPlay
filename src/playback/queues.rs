#![allow(dead_code)]

/// M0 queue defaults from the architecture. Real queue implementations arrive
/// with media pipelines in later milestones.
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
