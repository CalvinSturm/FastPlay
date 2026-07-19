# FastPlay v0.4.4

FastPlay `0.4.4` fixes two playback issues: full-range PQ video (notably
Topaz Video AI "HDR Enhanced" exports) now plays, and high-frame-rate
content (120 fps) now presents every frame.

## Fixes

### Full-range PQ video failed to open

Topaz Video AI's "HDR Enhanced" mode exports genuinely PQ-encoded video in
8-bit full-range H.264 — a combination FastPlay's HDR path declined at open
as unsupported. It now plays like any other HDR content: natively on an
HDR-active display, tone-mapped on SDR. All safety checks are unchanged
(non-standard matrices still decline; SDR content never enters the HDR
path), and the shader output is pixel-verified exact against spec math.

### One frame in six dropped at 120 fps

The clock that video frames schedule against advanced in ~10 ms steps
(Windows only updates the audio position once per engine period). At 120 fps
that made two frames come due at once on most steps, and the scheduler
dropped one — a structural ~17% loss that never showed at 60 fps and below.
The clock now advances smoothly between updates. Measured on a 240 Hz
display: a synthetic 120 fps clip went from 405 dropped frames to 6 of
2400; real 120 fps files from ~83% of frames presented to ~99%. A/V sync
is unaffected (the smoothing is bounded to 12 ms and re-converges every
update). Presenting 120 fps still requires a display faster than 120 Hz.

## Validation

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`
  (199 passing, including new clock-smoothing and signal-resolution tests),
  `cargo build --release`, `cargo wix`
- Full-range PQ shader output: 0/1023 vs full-range BT.2020 spec math
  (`bench/verify-colors-pq.ps1 -Mode shader-pq -FullRange`); all other
  pixel benches green; scrub → pause → resume audio behavior re-verified.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
