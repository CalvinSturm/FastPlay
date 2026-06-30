# Audit: Timeline Skip Crashes

> [!NOTE]
> Archived historical audit. This is not current implementation guidance.
> Use `ARCHITECTURE.md`, `docs/ROADMAP.md`, and `docs/TECH_DEBT.md` for live
> project direction.

## Problem Statement

Pressing arrow keys (or rapidly scrubbing the timeline) during playback crashes FastPlay. The crash is most likely an access violation (`0xc0000005`) or a Rust panic from an assertion/`unwrap`.

---

## Root Cause Analysis

After tracing the full seek path — `InputEvent::SeekRelativeSeconds` → `SessionCommand::Seek` → `seek()` → `execute_seek()` → `spawn_stream_worker()` — I identified **5 distinct crash vectors**, ordered by likelihood.

---

### Bug 1 (P0 — Most Likely): `SyncSender::send` blocks the worker, worker nonce check passes stale

**Location:** [session.rs:1003-1080](../../src/app/session.rs#L1003-L1080)

The event channel is a bounded `sync_channel` with capacity = `decoded_video_frames + decoded_audio_frames + 4` = `16 + 128 + 4 = 148`.

The problem is the interaction between:
1. **Worker backpressure**: `sender.send()` blocks when the channel is full
2. **Tick drain loop break condition** (line 513–517): the drain loop stops when `queued_video_frames.len() >= queued_video_capacity || queued_audio_frames.len() >= queued_audio_capacity`
3. **Rapid seeks**: each seek spawns a new worker. The old worker's `sender.send()` may **block indefinitely** because:
   - The channel is full of events from the old worker
   - The UI thread's drain loop breaks early due to full queues
   - `cancel_active_worker()` only bumps the nonce — it doesn't unblock the `SyncSender`

When the old worker is blocked in `SyncSender::send()`, **it holds its `WorkerGuard`** and `active_worker_count` stays ≥ 2. Combined with the `SEEK_WORKER_MIN_INTERVAL` and the `active_worker_count >= 2` gate at line 537-538, deferred seeks pile up but never execute because the stale worker never exits.

Eventually, one of these accumulations causes the system to deadlock or forces a seek to execute when GPU resources from the blocked worker have not been released — **triggering a D3D11 device-removed (TDR) crash** in `surface_from_raw_texture` or `VideoProcessorBlt`.

> [!CAUTION]
> The `SyncSender` blocking interaction with the tick drain loop is the most dangerous pattern. A single stale worker that can't drain creates cascading GPU resource exhaustion.

**Fix:**
- Change from `mpsc::sync_channel` to an unbounded `mpsc::channel`, OR
- After `cancel_active_worker()`, drain the event channel of all stale-generation events so the old worker's `send()` unblocks, OR
- Switch workers to `try_send()` so they drop frames instead of blocking (architecturally preferred — matches generation-based stale-work dropping)

---

### Bug 2 (P0): `surface_from_raw_texture` called on worker thread races with UI-thread `CopySubresourceRegion`

**Location:** [d3d11.rs:243-305](../../src/ffi/d3d11.rs#L243-L305) + [d3d11.rs:307-524](../../src/ffi/d3d11.rs#L307-L524)

`D3D11Device` is `Clone` — each worker gets a clone (line 1004: `let device = self.presenter.device().clone()`). Both the worker thread and the UI thread share the **same underlying `ID3D11DeviceContext`**. While `SetMultithreadProtected(true)` adds a critical section around context calls, this CritSec only serializes calls — it doesn't prevent the following race:

1. Worker thread calls `CopySubresourceRegion` (inside `surface_from_raw_texture`)
2. Concurrently, the UI thread calls `render_video_surface` → `VideoProcessorBlt` using the **same context**
3. The D3D11 runtime CritSec ensures these don't overlap, but under rapid seeking the workers pile up behind it, increasing latency to the point where the GPU driver times out (TDR)

With multiple workers alive (see Bug 1), **2+ workers plus the UI thread** contend on the same single immediate context. This is the classic D3D11 multithread contention crash.

> [!WARNING]
> `SetMultithreadProtected` only prevents corruption — it does NOT prevent TDR from context contention.

**Fix:**
- The architecture already has `active_worker_count >= 2` guard, but Bug 1 prevents stale workers from decrementing timely. Fix Bug 1 first.
- Consider using deferred contexts for worker surface copies (v1.1 optimization)

---

### Bug 3 (P1): `seek_discard_before_pts` not reset between seeks, causing missed frames

**Location:** [session.rs:896](../../src/app/session.rs#L896) + [session.rs:949](../../src/app/session.rs#L949)

In `execute_seek()`, `seek_discard_before_pts` is set to the absolute target (line 896). In `prepare_runtime_for_operation_inner()`, it's only cleared when `reset_audio_expectation` is true (line 949) — but `execute_seek()` passes `false` for `reset_audio_expectation` (via line 888).

This means if a seek target changes from 30s → 5s:
1. First seek sets `seek_discard_before_pts = absolute(30s)`
2. Second seek sets `seek_discard_before_pts = absolute(5s)` — ✅ correct, this overwrites

Actually, reviewing more carefully: `seek_discard_before_pts` IS overwritten each seek (line 896), so this is not directly a crash. However, `media_time_origin_pts` is NOT reset between seeks (it's only set once in `observe_media_time_origin`), which means the `absolute_media_position()` calculation at line 885 may produce incorrect absolute positions for containers where PTS doesn't start at 0.

**This is not a direct crash vector but contributes to seeks landing in wrong positions, which can produce unexpected decoder behavior.**

---

### Bug 4 (P1): No guard against seeking in `Idle`/`Opening`/`Error` states

**Location:** [session.rs:843-871](../../src/app/session.rs#L843-L871)

The `seek()` method only checks `self.current_source.is_none()` before proceeding (line 844). It does NOT check the playback state. If a user hits an arrow key while in `Opening` state (before `media_duration` is known), then:

1. `media_duration` is `None` → the clamping at line 849 is a no-op
2. `snapshot()` returns a PendingSeekTarget or Duration::ZERO
3. `seek()` proceeds, bumps `SeekGeneration`, and spawns a new worker
4. The original open worker's events now arrive with mismatched `seek_gen` and are silently discarded
5. The file never finishes opening → stuck in `Seeking` state with no recovery

While not strictly a crash, this leads to a hang that the user perceives as a crash.

**Fix:**
```rust
fn seek(&mut self, target: SeekTarget, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
    if self.current_source.is_none() {
        return Ok(());
    }
    // Don't seek during states where it makes no sense
    if !matches!(self.state, PlaybackState::Playing | PlaybackState::Paused 
        | PlaybackState::Priming | PlaybackState::Ended | PlaybackState::Draining
        | PlaybackState::Seeking)
    {
        return Ok(());
    }
    // ... rest
}
```

---

### Bug 5 (P2): `SyncSender` deadlock during `can_finish_playback` → `replay` → `seek`

**Location:** [session.rs:615-636](../../src/app/session.rs#L615-L636) + [session.rs:1518-1520](../../src/app/session.rs#L1518-L1520)

When `auto_replay` or `loop_range` is enabled and end-of-stream is reached (line 631), `tick_inner` calls `replay()` → `seek()` → `execute_seek()` → `spawn_stream_worker()`. The new worker starts sending events into the `SyncSender`. If the user was also simultaneously pressing arrow keys, there may be TWO seek operations spawned in the same tick iteration, because:

1. `can_finish_playback()` triggers `replay()` which calls `seek()` 
2. In the same tick, a keyboard seek was already queued and processed at line 347

This can race with Bug 1 to exhaust GPU decoder sessions.

---

## Summary Table

| # | Severity | Crash Type | Root Cause | File |
|---|----------|------------|------------|------|
| 1 | **P0** | Deadlock / TDR | `SyncSender::send` blocks stale workers; GPU resources not released | session.rs |
| 2 | **P0** | Access violation / TDR | Multiple workers contend on single `ID3D11DeviceContext` | d3d11.rs |
| 3 | **P1** | Wrong position | `media_time_origin_pts` not reset between seeks | session.rs |
| 4 | **P1** | Hang / stuck state | No state guard on `seek()` entry | session.rs |
| 5 | **P2** | Resource exhaustion | Auto-replay + keyboard seek = double-seek in one tick | session.rs |

---

## Proposed Fix Plan

### Phase 1: Fix the blocking channel (Bug 1) — **highest impact**

#### [MODIFY] [session.rs](../../src/app/session.rs)

1. Change `mpsc::sync_channel(event_capacity)` to `mpsc::channel()` (unbounded)
2. Remove the `queued_video_frames.len() >= capacity` break in the tick drain loop and replace it with a **count-limited drain** (`MAX_EVENTS_PER_TICK = 256`)
3. This prevents workers from blocking, ensures stale workers exit promptly when their nonce mismatches, and lets `WorkerGuard` decrement `active_worker_count` quickly

### Phase 2: State guard on seek (Bug 4)

#### [MODIFY] [session.rs](../../src/app/session.rs)

Add a state guard at the top of `seek()` to reject seeks during `Idle`, `Opening`, and `Error` states.

### Phase 3: Reset `media_time_origin_pts` on seek (Bug 3)

This is lower risk but fixes subtle position drift issues. In `execute_seek()`, clear `media_time_origin_pts` so the next worker re-establishes it from the first decoded frame's PTS.

---

## Verification Plan

### Automated Tests
- `cargo build` — must compile cleanly
- No new warnings

### Manual Verification
1. Open a video, press Left/Right arrow key rapidly 20+ times → should not crash or hang
2. Open a video, scrub the timeline back and forth rapidly → should not crash
3. Open a video with auto-replay enabled, seek to near end, let it replay → should not double-seek
4. Open a video, immediately press Left arrow before first frame → should be ignored gracefully

---

## Open Questions

> [!IMPORTANT]
> **Unbounded channel trade-off:** Switching from `sync_channel` to `channel` removes backpressure on the worker. Workers will produce frames as fast as they can decode. Since the tick loop already applies capacity limits (dropping excess video frames via `push_video_frame`'s overflow path), this should be safe, but memory usage may temporarily spike on fast-decode files. Do you want me to proceed with unbounded, or would you prefer a `try_send`-based approach where the worker drops frames it can't send?

