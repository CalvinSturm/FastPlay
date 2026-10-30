# FastPlay benchmark harness

A local, repeatable harness for the latency users actually feel: how long
until the first frame after opening a file, and after a seek. It implements
`docs/ROADMAP.md` §2 and reports `ARCHITECTURE.md` §24 metrics by slice.

It drives the real release build and reads back the metrics the app already
logs. Nothing is timed from outside the process, so harness scheduling jitter
does not leak into the numbers. The only app-side support is two automation
messages, which cost nothing unless posted, and three one-shot log lines (see
[App-side hooks](#app-side-hooks)).

This is a **local/optional tool**. It is not part of `cargo build` or CI: it
needs a GUI session and a media corpus, and it takes tens of minutes.

## What it measures

| Metric | Measured from → to | Log line |
|--------|--------------------|----------|
| open-to-first-frame | open request → first frame presented | `open_to_first_frame_ms` |
| open-to-first-audio | open request → audio started | `open_to_first_audio_ms` |
| launch-to-first-frame | process entry → first frame (adds window, D3D11 and WASAPI init) | `startup_to_open_ms` + `open_to_first_frame_ms` |
| seek-to-first-frame | seek request → first frame *of the new position* presented | `seek_to_first_frame_ms` |
| seek-to-A/V-settled | seek request → that frame presented **and** audio re-anchored | `seek_to_av_settled_ms` |
| pause / resume | request → sink stopped / first moving frame | `pause_to_stop_ms` / `play_to_motion_ms` |
| drops, underruns, HW fallbacks (`-Playthrough`) | one clean pass from 0 to EOF | `playback_summary` |

Open latency is split by kind:

- **`launch_first`**: the first launch of a clip × variant in this invocation.
  This is the closest the harness gets to a cold open. The OS file cache may
  still be warm from an earlier invocation (see Limitations).
- **`launch_repeat`**: later launches. Each one is a new process, but the file
  is cached.
- **`reopen`**: an in-process reopen of the same file through the normal open
  path (warm process, device and swap chain already up). This is the path
  that Next/Previous, drag-and-drop, and Recent take.

Each seek is also tagged with **`keyframe_distance_ms`**: how far past its
keyframe the target lands, which is the decode-forward work the seek has to
pay for. That distance is what drives seek latency.

Every event gets a status. A missing sample is a result, not noise:

| Status | Meaning |
|--------|---------|
| `ok` | metric logged |
| `superseded` / `superseded_before_settle` | the next seek was posted before this one landed. Its latency exceeded `-SeekGapMs`. |
| `no_first_frame` / `not_settled` / `missing` | never logged before close |
| `resumed` | an open resumed from a saved position (should not happen under the isolated profile) |
| `interfered` | physical input reached the benchmark window (see below). The whole run is excluded from the stats. |

## Requirements

- A **release** build: `cargo build --release`. The debug build is a
  console-subsystem app, and the harness's window lookup would miss it.
- `ffmpeg` on `PATH` to generate the synthetic corpus, and `ffprobe` for media
  info and keyframe positions (required).
- Windows with a desktop session. **Do not touch the mouse over the player
  while it runs.** Each run launches a foreground window, and Windows sends
  mouse-wheel input to whatever window is under the cursor. The harness
  detects stray input (volume changes, extra seeks) and marks the run
  `interfered`.

## Usage

```powershell
cargo build --release
pwsh -File bench/gen-corpus.ps1                 # synthetic corpus -> bench/corpus (git-ignored)
pwsh -File bench/run-bench.ps1 -Label baseline  # -> bench/results/<stamp>-baseline/
```

Add real files or folders alongside the synthetic corpus:

```powershell
pwsh -File bench/run-bench.ps1 -CorpusDir bench\corpus,D:\media\clip.mkv -Iterations 10
```

### Synthetic corpus (`gen-corpus.ps1`)

All clips are 40 s at 30 fps with a fixed 4 s GOP and no scene-cut keyframes,
so seek cost is identical across codecs and across runs. The content is
`testsrc2` plus temporal noise, which keeps bitrates realistic. Audio is AAC,
48 kHz stereo.

| File | Codec | Res | Range | Bitrate |
|------|-------|-----|-------|---------|
| `h264_1080p30_sdr.mp4` | H.264 High | 1080p | SDR | 12 Mb/s |
| `h264_2160p30_sdr.mp4` | H.264 High | 2160p | SDR | 40 Mb/s |
| `hevc_1080p30_sdr.mp4` | HEVC Main | 1080p | SDR | 8 Mb/s |
| `hevc_2160p30_pq.mp4` | HEVC Main10 | 2160p | HDR10 PQ | 30 Mb/s |
| `hevc_1080p30_hlg.mp4` | HEVC Main10 | 1080p | HLG | 10 Mb/s |

The software path is covered by the `sw` variant, which re-runs each clip with
`--force-sw`.

### Parameters

| Param | Default | Notes |
|-------|---------|-------|
| `-Exe` | `target/release/fastplay.exe` | Player under test |
| `-CorpusDir` | `bench/corpus` | Folders and/or files |
| `-OutDir` | `bench/results` | Git-ignored |
| `-Label` | none | Appended to the result folder name |
| `-Iterations` | `5` | Outer loop: machine drift spreads across all configs |
| `-Variants` | `hw,sw` | `sw` adds `--force-sw` |
| `-SeekFractions` | `0.53,0.12,0.895,0.30,0.74` | Absolute seek targets as fractions of duration. These give 1.2 / 0.8 / 3.8 / 0.0 / 1.6 s past a keyframe on the corpus, mixing forward and backward jumps. |
| `-WarmReopens` | `2` | In-process reopens per run |
| `-OpenWaitMs` | `2500` | Wait after each open |
| `-SeekGapMs` | `3000` | Wait after each seek. Slower seeks show up as `superseded`. The software path on 4K needs about 5000. |
| `-Playthrough` | off | Adds one full pass per run, for the drop/underrun/fallback counters |

## Output

Each invocation writes `bench/results/<stamp>[-<label>]/`:

- `events.csv`: one row per open, reopen, seek, pause and resume, with media
  slice columns (codec, resolution, bit depth, dynamic range, bitrate) plus
  decode mode, presentation path, keyframe distance and status.
- `runs.csv`: one row per launch. It holds `startup_to_open_ms`,
  `launch_to_first_frame_ms`, `display_hdr_active`, the playthrough counters,
  any errors or interference, and the log path.
- `summary.csv`: n, ok, failed, median, p95 and min/max per clip × variant ×
  metric. Medians are true medians. p95 is nearest-rank and is left blank
  below 10 samples, where it would just be the max.
- `report.json`: everything above, plus full media info, the environment (OS,
  CPU, GPU, driver, display mode and refresh rate, git commit and dirty flag,
  exe SHA-256) and the exact config, so a run can be reproduced and diffed.
- `runs/<id>/FastPlay/session-*.log`: the raw session logs, kept so a result
  can be re-parsed later.

## How it works

Each run launches the player with `%APPDATA%` pointed at a fresh per-run
folder. That rules out resume-at-saved-position, keeps the user's Recent
list untouched, and gives every run a private log directory. The player's log
ring flushes only on graceful exit, so the harness cannot watch events live.
Instead it follows a fixed schedule:

1. launch → wait `-OpenWaitMs`
2. `-WarmReopens` × (`WM_APP+4` reopen → wait)
3. for each seek fraction: `WM_APP+3` with the absolute target → wait `-SeekGapMs`
4. pause, then resume (space)
5. optional playthrough: seek to 0, reopen, then wait for the clip duration + 3 s
6. `WM_CLOSE`

Then it parses the flushed log **in order**. Each `[open]` or `[execute_seek]`
line starts a new event, and the metric lines that follow belong to it. That
is how a slow seek gets flagged `superseded` instead of quietly dropping out
of the sample set.

### App-side hooks

| Hook | Purpose |
|------|---------|
| `WM_APP+3` (`WM_APP_SEEK_TO_MS`, wparam = ms) | Absolute seek through `SessionCommand::Seek`, the path the arrow keys use. The keyboard only offers relative seeks from a moving playhead, which cannot hit repeatable targets. |
| `WM_APP+4` (`WM_APP_REOPEN_CURRENT`) | In-process reopen through `open_single_file`. It keeps the decode preference, so `--force-sw` survives the reopen. |
| `[open] open=N start_ms=… decode_preference=…` | Per-open marker used for attribution |
| `startup_to_open_ms` | Process entry → first open request |

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
| `verify-frameless-persistence.ps1` | Frameless mode persists across new processes in both directions without touching the user's real `%APPDATA%` |

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

- **Cold open is approximate.** `launch_first` is a new process, but the OS
  file cache is only cold if the file has not been read recently. Clearing the
  standby list needs admin tools (e.g. RAMMap), which the harness does not use.
- **Launch-to-first-frame misses loader time**: the time from `CreateProcess`
  to `main` (DLL loading) is not counted.
- **The fixed schedule censors slow events.** A seek slower than `-SeekGapMs`
  is recorded as `superseded`, not timed. Check the `failed` column before
  reading a median.
- **`pause_to_stop_ms` / `play_to_motion_ms` are coordinator-side**: both log
  ~0 ms because they time the state change, not the audible or visible
  result.
- **Synthetic content is not real content.** Noise plus a test pattern gives
  realistic bitrates but not realistic motion or entropy. A fixed 4 s GOP is a
  middle-of-the-road choice: camera files are often 0.5–1 s, and some
  encodes run 10 s or more. Add real files through `-CorpusDir`.
- Every run uses the default 1280×720 window. A 4K clip is downscaled at
  present, and fullscreen present cost is not covered.
- Numbers are specific to one machine, GPU and driver. Compare runs only on
  the same hardware, and do not publish them as universal latency promises
  (`ARCHITECTURE.md` §24).
