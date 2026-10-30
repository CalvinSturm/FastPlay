# <img width="36" alt="FastPlay icon" src="assets/icon/fastplay-mark.png" /> FastPlay

**FastPlay is a fast, keyboard-first video player for Windows, built for opening local files quickly, scrubbing smoothly, and staying out of your way.**

No media library. No plugin maze. No feature sprawl. Just fast local playback, responsive controls, and a focused Windows-native experience.

**[Download the latest FastPlay release for Windows x64](../../releases/latest)** • [All releases](../../releases) • [Report an issue](../../issues)

**Current status:** FastPlay is stable for everyday local playback on Windows x64. Development is now focused on maintenance, bug fixes, and useful feature requests.

https://github.com/user-attachments/assets/460ffc36-5a44-4030-bfc2-c05cfc38a724

## Get started

1. Install FastPlay using the MSI from the latest release above, or download the portable ZIP.
2. Open a video directly, drag a file onto FastPlay, or set FastPlay as your default Windows media app.
3. Drop multiple files or a folder onto the window to create a temporary play queue.
4. Use the keyboard for fast seeking, playback control, zoom, rotation, screenshots, and more.

FastPlay is designed for local playback. No account, migration, or internet connection is required.

## Why FastPlay exists

Most of the time, opening a video player is simple: you have a local file and you want to watch it.

FastPlay is built around making that moment as frictionless as possible:

* open the file quickly
* show the first frame quickly
* scrub and seek without fighting the player
* remember where you stopped
* fit the video cleanly to the screen
* stay responsive
* get out of the way

Mainstream players such as VLC and MPC-HC are excellent and support far more features.

FastPlay deliberately does less.

It is a focused Windows player for people who want a snappy, no-friction window for the local file in front of them.

## Features

### Fast local playback

* quick open and first-frame path
* responsive keyboard seeking
* accelerated seek while holding the arrow keys
* timeline scrubbing
* automatic per-file resume positions
* replay at end of playback
* in/out points with range playback and looping
* lightweight multi-file and folder play queues
* automatic advance to the next queued file
* playback metrics including open-to-frame latency, seek latency, and dropped frames

### Video

* FFmpeg-based demux and decode
* D3D11 hardware video decode on the preferred path
* software decoding fallback with D3D11 upload and presentation
* high-frame-rate playback, including 120 fps at full cadence on high-refresh displays
* automatic rotation from stream display-matrix metadata
* manual 90-degree rotation
* portrait-aware window sizing for phone videos
* cursor-centered zoom
* drag-to-pan while zoomed
* fit-to-screen sizing without black padding
* half-native-resolution window sizing

### HDR

* native HDR output on HDR displays
* HDR10 PQ support
* HLG support
* iPhone HDR recording support
* 10-bit PQ swapchain output
* HDR10 static metadata passed to the display
* GPU tone mapping to SDR when the active display is not using HDR

### Audio

* WASAPI shared-mode audio output
* audio-master playback timing when audio exists
* audio-only file playback
* `.mp3`, `.flac`, `.wav`, `.m4a`, and `.opus` playback
* idle background while playing audio-only files

### Window behavior

* normal framed windowed mode
* remembered frameless windowed mode
* move and resize frameless windows normally
* maximize frameless windows
* borderless fullscreen
* native application menu with a remembered On/Off toggle
* per-monitor DPI awareness
* minimum window size enforcement
* device-loss and resize recovery

Press `Ctrl+Shift+S` to switch between framed and frameless windowed modes. FastPlay remembers your preference for future instances.

Press `Ctrl+Shift+M` or choose **View → Show Menu Bar** to toggle the native menu bar. The choice is remembered in `%APPDATA%\FastPlay\settings.txt` alongside frameless mode; when the menu is hidden, press `Alt` to reveal it temporarily.

### Subtitles

FastPlay currently supports external `.srt` sidecar subtitles.

Features include:

* runtime subtitle toggle
* comma or period millisecond separators
* UTF-8 with BOM
* Windows-1252 fallback
* stripping of common formatting tags such as `<b>`, `<i>`, `<u>`, and `<font>`

## Compared to VLC and MPC-HC

FastPlay is not trying to beat mature media players on feature breadth.

