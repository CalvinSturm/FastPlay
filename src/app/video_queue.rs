//! The bounded, PTS-ordered queue of decoded video frames awaiting present.
//!
//! This is a conservative, behavior-preserving extraction from
//! `PlaybackSession`. The queue *data structure* — ordered insertion, capacity
//! bookkeeping, and front/peek access — lives here and is unit-testable. The
//! scheduling *policy* (when a frame is early/late/droppable, clock selection,
//! out-point handling) stays in `PlaybackSession`, which owns the presenter and
//! releases the surface of every frame this queue hands back (on pop, overflow
//! eviction, or clear). This type never touches D3D11/the presenter.

use std::collections::VecDeque;

use crate::media::video::DecodedVideoFrame;

/// Bounded queue of decoded video frames, ordered by PTS.
pub struct VideoFrameQueue {
    frames: VecDeque<DecodedVideoFrame>,
    capacity: usize,
}

impl VideoFrameQueue {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether the queue is at (or above) capacity. Used to backpressure the
    /// decode worker so it is paced to playback.
    pub fn is_full(&self) -> bool {
        self.frames.len() >= self.capacity
    }

    /// The next frame to present, if any.
    pub fn front(&self) -> Option<&DecodedVideoFrame> {
        self.frames.front()
    }

    /// The frame after the front, if any. Used to decide whether the front can
    /// be dropped to catch up to the master clock.
    pub fn peek_second(&self) -> Option<&DecodedVideoFrame> {
        self.frames.get(1)
    }

    /// Remove and return the oldest frame. The caller releases its surface.
    pub fn pop_front(&mut self) -> Option<DecodedVideoFrame> {
        self.frames.pop_front()
    }

    /// Iterate frames front-to-back (oldest PTS first).
    pub fn iter(&self) -> impl Iterator<Item = &DecodedVideoFrame> {
        self.frames.iter()
    }

    /// Insert `frame` keeping the queue ordered by PTS. Frames almost always
    /// arrive in order, so the common case is a cheap append; out-of-order
    /// arrivals are placed by binary search.
    pub fn insert_ordered(&mut self, frame: DecodedVideoFrame) {
        let insert_at = if self
            .frames
            .back()
            .map_or(true, |last| frame.pts() >= last.pts())
        {
            self.frames.len()
        } else {
            self.frames
                .binary_search_by(|queued| queued.pts().cmp(&frame.pts()))
                .unwrap_or_else(|pos| pos)
        };
        self.frames.insert(insert_at, frame);
    }

    /// If the queue is over capacity, pop and return the oldest frame so the
    /// caller can release its surface. Returns `None` once back within
    /// capacity. Call in a loop after [`insert_ordered`].
    pub fn evict_overflow(&mut self) -> Option<DecodedVideoFrame> {
        if self.frames.len() > self.capacity {
            self.frames.pop_front()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::generations::{OpenGeneration, SeekGeneration};
    use crate::render::surface_registry::VideoSurfaceHandle;
    use std::time::Duration;

    fn frame(pts_ms: u64, surface: u64) -> DecodedVideoFrame {
        DecodedVideoFrame::D3D11 {
            open_gen: OpenGeneration(0),
            seek_gen: SeekGeneration(0),
            pts: Duration::from_millis(pts_ms),
            surface: VideoSurfaceHandle(surface),
        }
    }

    fn pts_order(q: &VideoFrameQueue) -> Vec<u64> {
        q.iter().map(|f| f.pts().as_millis() as u64).collect()
    }

    #[test]
    fn new_is_empty() {
        let q = VideoFrameQueue::with_capacity(4);
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.front().is_none());
        assert!(q.peek_second().is_none());
    }

    #[test]
    fn in_order_inserts_append() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(0, 1));
        q.insert_ordered(frame(33, 2));
        q.insert_ordered(frame(66, 3));
        assert_eq!(pts_order(&q), vec![0, 33, 66]);
    }

    #[test]
    fn out_of_order_insert_is_placed_by_pts() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(0, 1));
        q.insert_ordered(frame(66, 2));
        q.insert_ordered(frame(33, 3)); // arrives late, belongs in the middle
        assert_eq!(pts_order(&q), vec![0, 33, 66]);
    }

    #[test]
    fn insert_at_front_when_earliest() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(50, 1));
        q.insert_ordered(frame(10, 2));
        assert_eq!(pts_order(&q), vec![10, 50]);
        assert_eq!(q.front().unwrap().surface(), VideoSurfaceHandle(2));
    }

    #[test]
    fn front_and_peek_second_track_order() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(0, 1));
        q.insert_ordered(frame(33, 2));
        assert_eq!(q.front().unwrap().pts(), Duration::from_millis(0));
        assert_eq!(q.peek_second().unwrap().pts(), Duration::from_millis(33));
    }

    #[test]
    fn pop_front_returns_oldest() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(0, 1));
        q.insert_ordered(frame(33, 2));
        let popped = q.pop_front().unwrap();
        assert_eq!(popped.surface(), VideoSurfaceHandle(1));
        assert_eq!(pts_order(&q), vec![33]);
    }

    #[test]
    fn is_full_at_capacity() {
        let mut q = VideoFrameQueue::with_capacity(2);
        assert!(!q.is_full());
        q.insert_ordered(frame(0, 1));
        assert!(!q.is_full());
        q.insert_ordered(frame(10, 2));
        assert!(q.is_full());
    }

    #[test]
    fn evict_overflow_drops_oldest_until_within_capacity() {
        let mut q = VideoFrameQueue::with_capacity(2);
        q.insert_ordered(frame(0, 1));
        q.insert_ordered(frame(10, 2));
        q.insert_ordered(frame(20, 3)); // now 3 > capacity 2
        let evicted = q.evict_overflow();
        assert_eq!(evicted.unwrap().surface(), VideoSurfaceHandle(1));
        assert!(q.evict_overflow().is_none(), "back within capacity");
        assert_eq!(pts_order(&q), vec![10, 20]);
    }

    #[test]
    fn evict_overflow_is_none_within_capacity() {
        let mut q = VideoFrameQueue::with_capacity(4);
        q.insert_ordered(frame(0, 1));
        assert!(q.evict_overflow().is_none());
    }

    #[test]
    fn equal_pts_insert_does_not_panic_and_keeps_all() {
        let mut q = VideoFrameQueue::with_capacity(8);
        q.insert_ordered(frame(10, 1));
        q.insert_ordered(frame(10, 2));
        assert_eq!(q.len(), 2);
        assert_eq!(pts_order(&q), vec![10, 10]);
    }
}
