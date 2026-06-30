# FastPlay v0.4.1

FastPlay `0.4.1` is a stability and playback-robustness release. It fixes an
intermittent crash/freeze when closing the window, and choppy audio on heavy
4K60 files that fall back to software video decoding. No charter changes:
`PlaybackSession` remains the single, concrete coordinator.

## Highlights

- Fixed an intermittent close crash/freeze caused by fragile D3D11 GPU teardown on exit.
- Fixed choppy audio on heavy 4K60 files when video falls back to software decoding.
- Audio playback is now protected from slow video decode paths using an independent audio decode worker.
- Improved shutdown reliability while preserving recent/resume state.
- Added env-gated audio diagnostics for underruns, queue depth, and WASAPI behavior.

## Fixes

### Close crash / freeze on exit (PR #16)

Releasing the D3D11 device, swap chain, and hardware-decode surfaces in-process
at exit could intermittently fault inside the graphics driver (an access
violation through a dangling vtable). The unhandled exception was handed to
Windows Error Reporting, which froze the still-visible window for several
seconds while it wrote a crash dump before the process died — experienced as a
"lag" when pressing the title-bar X. FastPlay now persists playback progress,
stops active work, flushes logs, and exits the process, letting the OS reclaim
GPU resources instantly and crash-free.

### Choppy audio during heavy 4K60 software decode (PR #17)

Some files cannot use D3D11VA/NVDEC hardware decode — for example true H.264
**Baseline** profile at 4K60, which the hardware rejects — and fall back to
software video decode that runs below realtime. With a single worker decoding
both streams sequentially, audio was produced only as fast as the slow video,
starving the audio sink and causing choppy/stuttering audio. Audio is now
decoded on an **independent worker with its own demuxer**, so it stays realtime
regardless of video decode speed. Under overload, video degrades first (late
frames are dropped) instead of starving audio.

## Technical notes

- Some high-resolution H.264 profile/format combinations may be rejected by
  D3D11VA/NVDEC and fall back to software decode. This is expected and correct
  (Architecture §20, Fallback A).
- FastPlay now keeps audio realtime even when video decode cannot sustain full
  framerate; both decode workers seek in place and stamp frames with
  generations for stale-work dropping.
- Audio diagnostics are available behind the `FASTPLAY_AUDIO_DIAG` environment
  variable (log-only, no UI): underrun count, decoded-audio queue depth, WASAPI
  padding/available frames, audio frames written per second, and video
  presented/dropped behavior.

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test` (140 passing)
- `cargo build --release`
- Manual playback smoke (1080p control + 2160×3840 60 fps ~80 Mbps 4K file):
  play, pause/resume, seek/scrub, close with X, reopen and confirm
  resume/recent, and confirm no new crash dumps or Application Error events.

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
