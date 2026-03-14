#![allow(dead_code)]

use std::collections::HashMap;

use crate::{
    ffi::d3d11::VideoSurface,
    playback::generations::{OpenGeneration, SeekGeneration},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoSurfaceHandle(pub(crate) u64);

#[derive(Debug)]
pub struct SurfaceEntry {
    pub registry_epoch: u64,
    pub open_gen: OpenGeneration,
    pub seek_gen: SeekGeneration,
    pub surface: VideoSurface,
}

#[derive(Debug, Default)]
pub struct SurfaceRegistry {
    entries: HashMap<VideoSurfaceHandle, SurfaceEntry>,
    next_handle: u64,
    epoch: u64,
}

impl SurfaceRegistry {
    pub fn clear_for_new_epoch(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.entries.clear();
    }

    pub fn insert(
        &mut self,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        surface: VideoSurface,
    ) -> VideoSurfaceHandle {
        self.next_handle = self.next_handle.saturating_add(1);
        let handle = VideoSurfaceHandle(self.next_handle);
        self.entries.insert(
            handle,
            SurfaceEntry {
                registry_epoch: self.epoch,
                open_gen,
                seek_gen,
                surface,
            },
        );
        handle
    }

    pub fn contains(&self, handle: VideoSurfaceHandle) -> bool {
        self.entries.contains_key(&handle)
    }

    pub fn get(&self, handle: VideoSurfaceHandle) -> Option<&SurfaceEntry> {
        self.entries.get(&handle)
    }
}
