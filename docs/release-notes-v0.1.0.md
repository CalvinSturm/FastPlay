# FastPlay v0.1.0

A Windows-first, latency-focused media player built in Rust. Designed to open fast, seek fast, and stay out of your way.

## Features

- **Hardware video decode** via D3D11 with software fallback
- **Audio playback** via WASAPI with audio-master sync
- **Timeline scrubbing** — hover near the bottom edge to seek
- **Drag and drop** to open files; subsequent drops resize in place
- **Accelerated keyboard seeking** — hold arrow keys for faster steps
- **Borderless fullscreen**, cursor-centered zoom, and view rotation
- **External `.srt` subtitle support** — auto-loaded from sidecar file
- **Volume control** with on-screen overlay
- **Auto-replay** toggle
- **Fit-to-screen** window sizing with no black bars

## Controls

| Key | Action |
|-----|--------|
| `Space` | Pause / resume / replay at end |
| `Left / Right` | Seek ±5s (hold for 15s steps) |
| `S` | Toggle subtitles |
| `R` | Toggle auto-replay |
| `MouseWheel` | Volume |
| `Ctrl+H` | Borderless fullscreen |
| `Ctrl+W` | Fit to screen height |
| `Ctrl+R / Ctrl+E` | Rotate ±90° |
| `Ctrl+MouseWheel` | Zoom at cursor |
| `Ctrl+0` | Reset zoom / pan / rotation |

## Requirements

- Windows 10 or later
- No other dependencies — all runtime DLLs are bundled

## Installation

Download `fastplay-0.1.0-x86_64.msi` and run it.
