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

impl DecodedVideoFrame {
    pub fn open_gen(&self) -> OpenGeneration {
        match self {
            Self::D3D11 { open_gen, .. } => *open_gen,
        }
    }

    pub fn seek_gen(&self) -> SeekGeneration {
        match self {
            Self::D3D11 { seek_gen, .. } => *seek_gen,
        }
    }

    pub fn op_id(&self) -> OperationId {
        match self {
            Self::D3D11 { op_id, .. } => *op_id,
        }
    }

    pub fn pts(&self) -> Duration {
        match self {
            Self::D3D11 { pts, .. } => *pts,
        }
    }

    pub fn surface(&self) -> VideoSurfaceHandle {
        match self {
            Self::D3D11 { surface, .. } => *surface,
        }
    }
}
