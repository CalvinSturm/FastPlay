# FastPlay v0.4.3

FastPlay `0.4.3` is an HDR release. On a display with Windows HDR enabled,
HDR10 (PQ) and HLG video — including iPhone recordings — now present
**natively in HDR** instead of being tone-mapped down to SDR. On SDR
displays the v0.4.2 tone-map path is unchanged.

## Highlights

- **Native HDR output.** HDR10 PQ and HLG present on a 10-bit PQ swapchain
  when the display is HDR-active; SDR displays keep the tone-map path.
- **HDR10 static metadata** (mastering display, MaxCLL/MaxFALL) is read from
  the stream and handed to the display pipeline.
- **Overlays and screenshots are HDR-aware**: subtitles/OSD render at the
  correct brightness on the HDR chain, and screenshots of HDR playback come
  out as normal SDR images.
- Fixed audio occasionally staying silent after scrubbing plus pause/play.
- Fixed the app icon losing its transparency.

## Notes

- The HDR-or-SDR decision is made per file open, for the monitor the window
  is actually on. Moving a playing HDR window onto a non-HDR monitor
  mid-playback looks washed out until the file is reopened.
- The conversion is FastPlay's own pixel shader on both paths (the GPU video
  processor offers no usable HDR conversions on real hardware): PQ passes
  through bit-transparently; HLG is completed to display light and
  PQ-encoded. The tone map stays fixed (203 cd/m² diffuse white, knee 0.75)
  and is not metadata-driven.
- The audio fix: scrub seeks pause the audio sink without clearing it, and
  resuming afterwards could leave it permanently wedged if its buffer was
  full. The resume path now resets the sink, which also stops a brief burst
  of old-position audio.
- The SDR path is byte-identical to v0.4.2 (±2/255 vs an ffmpeg reference).

## Validation

- `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`
  (198 passing), `cargo build --release`, `cargo wix`
- Shader output pixel-verified against CPU models / spec math: PQ
  passthrough 0/1023, HLG→PQ ≤1/1023, overlays 0/1023, end-to-end playback
  ≤1/255, static metadata round-trip exact — each with negative controls;
  full bench suite in `bench/`.
- Real 4K60 HEVC HLG plays sustained on hardware decode through the HDR
  path; multi-monitor behavior validated manually.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
