#![allow(dead_code)]

use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OpenGeneration(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SeekGeneration(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OperationId(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Default)]
pub struct GenerationState {
    open: OpenGeneration,
    seek: SeekGeneration,
}

impl GenerationState {
    pub fn open(&self) -> OpenGeneration {
        self.open
    }

    pub fn seek(&self) -> SeekGeneration {
        self.seek
    }

    pub fn bump_open(&mut self) -> OpenGeneration {
        self.open.0 = self.open.0.saturating_add(1);
        self.seek = SeekGeneration(0);
        self.open
    }

    pub fn bump_seek(&mut self) -> SeekGeneration {
        self.seek.0 = self.seek.0.saturating_add(1);
        self.seek
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OperationClock {
    next: NonZeroU64,
}

impl Default for OperationClock {
    fn default() -> Self {
        Self {
            next: NonZeroU64::MIN,
        }
    }
}

impl OperationClock {
    pub fn next(&mut self) -> OperationId {
        let current = self.next;
        self.next = current.checked_add(1).unwrap_or(current);
        OperationId(current)
    }
}
