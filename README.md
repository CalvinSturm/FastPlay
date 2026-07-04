# <img src="assets/icon/fastplay.ico" alt="FastPlay icon" width="36" /> FastPlay

FastPlay is a fast, lightweight native Windows video player focused on local playback, responsive seeking, hardware-accelerated decode, and simple controls.

It is intentionally focused on **local playback**. No media library. No plugin maze. No feature sprawl. Just fast open, smooth scrubbing, clean playback, responsive controls, and a tighter Windows-native experience.

**[Download FastPlay v0.4.1 for Windows x64](https://github.com/CalvinSturm/FastPlay/releases/download/v0.4.1/fastplay-0.4.1-x86_64.msi)** • [All releases](../../releases) • [Report an issue](../../issues)

**Current status:** early release, actively improving playback speed, seek feel, and UI polish on Windows x64.

<img width="550" height="480" alt="ezgif-1084bbf2cf26c3f5" src="https://github.com/user-attachments/assets/79667f65-6150-46d8-bb7c-a5024b53d2d1" />

## FastPlay Free and FastPlay Pro

FastPlay Free remains focused on fast local playback: opening local files quickly, smooth seeking, queue playback, resume, keyboard controls, screenshots, subtitles, in/out range, and loop range.

FastPlay Pro is the review-workflow layer for creators, editors, QA testers, and power users. The paid wedge is not paid playback; it is saving time while reviewing footage.

### Pro Preview v0.5.0 foundation

Implemented in the Pro Preview foundation:

- timestamp markers for the current media file
- marker overlay with keyboard selection, seek, and removal
- marker persistence in local app data
- marker export to `.txt` and `.csv`
- saved review queue storage foundation for a future UI
- centralized Free/Pro capability checks with a development-only override

Not yet implemented:

- Lemon Squeezy activation
- marker note editing UI
- reliable batch screenshots from markers
- saved review queue UI

FastPlay Pro launch pricing target: $19 one-time. Standard target: $29 one-time. These are planning targets, not a subscription.

## Why FastPlay exists

Most of the time you open a video player, you are not managing a library or transcoding a stream. You are opening one local file and watching it. FastPlay is built around that single moment: how fast the file opens, how quickly the first frame appears, whether it resumes where you left off, and how cleanly it scrubs and gets out of the way.

The mainstream players are excellent and do far more than this. FastPlay deliberately does less, so the path from double-click to watching stays short and the controls stay responsive. It is a focused tool for local playback on Windows, not a replacement for a full-featured media suite.

## Compared to VLC and MPC-HC

VLC and MPC-HC are mature, capable, and the right choice for most people when they need broad format support, streaming, playlists, deep configuration, and years of hardening. FastPlay is not trying to compete on breadth.

Where FastPlay differs is scope and intent:

| | FastPlay | VLC | MPC-HC |
|---|---|---|---|
| Primary focus | Fast local playback on Windows | Everything, everywhere | Lightweight local playback (Windows) |
| Platforms | Windows x64 only | Cross-platform | Windows |
| Format/codec breadth | Common formats via FFmpeg | Very broad | Broad |
| Streaming / network | No | Yes | Limited |
| Playlists / library | No | Yes | Basic |
| Per-file resume | Built in | Plugin/config | Limited |
| Render path | D3D11-first, GPU-resident | Multiple backends | DirectShow / madVR etc. |

Use VLC or MPC-HC when you need format coverage, streaming, or platform reach. Reach for FastPlay when you want a snappy, no-friction window for the local file in front of you.

## Benchmarks

FastPlay is built for fast local playback: low open-to-first-frame latency and responsive seeking. It records in-app playback metrics (open-to-frame latency, seek latency, dropped frames), and the benchmark corpus and scripts live under [`bench/`](bench/).

Measured cross-player comparisons are still pending, so FastPlay does not claim to be the "fastest" video player — that requires published benchmark data. Tracking page: [FastPlay benchmarks](https://calvinsturm.github.io/FastPlay/benchmarks/).

## Controls

| Key | Action |
|-----|--------|
| `Space` | Pause / resume / replay at end |
| `Left` | Seek backward 5s, hold for 15s steps |
| `Right` | Seek forward 5s, hold for 15s steps |
| `Ctrl+F` | Move one frame forward |
| `Ctrl+B` | Move one frame backward |
| `Ctrl+O` | Open media file |
| `Ctrl+Shift+O` | Recent files overlay (↑↓ select · Enter open · Del remove · Esc close) |
| `PageUp` / `PageDown` | Previous / next file in the play queue |
| `Ctrl+S` | Save screenshot |
| `M` | Add timestamp marker (Pro Preview / development override) |
| `Ctrl+M` | Marker overlay (↑↓ select · Enter seek · Del remove · E export · B batch placeholder · Esc close) |
| `S` | Toggle subtitles |
| `I` | Set in-point at current position |
| `Shift+I` | Clear in-point |
| `O` | Set out-point at current position |
| `Shift+O` | Clear out-point |
| `R` | Toggle loop range (if in/out set) · toggle auto-replay (if no range) |
| `MouseWheel` | Adjust volume |
| `Esc` | Exit borderless fullscreen |
| `Ctrl+H` | Toggle borderless fullscreen |
| `Ctrl+W` | Fill screen height with no black padding |
| `Ctrl+Q` | Snap window to half the video's native resolution |
| `Ctrl+R` | Rotate clockwise 90 degrees |
| `Ctrl+E` | Rotate counter-clockwise 90 degrees |
| `Ctrl+MouseWheel` | Zoom at cursor |
| `Ctrl+Drag` | Pan when zoomed in |
| `Ctrl+0` | Reset zoom, pan, and rotation |
| `H` (hold) | Show controls overlay |
| `[` / `]` | Decrease / increase playback speed |
| `\` | Reset playback speed to 1× |
| `Backspace` | Cancel scrub and return to original position |
| `` ` `` | Toggle HW/SW decode mode in title bar |

Timeline scrubbing is available by hovering near the bottom of the window and clicking or dragging.

### In / Out range

Press `I` to mark where playback starts and `O` to mark where it ends. The range adapts to however many points are set:

| In | Out | Plays | Space at end goes to |
|----|-----|-------|----------------------|
| — | — | start → end | start |
| ✓ | — | in-point → end | in-point |
| — | ✓ | start → out-point | start |
| ✓ | ✓ | in-point → out-point | in-point |

Press `R` while a range is active to loop it continuously. Use `Shift+I` / `Shift+O` to clear individual points. In/out points reset when a new file is opened.

## Features

### Playback
- `Ctrl+O` file open dialog and drag-and-drop file open
- lightweight play queue: drop multiple files or a folder, step through with `PageUp` / `PageDown`, and auto-advance to the next file at the natural end of each one
- recent-files overlay with automatic per-file resume playback
- quick open and first-frame path
- responsive keyboard seek with accelerated hold behavior
- timeline scrubbing overlay with playback position
- in/out point range with loop and auto-replay
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
- cursor-centered zoom and drag-to-pan
- 90-degree view rotation with auto-rotate from stream display matrix metadata
- fit-to-screen window sizing with no black padding
- half native resolution window sizing
- volume control with on-screen overlay
- hold-to-show keybind reference overlay

### Pro Preview review workflow
- local timestamp marker persistence for the current media file
- keyboard marker overlay for marker review and seeking
- marker removal from the focused marker overlay
- marker export for the current file as `.txt` and `.csv`
- Free-mode Pro copy that does not block normal playback
- development-only Pro override via `FASTPLAY_PRO_DEV=1`

### Subtitles
- external `.srt` subtitle overlay
- runtime subtitle toggle
- accepts both comma and period millisecond separators in SRT timestamps
- UTF-8 with BOM support and Windows-1252 fallback encoding
- strips common formatting tags (`<b>`, `<i>`, `<u>`, `<font>`)

### Platform
- per-monitor DPI awareness (Per-Monitor V2 with system-aware fallback)
- minimum window size enforcement (640×360)

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

* media library management, scraping, or collection organization
* streaming support
* plugin support
* browser or web UI
* advanced subtitle styling or embedded subtitle track selection
* HDR or tone mapping
* extra hardware backends beyond the current D3D11-first design

Lightweight queue/folder playback is implemented (see Features); full playlists, persistent playlist files, and media-library behavior remain non-goals.

## Known limitations

These are current real-world caveats, separate from the deliberate non-goals above:

* **Windows x64 only.** No macOS, Linux, or ARM builds.
* **Common formats, not exhaustive.** Decoding depends on the FFmpeg build in use; uncommon or exotic codecs may not play. Software decode is the fallback when hardware decode is unavailable.
* **External `.srt` subtitles only.** Embedded subtitle tracks and other subtitle formats are not loaded, and styling is intentionally minimal.
* **Queue is in-memory and session-only.** Dropping multiple files or a folder builds a play queue you can step through (`PageUp` / `PageDown`) with auto-advance at end of file, but the queue is not saved between runs, there are no persistent playlist files, folder scanning is non-recursive, and there is no shuffle, repeat, or in-window queue list.
* **No streaming or network sources.** Local files only.
* **Audio is WASAPI shared-mode.** No exclusive mode, no multi-track/audio-track switching.
* **HDR is not tone-mapped.** HDR content plays but is not color-managed.
* **Early release.** Behavior, shortcuts, and metrics are still changing between versions.

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

For the Pro review-workflow capability and storage boundary, see [`docs/pro-foundation.md`](./docs/pro-foundation.md).

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
