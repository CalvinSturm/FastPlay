# FastPlay

FastPlay is a **Windows-first, latency-focused media player** built in **Rust**.

It is designed around a simple idea: a player should feel fast because it opens quickly, seeks quickly, keeps video on the GPU, and avoids unnecessary UI and pipeline overhead.

## Current status

FastPlay currently supports:

- Windows-native windowing
- D3D11 + DXGI flip-model presentation
- FFmpeg-based demux/decode
- hardware video decode on the preferred D3D11 path
- software video decode fallback with D3D11 upload/present
- WASAPI shared-mode audio playback
- audio-master playback timing when audio exists
- generation-safe seek and reopen behavior
- minimal resize / recovery paths
- external `.srt` subtitle overlay
- subtitle toggle at runtime
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
- HDR/tone-mapping work
- extra hardware backends beyond the current D3D11-first design

## Architecture

FastPlay is built around these core paths:

### Preferred path
`FFmpeg -> AV_PIX_FMT_D3D11 -> D3D11 present`

### Software fallback path
`FFmpeg demux -> software decode -> D3D11 upload -> D3D11 present`

### Audio path
`FFmpeg decode -> WASAPI shared-mode sink`

### Subtitle path
- external `.srt` sidecar only
- CPU parsing/layout
- GPU alpha composition during present

For the full implementation charter, see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

For durable repo rules used during development, see [`AGENTS.md`](./AGENTS.md).

## Requirements

- Windows
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
cargo build
```

## Run

Normal playback:

```powershell
cargo run -- <path-to-media>
```

Force software decode fallback:

```powershell
cargo run -- --force-sw <path-to-media>
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

* `Space` — pause / resume
* `Left` — seek backward
* `Right` — seek forward
* `S` — toggle subtitles on/off

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
  render/     # presenter, swapchain, surface registry
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

## License

TBD
