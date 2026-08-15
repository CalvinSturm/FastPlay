# FastPlay benchmark harness

A local, repeatable harness that measures FastPlay's latency and playback-health
metrics, implementing `docs/ROADMAP.md` §2. It drives the real player and
aggregates the metrics the app already logs to its session log into p50/p95
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

1. Records the launch time, then launches the player on the clip and finds the
   render window by its `" - FastPlay"` title suffix (scoped to the launched
   PID).
2. Waits for `open_to_first_frame_ms` to confirm open.
3. Posts seek keypresses (`PostMessageW` `WM_KEYDOWN` VK_RIGHT/VK_LEFT), then a
   pause/resume (`WM_CHAR` space).
4. Unless `-LatencyOnly`, waits for the `playback_summary` line (full playthrough).
5. Closes via `WM_CLOSE` — the session log only flushes on a graceful exit.
6. Resolves that run's `session-<utc-stamp>-<pid>.log` **by the PID it launched
   and a write time after the launch** — not by "the newest log", which would
   race any other FastPlay instance exiting mid-benchmark, and not by PID alone,
   since Windows recycles PIDs and an older run holding the same one would
   satisfy the glob. Then parses and aggregates.

Nothing deletes logs here: the harness never removes a file it might be racing
the player to write. Retention is the app's own job (`logging::init` sweeps
after seven days).

## Correctness verifiers

Alongside the latency harness, `bench/` holds pixel-level verifiers. They also
drive the real player and read pixels back; they assert instead of measuring,
and exit non-zero on failure.

| Script | Verifies |
|--------|----------|
| `verify-colors.ps1` | SDR (BT.709) backbuffer matches an ffmpeg reference decode |
| `verify-colors-pq.ps1` | PQ pixels on the 10-bit chain: `-Mode vp` (the video-processor blt oracle) or `-Mode shader-pq` (the production shader's PQ passthrough, bit-exact vs spec math); `-FullRange` covers full-range PQ input; `-WrongMatrix` is the negative control |
| `verify-hlg-pq.ps1` | The shader's HLG→PQ output stage vs a double-precision CPU model (with a wrong-transfer negative control) |
| `verify-overlay-hdr.ps1` | The overlay renderer's PQ-chain shader variant (sRGB→BT.2020→203-nit→PQ), opaque and alpha-blended, vs a CPU model |
| `verify-subtitles-hdr.ps1` | Overlays composite correctly on top of HDR video |
| `verify-tonemap.ps1` | The HDR shader's SDR tone-map output matches a double-precision CPU model of its math (PQ clipped bars, PQ unclipped midtones, HLG); on an HDR-active display the same clips exercise the passthrough+screenshot composition instead |
| `verify-hdr-passthrough.ps1` | End-to-end HDR passthrough on an HDR-active display: runs `verify-tonemap` under the composed model, asserts from the reported session log that `HdrPqOutput` was actually selected and the chain swapped, then verifies a mid-playback resize leaves the bars byte-stable |
| `verify-hdr-caps.ps1` | Display HDR detection (`display_hdr_active` tracks the Windows HDR toggle; run with `-ExpectHdr on` / `off`) |
| `verify-hdr-metadata.ps1` | HDR10 static metadata flows from real x265 SEI through conversion to `SetHDRMetaData`, with the MSDN worked-example values asserted in the log |

`verify-subtitles-hdr.ps1` exists because HDR frames reach the backbuffer by a
different route than SDR ones: SDR is blitted by the D3D11 video processor,
while HDR is tone-mapped by our own pixel shader, which binds and clears the
render target itself before drawing. Overlays are then drawn onto that same
target. Nothing in the unit tests covers that ordering, and getting it wrong
either loses the subtitles or wipes the picture under them.

It plays the same clip twice — once with a sidecar `.srt`, once without — and
reads the backbuffer through the app's own screenshot path (`WM_APP+1`), which
captures *after* overlays are composited. Per clip it asserts that the frame is
not blank (so a black screen cannot pass), that the subtitle band changed when
the sidecar was present, and that the picture *above* the band did not. It runs
an HLG clip and an SDR control, so a harness fault shows up as both failing.

```powershell
pwsh -File bench\verify-subtitles-hdr.ps1
```

## Limitations

- p95 is only as good as the sample count — small synthetic corpora give rough
  tails. Prefer a real corpus and ≥10 iterations for reportable numbers.
- "Frame drops / underruns over long playback" scale with clip length; the
  synthetic clips are short. Use longer real media to stress steady-state.
- Numbers are machine/GPU/driver-specific; compare runs on the same hardware.
  Do not publish them as universal latency promises (see `ARCHITECTURE.md` §24).
