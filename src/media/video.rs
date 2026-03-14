#![allow(dead_code)]

use std::time::Duration;

use crate::{
    playback::generations::{OpenGeneration, OperationId, SeekGeneration},
    render::surface_registry::VideoSurfaceHandle,
};

#[derive(Clone, Debug)]
pub enum DecodedVideoFrame {
    D3D11 {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        pts: Duration,
        width: u32,
        height: u32,
        surface: VideoSurfaceHandle,
    },
}
