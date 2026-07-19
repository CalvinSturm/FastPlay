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
| `src/ffi/d3d11.rs` | 3036 | D3D11 FFI seam (inherently large; unsafe is correctly boxed here) |
| `src/app/session.rs` | 2413 | Coordinator; still large, but focused state/helpers have been extracted |
| `src/ffi/dxgi.rs` | 1842 | DXGI FFI seam |
| `src/ffi/ffmpeg.rs` | 1563 | FFmpeg FFI seam |

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
- `cargo clippy --all-targets -- -D warnings` — **clean**. There is **no
  baseline allow-list**; no `#![allow(...)]` debt is being hidden. Nothing to
  pay down here today.
- `cargo test --all-targets` — **140 passing, 0 failing**.

CI runs all three on `windows-latest`. There is no known lint debt to document
as deferred.

---

## 4. Recommended next refactors (summary)

In priority order; details and sequencing in `ROADMAP.md`:

The original v0.3.0 priorities are complete:

1. ✅ Split separable `PlaybackSession` responsibilities into owned helper
   units without changing coordinator ownership.
2. ✅ Add a local benchmark harness that emits p50/p95 for charter metrics.
3. ✅ Backfill unit tests for the extracted helpers.

Next maintenance work should be driven by observed defects, difficult review
areas, or benchmark regressions rather than a line-count target.

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

1. HDR10 static metadata (`SetHDRMetaData`): bind
   `AVMasteringDisplayMetadata`/`AVContentLightMetadata` payload structs
   (shim header lacks `libavutil/mastering_display_metadata.h`), implement
   `build_dxgi_hdr10_metadata` with units resolved from CTA-861.3, wire
   after HDR chain creation. Missing metadata must never fail playback.
   Until then `extract_hdr_metadata_from_frame` /
   `refine_color_from_first_frame` stay dead code (the former errors on
   metadata presence if wired as-is).
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