|                     | FastPlay                    | VLC                            | MPC-HC                          |
| ------------------- | --------------------------- | ------------------------------ | ------------------------------- |
| Primary focus       | Fast local Windows playback | Broad media playback           | Lightweight Windows playback    |
| Platforms           | Windows x64                 | Cross-platform                 | Windows                         |
| Streaming / network | No                          | Yes                            | Limited                         |
| Media library       | No                          | Yes                            | Basic                           |
| Per-file resume     | Built in                    | Configuration/plugin dependent | Limited                         |
| Render path         | D3D11-first                 | Multiple backends              | DirectShow / optional renderers |
| Philosophy          | Focused and minimal         | Broad and configurable         | Lightweight and configurable    |

Use VLC or MPC-HC when you need extensive format coverage, streaming, playlists, deep configuration, or broader platform support.

Use FastPlay when you want to double-click a local file and start watching it with as little friction as possible.

## Controls

| Key                   | Action                                       |
| --------------------- | -------------------------------------------- |
| `Space`               | Pause / resume / replay at end               |
| `Left`                | Seek backward 5s, hold for 15s steps         |
| `Right`               | Seek forward 5s, hold for 15s steps          |
| `Ctrl+F`              | Move one frame forward                       |
| `Ctrl+B`              | Move one frame backward                      |
| `Ctrl+O`              | Open media file                              |
| `Ctrl+Shift+O`        | Recent files overlay                         |
| `PageUp` / `PageDown` | Previous / next queued file                  |
| `Ctrl+S`              | Save screenshot                              |
| `Ctrl+Shift+S`        | Toggle framed / frameless windowed mode      |
| `Ctrl+Shift+M`        | Toggle the native menu bar                   |
| `S`                   | Toggle subtitles                             |
| `I`                   | Set in-point                                 |
| `Shift+I`             | Clear in-point                               |
| `O`                   | Set out-point                                |
| `Shift+O`             | Clear out-point                              |
| `Ctrl+Shift+E`        | Export the marked In/Out range as MP4        |
| `R`                   | Loop active range or toggle auto-replay      |
| `MouseWheel`          | Adjust volume                                |
| `Esc`                 | Exit borderless fullscreen                   |
| `Ctrl+H`              | Toggle borderless fullscreen                 |
| `Ctrl+W`              | Fill screen height without black padding     |
| `Ctrl+Q`              | Half-native-resolution window size           |
| `Ctrl+R`              | Rotate clockwise 90°                         |
| `Ctrl+E`              | Rotate counter-clockwise 90°                 |
| `Ctrl+MouseWheel`     | Zoom at cursor                               |
| `Ctrl+Drag`           | Pan while zoomed                             |
| `Drag`                | Move window in frameless mode                |
| `Ctrl+0`              | Reset zoom, pan, and rotation                |
| `H` (hold)            | Show controls overlay                        |
| `[` / `]`             | Decrease / increase playback speed           |
| `\`                   | Reset playback speed to 1×                   |
| `Backspace`           | Cancel scrub and return to original position |
| `` ` ``               | Toggle HW / SW decode mode in title bar      |

The recent-files overlay supports:

* `↑` / `↓` to select
* `Enter` to open
* `Del` to remove an entry
* `Esc` to close

Timeline scrubbing is available by hovering near the bottom of the window and clicking or dragging.

## In / Out range

Press `I` to mark where playback begins and `O` to mark where it ends.

| In | Out | Playback range       | Space at end returns to |
| -- | --- | -------------------- | ----------------------- |
| —  | —   | start → end          | start                   |
| ✓  | —   | in-point → end       | in-point                |
| —  | ✓   | start → out-point    | start                   |
| ✓  | ✓   | in-point → out-point | in-point                |

Press `R` while a range is active to loop it continuously.

Use `Shift+I` and `Shift+O` to clear individual points.

In/out points reset when a new file is opened.

With both marks set, press `Ctrl+Shift+E` or choose **Media → Export Marked Range**. FastPlay uses stream copy for safe MP4-family keyframe cuts and accurate H.264/AAC transcoding otherwise. Release packages include the required FFmpeg tools.

## Goals

FastPlay prioritizes:

* low open-to-first-frame latency
* responsive seeking and scrubbing
* GPU-resident presentation on the normal hardware path
* predictable playback behavior
* narrow, maintainable architecture
* Windows-specific performance
* useful controls without unnecessary product complexity

## Non-goals

FastPlay does not currently aim to provide:

* media library management
* metadata scraping or collection organization
* network or streaming playback
* plugin support
* a browser or web interface
* full persistent playlist management
* advanced subtitle styling
* embedded subtitle track selection
* metadata-driven dynamic HDR tone mapping
* exposure or curve controls
* additional hardware rendering backends beyond the current D3D11-first design

