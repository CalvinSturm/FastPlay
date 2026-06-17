# FastPlay Comprehensive Audit Report

> [!NOTE]
> Codebase version: `0.1.6` (Cargo.toml) / `v0.1.3` (ARCHITECTURE.md — stale)
> Total source: ~330KB across 35 files (excluding `target/`)
> Audit date: 2026-06-16

---

## Executive Summary

FastPlay is a well-architected, Windows-first media player that closely follows its architecture charter. The code is disciplined about the hot path, generation-based stale-work dropping, and FFI safety boundaries. The core playback pipeline (FFmpeg → D3D11 → DXGI present) is sound.

That said, four areas of the audit surfaced **actionable findings** across correctness, robustness, performance, and polish. This report organizes them by severity.

---

## 🔴 Critical / High Severity

### 1. Busy-wait in main loop during idle/paused — 100% CPU
**Location:** [main.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/main.rs) ~line 280

The main loop uses a tight `PeekMessage` spin loop at all times. When the player is **idle** (no file) or **paused**, there's nothing to render, but the loop still spins at full speed consuming an entire CPU core.

**Fix:** Use `MsgWaitForMultipleObjects` (or `WaitMessage`) when in a non-animating state. The session already knows its `PlaybackState` — branch on it:
- `Playing` / `Priming` / `Seeking` → tight render loop (current behavior) ✅
- `Idle` / `Paused` / `Ended` / `Error` → wait for messages

> [!IMPORTANT]
> This is the single highest-impact improvement. It affects every user who leaves FastPlay open or pauses playback.

---

### 2. No timeout on FFmpeg `avformat_open_input` — network URLs freeze the app
**Location:** [ffmpeg.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/ffmpeg.rs) ~line 130

