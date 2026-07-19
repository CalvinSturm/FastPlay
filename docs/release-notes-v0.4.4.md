# FastPlay v0.4.4

FastPlay `0.4.4` is a playback-correctness release with two fixes:
full-range PQ video (notably Topaz Video AI "HDR Enhanced" exports) now
plays, and high-frame-rate content (120 fps) now presents every frame
instead of silently dropping one in six. No charter changes:
`PlaybackSession` remains the single, concrete coordinator.

## Highlights

- **Full-range PQ files play.** 8-bit full-range H.264 with genuine
  PQ/BT.2020 signalling — the shape Topaz Video AI "HDR Enhanced" exports —
  previously failed to open with an HDR-combination error.
- **120 fps content presents every frame.** A structural ~17% frame drop at
  high frame rates is fixed; on a high-refresh display, 120 fps files now
  play at full cadence.

## Fixes

### Full-range PQ video failed to open

FastPlay's HDR path carried its validated colorimetry as a DXGI color-space
value, and DXGI's enum simply has no full-range-PQ variant — so full-range
PQ was declined at open as an unsupported HDR combination. But the
combination exists in the wild: Topaz Video AI's "HDR Enhanced" mode writes
genuinely PQ-encoded pixels into 8-bit full-range H.264.

Surfaces now carry the validated signal itself (transfer + range), which
the HDR shader consumes directly; its range normalization is
transfer-agnostic and already served full-range HLG (iPhone recordings).
All the real safety checks are unchanged: constant-luminance and
non-BT.2020 matrices still decline with a typed error, and SDR content can
never reach the HDR shader. Validated by pixel: full-range PQ bars through
the production shader measure an exact 0/1023 against full-range BT.2020
spec math on every bar.

### One frame in six dropped at 120 fps

The audio master clock — which video frames schedule against — derived its
position from WASAPI's buffered-frames counter, which Windows only updates
once per audio engine period (~10 ms). The clock was a 10 ms staircase:
at 120 fps (8.33 ms frames), nearly every step made two frames due at
once, and the scheduler's catch-up logic dropped the older one. The
analytic drop rate (1 − 8.33/10 ≈ 16.7%) matched the measured 17% exactly.
At 60 fps and below a step never crosses two frame boundaries, which is
why the loss never showed on ordinary content.

The clock now advances smoothly between counter updates by extrapolating
with wall time (audio hardware consumes samples in real time), clamped to
12 ms so a stalled audio device can never run the clock ahead, and
guaranteed monotonic. Measured on a 240 Hz display: a synthetic 120 fps
clip went from 405 dropped frames of 2400 to 6; real 120 fps files went
from ~83% of frames presented to ~99%.

## Technical notes

- Full-range PQ plays natively (PQ output) on an HDR-active display and
  tone-maps to SDR otherwise, exactly like studio-range HDR10.
- Presenting 120 fps at full cadence still requires a display refresh rate
  above 120 Hz; on a 60 Hz display roughly every other frame is skipped by
  design (as in any player).
- A/V sync is unaffected by the clock smoothing: the extrapolation is
  bounded to 12 ms of lead over the raw hardware position and re-converges
  on every counter update.

## Validation

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`
  (199 passing; new tests for signal resolution across both transfers ×
  matrix × all three range tags, and for the clock smoothing: staircase
  fill, stall clamp, monotonicity, seek reset)
- `cargo build --release`, `cargo wix`
- Full-range PQ shader output vs full-range BT.2020 spec math: **0/1023**
  on all bars (`bench/verify-colors-pq.ps1 -Mode shader-pq -FullRange`);
  studio-range, HLG, tone-map, overlay, and SDR benches all green.
- 120 fps: synthetic clip drops 405 → 6 of 2400; two real-world 120 fps
  full-range-PQ files present 937 and 941 of 948 frames (~119 fps
  effective) on hardware decode with zero audio underruns.
- Scrub → pause → resume audio behavior re-verified on the smoothed clock.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
