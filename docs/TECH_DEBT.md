# Technical Debt

Status: 2026-06-26 · FastPlay v0.4.0

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
- `render/` — presenter, swapchain, surface registry, timeline, HDR, overlay raster.
- `audio/` — WASAPI sink.
- `platform/` — Win32 window, input, file dialog.
- `ffi/` — the four unsafe seams: `ffmpeg`, `d3d11`, `dxgi`, `wasapi`.

The hot path and ownership model are unchanged from the charter:
`FFmpeg demux → D3D11 hw decode → opaque surface handle → DXGI flip present`,
with WASAPI shared-mode audio as the master clock, bounded queues, and
generation/op-id based stale-work rejection. `tick(now)` on the UI thread is the
only coordinator entrypoint.

### Largest files (lines)

Refreshed 2026-07-21 (see [`audits/codebase-review.md`](./audits/codebase-review.md) §4).

| File | Lines | Role |
|------|------:|------|
| `src/ffi/d3d11.rs` | 4322 | D3D11 FFI seam (inherently large; unsafe is correctly boxed here). The pure geometry/blend layer moved out to `render/overlay_raster.rs` in Stage 3; the GDI text path correctly stays |
| `src/app/session.rs` | 2892 | Coordinator; still large, but focused state/helpers have been extracted |
| `src/ffi/dxgi.rs` | 2096 | DXGI FFI seam. The input keymap moved out to `platform/input.rs` in Stage 2 |
| `src/ffi/ffmpeg.rs` | 2190 | FFmpeg FFI seam |
| `src/render/hdr.rs` | 1516 | Pure HDR classification and path decision — 736 lines of code, 780 of tests (41 tests). Not debt; the model to follow |

The `ffi/*` files are large because they are the designated unsafe seams; their
size is acceptable and expected. `app/session.rs` remains the largest safe-Rust
module because it owns orchestration, but v0.3.0 moved separable state and pure
logic into focused modules.

---

## 2. Known maintainability risks

### R1 — `PlaybackSession` width — **substantially paid down**

v0.3.0 extracted viewport, clip-range, overlay, audio coordination, video
queueing, input dispatch, and decode-thread lifecycle into named modules owned
by `PlaybackSession`. This reduced `session.rs` to 2323 lines and made the
separable logic independently testable.

The remaining width is concentrated in orchestration that the architecture
assigns to the concrete coordinator: open/close, seek/scrub transitions,
generation checks, event application, and present scheduling. This is a
maintainability watch item, not a standing mandate for another split. Further
extraction needs a concrete reviewability, correctness, or testability payoff.

### R2 — Coordinator-adjacent test coverage — **substantially improved**

The suite now contains 140 unit tests. In addition to the earlier subtitle,
timeline, queue, and drop-stat coverage, v0.3.0 added direct tests for viewport
geometry, clip-range behavior, overlay decisions, audio coordination, video
queue behavior, input dispatch, decode-thread command handling, recent-file
persistence policy, and resume timeline normalization; v0.4.0 adds play-queue
construction/navigation, the shared media-extension helper, drop classification,
the edge-triggered ended-signal latch, and auto-advance planning.

FFI-coupled end-to-end coordinator paths still rely on build checks, the local
benchmark harness, and manual playback validation. That residual gap is
expected until stable integration-test seams can be added without weakening the
ownership model.

### R3 — Benchmark harness — **resolved** (`bench/`)

`ARCHITECTURE.md §24` specifies percentile metrics (open-to-first-frame,
seek-to-first-frame, pause/resume, drops, underruns, hw-fallback). A repeatable
harness now produces p50/p95 from these by driving the release build and parsing
`session.log` — see [`bench/README.md`](../bench/README.md) and `ROADMAP.md §2`.
Remaining: it is local/optional and not yet wired into CI, so regressions are
observable on demand but not automatically gated. Promote to CI once it is
proven stable across machines.

### R4 — Historical / loose working docs — **resolved**

Point-in-time audit and planning notes have been moved out of the repo root and
archived under [`docs/audits/`](./audits/). They are now explicitly marked as
historical context, not current guidance.

---

## 3. CI / lint baseline (as of this pass)

- `cargo fmt --check` — **clean and now enforced in CI** (was previously
  disabled). The reformat was mechanical and limited to `src/ffi/dxgi.rs` and
  `src/ffi/ffmpeg.rs`.
- `cargo clippy --all-targets -- -D warnings` — **clean**, but see R5 below: the
  run is clean *against a crate-wide allow-list*, which is a weaker signal than
  this section previously claimed.
- `cargo test --all-targets` — **230 passing, 0 failing** (2026-07-21).

