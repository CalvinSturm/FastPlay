# <img src="assets/icon/fastplay.ico" alt="FastPlay icon" width="36" /> FastPlay

> **The Windows video player for people who care how fast playback feels.**

FastPlay is a Windows-first video player built for the parts of playback people actually notice: opening a file, reaching the first frame quickly, scrubbing without friction, seeking responsively, and staying out of the way while you watch.

It is intentionally focused on **local playback**. No media library. No plugin maze. No feature sprawl. Just fast open, clean playback, responsive controls, and a tighter Windows-native experience.

[Download MSI installer](https://github.com/CalvinSturm/FastPlay/releases/download/v0.1.0/fastplay-0.1.0-x86_64.msi) • [All releases](../../releases) • [Report an issue](../../issues)

**Current status:** early release, actively improving playback speed, seek feel, and UI polish on Windows x64.

---

## Why FastPlay

Most video players try to do everything.

FastPlay is built around a narrower bet: **local playback should feel instant**.

When you open a file, jump around the timeline, toggle fullscreen, or check a scene frame by frame, the player should respond immediately and stay out of your way.

That focus drives the project:

- fast open-to-first-frame behavior
- responsive scrubbing and seeking
- low-overhead controls and UI
- focused local playback on Windows
- GPU-native rendering on the preferred path

## Who it is for

FastPlay is for people who notice when playback feels slow.

That includes:

- Windows users who want a focused local player
- people previewing local media files repeatedly
- users who scrub a lot
- people checking scenes frame by frame
- anyone who wants a sharper alternative to bloated media hubs

## What it is not trying to be

FastPlay is intentionally **not** trying to be:

- a media library manager
- a streaming platform
- a plugin ecosystem
- a feature-maximal VLC clone

The product idea is simpler than that:

> **fast local playback on Windows, without the usual extra surface area**

---

## Current capabilities

FastPlay currently includes:

- drag and drop file open
- keyboard seeking
- timeline scrubbing overlay
- borderless fullscreen
- zoom and pan
- rotation controls
- external `.srt` subtitle support
- volume overlay
- playback metrics, including open-to-frame and seek latency

## Why it feels different

FastPlay is not being built as a giant media center.

It is being built around the core local playback loop:

- open a file fast
- reach the first frame quickly
- seek and scrub responsively
- keep the interface focused
- make playback feel immediate

That is the product.

## Why Windows-first

FastPlay is intentionally Windows-first.

Instead of stretching toward cross-platform abstraction too early, the project is focused on building a player that feels excellent on the native Windows path first. That keeps the architecture aligned with responsiveness, rendering behavior, playback smoothness, and practical desktop UX where the product is currently strongest.

Windows-first is not an apology. It is a product decision.

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
