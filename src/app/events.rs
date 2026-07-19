#![allow(dead_code)]

use crate::playback::generations::{OpenGeneration, OperationId, SeekGeneration};
use crate::{
    ffi::ffmpeg::{PendingAudioFrame, PendingVideoFrame},
    media::video::VideoDecodeMode,
    render::hdr::VideoPresentationPath,
};

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
    /// The presentation path chosen for this open, emitted unconditionally
    /// (including `ExistingSdr` and audio-only opens) before any frame
    /// event of the same generation — the shared FIFO channel guarantees
    /// the coordinator sees it first and can (re)build the matching
    /// swapchain kind ahead of the first frame.
    PresentationPathSelected {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        path: VideoPresentationPath,
    },
    VideoFrameReady(PendingVideoFrame),
    AudioFrameReady(PendingAudioFrame),
    VideoStreamEnded {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
    /// The opened file has no video stream (audio-only media). The video
    /// worker emits this instead of `OpenFailed` so the coordinator can play
    /// the file audio-only: no video frame will ever arrive, so it drives the
    /// clock and end-of-stream from audio alone.
    NoVideoStream {
        open_gen: OpenGeneration,
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
