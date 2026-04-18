#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDropCause {
    QueueOverflow,
    SurfaceMismatch,
    SchedulerLate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoDropBuckets {
    pub queue_overflow: u64,
    pub surface_mismatch: u64,
    pub scheduler_late: u64,
}

impl VideoDropBuckets {
    pub fn note(&mut self, cause: VideoDropCause) {
        match cause {
            VideoDropCause::QueueOverflow => {
                self.queue_overflow = self.queue_overflow.saturating_add(1);
            }
            VideoDropCause::SurfaceMismatch => {
                self.surface_mismatch = self.surface_mismatch.saturating_add(1);
            }
            VideoDropCause::SchedulerLate => {
                self.scheduler_late = self.scheduler_late.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_increments_matching_bucket_only() {
        let mut buckets = VideoDropBuckets::default();
        buckets.note(VideoDropCause::QueueOverflow);
        buckets.note(VideoDropCause::QueueOverflow);
        buckets.note(VideoDropCause::SurfaceMismatch);

        assert_eq!(buckets.queue_overflow, 2);
        assert_eq!(buckets.surface_mismatch, 1);
        assert_eq!(buckets.scheduler_late, 0);
    }

    #[test]
    fn note_saturates_at_u64_max() {
        let mut buckets = VideoDropBuckets {
            scheduler_late: u64::MAX,
            ..Default::default()
        };
        buckets.note(VideoDropCause::SchedulerLate);
        assert_eq!(buckets.scheduler_late, u64::MAX);
    }

    #[test]
    fn default_is_zeroed() {
        let buckets = VideoDropBuckets::default();
        assert_eq!(buckets.queue_overflow, 0);
        assert_eq!(buckets.surface_mismatch, 0);
        assert_eq!(buckets.scheduler_late, 0);
    }
}
