# <img src="assets/icon/fastplay.ico" alt="FastPlay icon" width="36" /> FastPlay

> **The Windows video player for people who care how fast playback feels.**

FastPlay is a Windows video player built for the parts of playback people actually notice: opening a file, reaching the first frame quickly, scrubbing without friction, adjusting the picture easily, and getting out of the way while you watch.

It is intentionally focused on **local playback**. No media library. No plugin maze. No feature sprawl. Just fast open, clean playback, responsive controls, and a tighter Windows-native experience.

[Download MSI installer](https://github.com/CalvinSturm/FastPlay/releases/download/v0.1.0/fastplay-0.1.0-x86_64.msi) • [All releases](../../releases) • [Report an issue](../../issues)

**Current status:** early release, actively improving playback speed, seek feel, and UI polish on Windows x64.

---

## Why FastPlay

FastPlay is built around a simple idea:

> **fast local playback on Windows, without the usual extra surface area**

It is not trying to be a media hub, streaming platform, or plugin-heavy all-in-one app. The focus is narrower: fast file open, responsive seeking, clear controls, and playback that stays out of the way.

---

## Current capabilities

- drag and drop file open
- keyboard seeking
- timeline scrubbing overlay
- borderless fullscreen
- picture zoom, pan, and rotation
- keyboard-first playback controls
- external `.srt` subtitle support
- volume overlay
- playback metrics, including open-to-frame and seek latency

---

## Controls

| Key | Action |
|-----|--------|
| `Space` | Pause, resume, or replay at end |
| `Left / Right` | Seek ±5s, hold for 15s steps |
| `S` | Toggle subtitles |
| `R` | Toggle auto-replay |
| `MouseWheel` | Adjust volume |
| `Ctrl+H` | Toggle borderless fullscreen |
| `Ctrl+W` | Fit to screen height |
| `Ctrl+R / Ctrl+E` | Rotate ±90° |
| `Ctrl+MouseWheel` | Zoom at cursor |
| `Ctrl+0` | Reset zoom, pan, and rotation |

---

## Install

### For users

Download the latest MSI here:

[fastplay-0.1.0-x86_64.msi](https://github.com/CalvinSturm/FastPlay/releases/download/v0.1.0/fastplay-0.1.0-x86_64.msi)

Or browse all builds on the [Releases](../../releases) page.

### For developers

Clone the repo and build locally:

```bash
git clone https://github.com/CalvinSturm/FastPlay.git
cd FastPlay
cargo build --release
```

---

## License

MIT
