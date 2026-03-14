use std::time::Duration;

use crate::playback::generations::{OpenGeneration, OperationId, SeekGeneration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioStreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bytes_per_sample: u16,
    pub channel_mask: u64,
}

impl AudioStreamFormat {
    pub const fn stereo_f32_48khz() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            bytes_per_sample: 4,
            channel_mask: 0x3,
        }
    }

    pub const fn bytes_per_frame(self) -> u16 {
        self.channels.saturating_mul(self.bytes_per_sample)
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct DecodedAudioFrame {
    pub open_gen: OpenGeneration,
    pub seek_gen: SeekGeneration,
    pub op_id: OperationId,
    pub pts: Duration,
    pub format: AudioStreamFormat,
    pub frame_count: u32,
    pub data: Vec<u8>,
}

impl DecodedAudioFrame {
    pub fn pts(&self) -> Duration {
        self.pts
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.format.bytes_per_frame() as usize
    }
}
