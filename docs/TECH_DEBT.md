# Technical Debt

Status: 2026-06-25 · FastPlay v0.2.2

This document tracks **maintainability** debt only. It does not propose
architecture changes. `ARCHITECTURE.md` remains the locked charter; everything
here is expected to preserve its invariants — in particular, `PlaybackSession`
stays the single, concrete coordinator.

For the sequenced plan of what to pay down (and in what order), see
[`ROADMAP.md`](./ROADMAP.md).

---

## 1. Current architecture summary

Single Rust crate, Windows-only, organized into the modules described in
`ARCHITECTURE.md §5`:

- `app/` — coordinator (`session.rs`), state, events, commands, overlays, timeline UI.
- `playback/` — clock, metrics, queues, generations, decode control.
- `media/` — source, video, audio, seek, subtitle.
- `render/` — presenter, swapchain, surface registry, timeline.
- `audio/` — WASAPI sink.
- `platform/` — Win32 window, input, file dialog.
- `ffi/` — the four unsafe seams: `ffmpeg`, `d3d11`, `dxgi`, `wasapi`.

The hot path and ownership model are unchanged from the charter:
`FFmpeg demux → D3D11 hw decode → opaque surface handle → DXGI flip present`,
with WASAPI shared-mode audio as the master clock, bounded queues, and
generation/op-id based stale-work rejection. `tick(now)` on the UI thread is the
only coordinator entrypoint.

### Largest files (lines)

| File | Lines | Role |
|------|------:|------|
| `src/ffi/d3d11.rs` | 2714 | D3D11 FFI seam (inherently large; unsafe is correctly boxed here) |
| `src/app/session.rs` | 2583 | **Coordinator — primary maintainability concern** |
| `src/ffi/dxgi.rs` | 1812 | DXGI FFI seam |
| `src/ffi/ffmpeg.rs` | 1422 | FFmpeg FFI seam |

The `ffi/*` files are large because they are the designated unsafe seams; their
size is acceptable and expected. The maintainability risk is concentrated in
`app/session.rs`.

---

## 2. Known maintainability risks

### R1 — `PlaybackSession` is a very wide coordinator (highest priority)

`session.rs` is ~2.6k lines: one struct with ~70 fields and ~60 methods. Its
responsibilities are already *visible* as method clusters but are not separated
into named units, so cross-cutting state (clocks, generations, view transform,
clip range, audio anchors) is all flat on the struct. Observed clusters:

- **Lifecycle / open-close**: `open`, `fail_open`, `shutdown`,
  `prepare_runtime_for_operation*`, `begin_operation`, `spawn_decode_thread`,
  `teardown_decode_thread`, `decode_thread_serves_current`.
- **Seek / scrub**: `scrub_seek`, `seek`, `execute_seek`, `seek_is_settled`,
  `pending_seek_*`, `toggle_pending_seek_pause`.
- **Clip range**: `in_point`, `out_point`, `loop_range`,
  `position_is_in_active_range`, `range_resume_target`, `desired_restart_position`.
- **Viewport / zoom / pan / rotation**: `zoom_at_cursor`, `clamp_pan`,
  `rotate_view`, `reset_view`, `fit_window`, `half_size_window`, the `view_*`
  and `*_rotation_quarter_turns` fields.
- **Audio**: `submit_due_audio`, `push_audio_frame`, `adjust_volume_steps`,
  `recover_audio_endpoint`, the audio anchor/clock fields.
- **Video scheduler / frame selection**: `advance_video_playback`,
  `present_video_frame`, `drop_video_frame`, `push_video_frame`,
  `clear_video_queue`, `step_frame`, `is_current_frame`.
- **Overlay**: `set_timeline_overlay`, `refresh_volume_overlay`,
  `update_subtitle_overlay`, `show_error_idle_overlay`, `update_window_title`.
- **Input dispatch**: `apply_command`, `handle_event`.

Risk: any change touches a 2.6k-line file; state invariants between clusters are
implicit; the file is hard to review and to test in isolation.

This is the **single most valuable thing to pay down**, and it can be done
without violating the charter (see `ROADMAP.md §1`).

### R2 — Coordinator logic is under-tested relative to its size

Current unit tests (27 passing) cover focused, pure helpers: drop-stat buckets,
subtitle parsing, timeline geometry/format, queue defaults, and resume-target
math. The large stateful coordinator paths (seek settle, frame selection,
audio submission, recovery) have little direct coverage because they are
entangled with I/O and FFI. Extracting the clusters in R1 into helpers with
plain inputs/outputs is the enabler for testing them.

### R3 — Benchmark harness — **resolved** (`bench/`)

`ARCHITECTURE.md §24` specifies percentile metrics (open-to-first-frame,
seek-to-first-frame, pause/resume, drops, underruns, hw-fallback). A repeatable
harness now produces p50/p95 from these by driving the release build and parsing
`session.log` — see [`bench/README.md`](../bench/README.md) and `ROADMAP.md §2`.
Remaining: it is local/optional and not yet wired into CI, so regressions are
observable on demand but not automatically gated. Promote to CI once it is
proven stable across machines.

### R4 — Historical / loose working docs in the repo root

Root-level files like `fastplay_audit_6.16.md`, `fastplay_phase 3.md`, and
`implementation_plan.md` are point-in-time notes, not current guidance. They
risk being mistaken for live docs. Low priority: consider moving to `docs/` or
an archive folder. Not addressed in this pass to keep it small.

---

## 3. CI / lint baseline (as of this pass)

- `cargo fmt --check` — **clean and now enforced in CI** (was previously
  disabled). The reformat was mechanical and limited to `src/ffi/dxgi.rs` and
  `src/ffi/ffmpeg.rs`.
- `cargo clippy --all-targets -- -D warnings` — **clean**. There is **no
  baseline allow-list**; no `#![allow(...)]` debt is being hidden. Nothing to
  pay down here today.
- `cargo test --all-targets` — **27 passing, 0 failing**.

CI runs all three on `windows-latest`. There is no known lint debt to document
as deferred.

---

## 4. Recommended next refactors (summary)

In priority order; details and sequencing in `ROADMAP.md`:

1. Split `PlaybackSession`'s responsibilities into named helper units **without
   removing it as the single coordinator** — it keeps owning state transitions,
   stale-work rejection, and `tick(now)`; helpers are concrete sub-structs /
   free functions it calls.
2. Add a benchmark harness that emits p50/p95 for the charter metrics.
3. Backfill unit tests on the extracted helpers.

---

## 5. Explicit non-goals

These stay out of scope (consistent with `ARCHITECTURE.md §2` and `AGENTS.md`):

- Making `PlaybackSession` a trait, or introducing a second coordinator.
- Workspace / multi-crate split.
- Streaming, playlists / media library, plugins, web UI.
- HDR tone mapping, frame interpolation, extra hw decode backends.
- Cross-platform abstractions.
- Any change to steady-state playback behavior in the name of "cleanup".
