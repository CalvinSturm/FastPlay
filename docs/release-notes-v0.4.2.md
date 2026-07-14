# FastPlay v0.4.2

FastPlay `0.4.2` is a media-compatibility and correctness release. HDR video
(HDR10 PQ and HLG — including iPhone recordings) now plays, audio-only files
play, and three long-standing defects are fixed: audio going silent after a
scrub, a stretched picture when zooming a rotated video, and a hard crash when
switching files rapidly through a play queue. No charter changes:
`PlaybackSession` remains the single, concrete coordinator.

## Highlights

- **HDR video plays.** HDR10 PQ and HLG are tone-mapped to SDR in a pixel shader.
- **Audio-only files play** (`.mp3`, `.flac`, `.wav`, `.ogg`, `.aac`, `.m4a`, `.opus`).
- Fixed audio going permanently silent after scrubbing early in a file.
- Fixed the picture stretching when zooming a rotated video.
- Fixed an access-violation crash when spamming PageUp/PageDown through a queue.

## Features

### HDR playback (HDR10 PQ and HLG)

HDR-tagged video previously failed to open. It is now tone-mapped to SDR in
FastPlay's own pixel shader: the decoded NV12/P010 frame is sampled through
per-plane shader-resource views, then converted BT.2020 NCL YCbCr → R'G'B' →
PQ or HLG EOTF (plus the HLG OOTF) → normalized to diffuse white (203 cd/m²,
per BT.2408) → a knee/shoulder tone curve that rolls off highlights → BT.2020 →
BT.709 → sRGB, into the existing SDR swapchain.

This deliberately does *not* use the GPU video processor's HDR→SDR conversion,
which was the previous design. That approach is unreachable on real hardware:
on an NVIDIA RTX 3080 Ti, the video processor advertises **no** HLG (`GHLG`)
input conversion at all — no format, no output space — and accepts PQ only to
linear-scRGB or HDR10 outputs, never to the gamma-2.2 sRGB an 8-bit SDR
backbuffer scans out. Doing the transfer math ourselves is both portable and
exact.

## Fixes

### Audio silent after an early scrub

The audio worker decodes independently of video, and its cancellation predicate
watches the command sequence — which *any* seek bumps. A seek arriving while the
worker was still opening the file cancelled that open, and the worker exited
permanently. Nothing brought it back: the coordinator gates its respawn on the
existence of the worker's control channel, which is an `Arc` that outlives the
thread. Video played on; audio was gone for the rest of the file. Intermittent,
because it only fires when the seek lands inside the audio-open window — which is
why it favoured large files.

The audio open now distinguishes "this file has no audio stream" (permanent —
exit) from "a seek superseded the open" (transient — serve the seek and reopen).

### Stretched picture when zooming a rotated video

Zoom/pan clips the destination rect and maps the visible region back to a source
rect to sample. That mapping was done on the source's own axes, while the base
rect was already built from *rotated* display dimensions. At 90°/270° the
destination's horizontal axis is the source's **vertical** one, so the crop came
off the wrong axis with the wrong shape and the rotate-then-fit smeared it. At
180° the axes line up, but the crop was taken from the opposite side, so a
*panned* zoom framed the wrong region.

### Crash when switching files rapidly (access violation)

Spamming PageUp/PageDown through a play queue killed the process with an access
violation, usually after 30–50 presses.

FFmpeg's D3D11VA hardware-device context (`AVD3D11VADeviceContext`) **takes
ownership** of the `ID3D11Device` it is given: it releases the device when the
decoder is torn down, and does not AddRef it on init. FastPlay was handing it a
borrowed pointer, so every decoder teardown released the shared device against an
AddRef that never happened. A single open survives that — the device carries
dozens of other references — but queue navigation is a full open+teardown per
press, so each one nets one release. The refcount walks to zero, the device is
freed out from under the swapchain, the decode workers and the video processor,
and the next call faults inside `d3d11.dll`.

Present since before this release line; not introduced by the HDR work.

## Technical notes

- HDR is **tone-mapped to SDR, never passed through**. On an HDR display it is
  still shown as SDR: native HDR passthrough is not implemented, so HDR content
  will not use the display's full brightness or gamut.
- Tone mapping is fixed (no exposure or curve controls) and ignores
  mastering-display / content-light metadata, so grading choices in the source
  are not honoured exactly.
- HDR is declined cleanly at open on a device that cannot sample the decoder's
  output format, rather than erroring at the first draw (which device recovery
  misreads as device-lost and retries forever).
- The SDR path is unchanged: it still blits through the D3D11 video processor and
  measures within ±2/255 of an ffmpeg reference decode.

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets`
- `cargo test` (187 passing)
- `cargo build --release`, `cargo wix`
- `bench\verify-colors.ps1` — SDR backbuffer within ±2/255 of an ffmpeg reference.
- `bench\verify-subtitles-hdr.ps1` (new) — subtitle overlays composite correctly
  on top of HDR video, with the picture beneath them untouched.
- HDR tone-map verified **by pixel** against a CPU model of the shader, on
  synthetic HLG and PQ clips with known values: exact match, 0/255 delta on every
  channel, including seven unclipped PQ midtones.
- Rotated zoom verified by rendering a clip containing a perfect circle: a 90°
  display matrix rendered it at 720×280 (aspect 2.571) before, 448×450 (aspect
  0.996) after; unrotated and 180° clips measure identical before and after.
- Queue-switch crash: previously crashed after 32–48 presses on mixed, SDR-only
  and HDR-only queues; now survives 150 presses at 35 ms and 120 at 55 ms on all
  three.
- Media-shape regressions: A/V, audio-only `.mp3`, video-only `.mp4`, and 4K60
  HLG all play and seek.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
