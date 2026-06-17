# FastPlay v0.2.1

FastPlay `0.2.1` is a playback hotfix over `0.2.0`. It restores smooth video playback after a regression that left the picture frozen on the first few frames while audio kept playing, and removes the chronic stutter that the same area introduced.

## Highlights

- fixed video freezing on the first few frames shortly after opening a file — the picture is no longer stuck while audio plays on
- fixed constant stutter/judder during otherwise normal playback
- seeking and scrubbing stay responsive; audio no longer underruns during playback

## Fixes and Improvements

### Playback

- **Frozen video:** the decoder had lost its backpressure and raced to the end of the file the instant a clip opened, discarding nearly every frame before it could be shown (only a handful of frames ever reached the screen) while audio continued normally. The decode pipeline is now correctly paced to playback, so frames are presented in order instead of being dropped en masse. Audio runs on its own path and is unaffected by video buffering.
- **Constant stutter:** with pacing restored, the decoded-frame buffer was too shallow to cover the reordering window of typical H.264/HEVC video, so it kept draining dry between frames and playback juddered. The buffer has been deepened well past the codec reorder window plus a presentation cushion, giving smooth, continuous playback. The added buffering is plain GPU memory and does not affect the hardware decoder or seek/scrub latency.

## Upgrade Notes

- existing MSI installs upgrade in place through the WiX `MajorUpgrade` path
- no settings or configuration changes are required
