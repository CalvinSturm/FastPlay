use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

use crate::playback::generations::{OperationId, SeekGeneration};

/// A command sent from the UI thread to the persistent decode thread.
pub enum DecodeCommand {
    /// Seek to `target` within the already-open file and resume decoding,
    /// stamping produced frames with `seek_gen` / `op_id`.
    Seek {
        target: Duration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    },
    /// Stop decoding and exit the thread.
    Shutdown,
}

/// Shared control channel between the UI thread and one persistent decode
/// thread.
///
/// The decode thread "serves" a monotonically increasing command sequence. The
/// UI bumps that sequence on every command; the decode thread's cancellation
/// predicate compares the sequence it is serving against the latest, so an
/// in-flight decode aborts as soon as a newer command arrives. Only the latest
/// pending command is retained (`Seek` coalescing), so rapid scrubbing collapses
/// to the most recent target instead of decoding every intermediate position.
pub struct DecodeControl {
    /// Latest command sequence. Bumped under `pending`'s lock on every command.
    seq: AtomicU64,
    shutdown: AtomicBool,
    /// The single pending command (latest wins). `None` once the decode thread
    /// has taken it.
    pending: Mutex<Option<DecodeCommand>>,
    /// Signals the decode thread when it is idle (blocked in `wait_next`).
    cv: Condvar,
}

impl DecodeControl {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            pending: Mutex::new(None),
            cv: Condvar::new(),
        }
    }

    /// The latest command sequence. Cheap; used by the decode thread's
    /// per-frame cancellation check.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, Option<DecodeCommand>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// UI thread: queue a seek, superseding any pending command, and wake the
    /// decode thread. Returns the new command sequence.
    pub fn send_seek(
        &self,
        target: Duration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    ) -> u64 {
        let mut pending = self.lock_pending();
        *pending = Some(DecodeCommand::Seek {
            target,
            seek_gen,
            op_id,
        });
        // Bump the sequence under the lock so `wait_next` always reads a
        // sequence consistent with the command it takes.
        let seq = self.seq.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        drop(pending);
        self.cv.notify_all();
        seq
    }

    /// UI thread: signal the decode thread to stop and exit. Idempotent.
    pub fn send_shutdown(&self) {
        let mut pending = self.lock_pending();
        *pending = Some(DecodeCommand::Shutdown);
        self.shutdown.store(true, Ordering::Release);
        self.seq.fetch_add(1, Ordering::AcqRel);
        drop(pending);
        self.cv.notify_all();
    }

    /// Decode thread: block until a command newer than what it is currently
    /// serving is available (or shutdown), then return that command together
    /// with the sequence number it should now serve.
    pub fn wait_next(&self) -> (DecodeCommand, u64) {
        let mut pending = self.lock_pending();
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return (DecodeCommand::Shutdown, self.seq.load(Ordering::Acquire));
            }
            if let Some(command) = pending.take() {
                return (command, self.seq.load(Ordering::Acquire));
            }
            pending = self.cv.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }
}
