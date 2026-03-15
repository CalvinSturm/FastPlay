# FastPlay

FastPlay is a **Windows-first, latency-focused media player** built in **Rust**.

It is designed around a simple idea: a player should feel fast because it opens quickly, seeks quickly, keeps video on the GPU, and avoids unnecessary UI and pipeline overhead.

## Current status

FastPlay currently supports:

- Windows-native windowing with drag-and-drop file open
- D3D11 + DXGI flip-model presentation
- FFmpeg-based demux/decode
- hardware video decode on the preferred D3D11 path
- software video decode fallback with D3D11 upload/present
- cached D3D11 video processor (avoids per-frame kernel-mode allocations)
- WASAPI shared-mode audio playback
- audio-master playback timing when audio exists
- generation-safe seek and reopen behavior
- timeline scrubbing overlay with playback position
- borderless fullscreen
- cursor-centered zoom and pan
- view rotation (90-degree increments)
- fit-to-screen window sizing (no black padding)
- auto-replay toggle
- spacebar replay at end of playback
- accelerated keyboard seeking (hold for faster seek)
- timeline appears briefly on keyboard seek
- device-loss and resize recovery paths
- external `.srt` subtitle overlay with runtime toggle
- volume control with on-screen overlay
- playback metrics (open-to-frame, seek latency, dropped frames, etc.)
- embedded application icon
- no console window in release builds
- narrow validation hook for forced software decode

This is still a focused engineering project, not a general-purpose consumer media player.

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
- browser/web UI
- advanced subtitle styling
- embedded subtitle track selection
- HDR/tone-mapping (deferred until full pipeline is ready)
- extra hardware backends beyond the current D3D11-first design

## Architecture

FastPlay is built around these core paths:

### Preferred path
`FFmpeg -> AV_PIX_FMT_D3D11 -> D3D11 video processor -> DXGI present`

### Software fallback path
`FFmpeg demux -> software decode -> D3D11 upload -> D3D11 video processor -> DXGI present`

### Audio path
`FFmpeg decode -> WASAPI shared-mode sink`

### Subtitle path
- external `.srt` sidecar only
- CPU parsing/layout
- GPU alpha composition during present

For the full implementation charter, see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

For durable repo rules used during development, see [`AGENTS.md`](./AGENTS.md).

## Requirements

- Windows 10 or later
- Rust toolchain
- FFmpeg development headers/libs available locally
- D3D11 / DXGI / WASAPI-capable system

## FFmpeg setup

`build.rs` currently supports these FFmpeg discovery patterns:

### Preferred
Set:

- `FFMPEG_DIR`

### Or set explicitly
- `FFMPEG_INCLUDE_DIR`
- `FFMPEG_LIB_DIR`
- optional: `FFMPEG_BIN_DIR`

### Current fallback search locations
- `%VCPKG_ROOT%/installed/x64-windows`
- `%USERPROFILE%/vcpkg/installed/x64-windows`
- `C:\tools\vcpkg\installed\x64-windows`

The build expects the usual FFmpeg development layout with `include/` and `lib/`. Runtime DLL staging currently works when a `bin/` directory is available.

## Build

```powershell
cargo build --release
```

## Run

Normal playback:

```powershell
cargo run --release -- <path-to-media>
```

Or drag and drop a media file onto the FastPlay window. Subsequent drops resize in place without moving the window.

Force software decode fallback:

```powershell
cargo run --release -- --force-sw <path-to-media>
```

## External subtitles

FastPlay currently supports **external sidecar `.srt` files only**.

Place a subtitle file next to the media file with the same basename:

```text
movie.mp4
movie.srt
```

The subtitle sidecar will be auto-loaded if present.

## Controls

| Key | Action |
|-----|--------|
| `Space` | Pause / resume / replay at end |
| `Left` | Seek backward 5s (hold for 15s steps) |
| `Right` | Seek forward 5s (hold for 15s steps) |
| `S` | Toggle subtitles on/off |
| `R` | Toggle auto-replay |
| `MouseWheel` | Adjust volume |
| `Ctrl+H` | Toggle borderless fullscreen |
| `Ctrl+W` | Fill screen height, no black padding |
| `Ctrl+R` | Rotate clockwise 90 degrees |
| `Ctrl+E` | Rotate counter-clockwise 90 degrees |
| `Ctrl+MouseWheel` | Zoom at cursor |
| `Ctrl+0` | Reset zoom / pan / rotation |

Timeline scrubbing is available by hovering near the bottom of the window and clicking/dragging.

## Validation notes

The repo may generate local validation artifacts under `validation/`.

Ignored by default:

* `validation/*.log`
* `validation/*.mp4`

## Current limitations

* Windows-only
* external `.srt` only
* no embedded subtitle track support
* no ASS styling engine
* no advanced subtitle settings UI
* no HDR passthrough or tone mapping
* no streaming
* no playlists/library
* no broad endpoint-notification system for audio devices
* recovery paths are intentionally minimal and correctness-focused
* software fallback is intentionally narrow and session-scoped

## Important implementation note: software fallback textures

Software-decoded frames are uploaded into D3D11 and presented through the existing video-processor path.

That path has a validated texture-compatibility requirement today, so changes to software-upload texture creation should be re-validated against runtime playback. The detailed constraint is documented in [`ARCHITECTURE.md`](./ARCHITECTURE.md) and [`AGENTS.md`](./AGENTS.md).

## Project structure

```text
src/
  app/        # session coordinator, commands, events, state
  audio/      # audio sink abstractions
  ffi/        # FFmpeg / D3D11 / DXGI / WASAPI interop
  media/      # media-domain types, source, video, audio, seek, subtitle
  platform/   # Win32 window/input
  playback/   # clock, metrics, queue policy, generations
  render/     # presenter, swapchain, surface registry, timeline overlay
```

## Development principles

* `PlaybackSession` is the only coordinator
* public APIs do not expose raw COM pointers
* unsafe code stays boxed inside `src/ffi/*`
* stale work is dropped before side effects
* milestone-driven development
* architecture changes are explicit, not accidental

## Roadmap status

Completed milestones:

* M0: window + D3D11 + DXGI shell
* M1: first-frame FFmpeg D3D11 decode/present
* M2: steady-state D3D11 video playback
* M3: WASAPI audio playback + audio-master sync
* M4: seek, stale-drop enforcement, and recovery paths
* M5: software decode fallback path
* M6: external `.srt` subtitle overlay
* M7: borderless fullscreen, cursor-centered zoom, view rotation
* M8: timeline scrubbing, playback overlays (volume, position)
* M9: drag-and-drop, accelerated seek, fit-to-screen, auto-replay, metrics wiring
* M10: end-of-playback replay, embedded icon, no-console release, timeline UX polish

## License

TBD
