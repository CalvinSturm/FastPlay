# FastPlay SEO & GEO Plan

This document records the search-engine optimization (SEO) and generative-engine
optimization (GEO) strategy for the FastPlay GitHub Pages site
(`https://calvinsturm.github.io/FastPlay/`).

Guiding rules:

- Human-readable copy first, then optimize for search and AI extraction.
- No keyword stuffing.
- No unsupported claims. Do not claim "fastest video player" without measured
  benchmark evidence.
- Preserve FastPlay's honest technical positioning: a fast, lightweight native
  Windows video player for local playback.

## Core positioning

> FastPlay is a fast, lightweight native Windows video player built for local
> playback, smooth scrubbing, hardware-accelerated decode, and responsive
> controls.

## Target keyword cluster

Primary intents:

- play videos fast
- fast Windows media player / fast Windows video player
- faster video player / fastest video player (benchmark-gated)
- lightweight Windows video player
- VLC alternative for Windows
- fast local video player
- video player with smooth scrubbing
- video player with resume playback
- Rust Windows video player
- hardware accelerated Windows video player

## Main page metadata (`docs/index.html`)

- **Title:** `FastPlay - Fast Windows Video Player for Local Playback`
- **Meta description:** FastPlay is a fast, lightweight native Windows video
  player for local playback, smooth scrubbing, hardware-accelerated decode,
  responsive seeking, and simple controls.
- **H1:** `A fast Windows video player for local files`
- **Canonical:** `https://calvinsturm.github.io/FastPlay/`
- Open Graph + Twitter card metadata with the demo image as `og:image`.

## Intent page map

Static subpages under `docs/`, served from the `/FastPlay/` base path:

| URL | Title | Primary intent |
|-----|-------|----------------|
| `/fast-windows-video-player/` | Fast Windows Video Player - FastPlay | fast Windows video/media player, faster video player, fast local video player |
| `/play-videos-fast/` | Play Videos Fast on Windows - FastPlay | play videos fast, play videos faster on Windows |
| `/lightweight-video-player/` | Lightweight Video Player for Windows - FastPlay | lightweight/simple Windows media player |
| `/vlc-alternative/` | Fast VLC Alternative for Windows - FastPlay | VLC alternative for Windows |
| `/benchmarks/` | FastPlay Video Player Benchmarks | fastest video player intent, without unsupported claims |

Each page links back to the home page and laterally to the other intent pages
using descriptive anchor text.

## FAQ / GEO strategy

A visible FAQ section on the home page uses semantic HTML (`<article>` +
headings) so the answers are extractable by AI answer engines. The questions
cover: what FastPlay is, VLC comparison, supported platforms, price/license,
what makes it fast, subtitle support, and the "fastest" question (answered
honestly, deferring to benchmarks).

A "FastPlay facts" table provides a structured, machine-readable quick reference
(product, developer, category, platform, license, technology, best-for, and
explicitly what it is *not* designed for).

## SoftwareApplication schema notes

JSON-LD `SoftwareApplication` block in the home page `<head>`:

- `softwareVersion` tracks the current release (`0.5.0`).
- Accurate `license` (MIT), `programmingLanguage` (Rust), `operatingSystem`
  (Windows 10/11), `offers.price` 0, `codeRepository`, `downloadUrl`,
  `screenshot`/`image`.
- **No** fake ratings, reviews, download counts, or benchmark results.

Update `softwareVersion` (here and on each intent page) whenever a new release
ships.

## FAQPage schema notes

A `FAQPage` JSON-LD block mirrors the visible FAQ answers exactly. No
schema-only questions that are not also visible to users.

## Benchmark requirements

The benchmarks page currently shows the metric list with a "pending" note. To
make any "fastest"/comparative claim, we need measured data for:

- cold launch time, time to first frame, seek latency, resume load time
- memory usage, CPU usage during playback
- GPU decode path vs software fallback path

The corpus and scripts live under `bench/`. Until real numbers exist, copy must
stay at "designed for fast local playback".

## Internal linking plan

- Home page links to all five intent pages (after the at-a-glance section and in
  the footer) with descriptive anchors.
- Each intent page links back home and to its siblings.
- The "vs VLC" home section links to the VLC-alternative page; the FAQ links to
  the benchmarks page.

## Screenshot / alt text plan

Current screenshots are hosted as GitHub user-attachment URLs (no local image
files in `docs/`), so filenames cannot be renamed without re-hosting. Alt text
on the home page demo image has been made descriptive. When screenshots are
added to the repo, prefer descriptive filenames such as:

- `fastplay-fast-windows-video-player.png`
- `fastplay-local-video-playback.png`
- `fastplay-smooth-scrubbing-windows.png`
- `fastplay-responsive-seek.png`
- `fastplay-windows-video-player-controls.png`

with alt text describing the scene and the local-playback context.

## Future article ideas

- Building a Fast Native Windows Video Player in Rust
- How FastPlay Optimizes Time to First Frame
- How FastPlay Handles Responsive Seeking
- Why Smooth Scrubbing Is Hard in a Windows Video Player
- FFmpeg, D3D11, DXGI, and WASAPI in FastPlay
- FastPlay vs VLC for Local Windows Playback
- How a Stale Audio Batch Caused A/V Desync After Backward Seeks
