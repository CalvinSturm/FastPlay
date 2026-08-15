# FastPlay v0.4.6

FastPlay `0.4.6` adds a frameless windowed mode that stays out of the way and
remembers how you left it. It also makes the portable Windows build reproducible
from the repository.

## Highlights

### Toggleable frameless windowed mode

- Press `Ctrl+Shift+S` to switch between framed and frameless windowed modes.
  The existing `Ctrl+S` screenshot shortcut is unchanged.
- Move a frameless instance by dragging anywhere in the visible client area
  outside the timeline. `Ctrl+Drag` still pans zoomed video.
- Resize from every edge and corner with DPI-aware hit targets.
- Fullscreen, maximize, restore, timeline interaction, and the normal shutdown
  path continue to work in either mode. No window animations were added.

### Remembered across new instances

- FastPlay saves the selected window style in
  `%APPDATA%\FastPlay\settings.txt` and applies it when a new process starts.
- A new process is created directly in the preferred style, avoiding a framed
  flash before switching to frameless mode.
- Existing settings files containing only `volume=` remain compatible. Toggling
  back to framed mode updates the same preference immediately.

### Portable packaging in the repository

- The PowerShell packager creates a versioned Windows x64 ZIP from the release
  build and keeps its runtime DLL set aligned with the WiX installer manifest.
- The archive includes portable usage guidance and third-party notices. FastPlay
  itself continues to store settings, recent files, resume positions, and logs
  under `%APPDATA%\FastPlay`.

### Updated Windows icon

- The executable, installer, and Start menu now use the new FastPlay badge, with
  dedicated icon entries from 16 through 256 pixels.
- Explorer video thumbnails use a separate mark-only FastPlay overlay so the
  file association stays recognizable without covering the thumbnail in the
  full app badge.

## Performance

The frameless implementation does not change the steady-state playback path.
Local A/B launch measurements found no regression:

- frameless window implementation, 15 samples per revision: open-to-first-frame
  p50 `56 ms` before and after; p95 `74 ms` before and `65 ms` after
- persisted startup preference, 30 alternating samples: launch-to-window p50
  `28.3 ms` before and `25.9 ms` after; p95 `36.4 ms` before and `36.0 ms`
  after

These measurements describe one development machine and are not universal
latency claims.

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets` (266 passing)
- `cargo build --release`
- `cargo wix`
- portable ZIP structure plus extracted hardware-D3D11 and software-fallback
  playback checks
- MSI administrative extraction and extracted-payload hardware playback check
- installed MSI in-place upgrade from `0.4.5` to `0.4.6`
- multi-process framed → frameless → framed persistence validation with isolated
  temporary settings

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network connection, or settings migration is required.
- Use `Ctrl+Shift+S` at any time to return new instances to framed mode.
