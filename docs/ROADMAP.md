# Roadmap

Status: 2026-06-26 · FastPlay v0.4.0

Practical, prioritized plan for maintaining FastPlay before new features land.
This complements (does not replace) `ARCHITECTURE.md`, which stays the locked
charter, and [`TECH_DEBT.md`](./TECH_DEBT.md), which catalogs current debt.

Guiding constraint for everything below: **`PlaybackSession` remains the single,
concrete coordinator.** Refactors extract helpers that it *owns and calls*; they
never become a second coordinator and never mutate session state from a worker.
All charter invariants (§29) hold.

Each item below is intended to be its own small, reviewable PR.

---

## 1. Refactor: split `PlaybackSession` responsibilities — **implemented**

The v0.3.0 refactor reduced `session.rs` from one flat coordinator into
`PlaybackSession` plus focused, individually testable helpers while preserving
the public coordinator surface and `tick(now)` contract.

Completed extractions:

- `viewport.rs` — zoom, pan, rotation, and display geometry
- `clip_range.rs` — in/out points, looping, and restart decisions
- `overlay.rs` — timeline, volume, subtitle, help, and error overlays
- `audio_controller.rs` — audio batching, anchors, and submission state
- `video_queue.rs` — frame queueing and drop bookkeeping
- `input_dispatch.rs` — input-event to command routing
- `decode_thread.rs` — decode-thread handle lifecycle

Seek, open/close, state transitions, stale-work rejection, and presentation
scheduling remain in `PlaybackSession` because they are coordinator
responsibilities under the locked architecture. Further extraction should be
evidence-driven rather than pursued solely to reduce line count.

---

## 2. Benchmark harness — **implemented** (`bench/`)

A repeatable harness that turns the metrics the app already logs to
`session.log` into reportable percentiles, matching `ARCHITECTURE.md §24`. It is
Windows-only, drives the real release build via `PostMessageW`, and does not
touch the steady-state hot path or app code. See [`bench/README.md`](../bench/README.md).

- `bench/gen-corpus.ps1` — generates a synthetic ffmpeg corpus (no media in repo).
- `bench/run-bench.ps1` — drives open → seeks → pause/resume → playthrough →
  graceful close per clip/iteration, parses `session.log`, and emits a p50/p95
  console table plus `bench/results/*.json` and `*.csv`.

It is intentionally **not** wired into `cargo`/CI yet (local/optional first, per
the note below); promote it once it has proven stable across machines.

Target measurements (p50 / p95 unless noted) — all captured:

- open-to-first-frame
- seek-to-first-frame
- pause/resume latency
- frame drops over long playback (count / rate)
- audio underruns (count)
- hardware-decode fallback count

Notes:

- Drive it from a fixed local corpus (codec / resolution / container slices per
  §24); do not ship media in the repo.
- Emit machine-readable output (JSON/CSV) so results can be diffed across builds.
- Report p50/p95 only; **do not make universal latency promises** (§24).
- Treat as a local/optional tool first; wire into CI only once it is stable and
  fast enough not to dominate the job.

---

## 3. Product basics

Small, charter-aligned quality-of-life features. Each must still pass the
implementation filter in `ARCHITECTURE.md §30` and not introduce a media
library or sprawl.

- ✅ **Recent files** — implemented in v0.3.0 as a capped,
  most-recently-opened overlay with no indexing or library behavior.
- ✅ **Resume playback position** — implemented in v0.3.0 for CLI, file-dialog,
  drag/drop, and recent-file opens, with near-end resume suppression.
- ✅ **Open next / previous file in folder** — implemented in v0.4.0 as a
  lightweight, in-memory play queue owned by the event loop (not by
  `PlaybackSession`): build a queue by dropping multiple files or a folder,
  step with `PageUp` / `PageDown`, and auto-advance at the natural end of each
  file. No persistent playlists, recursive scanning, shuffle, or repeat.
- **Optional file associations** — already partially present via MSI; make it an
  explicit, optional toggle.
- **Portable ZIP artifact** — ship a no-installer build alongside the MSI.

---

## 4. Creator review mode (later)

Review-workflow features for the creator audience. Build on the clip-range and
overlay controllers from §1. None of these change the playback hot path.

- copy timestamp (current position to clipboard)
- mark in/out (extends existing in/out points)
- loop range (extends existing loop-range toggle)
- timestamp notes
- export marks as JSON / CSV
- screenshot current frame (already present; fold into review export)
- timeline hover thumbnails

---

## 5. Explicit non-goals

Unchanged from `ARCHITECTURE.md §2` and `AGENTS.md`. None of the following are on
this roadmap:

- streaming
- playlists / full media library
- browser / web UI
- plugin system
- metadata-driven tone mapping (HDR10 PQ/HLG present natively on HDR-active
  displays and tone-map to SDR otherwise, in FastPlay's own pixel shader;
  see `ARCHITECTURE.md` §21), frame interpolation
- CUDA/NVDEC split path or additional hardware decode backends
- subtitle styling (ASS) engine
- cross-platform support
- making `PlaybackSession` a trait or adding a second coordinator
- large playback rewrites

---

## Sequencing at a glance

1. ✅ rustfmt-clean + CI fmt enforcement + docs.
2. ✅ `PlaybackSession` helper extractions (§1).
3. ✅ benchmark harness (§2) — `bench/`.
4. ✅ first product basics: recent files and resume playback (§3).
5. ✅ lightweight play queue: next/previous + folder playback + auto-advance
   (§3), shipped in v0.4.0.
6. **Next:** choose among the remaining product basics (portable ZIP artifact,
   optional file-association toggle) based on measured user value and
   architecture fit.
7. **Later:** creator review mode (§4).