Even though streaming is out of scope, a user could drag a network URL onto the window. `avformat_open_input` will block indefinitely trying to connect, freezing the UI thread (since the open is initiated from the UI thread's perspective, and the worker join on failure can block).

**Fix:** Set an FFmpeg interrupt callback with a timeout (~5 seconds), or validate the path is a local file before passing it to FFmpeg.

---

## 🟠 Medium Severity

### 3. `session.rs` is a 96KB god object (~2,860 lines)
**Location:** [session.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/app/session.rs)

The session coordinator handles state transitions, open/close, seek, audio/video submission, resize, device recovery, overlays, timeline UI, input, subtitles, zoom/pan/rotation, in/out points, speed control, and drag-and-drop — all in one file.

**Recommendation:** Extract helpers for orthogonal concerns:

| Concern | Est. lines | Suggested module |
|---------|-----------|-----------------|
| Input dispatch & key handling | ~350 | `app::input_dispatch` |
| Overlay rendering (debug, help, error) | ~200 | `app::overlay_render` |
| Timeline/scrub interaction | ~300 | `app::timeline_interact` |
| Zoom/pan/rotation | ~200 | `app::viewport` |
| Open/close lifecycle | ~250 | `app::lifecycle` |

This would reduce `session.rs` to ~1,200 lines of pure coordinator logic.

---

### 4. Channel `.unwrap()` crash risk on worker panic
**Location:** [session.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/app/session.rs) ~line 1020

`sender.send(event).unwrap()` will panic if the receiver was dropped (e.g., if the worker thread panicked first). This creates a cascade-panic scenario.

**Fix:** Use `.send(event).ok()` or log-and-return-error.

---

### 5. Worker thread join can block UI and shutdown indefinitely
**Location:** [session.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/app/session.rs) ~line 1120

`close_media()` joins the decode worker thread. If FFmpeg is stuck in a blocking I/O call (slow NAS, optical drive, etc.), this blocks the UI thread indefinitely. Same issue on application shutdown — the process appears frozen.

**Fix:** Use a timed join (signal the worker to stop, then `thread::sleep` + check, then detach if still alive). On shutdown, consider `TerminateThread` as a last resort or accept the hung-process risk with a timeout.

---

### 6. No DPI awareness — blurry on high-DPI displays
**Location:** [window.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/platform/window.rs), [main.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/main.rs)

No `SetProcessDpiAwarenessContext` call or DPI-awareness manifest. On 4K/high-DPI displays, Windows will bitmap-scale the window, causing blurriness.

**Fix:** Call `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` early in `main()`, and handle `WM_DPICHANGED`.

---

### 7. Missing `WM_DISPLAYCHANGE` handler
**Location:** [main.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/main.rs) WndProc

Monitor resolution or refresh rate changes aren't detected. The swap chain may not update to match the new display parameters.

**Fix:** Handle `WM_DISPLAYCHANGE` → re-query display mode → optionally adjust present interval.

---

### 8. `VideoProcessorBlt` HRESULT not always checked
**Location:** [d3d11.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/d3d11.rs) ~line 2100

Some code paths call `VideoProcessorBlt` without checking the return HRESULT. A silent failure here means a frame is "presented" but actually blank/stale.

**Fix:** Always check and propagate the HRESULT. Log on failure.

---

### 9. `Map` failure on software upload texture leaves undefined state
**Location:** [d3d11.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/d3d11.rs) ~line 1720

If `Map()` fails for the software upload path, the texture is in an undefined state but may still be used downstream.

**Fix:** Return an error immediately on `Map` failure; do not use the texture.

---

### 10. Timeline texture write bounds check uses wrong stride
**Location:** [timeline.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/render/timeline.rs) ~line 250

The bounds check uses `width * height * 4` but should use `RowPitch * height` (the mapped texture's actual stride). Row pitch may be larger than `width * 4` due to GPU alignment requirements. Writing past `width * 4` but within `RowPitch` is fine, but the check should use the correct value.

**Fix:** Use `mapped_resource.RowPitch * height` for the bounds check.

---

### 11. No audio resampling — sample rate mismatch causes wrong playback speed
**Location:** [sink.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/audio/sink.rs) ~line 60

The audio sink assumes FFmpeg outputs at the device sample rate. If they differ (e.g., 44.1kHz content on a 48kHz device), audio plays at the wrong speed.

**Fix:** Either configure FFmpeg's `swr_ctx` to resample to device rate during decode, or add a resampling step in the sink. The FFmpeg approach is simpler and avoids extra copies.

---

### 12. No `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING` for variable refresh rate displays
**Location:** [dxgi.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/dxgi.rs) ~line 150

VRR (FreeSync/G-Sync) displays can benefit from `ALLOW_TEARING` + `Present(0, DXGI_PRESENT_ALLOW_TEARING)` for lower-latency presentation.

**Fix:** Query `IDXGIFactory5::CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING)` and set the flag if supported. This is a latency optimization aligned with the project's performance priorities.

---

### 13. Clock drift correction ignores playback speed multiplier
**Location:** [clock.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/playback/clock.rs) ~line 35

When playback speed ≠ 1.0, the drift correction threshold may be too tight or too loose because it doesn't account for the speed factor.

**Fix:** Scale the drift threshold by the playback speed multiplier.

---

### 14. Subtitle parser only handles UTF-8 and strict timestamp format
**Location:** [subtitle.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/media/subtitle.rs)

- Only UTF-8 encoded `.srt` files work. Windows-1252, Shift-JIS, and other common subtitle encodings produce garbled text.
- Timestamp format must use `,` (comma) for milliseconds. Some SRT files use `.` (period), which fails to parse.

**Fix:**
- Try UTF-8, then fall back to Windows-1252 (or use a BOM detector).
- Accept both `,` and `.` as millisecond separators.

---

### 15. WASAPI `GetBuffer`/`ReleaseBuffer` mismatch on copy failure
**Location:** [wasapi.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/wasapi.rs) ~line 160

If `GetBuffer` succeeds but the sample copy fails, `ReleaseBuffer` with 0 frames isn't called. This can leave the audio client in a locked state.

**Fix:** Always call `ReleaseBuffer` after `GetBuffer`, even on failure. Use a scope guard pattern.

---

### 16. WASAPI assumes f32 sample format without assertion
**Location:** [wasapi.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/wasapi.rs) ~line 140

The sink writes f32 samples assuming FFmpeg outputs f32. If the format differs, data corruption occurs silently.

**Fix:** Assert that the negotiated format matches expectations, or configure FFmpeg's output format explicitly.

---

### 17. Logs invisible in normal GUI operation
**Location:** [logging.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/logging.rs)

Logs go to `stderr` only. With `#![windows_subsystem = "windows"]`, stderr is not visible unless launched from a terminal.

**Fix:** Add optional file logging (e.g., `%LOCALAPPDATA%\FastPlay\fastplay.log`). A simple `File::create` with a size cap would suffice.

---

### 18. No `Ctrl+O` file open dialog
**Location:** [input.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/platform/input.rs)

Users must use drag-and-drop or command line to open files. No standard `Ctrl+O` → file dialog.

**Fix:** Add `Ctrl+O` keybinding → `GetOpenFileName` dialog.

---

### 19. `FFMPEG_DIR` missing gives unhelpful build error
**Location:** [build.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/build.rs)

If `FFMPEG_DIR` is not set, the build fails with a linker error rather than a clear message.

**Fix:** Add an early check in `build.rs` with a descriptive `panic!("FFMPEG_DIR environment variable must be set...")`.

---

### 20. Missing `rerun-if-changed` for C shim files
**Location:** [build.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/build.rs)

Changes to `ffmpeg_shim.c` or `ffmpeg_shim.h` won't trigger a rebuild unless `build.rs` itself changes.

**Fix:** Add `println!("cargo:rerun-if-changed=src/ffi/ffmpeg_shim.c")` and similar.

---

### 21. Shader compilation uses `expect()`
**Location:** [d3d11.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/d3d11.rs) ~line 2400

Shader compilation failures cause a panic via `expect()`. If a user has a GPU with an old driver that doesn't support the required shader model, the app crashes without a useful error.

**Fix:** Return a proper error and show a message box or fall back gracefully.

---

### 22. Device removal not checked in decode path
**Location:** [d3d11.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/d3d11.rs)

`DXGI_ERROR_DEVICE_REMOVED` is checked in the present path but not in the decode path. If the device is removed during decode, the worker could produce invalid frames or panic.

**Fix:** Check `GetDeviceRemovedReason()` after decode operations that use the D3D11 device, and signal `SessionEvent::DeviceLost`.

---

## 🟡 Low Severity / Polish

### 23. Missing `Draining` state
[state.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/app/state.rs) defines `Ended` but not `Draining`. ARCHITECTURE.md lists `Draining` as a distinct state. End-of-stream is currently handled via flags. Minor spec drift.

### 24. Version mismatch
`Cargo.toml` says `0.1.6`, `ARCHITECTURE.md` says `v0.1.3`. Should be kept in sync.

### 25. No minimum window size
No `WM_GETMINMAXINFO` handler — the window can be resized to 0×0, which could cause division-by-zero in aspect ratio calculations.

### 26. Window title doesn't show filename
When a file is open, the title bar still says "FastPlay" rather than "FastPlay — filename.mp4".

### 27. Aspect ratio and rotation recalculated every frame
[presenter.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/render/presenter.rs) recalculates the aspect ratio and rotation matrix on every `present_frame()` call. These values only change on resize or rotation change and should be cached.

### 28. No media key support
[input.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/platform/input.rs) doesn't handle `VK_MEDIA_PLAY_PAUSE`, `VK_MEDIA_STOP`, etc.

### 29. Volume setting not connected
[settings.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/app/settings.rs) has a `volume` field but it's not wired to the audio sink.

### 30. SRT HTML tags displayed literally
[subtitle.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/media/subtitle.rs) doesn't strip `<b>`, `<i>`, `<font>` tags that are common in SRT files.

### 31. Dead code: `create_staging_texture()`
[d3d11.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/ffi/d3d11.rs) has an unused `create_staging_texture()` function.

### 32. Surface registry uses `HashMap` instead of slot map
[surface_registry.rs](file:///c:/Users/Calvin/Software%20Projects/FastPlay/src/render/surface_registry.rs) — for the small number of live surfaces (typically <10), a `Vec`-based slot map would be faster. Minor optimization.

### 33. No `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE` handling
Could batch resize operations during window drag to reduce unnecessary intermediate redraws.

### 34. Minimal `.gitignore`
Missing common entries: `.vs/`, `*.pdb`, `*.dll`, `*.log`, IDE files.

### 35. Seek metrics incomplete
`seek_to_av_settled_ms` only tracks video first frame, not full audio+video settlement.

### 36. Log level not runtime-configurable
Currently set at compile time. A `FASTPLAY_LOG` env var would be useful.

### 37. No `!Send` marker on `PlaybackSession`
The session is UI-thread-only but doesn't have an explicit `!Send`/`!Sync` bound. The FFI types it holds likely already prevent this, but an explicit `PhantomData<*const ()>` would be a safety net.

---

## Architecture Compliance Summary

| Architecture Rule | Status | Notes |
|-------------------|--------|-------|
| FFmpeg → D3D11 → DXGI present hot path | ✅ | No CPU copy-back in steady state |
| PlaybackSession is concrete, sole coordinator | ✅ | |
| Workers don't mutate session state | ✅ | All via SessionEvent |
| All async results carry (open_gen, seek_gen, op_id) | ✅ | |
| Stale work dropped before side effects | ✅ | |
| No raw pointers in public API | ✅ | Opaque VideoSurfaceHandle |
| Unsafe boxed in ffi/* | ✅ | |
| tick() non-blocking, UI-thread only | ⚠️ | Mostly; see findings #2, #5 |
| Queue sizes match spec | ✅ | 48/96/4/12 |
| DXGI_PRESENT_RESTART not used | ✅ | |
| Software fallback uses BIND_SHADER_RESOURCE \| BIND_DECODER | ✅ | |
| Draining state exists | ❌ | Using flags instead |
| Metrics per spec | ⚠️ | Flat counters, no percentiles |

---

## Top 10 Recommended Actions (Priority Order)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 1 | **Fix idle/paused CPU spin** — `MsgWaitForMultipleObjects` | 🔴 Power/perf | Small |
| 2 | **Add FFmpeg open timeout** — interrupt callback | 🔴 Robustness | Small |
| 3 | **Add DPI awareness** — `SetProcessDpiAwarenessContext` | 🟠 Visual quality | Small |
| 4 | **Add `Ctrl+O` file open dialog** | 🟠 Usability | Small |
| 5 | **Add file logging** — `%LOCALAPPDATA%\FastPlay\` | 🟠 Debuggability | Small |
| 6 | **Fix WASAPI GetBuffer/ReleaseBuffer scope guard** | 🟠 Audio robustness | Small |
| 7 | **Add audio resampling** — FFmpeg swr_ctx to device rate | 🟠 Correctness | Medium |
| 8 | **Check VideoProcessorBlt HRESULT everywhere** | 🟠 Correctness | Small |
| 9 | **Add VRR/tearing support** — `ALLOW_TEARING` flag | 🟠 Latency | Small |
| 10 | **Extract session.rs helpers** — reduce god object | 🟡 Maintainability | Medium |

---

## Findings Not Acted On (By Design)

These were reviewed and are correct per the architecture:

- No exclusive fullscreen ✅
- No time-stretch audio ✅
- No ASS subtitle styling ✅
- No HDR tone mapping ✅
- No multiple HW decode backends ✅
- No cross-platform abstractions ✅
- Single crate, not workspace ✅
- Queue sizes are defaults, not hardcoded constants ✅