FastPlay does support lightweight queue and folder playback, but those queues are temporary and intentionally much simpler than a full playlist or media-library system.

## Known limitations

* **Windows x64 only.** There are currently no macOS, Linux, Windows ARM, or other platform builds.
* **Format support is broad but not exhaustive.** Playback depends on the FFmpeg build used by FastPlay. Uncommon or exotic formats may not work.
* **External `.srt` subtitles only.** Embedded subtitle tracks and other subtitle formats are not currently loaded.
* **Queues are session-only.** There are no persistent playlist files, shuffle mode, or in-window queue browser.
* **Folder scanning is non-recursive.**
* **No streaming or network sources.** FastPlay is intentionally focused on local files.
* **WASAPI shared-mode audio only.** There is currently no exclusive mode or runtime audio-track switching.
* **HDR mode is selected when a file is opened.** If an HDR video is opened on an HDR display and the playing window is then moved to a non-HDR display, the image may appear washed out until the file is reopened.

## Requirements

For normal use:

* Windows 10 or later
* Windows x64
* D3D11 / DXGI / WASAPI-capable system

Rust and FFmpeg development files are **not** required when using the normal MSI or portable release.

## Portable version

The portable Windows ZIP can be downloaded from the release links at the top of this page.

Portable FastPlay does not require installation, but it still stores:

* settings
* recent-file history
* resume positions
* logs

under:

```text
%APPDATA%\FastPlay
```

## Building from source

### Requirements

* Rust toolchain
* FFmpeg development headers and libraries
* Windows development environment with D3D11, DXGI, and WASAPI support

### FFmpeg setup

`build.rs` supports several FFmpeg discovery methods.

#### Preferred

Set:

```text
FFMPEG_DIR
```

#### Or configure paths explicitly

```text
FFMPEG_INCLUDE_DIR
FFMPEG_LIB_DIR
FFMPEG_BIN_DIR
```

`FFMPEG_BIN_DIR` is optional.

### Fallback search locations

FastPlay also checks:

```text
%VCPKG_ROOT%/installed/x64-windows
%USERPROFILE%/vcpkg/installed/x64-windows
C:\tools\vcpkg\installed\x64-windows
```

The build expects the standard FFmpeg development layout containing `include/` and `lib/`.

Runtime DLL staging also works when a `bin/` directory is available.

### Build

```powershell
cargo build --release
```

### Create a portable ZIP

```powershell
.\scripts\package-portable.ps1 -SkipBuild
```

The resulting archive is written to:

```text
target\dist\
```

## Run from source

Normal playback:

```powershell
cargo run --release -- <path-to-media>
```

Force software decoding:

```powershell
cargo run --release -- --force-sw <path-to-media>
```

You can also drag and drop media directly onto the FastPlay window.

## External subtitles

FastPlay supports external sidecar `.srt` subtitles.

Place the subtitle file beside the video using the same basename:

```text
movie.mp4
movie.srt
```

FastPlay automatically loads the matching subtitle file when the video is opened.

## Architecture

FastPlay is intentionally Windows-focused and uses a D3D11-first playback architecture.

### Preferred video path

```text
FFmpeg
  ↓
AV_PIX_FMT_D3D11
  ↓
D3D11 video processor
  ↓
DXGI present
```

### Software decoding fallback

```text
FFmpeg demux
  ↓
software decode
  ↓
D3D11 upload
  ↓
D3D11 video processor
  ↓
DXGI present
```

### Audio path

```text
FFmpeg decode
  ↓
WASAPI shared-mode sink
```

### Subtitle path

```text
SRT sidecar
  ↓
CPU parsing and layout
  ↓
GPU alpha composition during present
```

For the full implementation charter, see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

## Project structure

```text
src/
  app/        # session coordinator, commands, events, state
  audio/      # audio sink abstractions
  ffi/        # FFmpeg / D3D11 / DXGI / WASAPI interop
  media/      # source, video, audio, seek, subtitle
  platform/   # Win32 window/input
  playback/   # clock, metrics, queue policy, generations
  render/     # presenter, swapchain, surface registry, timeline overlay
```

## Feedback and feature requests

FastPlay is now stable for my own everyday use, so active development will be more selective while I work on other projects.

If there is a bug, format issue, workflow improvement, or feature that would make FastPlay better for you, please [open an issue](../../issues).

Useful requests are welcome.

## License

MIT
