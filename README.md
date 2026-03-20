# <img src="assets/icon/fastplay.ico" alt="FastPlay icon" width="36" /> FastPlay

FastPlay is a Windows video player built for the parts of playback people actually notice: opening a file, reaching the first frame quickly, scrubbing without friction, adjusting the picture easily, and getting out of the way while you watch.

It is intentionally focused on **local playback**. No media library. No plugin maze. No feature sprawl. Just fast open, clean playback, responsive controls, and a tighter Windows-native experience.

[Download MSI installer](https://github.com/CalvinSturm/FastPlay/releases/download/v0.1.1/fastplay-0.1.1-x86_64.msi) • [All releases](../../releases) • [Report an issue](../../issues)

**Current status:** early release, actively improving playback speed, seek feel, and UI polish on Windows x64.

![demo](https://github.com/user-attachments/assets/ac8ae5f1-b4e3-42ca-b21e-c20c1c5de5c0)

## Controls

| Key | Action |
|-----|--------|
| `Space` | Pause / resume / replay at end |
| `Left` | Seek backward 5s, hold for 15s steps |
| `Right` | Seek forward 5s, hold for 15s steps |
| `S` | Toggle subtitles |
| `R` | Toggle auto-replay |
| `MouseWheel` | Adjust volume |
| `Ctrl+H` | Toggle borderless fullscreen |
| `Ctrl+W` | Fill screen height with no black padding |
| `Ctrl+Q` | Snap window to half the video's native resolution |
| `Ctrl+R` | Rotate clockwise 90 degrees |
| `Ctrl+E` | Rotate counter-clockwise 90 degrees |
| `Ctrl+MouseWheel` | Zoom at cursor |
| `Ctrl+0` | Reset zoom, pan, and rotation |

Timeline scrubbing is available by hovering near the bottom of the window and clicking or dragging.

## Features

### Playback
- drag-and-drop file open
- quick open and first-frame path
- responsive keyboard seek with accelerated hold behavior
- timeline scrubbing overlay with playback position
- auto-replay toggle
- replay at end of playback
- playback metrics such as open-to-frame latency, seek latency, and dropped frames

### Video and audio
- FFmpeg-based demux and decode
- hardware video decode on the preferred D3D11 path
- software video decode fallback with D3D11 upload and present
- WASAPI shared-mode audio playback
- audio-master playback timing when audio exists
- generation-safe seek and reopen behavior
- device-loss and resize recovery paths

### Viewing controls
- borderless fullscreen
- cursor-centered zoom and pan
- 90-degree view rotation
- fit-to-screen window sizing with no black padding
- half native resolution window sizing
- volume control with on-screen overlay

### Subtitles
- external `.srt` subtitle overlay
- runtime subtitle toggle

## Goals

FastPlay prioritizes:

- low open-to-first-frame latency
- responsive seek behavior
- GPU-resident presentation on the normal hardware path
- bounded queues and explicit ownership
- narrow, maintainable architecture
- Windows-specific performance rather than broad platform support

## Non-goals

FastPlay does **not** currently aim to provide:

- playlists or media library management
- streaming support
- plugin support
- browser or web UI
- advanced subtitle styling or embedded subtitle track selection
- HDR or tone mapping
- extra hardware backends beyond the current D3D11-first design

## Requirements

- Windows 10 or later
- Rust toolchain
- FFmpeg development headers and libraries available locally
- D3D11 / DXGI / WASAPI-capable system

## FFmpeg setup

`build.rs` supports these FFmpeg discovery patterns.

### Preferred

Set `FFMPEG_DIR`.

### Or set explicitly

- `FFMPEG_INCLUDE_DIR`
- `FFMPEG_LIB_DIR`
- optional: `FFMPEG_BIN_DIR`

### Fallback search locations

- `%VCPKG_ROOT%/installed/x64-windows`
- `%USERPROFILE%/vcpkg/installed/x64-windows`
- `C:\tools\vcpkg\installed\x64-windows`

The build expects the usual FFmpeg development layout with `include/` and `lib/`. Runtime DLL staging works when a `bin/` directory is available.

## Build

```powershell
cargo build --release
```

## Run

Normal playback:

```powershell
cargo run --release -- <path-to-media>
```

Or drag and drop a media file onto the FastPlay window.

Force software decode fallback:

```powershell
cargo run --release -- --force-sw <path-to-media>
```

## External subtitles

FastPlay supports **external sidecar `.srt` files only**. Place a subtitle file next to the media file with the same basename:

```text
movie.mp4
movie.srt
```

The subtitle sidecar will be auto-loaded if present.

## Architecture

### Preferred path

`FFmpeg -> AV_PIX_FMT_D3D11 -> D3D11 video processor -> DXGI present`

### Software fallback path

`FFmpeg demux -> software decode -> D3D11 upload -> D3D11 video processor -> DXGI present`

### Audio path

`FFmpeg decode -> WASAPI shared-mode sink`

### Subtitle path

- external `.srt` sidecar only
- CPU parsing and layout
- GPU alpha composition during present

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

## License

MIT
