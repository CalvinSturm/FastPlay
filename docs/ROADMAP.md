# Roadmap

Status: 2026-06-25 · FastPlay v0.2.2

Practical, prioritized plan for maintaining FastPlay before new features land.
This complements (does not replace) `ARCHITECTURE.md`, which stays the locked
charter, and [`TECH_DEBT.md`](./TECH_DEBT.md), which catalogs current debt.

Guiding constraint for everything below: **`PlaybackSession` remains the single,
concrete coordinator.** Refactors extract helpers that it *owns and calls*; they
never become a second coordinator and never mutate session state from a worker.
All charter invariants (§29) hold.

Each item below is intended to be its own small, reviewable PR.

---

## 1. Refactor: split `PlaybackSession` responsibilities (do first)

Goal: reduce `session.rs` (~2.6k lines) from one flat struct into the coordinator
plus a handful of focused, individually testable helper units, while keeping the
public coordinator surface and `tick(now)` contract identical.

Approach: extract one cluster per PR. Prefer **concrete sub-structs that the
session owns** (e.g. `session.view`, `session.clip_range`) or **free functions
that take plain inputs and return plain outputs**. The session continues to own
the orchestration order in `tick(now)` (`ARCHITECTURE.md §13`).

Suggested order (lowest risk / highest clarity first):

1. **Viewport / zoom / pan / rotation controller** — `view_zoom`, `view_pan_*`,
   `*_rotation_quarter_turns` plus `zoom_at_cursor`, `clamp_pan`, `rotate_view`,
   `reset_view`, `fit_window`, `half_size_window`. Nearly pure geometry; easiest
   to extract and unit-test.
2. **Clip range controller** — `in_point`, `out_point`, `loop_range`,
   `position_is_in_active_range`, `range_resume_target`, `desired_restart_position`.
   Already has resume-target tests to pin behavior.
3. **Overlay controller** — timeline/volume/subtitle/error overlays and title.
4. **Audio controller** — due-audio submission, anchors, volume, endpoint
   recovery (keeps WASAPI ownership in `audio::sink` / `ffi::wasapi`).
5. **Video scheduler / frame selection** — `advance_video_playback`,
   `present_video_frame`, drop/clear/push, `step_frame`, `is_current_frame`.
6. **Seek / scrub controller** — seek issue, settle tracking, pending-pause.
7. **Lifecycle / open-close handling** — open, fail-open, runtime prep, decode
   thread spawn/teardown. Extract last; highest coupling to FFI and generations.
8. **Input dispatch** — `apply_command` / `handle_event` routing to the above.

Each PR: extract, keep behavior identical, run `fmt`/`clippy`/`test`, and add
unit tests for the newly isolated logic (this is the R2 payoff in
`TECH_DEBT.md`).

Done when: `session.rs` is materially smaller, each helper compiles and tests in
isolation, and the charter invariants are unchanged.

---

## 2. Benchmark harness (do after the first extractions)

Add a repeatable harness that turns the existing `MetricsCollector` data into
reportable percentiles, matching `ARCHITECTURE.md §24`. Keep it Windows-only and
out of the steady-state hot path.

Target measurements (p50 / p95 unless noted):

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

## 3. Product basics (after refactor + harness)

Small, charter-aligned quality-of-life features. Each must still pass the
implementation filter in `ARCHITECTURE.md §30` and not introduce a media
library or sprawl.

- **Recent files** — most-recently-opened list (no library, no indexing).
- **Resume playback position** — remember last position per file; resume on open.
- **Open next / previous file in folder** — sibling-file navigation only.
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
- HDR tone mapping, frame interpolation
- CUDA/NVDEC split path or additional hardware decode backends
- subtitle styling (ASS) engine
- cross-platform support
- making `PlaybackSession` a trait or adding a second coordinator
- large playback rewrites

---

## Sequencing at a glance

1. **Now:** rustfmt-clean + CI fmt enforcement + these docs (this pass).
2. **Next:** `PlaybackSession` helper extractions (§1), one cluster per PR, with
   tests.
3. **Then:** benchmark harness (§2).
4. **After:** product basics (§3).
5. **Later:** creator review mode (§4).