CI runs all three on `windows-latest`.

### R5 — Lint allow-list — **paid down** (2026-07-21)

This section previously stated there was "no baseline allow-list" and no
`#![allow(...)]` debt. That was wrong, and the codebase review corrected it:

- ~~`src/main.rs` disables **12 clippy categories crate-wide**.~~ **DONE — now
  one.** Each was measured individually (remove it, count what clippy then
  reports, and where):
  - `explicit_auto_deref` was hiding **nothing at all** — deleted.
  - `upper_case_acronyms`, `useless_transmute` and `type_complexity` fire
    *only* on the bindgen output, never on hand-written code. Scoped to
    `ffi/ffmpeg.rs`, which is where that output is `include!`d.
  - `manual_c_str_literals`, `field_reassign_with_default`, `cmp_null`,
    `manual_dangling_ptr`, `unnecessary_cast` are Win32/COM idioms confined to
    specific seams. Scoped to `ffi/d3d11.rs`, `ffi/dxgi.rs`, `ffi/runtime.rs`
    and `platform/open_dialog.rs`.
  - `manual_is_multiple_of` and `unnecessary_map_or` fired only in **safe
    application code** — three sites in `app/viewport.rs`, `app/session.rs` and
    `app/video_queue.rs`. Those were fixed rather than allowed.
  - `too_many_arguments` stays crate-wide: it is genuinely spread across
    `app/`, `render/` and `ffi/`, and bundling 8-12 parameter GPU/present calls
    into structs purely to satisfy it would obscure more than it clarified.

  Verified by injecting an `unnecessary_map_or` into `app/drop_stats.rs`: it now
  fails `-D warnings`, where the blanket allow used to absorb it silently.
- ~~**Seven modules** disable `dead_code` file-wide.~~ **DONE.** All seven
  blanket allows are gone. Removing them surfaced exactly five items, which is
  the point — a module-wide allow cannot distinguish reserved API from rot:
  - deleted as genuinely dead: `SessionCommand::Tick` (constructed nowhere, only
    a no-op match arm);
  - kept with a per-item allow and a stated reason: `SessionEvent::AudioEndpointChanged`
    (see R7), `media_ext::is_subtitle`, `PlayQueue::{is_empty, items, cursor}`,
    `RecentFiles::{is_empty, clear}`.
  - `playback/generations.rs` and `playback/queues.rs` had nothing to hide at
    all; their allows were pure noise.

  Two module comments were also stale, claiming the play queue was "not yet
  wired into the open flow" long after `main.rs` started driving it.

Net effect: **12 crate-wide allows became 1**, plus 11 module-scoped ones and
three real fixes. A new violation of any of those categories outside the seam
that needs it now fails CI. See
[`audits/codebase-review.md`](./audits/codebase-review.md) §10 Stage 5.

### R7 — Audio endpoint-change detection — **resolved** (2026-07-22)

Surfaced by R5's pay-down: the charter specified a
`SessionEvent::AudioEndpointChanged` that **nothing constructed**. Investigating
it showed the gap was real but the specified shape was wrong, so both were
fixed.

There are two cases, and only one of them produces an error to react to:

- **The device in use goes away** (unplugged, disabled). WASAPI fails with
  `AUDCLNT_E_DEVICE_INVALIDATED`, `submit_due_audio` sees it and calls
  `recover_audio_endpoint`. This already worked, within one tick.
- **The default moves to another device** (headphones plugged in, output changed
  in the volume flyout, Bluetooth connected). An `IAudioClient` is bound to one
  `IMMDevice` for life, so every call keeps succeeding and audio keeps playing
  out of the *old* endpoint. No error, ever. No reactive scheme can see this,
  and FastPlay kept rendering to the old device indefinitely.

Case 2 is now handled by an `IMMNotificationClient` registered at session
construction. It is **not** a `SessionEvent`: that enum carries worker output and
is generation-stamped, and an endpoint change is neither — stamping it would have
made a change arriving mid-seek get rejected as stale and lost. It is polled at
`tick` instead, the same shape as a window resize request. `ARCHITECTURE.md` §7,
§6 and §19 were amended to describe this.

---

## 4. Recommended next refactors (summary)

In priority order; details and sequencing in `ROADMAP.md`:

The original v0.3.0 priorities are complete:

1. ✅ Split separable `PlaybackSession` responsibilities into owned helper
   units without changing coordinator ownership.
2. ✅ Add a local benchmark harness that emits p50/p95 for charter metrics.
3. ✅ Backfill unit tests for the extracted helpers.

