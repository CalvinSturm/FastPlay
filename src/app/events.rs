#![allow(dead_code)]

use crate::{
    ffi::ffmpeg::{PendingAudioFrame, PendingVideoFrame},
    media::video::VideoDecodeMode,
};
use crate::playback::generations::{OpenGeneration, OperationId, SeekGeneration};

/// All asynchronous completions flow through this enum so the coordinator stays
/// the only state owner.
#[derive(Debug)]
pub enum SessionEvent {
    DecodeModeSelected {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        mode: VideoDecodeMode,
        hw_fallback_count: u64,
        /// Clockwise quarter-turns from the stream's display matrix (0–3).
        rotation_quarter_turns: u8,
    },
    MediaDurationKnown {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        duration: std::time::Duration,
    },
    VideoFrameReady(PendingVideoFrame),
    AudioFrameReady(PendingAudioFrame),
    VideoStreamEnded {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
    AudioStreamEnded {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
    OpenFailed {
        open_gen: OpenGeneration,
        op_id: OperationId,
        error: String,
    },
    PlaybackFailed {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        error: String,
    },
    DeviceLost {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
    AudioEndpointChanged {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
}
