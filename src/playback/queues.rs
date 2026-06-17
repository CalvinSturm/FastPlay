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
            // The decoded-video queue doubles as the decoder's run-ahead
            // buffer. Because the UI backpressures the worker once this queue
            // fills (so the decoder stays paced to playback instead of racing
            // to EOF), the depth must comfortably exceed the codec's B-frame
            // reorder window — H.264 High / HEVC DPBs hold up to 16 frames —
            // plus a presentation cushion to absorb decode hiccups. The old
            // depth of 4 was below the reorder window, so the queue drained dry
            // between reorder bursts and playback juddered constantly. Each
            // queued frame is its own copied texture (see
            // `surface_from_raw_texture`), so this does not pin the hardware
            // decoder's reference pool; the only cost is GPU memory
            // (~3 MB/frame at 1080p NV12).
            decoded_video_frames: 32,
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
        assert_eq!(defaults.decoded_video_frames, 32);
        assert_eq!(defaults.decoded_audio_frames, 12);
    }
}
