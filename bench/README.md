# FastPlay benchmark harness

A local, repeatable harness that measures FastPlay's latency and playback-health
metrics, implementing `docs/ROADMAP.md` §2. It drives the real player and
aggregates the metrics the app already logs to `session.log` into p50/p95
reports — there is no instrumentation build and no change to app code.

This is a **local/optional tool**. It is intentionally not part of `cargo build`
/ CI (per the roadmap): it requires a GUI session, a media corpus, and several
minutes to run. It can be wired into CI later once it has proven stable.

## What it measures

| Metric | Source log line | Reported as |
|--------|-----------------|-------------|
| open-to-first-frame | `open_to_first_frame_ms` | p50 / p95 |
| seek-to-first-frame | `seek_to_first_frame_ms` | p50 / p95 |
| seek-to-A/V-settled | `seek_to_av_settled_ms` | p50 / p95 |
| pause latency | `pause_to_stop_ms` | p50 / p95 |
| resume latency | `play_to_motion_ms` | p50 / p95 |
| frame drops over a playthrough | `playback_summary … dropped_video_frames` | total |
| audio underruns over a playthrough | `playback_summary … audio_underruns` | total |
| hardware-decode fallbacks | `playback_summary … hw_fallback_count` | max |

Percentiles use the nearest-rank method. Use a larger corpus and more
`-Iterations` for stable percentiles (especially p95).

## Requirements

- A **release** build: `cargo build --release`. (The debug build is a
  console-subsystem app whose render window the title match would miss.)
- `ffmpeg` on `PATH` to generate the synthetic corpus (`gen-corpus.ps1`).
- `ffprobe` on `PATH` (optional) for exact clip durations; falls back to 30s.
- Windows + a desktop session (the harness drives the window with `PostMessageW`).

## Usage

```powershell
# 1. Build the player
cargo build --release

# 2. Generate a synthetic corpus (writes to bench/corpus, git-ignored)
pwsh -File bench/gen-corpus.ps1

# 3. Run the benchmark
pwsh -File bench/run-bench.ps1 -Iterations 5
```

Point it at your own corpus of real files for representative numbers:

```powershell
pwsh -File bench/run-bench.ps1 -CorpusDir D:\media\bench -Iterations 10
```

Fast latency-only pass (skips the full playthrough, so no drop/underrun/fallback
counters):

```powershell
pwsh -File bench/run-bench.ps1 -LatencyOnly
```

### Parameters

| Param | Default | Notes |
|-------|---------|-------|
| `-Exe` | `target/release/fastplay.exe` | Player under test |
| `-CorpusDir` | `bench/corpus` | Folder of media files |
| `-OutDir` | `bench/results` | JSON + CSV output (git-ignored) |
| `-Iterations` | `3` | Runs per clip |
| `-SeeksPerRun` | `5` | Seek keypresses per run |
| `-LatencyOnly` | off | Skip play-to-end (faster) |

## Output

- Console: a p50/p95 table plus the playthrough counters.
- `bench/results/bench-<timestamp>.json`: full report (metadata, per-run rows,
  aggregated summary) — machine-readable for diffing across builds.
- `bench/results/bench-<timestamp>.csv`: one row per run.

## How it works

For each clip × iteration the harness:

1. Deletes `%APPDATA%\FastPlay\session.log`.
2. Launches the player on the clip and finds the render window by its
   `" - FastPlay"` title suffix (scoped to the launched PID).
3. Waits for `open_to_first_frame_ms` to confirm open.
4. Posts seek keypresses (`PostMessageW` `WM_KEYDOWN` VK_RIGHT/VK_LEFT), then a
   pause/resume (`WM_CHAR` space).
5. Unless `-LatencyOnly`, waits for the `playback_summary` line (full playthrough).
6. Closes via `WM_CLOSE` — `session.log` only flushes on a graceful exit.
7. Parses the log and aggregates.

## Limitations

- p95 is only as good as the sample count — small synthetic corpora give rough
  tails. Prefer a real corpus and ≥10 iterations for reportable numbers.
- "Frame drops / underruns over long playback" scale with clip length; the
  synthetic clips are short. Use longer real media to stress steady-state.
- Numbers are machine/GPU/driver-specific; compare runs on the same hardware.
  Do not publish them as universal latency promises (see `ARCHITECTURE.md` §24).
