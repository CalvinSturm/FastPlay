# FastPlay v0.5.0

FastPlay `0.5.0` adds a compact native menu and fast marked-range MP4 export,
while preserving the single-player architecture and GPU-resident playback path.

## Highlights

### Native menu without permanent chrome

- Media, Playback, View, and Help commands are available from a compact native
  menu row.
- Press `Ctrl+Shift+M` to show or hide the menu. The choice is remembered for
  new instances, and `Alt` temporarily reveals a hidden menu.
- Switching menu visibility preserves the client size and screen position.

### Export a marked clip

- Set In and Out points, then press `Ctrl+Shift+E` or choose
  **Media → Export Marked Range**.
- MP4-family inputs use stream copy when the In point is keyframe-safe;
  otherwise FastPlay transcodes to H.264/AAC for accurate boundaries.
- Export runs asynchronously, refuses overwrites, cleans temporary files after
  failure, and reports completion in the player.
- The MSI and portable ZIP include `ffmpeg.exe` and `ffprobe.exe`, so export
  works without a separate FFmpeg installation.

### Additional polish

- Screenshots are saved as PNG.
- Frameless hit testing and middle-click input behavior are corrected.
- The local benchmark harness can run reproducible open/seek baselines over
  multiple corpus directories.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No settings migration is required.
- Packaged FFmpeg tools increase the installer and portable archive sizes.

## Validation

- `cargo fmt --check`
- `cargo check --all-targets`
- `cargo test --all-targets` (281 unit tests and 4 integration tests)
- `cargo clippy --all-targets -- -D warnings`
- `cargo build --release`
- `cargo wix`
- portable ZIP contents and MSI administrative extraction verified, including
  `ffmpeg.exe` and `ffprobe.exe`
- packaged FFmpeg tools produced and reprobed a 2.500-second H.264/AAC clip
- portable software-fallback and MSI-extracted hardware-path launches reached
  responsive playback windows with the expected media title
