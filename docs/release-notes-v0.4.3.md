# FastPlay v0.4.3

FastPlay `0.4.3` is an HDR release. On a display with Windows HDR enabled,
HDR10 (PQ) and HLG video — including iPhone recordings — now present
**natively in HDR** instead of being tone-mapped down to SDR: full display
brightness, BT.2020 color, and the source's own PQ code values delivered
bit-transparently. On SDR displays the v0.4.2 tone-map path is unchanged.
No charter changes: `PlaybackSession` remains the single, concrete
coordinator.

## Highlights

- **Native HDR output.** HDR10 PQ and HLG present on a 10-bit PQ swapchain
  when the display is HDR-active; SDR displays keep the tone-map path.
- **HDR10 static metadata** (mastering display, MaxCLL/MaxFALL) is read from
  the stream and handed to the display pipeline.
- **Overlays and screenshots are HDR-aware**: subtitles/OSD render correctly
  on the HDR chain, and screenshots of HDR playback come out as normal SDR
  images.
- Fixed the app icon losing its transparency.

## Features

### Native HDR output (HDR10 PQ and HLG)

At file-open, FastPlay asks the display the window is actually on — per
monitor — whether Windows HDR is active. If it is, the swapchain is rebuilt
as 10-bit `R10G10B10A2` committed to the HDR10 color space
(`RGB_FULL_G2084_NONE_P2020`), and the same pixel shader that previously
tone-mapped to SDR now writes PQ instead:

- **PQ input passes through bit-transparently** — the BT.2020 YCbCr → R'G'B'
  matrix is the entire conversion; no EOTF round trip, no tone curve.
  Measured 0/1023 against spec math on every SMPTE bar.
- **HLG is completed to display light** (BT.2100 OOTF, system gamma 1.2 at
  the 1000-nit nominal peak) and PQ-encoded. Measured within 1/1023 of a
  double-precision model.

The conversion is deliberately not the GPU video processor, for the reason
established in v0.4.2: on real hardware (NVIDIA RTX 3080 Ti) the video
processor advertises no HLG conversions at all. One shader now serves both
outputs, so geometry, zoom, and rotation are common and pixel-verifiable.

If any requirement is missing — SDR display, HDR toggled off, the swapchain
or device lacking support — content routes to the existing tone-map path.
HDR content never falls through to the plain SDR path, and a surface that
disagrees with the live swapchain is a typed error, never wrong colors.

The swapchain kind changes only between files, never mid-playback; window
resizes and device-loss recovery rebuild the same kind (re-committing the
HDR color space after every resize).

### HDR10 static metadata

Mastering-display and content-light SEI (the HDR10 grading envelope) is read
from the first decoded frame and applied to the swapchain via
`SetHDRMetaData`, with the DXGI unit conversions pinned by unit tests
against Microsoft's own worked example. Metadata is advisory: files without
it, or with malformed values, play exactly as before.

### HDR-aware overlays and screenshots

- Subtitles, the timeline, volume, and help overlays render through a
  PQ-aware shader variant on the HDR chain (sRGB → BT.2020 → 203 cd/m²
  reference white → PQ), so text reads at the correct brightness instead of
  a few nits. The SDR chain binds the exact original shader, byte-identical.
- `Ctrl+S` screenshots of HDR playback tone-map the 10-bit backbuffer to SDR
  on the CPU (the same audited curve the SDR path uses), so saved BMPs look
  right in ordinary viewers.

## Fixes

### App icon transparency

The window/taskbar icon had lost its alpha channel and rendered on a solid
background; the icon's transparency is restored.

## Technical notes

- **The HDR decision is per-open, per-monitor.** Moving a playing HDR window
  onto a monitor without HDR mid-playback leaves the PQ output being mapped
  by Windows, which looks bright and washed out; reopening the file (or
  switching files) re-decides for the new monitor. On mixed setups, the
  window's monitor at open time wins.
- Overlay alpha blending on the HDR chain happens in PQ space: solid text
  and boxes are exact; anti-aliased edges blend very slightly dark.
- The HLG OOTF uses the 1000-nit nominal peak (not the panel's measured
  peak), and the Windows SDR-brightness slider does not affect HDR output —
  both by design for this release.
- The SDR path is byte-identical to v0.4.2 and still measures within ±2/255
  of an ffmpeg reference decode.

## Validation

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`
  (198 passing)
- `cargo build --release`, `cargo wix`
- Shader PQ passthrough vs BT.2020 spec math: **0/1023** on all bars, with a
  wrong-transfer negative control diverging by 228
  (`bench/verify-colors-pq.ps1 -Mode shader-pq`).
- HLG→PQ vs a double-precision CPU model: max **1/1023**, negative control
  228 (`bench/verify-hlg-pq.ps1`).
- Overlay shader on the PQ chain vs CPU model: **0/1023**, opaque and
  alpha-blended regions (`bench/verify-overlay-hdr.ps1`).
- End-to-end through the real player (`bench/verify-hdr-passthrough.ps1`):
  PQ 0/255, dimmed-PQ 1/255, HLG 1/255 against the composed model, with a
  session-log oracle proving the HDR path was taken and the chain swapped,
  and a mid-playback resize leaving all bars byte-stable.
- Static metadata end-to-end with real x265 SEI carrying Microsoft's worked
  example values: extracted, converted, and applied verbatim
  (`bench/verify-hdr-metadata.ps1`).
- Display detection tracks the Windows HDR toggle
  (`bench/verify-hdr-caps.ps1`).
- Real content: 4K60 HEVC Main10 HLG (iPhone) plays sustained on hardware
  decode through the HDR path with zero drops.
- Multi-monitor validated manually: HDR on the HDR monitor, tone-map on the
  SDR monitor, decided by the window's monitor at open.
- SDR regression: backbuffer within ±2/255 of the ffmpeg reference
  (`bench/verify-colors.ps1`), all overlay/subtitle benches passing.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
