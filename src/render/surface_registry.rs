use crate::{
    ffi::d3d11::VideoSurface,
    playback::generations::{OpenGeneration, SeekGeneration},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoSurfaceHandle(pub(crate) u64);

#[derive(Debug)]
pub struct SurfaceEntry {
    pub open_gen: OpenGeneration,
    pub seek_gen: SeekGeneration,
    pub surface: VideoSurface,
}

/// A registry of video surfaces keyed by sequential handles.
///
/// Internally backed by a `Vec` rather than a `HashMap` — since handles are
/// sequential u64 values, the index into the vec is `handle - epoch_base - 1`,
/// giving O(1) access with no hashing.  On each epoch reset the vec is cleared
/// and `epoch_base` advances, so handles from the previous epoch produce
/// out-of-range indices and are safely ignored.
#[derive(Debug, Default)]
pub struct SurfaceRegistry {
    entries: Vec<Option<SurfaceEntry>>,
    next_handle: u64,
    /// Value of `next_handle` at the start of the current epoch.
    /// Handles issued in the current epoch satisfy `handle > epoch_base`.
    epoch_base: u64,
}

impl SurfaceRegistry {
    pub fn clear_for_new_epoch(&mut self) {
        self.epoch_base = self.next_handle;
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
        let index = (self.next_handle - self.epoch_base - 1) as usize;
        if index >= self.entries.len() {
            self.entries.resize_with(index + 1, || None);
        }
        self.entries[index] = Some(SurfaceEntry { open_gen, seek_gen, surface });
        handle
    }

    fn index_of(&self, handle: VideoSurfaceHandle) -> Option<usize> {
        // Returns None for handles from a previous epoch (handle.0 <= epoch_base).
        let index = handle.0.checked_sub(self.epoch_base + 1)?;
        Some(index as usize)
    }

    pub fn get(&self, handle: VideoSurfaceHandle) -> Option<&SurfaceEntry> {
        let index = self.index_of(handle)?;
        self.entries.get(index)?.as_ref()
    }

    pub fn remove(&mut self, handle: VideoSurfaceHandle) -> Option<SurfaceEntry> {
        let index = self.index_of(handle)?;
        self.entries.get_mut(index)?.take()
    }
}
