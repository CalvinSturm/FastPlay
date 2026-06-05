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
    /// Number of slots currently holding a surface. Tracked incrementally so
    /// the registry can self-compact (advance the epoch) in O(1) once it
    /// empties, instead of letting `entries` grow unbounded across long
    /// playback / scrubbing.
    alive: usize,
}

impl SurfaceRegistry {
    pub fn clear_for_new_epoch(&mut self) {
        self.epoch_base = self.next_handle;
        self.entries.clear();
        self.alive = 0;
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
        self.alive += 1;
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
        let taken = self.entries.get_mut(index)?.take();
        if taken.is_some() {
            self.alive -= 1;
            // No live surfaces remain — and therefore no outstanding handle
            // (queued frame or current_surface) references this epoch. Advance
            // the epoch so the backing Vec is reclaimed. `next_handle` keeps
            // climbing monotonically, so any stale handle is still safely
            // rejected by `index_of`.
            if self.alive == 0 {
                self.clear_for_new_epoch();
            }
        }
        taken
    }

    /// Count how many slots currently hold a surface (diagnostic only).
    pub fn count_alive(&self) -> usize {
        self.alive
    }
}
