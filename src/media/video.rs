use std::time::Duration;

use crate::{
    playback::generations::{OpenGeneration, SeekGeneration},
    render::surface_registry::VideoSurfaceHandle,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDecodePreference {
    Auto,
    ForceSoftware,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDecodeMode {
    HardwareD3D11,
    Software,
}

impl VideoDecodeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::HardwareD3D11 => "HW:D3D11",
            Self::Software => "SW",
        }
    }
}

#[derive(Clone, Debug)]
pub enum DecodedVideoFrame {
    D3D11 {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        pts: Duration,
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
