# FastPlay v0.1.5

FastPlay `0.1.5` focuses on scrub UX, hot-path performance, and crash diagnostics.

## Highlights

- the timeline scrub overlay is now shown **while** actively scrubbing, with a live seek preview
- view rotation is preserved across scrub seeks and hardware→software decode fallback
- tracing moved to an in-memory ring buffer, removing per-line disk writes from the seek/UI hot path while still capturing the trace leading up to a crash
- steady-state playback efficiency improvements (fewer per-tick audio-device queries, leaner audio buffering)
- added GitHub Actions CI, timeline-math tests, and frame-drop diagnostics

## Fixes and Improvements

### Timeline and Scrub UX

- the timeline overlay now stays visible during active scrub-drag and shows the pending seek target, instead of being hidden until scrubbing ends (changed from `0.1.4`)
- view rotation is no longer reset by scrub seeks or by a mid-stream hardware→software decode fallback — the displayed orientation is preserved

### Performance and Logging

- replaced the unbuffered stderr→`session.log` redirect with an in-memory ring buffer; tracing no longer costs a file-write syscall per line on the seek/UI hot path
- the ring is flushed to `session.log` on normal exit, on panic, and from the vectored crash handler, so the pre-crash trace survives a hard D3D11 fault
- the WASAPI buffered-frame count is queried once per tick and reused for the master clock and end-of-playback checks, instead of re-querying several times per tick
- audio volume scaling is done in a single pass (dropping a redundant buffer copy), and the audio batch buffer is pre-sized to avoid per-batch reallocation

### Stability and Internals

- tightened playback queue bounds and worker completion sends to reduce stale work
- aligned runtime module boundaries and ownership with the architecture charter
- added frame-drop cause statistics (queue overflow / surface mismatch / scheduler-late) to the end-of-playback summary
- added a GitHub Actions CI workflow and unit tests covering timeline math

## Upgrade Notes

- existing MSI installs upgrade in place through the WiX `MajorUpgrade` path
- the timeline overlay now appears during scrubbing; in `0.1.4` it was intentionally hidden while the mouse was actively scrubbing
- session diagnostics in `%APPDATA%\FastPlay\session.log` are now written on exit/panic/crash rather than continuously during the session