Next maintenance work should be driven by observed defects, difficult review
areas, or benchmark regressions rather than a line-count target. The 2026-07-21
[codebase review](./audits/codebase-review.md) §10 sequences the current
candidates, in value order:

1. Extract the input keymap out of `window_proc` into `platform/input.rs` as a
   pure function. It is the single largest untested surface in the program and
   it has already shipped one user-visible bug (a held Ctrl+S toggled subtitles,
   because a guarded match arm fell through to an unguarded one).
2. Extract the pure geometry/blend layer of the overlay rasterizer out of
   `ffi/d3d11.rs` into `render/`, with unit tests. The GDI text path stays in
   the seam.
3. De-duplicate the worker plumbing (`worker_send`, the device-lost event
   mapping) — but only as private helpers, not a worker trait; there are two
   consumers and they differ.
4. R5 above (lint allow-list).
5. Extend the worker-liveness discipline to the audio handle (see R6).

### R6 — Worker liveness — **paid down** (2026-07-21)

A `DecodeControl` is an `Arc` that deliberately outlives its worker thread, so
holding one is *not* evidence that a worker is alive. This has now produced two
defects: audio (fixed in `b603f6f`) and video (fixed 2026-07-21 — a seek arriving
during a decode-worker reopen cancelled the open, the worker exited, and the
coordinator kept sending seeks to a channel nobody was reading, so video never
returned for that file).

Both handles now go through `DecodeThreadHandle::seek_delivery`, which returns a
three-way `SeekDelivery` rather than a boolean. The third state is load-bearing:
a first attempt gated only on liveness (`worker_count() > 0`) fixed the wedge but
introduced a performance regression, because "no worker is running" has two
causes that need opposite responses.

- `InPlace` — a live worker with the right preference; send it a seek command.
- `Respawn` — the worker died on an error or cancelled open and the file still
  has a stream of that kind. Not respawning is the original bug.
- `Retired` — the worker exited because the file has no stream of its kind at
  all (`NoVideoStream` / `NoAudioStream`). Respawning here reopens and
  re-demuxes the file on *every* seek to rediscover the same absence. Measured
  on an audio-only `.m4a` with 8 seeks: 9 video-worker spawns before, 1 after.

The workers set the retirement flag immediately before their permanent-exit
returns; `prepare_spawn` clears it, so the verdict never outlives its open.

---

## 5. Explicit non-goals

These stay out of scope (consistent with `ARCHITECTURE.md §2` and `AGENTS.md`):

- Making `PlaybackSession` a trait, or introducing a second coordinator.
- Workspace / multi-crate split.
- Streaming, playlists / media library, plugins, web UI.
- Metadata-driven (dynamic) tone mapping, frame interpolation, extra hw
  decode backends. (HDR output itself shipped: PQ/HLG present on a 10-bit
  PQ swapchain when the display is HDR-active — `HdrPqOutput` — and
  tone-map to SDR in the same pixel shader otherwise.)
- Cross-platform abstractions.
- Any change to steady-state playback behavior in the name of "cleanup".

---

## 6. HDR follow-ups (updated after the HDR-output branch; not scheduled)

Superseded items from `docs/audits/2026-07-14-hdr-final-audit.md`: the
passthrough arm now plays (HdrPqOutput), `display_hdr_active` is real,
open-time path/caps diagnostics are logged, and the tone-map-on-HDR-desktop
question is moot (HDR desktops get PQ output). Still open:

1. ~~HDR10 static metadata~~ — DONE: `SetHDRMetaData` is wired (first-frame
   side data → `HdrMetadataKnown` event → `build_dxgi_hdr10_metadata`,
   unit-tested against the MSDN worked example → HDR chain), verified
   end-to-end by `bench/verify-hdr-metadata.ps1` with real x265 SEI.
   Advisory only: absence or failure never gates playback.
   `refine_color_from_first_frame` (frame-tag classification refinement)
   was deleted; reintroduce from scratch if real-world files need it.
2. A scripted entry point for mixed HDR/SDR queue validation (queues are only
   reachable via OLE drag-drop today; the CLI seeds a single-file queue).
   The SDR→HDR chain swap is exercised end-to-end by
   `bench/verify-hdr-passthrough.ps1` (app starts on the SDR chain); the
   HDR→SDR direction is the same kind-driven code but has no automated
   in-process driver.
3. HLG OOTF is fixed at the 1000-nit nominal peak; the display's real peak
   (`HdrDisplayDescriptor.max_luminance`, ~418 nits on the dev panel) could
   drive the system gamma for closer-to-reference HLG.
4. PQ-space straight-alpha blending for overlays on the HDR chain
   (anti-aliased edges blend slightly dark; solid text exact).
