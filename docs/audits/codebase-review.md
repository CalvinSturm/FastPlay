# Codebase review — oversized files, maintainability, correctness, refactoring risk

**Date:** 2026-07-21
**Scope:** the whole crate — architecture, execution paths, largest files, correctness, concurrency, resource lifetime, testability, and a staged refactoring plan.
**Repository state audited:** `main` @ `6e4d920` ("Replace the app icon with the glyph on transparency"), clean working tree.
**Changes made during this review:** three confirmed items were fixed and two regression tests added (see §7 and §12). Everything else is analysis only.

---

## 1. Executive summary

**The premise of the review request does not hold, and that changes the recommendation.**
The task was framed around files of 5,000–10,000+ lines. There are none. The crate is
**21,283 lines across 52 Rust files** (pre-fix), and the largest single file is
`src/ffi/d3d11.rs` at **4,594 lines**. Four files exceed 2,000 lines; three of them are the
designated unsafe FFI seams that `AGENTS.md` explicitly requires to be large and boxed, and
the fourth is the single concrete coordinator that `ARCHITECTURE.md §29` requires to stay
concrete.

More importantly, **this repository has already done the refactor the request anticipates.**
`docs/ROADMAP.md §1` and `docs/TECH_DEBT.md §2` record a completed v0.3.0 extraction of
`PlaybackSession` into seven owned, individually tested helpers; CI enforces `cargo fmt
--check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets` on
`windows-latest`; and 199 unit tests passed at baseline. Splitting further on line count
would be work against the grain of a charter that is deliberately locked.

The real findings are narrower and more useful than "these files are too big":

- **One High-severity confirmed defect** that permanently kills video for the rest of a file,
  reachable by ordinary scrubbing on any file that falls back to software decode. The
  identical bug was found and fixed for the *audio* worker in `b603f6f`; the fix was never
  applied to the video worker, and `docs/TECH_DEBT.md` does not track it.
- **One Medium confirmed defect** in keyboard handling (holding Ctrl+S toggles subtitles).
- **One genuine misplaced responsibility**: the entire input keymap — pure, total, trivially
  testable policy — lives inside `window_proc` in the unsafe FFI seam, which is precisely why
  the keyboard defect existed and was never caught by the 199-test suite.
- **Documentation drift** in `docs/TECH_DEBT.md`, which asserts a clean lint baseline that the
  code contradicts (12 crate-wide clippy allows, 7 module-wide `dead_code` allows).

**Verdict: the codebase is safe to continue developing on, and was safe before this review.**
It is well-factored, densely commented with genuine rationale (not noise), consistently
error-typed, and disciplined about ownership and threading. It does not need restructuring.
It needed three small fixes and one honest correction to its own debt register.

**Finding counts:** Critical **0** · High **1** · Medium **5** · Low **9** (L9 was found later, while implementing Stage 5).

---

## 2. Repository architecture overview

**Language / build:** single Rust crate (`fastplay` v0.4.4, edition 2021), Windows-only.
`build.rs` discovers FFmpeg (`FFMPEG_DIR`, explicit `FFMPEG_INCLUDE_DIR`/`FFMPEG_LIB_DIR`, or
a vcpkg root), compiles a small C shim (`src/ffi/ffmpeg_shim.c`), generates bindings with
`bindgen`, stages the FFmpeg DLLs beside the binary, and embeds the icon resource. Entry point
`src/main.rs`; `windows_subsystem = "windows"` in release only, so debug builds keep a console.

**Governing documents.** `ARCHITECTURE.md` is a locked charter; `AGENTS.md` forbids revising
it, mandates a single crate, requires unsafe code to stay boxed in `src/ffi/*`, requires
`PlaybackSession` to remain a concrete type, and forbids workers mutating session state
directly. `docs/ROADMAP.md` and `docs/TECH_DEBT.md` track sequencing and debt. This review
treats all four as binding constraints on any recommendation.

**Module boundaries** (`src/`):

| Module | Role |
|---|---|
| `app/` | Coordinator (`session.rs`) plus its owned helpers: viewport, clip range, overlays, audio control, video queue, input dispatch, decode-thread lifecycle, play queue, recent files |
| `playback/` | Clock, metrics, bounded-queue defaults, generations/op-ids, decode control channel, audio diagnostics |
| `media/` | Source, video/audio frame types, seek targets, sidecar subtitles |
| `render/` | Presenter, swapchain, surface registry, timeline model, HDR decision logic |
| `audio/` | WASAPI sink |
| `platform/` | Window facade, `InputEvent` enum, file dialog |
| `ffi/` | The four unsafe seams: `d3d11`, `dxgi`, `ffmpeg`, `wasapi`, plus `runtime` |

**Main execution path.** `main::run` sets DPI awareness, raises the multimedia timer to 1 ms,
creates the window and `PlaybackSession`, then loops: `pump_messages` → drain
`InputEvent`s → `timeline_ui.update` → `session.tick(now)` → poll the ended signal for
play-queue auto-advance → pace (block on the message queue when quiescent, else sleep 1 ms).

`tick(now)` is the only coordinator entrypoint. It asserts UI-thread affinity and
non-reentrancy (`session.rs:668-682`), then: drain audio events → drain video events (each
gated on its *own* queue so a full video queue cannot starve audio) → handle resize → submit
due audio to WASAPI → advance video against the master clock → update overlays → present →
record metrics → test for end of playback.

**Threading.** Three threads. The UI thread owns everything. Two persistent decode workers —
video (`spawn_decode_thread`) and an independent audio-only worker (`spawn_audio_thread`, with
its own demuxer so audio is never gated behind sub-realtime video). Workers communicate only
through two bounded `SyncSender<SessionEvent>` channels; a `DecodeControl` (mutex + condvar +
atomic sequence) carries seek/shutdown commands the other way with latest-command-wins
coalescing. Stale work is rejected by `(OpenGeneration, SeekGeneration, OperationId)` triples
via `is_current_frame`.

**Clock.** WASAPI shared-mode audio is the master clock; video is scheduled against it with
smoothing to cope with the ~10 ms staircase of `GetCurrentPadding`. A `PlaybackClock` takes
over when audio is unanchored or underruns.

**Video path.** FFmpeg demux → D3D11VA hardware decode → `AV_PIX_FMT_D3D11` → opaque
`VideoSurfaceHandle` through a surface registry → DXGI flip-model present. Software decode is
an explicit fallback (sws_scale to NV12 → `CreateTexture2D` upload). HDR (PQ/HLG) presents
natively on a 10-bit PQ swapchain when the display is HDR-active, and tone-maps to SDR in the
same pixel shader otherwise.

---

## 3. Validation commands and results

Toolchain: `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0`. Windows 11.

### Baseline (before any change, `main` @ `6e4d920`)

| Command | Result |
|---|---|
| `cargo fmt --check` | **clean** (no output) |
| `cargo clippy --all-targets -- -D warnings` | **clean** — `Finished dev profile in 12.63s`, zero warnings |
| `cargo test --all-targets` | **199 passed; 0 failed; 0 ignored** |

### After the fixes in §7

| Command | Result |
|---|---|
| `cargo fmt --check` | **clean** |
| `cargo clippy --all-targets -- -D warnings` | **clean** — `Finished dev profile in 1.41s` |
| `cargo test --all-targets` | **201 passed; 0 failed; 0 ignored** (+2 new regression tests) |
| `cargo build --release` | **clean** — `Finished release profile in 4.76s` |

No type checker or static analyser beyond `rustc`/`clippy` applies; there is no separate
linter or formatter config to run. `bench/run-bench.ps1` exists but is deliberately not wired
into CI (`ROADMAP.md §2`) and needs a generated local corpus; it was not run.

**Note on the lint baseline (as audited):** `clippy` was clean, but `src/main.rs:6-17`
disabled 12 lint categories crate-wide before clippy ever ran, and 7 modules disabled
`dead_code` wholesale — so a clean run was a weaker signal than `docs/TECH_DEBT.md §3`
claimed. Both were paid down in Stages 5 and M4: one crate-wide allow remains
(`too_many_arguments`), the rest are scoped to the module that needs them or were fixed
outright, and no blanket `dead_code` allow survives. A clean clippy run now means what it
says everywhere outside the FFI seams.

---

## 4. Largest-file inventory

Line counts at the audited commit. Generated files (`OUT_DIR` bindings), `target/`, `bench/`
corpus and results, and `validation/` logs are excluded.

| Rank | File | Lines | Code / test split | Why it is large | Size justified? |
|---:|---|---:|---|---|---|
| 1 | `src/ffi/d3d11.rs` | 4,594 | 4,295 / 299 | Device+decoder+renderer FFI seam, ~950 lines of inline HLSL shader source, **~1,240 lines of CPU 2D rasterizer** | **Partly.** The FFI and shaders belong; the rasterizer does not (§6.2) |
| 2 | `src/app/session.rs` | 2,892 | 2,854 / 38 | The single concrete coordinator: open/close, seek, generations, event application, present scheduling, two worker bodies | **Yes** — mandated concrete by charter |
| 3 | `src/ffi/dxgi.rs` | 2,295 | 2,295 / 0 | Window creation, swapchain, HDR caps, OLE drop target, **`window_proc` at 626 lines** | **Partly.** The Win32 plumbing belongs; the keymap does not (§6.1) |
| 4 | `src/ffi/ffmpeg.rs` | 2,190 | 2,114 / 76 | Demux + HW/SW decode + audio resample + HDR side-data, two session types | **Yes** — designated unsafe seam |
| 5 | `src/render/hdr.rs` | 1,516 | 736 / 780 | Pure HDR classification and path decision | **Yes** — **41 tests**, the best-covered file in the repo |
| 6 | `src/main.rs` | 877 | 767 / 110 | Event loop, play-queue/recent orchestration, CLI parsing | Yes |
| 7 | `src/render/presenter.rs` | 480 | — | Surface registry + overlay compositing facade | Yes |
| 8 | `src/app/play_queue.rs` | 465 | — | Queue construction and cursor discipline | Yes |

### Largest functions

| Function | File:line | Lines | Assessment |
|---|---|---:|---|
| `window_proc` | `ffi/dxgi.rs:1652` | **626** | **Hotspot** — three unrelated responsibilities (§6.1) |
| `spawn_decode_thread` | `app/session.rs:1430` | 368 | Acceptable — a worker body is one coherent unit; ~90 lines are load-bearing comments |
| `handle_event` | `app/session.rs:936` | 291 | Acceptable — flat dispatch over 13 event variants, uniform shape |
| `render_video_surface` | `ffi/d3d11.rs:906` | 265 | Acceptable — one D3D11 draw sequence, not decomposable without churn |
| `upload_nv12_surface_contiguous` | `ffi/d3d11.rs:2341` | 250 | Acceptable |
| `run_to_eof` (video) | `ffi/ffmpeg.rs:513` | 230 | Acceptable — decode loop plus mid-stream HW→SW fallback |
| `render_recent_bitmap` | `ffi/d3d11.rs:3656` | 230 | **Hotspot** — CPU rasterizer in the FFI seam (§6.2) |
| `tick_inner` | `app/session.rs:733` | 203 | Acceptable — the documented canonical tick order |
| `render_help_bitmap` | `ffi/d3d11.rs:3460` | 196 | **Hotspot** (§6.2) |
| `spawn_audio_thread` | `app/session.rs:1798` | 175 | Acceptable |

### Most-depended-upon modules

`app/session.rs` imports from every other module and is imported only by `main.rs` — the
intended shape for a coordinator. `ffi/d3d11.rs::D3D11Device` is the widest fan-in
(`ffmpeg.rs`, `presenter.rs`, `swapchain.rs`, `hdr_validate.rs`). `render/hdr.rs` is imported
by `ffi/ffmpeg.rs`, `ffi/dxgi.rs`, `render/presenter.rs`, `render/swapchain.rs`, and
`app/session.rs` — a pure module with wide fan-in, which is the healthy direction. No
circular module dependencies exist.

---

## 5. Prioritized findings

Labels: **CD** confirmed defect · **HR** high-confidence risk · **MI** maintainability issue ·
**TG** testing gap · **OI** optional improvement.

| # | Pri | Label | Finding | Location |
|---|---|---|---|---|
| H1 | **High** | CD | A seek arriving during a decode-worker reopen kills video for the rest of the file | `app/session.rs:1591`, `app/decode_thread.rs:106` |
| M1 | Medium | CD | Holding Ctrl+S toggles subtitles on every key repeat | `ffi/dxgi.rs:1826` |
| M2 | Medium | MI | The entire input keymap lives inside `window_proc` in the unsafe FFI seam | `ffi/dxgi.rs:1652-2278` |
| M3 | Medium | MI | ~1,240 lines of CPU 2D rasterizer live inside the D3D11 FFI seam | `ffi/d3d11.rs:3058-4294` |
| ~~M4~~ | Medium | MI | ~~`TECH_DEBT.md` asserts "no baseline allow-list"; 12 crate allows + 7 module `dead_code` allows~~ — **DONE**: 12 crate-wide became 1, the rest scoped or fixed; all 7 `dead_code` allows gone | `docs/TECH_DEBT.md §3`, `main.rs` |
| M5 | Medium | TG | Neither `window_proc` nor the rasterizer has a single direct test; both are untestable in place | `ffi/dxgi.rs`, `ffi/d3d11.rs` |
| L1 | Low | HR | `AVFrame` not unreferenced on two of four error paths in `receive_video_frames` | `ffi/ffmpeg.rs:1528`, `:1543` |
| L2 | Low | HR | Two latent `clamp` panics in timeline rendering, unreachable at the enforced minimum window size | `ffi/d3d11.rs:3276`, `:3289`, `:3305`; `render/timeline.rs:61` |
| L3 | Low | HR | A transient audio open error silences audio for the rest of the file | `app/session.rs:1902-1907` |
| L4 | Low | MI | Duplicated worker plumbing: `worker_send` (~35 lines) verbatim ×2; device-lost mapping ×3; SAR guard ×6 | `app/session.rs`, `ffi/ffmpeg.rs` |
| L5 | Low | MI | `upload_nv12_surface_contiguous` does not validate `data.len()` against `stride × height` | `ffi/d3d11.rs:2341` |
| L6 | Low | MI | Redundant double-check on the audio control channel | `app/session.rs:1311-1314` |
| L7 | Low | OI | `recent.rs` persists via non-atomic truncate-then-write | `app/recent.rs:125` |
| L8 | Low | MI | Stale doc listings: `TECH_DEBT.md` file table, `ARCHITECTURE.md §5` repo shape | both |
| L9 | Low | MI | `SessionEvent::AudioEndpointChanged` is charter-specified but never constructed; endpoint recovery is reactive-only | `app/events.rs:86`, `ARCHITECTURE.md:268` |

---

## 6. Detailed hotspot reviews

### 6.1 `window_proc` — `src/ffi/dxgi.rs:1652-2278` (626 lines)

**Current responsibilities — three, unrelated:**

1. **Win32 message plumbing** (correct and belongs here): `WM_NCCREATE` user-data install,
   `WM_SIZE` → `ResizeRequest`, `WM_CLOSE` deliberately *not* destroying the HWND so the swap
   chain is released first, `WM_DESTROY` reclaiming the `Rc<WindowState>`, `WM_GETMINMAXINFO`
   enforcing a 640×360 client minimum, `WM_DPICHANGED`.
2. **Modal-loop keep-alive** (belongs here, and is genuinely clever): `WM_ENTERSIZEMOVE`,
   `WM_SYSCOMMAND`, `WM_ENTERMENULOOP`, `WM_NCRBUTTONUP` each arm a `SetTimer` so playback
   keeps ticking inside `DefWindowProcW`'s blocking loops; `WM_NCLBUTTONDOWN` reimplements
   caption drag-detection with `SetCapture` specifically to avoid `DefWindowProcW`'s blocking
   `DragDetect` loop that would freeze playback.
3. **The input keymap** (does **not** belong here): ~35 arms mapping VK code + `GetKeyState`
   modifiers + the lparam bit-30 repeat flag to `InputEvent`. This is pure, total, and has no
   Win32 dependency beyond the three inputs.

**Why it became large:** the keymap grew arm by arm alongside features (in/out points,
playback rate, play queue, recent overlay, help). Each addition was individually reasonable;
nobody paid the extraction cost, and the file was already "the Win32 file".

**Concrete risk this introduces:** the keymap is a linear `match` with guard clauses, and
guard-clause fallthrough is silent. `0x53 if ctrl_held && !repeat` followed later by a bare
`0x53` means a held Ctrl+S falls through — that is **M1**, and it shipped. There is no test
that could have caught it, because reaching the code requires an `HWND` and live
`GetKeyState`. `src/platform/input.rs` is 54 lines containing only the `InputEvent` enum: the
right home exists and is empty.

**What should stay together:** all of (1) and (2). The modal-tick machinery in particular is
subtle, correct, and heavily commented — do not disturb it.

**What can be separated safely:** the keymap, as a free function
`command_for_key(vk: u32, ctrl: bool, shift: bool, is_repeat: bool) -> Option<InputEvent>` in
`platform/input.rs`. `window_proc`'s `WM_KEYDOWN`/`WM_KEYUP` arms reduce to reading modifier
state, calling it, and pushing the result. `AGENTS.md` requires *unsafe* code to be boxed in
`ffi/*`; it does not require *policy* to live there, and this move reduces the unsafe surface
rather than growing it.

### 6.2 The CPU rasterizer in `src/ffi/d3d11.rs:3058-4294` (~1,240 lines)

**Current responsibilities:** produce overlay bitmaps as
`SubtitleBitmap { width, height, pixels: Vec<u8> }` for subtitles, the timeline, the idle
prompt, the help sheet, the recent-files list, and the volume indicator. Helpers:
`draw_timeline_label`, `fill_rect`, `fill_circle_aa`, `fill_rounded_rect`, `blend_pixel`,
`truncate_chars`.

**Why it became large:** each overlay was added where the texture-upload code already lived.
`SubtitleBitmap` is a private type in this file, so the natural gravity was inward.

**Is its size justified? Partly, and the distinction matters.** The `render_*_bitmap`
functions *do* use GDI (`CreateCompatibleDC`, `CreateFontW`, `GetTextExtentPoint32W`) for text
measurement and glyph rasterization, so they are genuinely FFI-adjacent. But the geometry and
compositing layer beneath them — `fill_rect`, `fill_circle_aa`, `fill_rounded_rect`,
`blend_pixel`, and the whole of `render_timeline_bitmap` apart from its two label calls — is
**pure safe Rust operating on a `&mut [u8]`**, with no COM, no HANDLE, and no unsafe. Roughly
a quarter of the largest file in the repo is not FFI at all, and it has **zero direct tests**.

**Concrete risks this introduces:**

- Untestable pixel logic. `blend_pixel` implements source-over compositing with integer
  rounding; `fill_rounded_rect` computes per-pixel coverage across five geometric cases. Both
  are exactly the kind of code that benefits from unit tests, and neither has one.
- Two latent panics (**L2**). `d3d11.rs:3276` and `:3289` call
  `x.clamp(0, width as i32 - 2)`, and `std::clamp` panics when `min > max` — so any viewport
  width below 2 panics. `d3d11.rs:3305` and `render/timeline.rs:61` call
  `clamp(layout.track_left, layout.track_right)`, which inverts below width 32.
  **Both are currently unreachable**: `MIN_CLIENT_WIDTH = 640` (`dxgi.rs:98`) is enforced
  through `WM_GETMINMAXINFO`. They are recorded as latent, not as bugs, and are deliberately
  **not** being fixed defensively — adding clamps would mask a real invariant violation if the
  minimum-size guarantee ever broke. The right fix, if the layer moves, is a debug assertion.
  **Resolved that way in Stage 3:** `render::overlay_raster::MIN_TIMELINE_WIDTH_PX` (32px) now
  carries a `debug_assert!` explaining why the enforced 640px minimum makes it unreachable, so
  a future change that breaks the guarantee fails loudly in debug rather than rendering a
  degenerate overlay.

**What should stay together:** the GDI text path and the D3D11 texture upload.

**What can be separated safely:** the pure geometry/blend layer, into
`src/render/overlay_raster.rs`, with `SubtitleBitmap` (or an equivalent) made `pub(crate)`.

### 6.3 `src/app/session.rs` (2,892 lines) — large, and correctly so

`PlaybackSession` holds 45 fields and orchestrates open/close, seek, generation gating, event
application, clock ownership, present scheduling, and the two worker bodies. That is wide by
any conventional measure, and it is **the architecture working as specified**:
`ARCHITECTURE.md §6` assigns exactly these responsibilities to the concrete coordinator, and
§29 forbids a second one. The v0.3.0 refactor already extracted everything separable —
viewport, clip range, overlays, audio control, video queue, input dispatch, decode-thread
lifecycle — into owned, tested helpers.

Three observations worth recording:

- **State-machine density.** `PlaybackState` has 8 variants and transitions are written
  inline at ~20 sites. The transitions are individually well-commented and correct as far as
  this review could trace them, but there is no single place to read the machine. That is a
  real reviewability cost, and the honest answer is that centralizing it would be a large,
  risky change to the most timing-sensitive code in the program. **Do not attempt it.**
- **Comment quality is unusually high.** The comments explain *why* — WASAPI padding
  staircases, DXGI teardown ordering, why the video and audio event channels are separate,
  why `DXGI_PRESENT_RESTART` is avoided, why the process hard-exits rather than releasing D3D
  objects. This is the main reason a file this size remains reviewable.
- **`tick_inner` has one early return** (`session.rs:809-817`) on the out-point clip-stop
  path that skips the present block, the metrics blocks, and the end-of-playback check for
  that tick. It is deliberate and harmless (the next tick picks up), but it is the one place
  where the "canonical order of operations" contract in `ARCHITECTURE.md §13` is not visibly
  linear.

### 6.4 `src/ffi/ffmpeg.rs` (2,190 lines) — well-structured for what it is

Resource lifetimes are handled properly: `InputContext`, `Packet`, `Frame`, and the codec
contexts are newtypes with `Drop`. `InterruptState` is `Box`ed with a documented reason (the
`AVFormatContext` holds a raw pointer to it, so the address must be stable), declared last in
`DecodeSession` so it outlives the context that references it — an easy thing to get wrong,
gotten right and commented. Blocking I/O is deadline-guarded (30 s open, 15 s read, 15 s seek)
through an interrupt callback, so a dead network share cannot wedge a worker.

The one flaw found is **L1**: of four error paths in `receive_video_frames`, two unref the
`AVFrame` and two do not. Fixed in §7.

`AudioOpen`'s doc comment (`ffmpeg.rs:196-213`) deserves specific credit: it explains the
cancelled-open hazard precisely and explains why exiting is wrong. It is the reason **H1** was
identifiable as a defect rather than a hypothesis.

### 6.5 `src/render/hdr.rs` (1,516 lines) — the model to point at

736 lines of code, 780 lines of tests, **41 tests**. Pure decision logic: classify stream color
tags, decide the presentation path, resolve shader signals, convert HDR10 static metadata
(unit-tested against the MSDN worked example). Every error is typed (`HdrError`) rather than
stringly. **This file should not be split.** It is what the rest of the pure logic in the
codebase should look like, and the staged plan in §10 is essentially "make more code look
like this."

---

## 7. Confirmed defects and high-confidence risks

### H1 — **Confirmed defect, High.** A seek during a decode-worker reopen kills video permanently

**Locations (pre-fix):** `src/app/session.rs:1591`; `src/app/decode_thread.rs:106`.

`DecodeThreadHandle::serves()` returned `self.control.is_some() && self.preference == Some(p)`.
`control` is an `Arc<DecodeControl>` that deliberately outlives the worker thread, so
`serves()` could not distinguish a running worker from an exited one. Meanwhile the video
worker body returned outright on `VideoOpen::Cancelled`, leaving `control` registered forever.

**Trigger sequence, all steps reachable:**

1. A file cannot use hardware decode and falls back to software. `handle_event` sets
   `self.decode_preference = ForceSoftware` (`session.rs:957`) while the running worker was
   spawned with `Auto`.
2. The user seeks. `serves()` is now false (preference mismatch), so `execute_seek` tears the
   worker down and spawns a fresh one (`session.rs:1297-1305`) with `state = Seeking`.
3. The user scrubs again before the new worker finishes `DecodeSession::open` — which is slow
   (`avformat_find_stream_info`, decoder allocation, an initial seek). `seek()` only rejects
   in `Idle | Opening | Error` (`session.rs:1233`), and `Seeking` is not in that set, so the
   seek proceeds. `serves()` is now true, so the coordinator sends an in-place seek command.
4. `send_seek` bumps `DecodeControl::seq`. The worker's cancellation predicate is
   `control.seq() != serving.get() || control.is_shutdown()` (`session.rs:1485`), so the open
   aborts and returns `VideoOpen::Cancelled` — and the worker **returned**.

The `Seek` command sits in `DecodeControl::pending` forever. The coordinator believes a worker
is serving it. **No video frame ever arrives again for that file**; audio continues, because
the audio worker is independent. Only opening another file recovers.

**Why this is a defect and not a hypothesis:** the identical bug was diagnosed and fixed for
the audio worker in commit `b603f6f`. `spawn_audio_thread` (`session.rs:1857-1909`) retries
the open on `Cancelled`, and its comment states the reasoning verbatim: *"Exiting on that (as
this once did) killed audio for the rest of the file: the coordinator only re-sends seeks to
the worker's control channel, which outlives the thread, so nothing ever noticed it was
gone."* The tri-state `AudioOpen` enum was introduced for exactly this. `VideoOpen` has the
same tri-state shape — and the video worker never got the corresponding fix.

**Fix applied.** Two changes, layered:

1. `spawn_decode_thread` now wraps `DecodeSession::open` in a retry loop mirroring
   `spawn_audio_thread`: on `Cancelled`, return if shutting down, otherwise take the pending
   command via `control.wait_next()`, adopt its `seek_gen`/`op_id`/sequence, and reopen at its
   target. `NoVideoStream` and `Err` still return, as before. `hdr_capabilities` is cloned per
   attempt so every attempt sees the same decide-at-open snapshot.
2. `DecodeThreadHandle::serves()` additionally requires `worker_count() > 0`. This is a second
   line of defence for the paths that legitimately do exit (a decoder open error, for
   example), and it cannot produce a false negative: `prepare_spawn` increments the counter
   *before* the thread is spawned, and only the worker's exit guard decrements it.

Covered by two new tests in `src/app/decode_thread.rs`:
`does_not_serve_once_the_worker_has_exited` and
`serves_again_after_a_respawn_replaces_the_dead_worker`.

### M1 — **Confirmed defect, Medium.** Holding Ctrl+S toggles subtitles

**Location (pre-fix):** `src/ffi/dxgi.rs:1826`.

`ffi/dxgi.rs:1722` guards the screenshot arm with `ctrl_held && (lparam >> 30) & 1 == 0` — the
repeat flag, so only the first press saves a screenshot. Line 1826 was a bare `0x53 =>` arm
mapping S to `ToggleSubtitles`. Every auto-repeat of a held Ctrl+S therefore failed the first
guard and fell through to the second arm, toggling subtitles at the keyboard repeat rate.

**Fix applied:** the fallthrough arm is now `0x53 if !ctrl_held =>`, with a comment recording
why the guard is load-bearing.

**No test was added**, and that is the point of **M5**: this code cannot be tested without an
`HWND` and live `GetKeyState`. Stage 2 of §10 makes it testable; this defect is the concrete
justification for that stage.

*Audit note:* the neighbouring guarded arms were checked for the same fallthrough shape.
`0x48` (H) is safe — a held H falls through to `_`, a no-op. `0x52` (R), `0x4F` (O), `0x49` (I)
order their ctrl-guarded arm first and their bare arm second with no repeat guard, so they do
not fall through. `0x53` was the only instance.

### L1 — **High-confidence risk, Low severity.** `AVFrame` not unreferenced on two error paths

**Locations (pre-fix):** `src/ffi/ffmpeg.rs:1528`, `:1543`.

`receive_video_frames` has four error exits after a successful `avcodec_receive_frame`. Two
unref the frame first (`:1475` cancellation, `:1498` unexpected pixel format); two did not —
the `surface_from_raw_texture` failure and the software `convert` failure. On the hardware
path the retained frame pins a D3D11VA decoder-pool surface, and after the error propagates
the worker parks in `wait_next` holding it. Bounded by `Frame`'s `Drop` (so not a leak across
opens) and by `avcodec_receive_frame` unreffing its destination on the next call — but it is an
inconsistency between four adjacent paths in one function, which is how the next one gets
written wrong.

**Fix applied:** both paths now `av_frame_unref(frame)` before returning, matching their
siblings.

### L2 — **High-confidence risk, Low severity.** Two latent `clamp` panics

Detailed in §6.2. Unreachable at the enforced 640×360 minimum client size.
**Deliberately not fixed** — see the reasoning in §6.2 and §11.

### L3 — **High-confidence risk, Low severity.** A transient audio open failure silences audio

**Location:** `src/app/session.rs:1902-1907`.

The audio worker correctly retries a *cancelled* open, but returns on `Err` after logging
`[audio_worker] open failed (continuing without audio)`. Since the control channel outlives
the thread and `execute_seek` only checks `control().is_some()` (`session.rs:1311`), later
seeks are delivered to a dead worker and audio stays silent for the rest of the file. This is
the same structural hazard as **H1**, one severity band lower because the failure is degraded
audio rather than frozen video, and because a genuinely unopenable audio stream is usually
permanent rather than transient. Not fixed in this pass; the correct fix is to give the audio
handle the same liveness check `serves()` now has, which needs its own small design pass.

### L9 — **Maintainability, Low.** Charter-specified endpoint detection was never implemented

*Found during Stage 5, not the original sweep.*

`ARCHITECTURE.md:268` lists `AudioEndpointChanged { open_gen, seek_gen, op_id }` in the locked
event model, and §6 assigns "audio endpoint recovery detection" to the workers.
`app/events.rs:86` defines it and `PlaybackSession::handle_event` has a live arm for it — but
**nothing anywhere constructs it.** There is no `IMMNotificationClient`, no
`RegisterEndpointNotificationCallback`, nothing that would observe a device change.

Endpoint changes are only noticed *reactively*: a WASAPI write fails, and `submit_due_audio`
calls `recover_audio_endpoint` directly (`session.rs:2133`). Recovery does work — one failed
write later — so this is a latency and fidelity gap against the charter, not a broken feature.

The variant is deliberately **kept**, with a `dead_code` allow that records exactly this. It is
charter-specified and `AGENTS.md` forbids revising the charter, so deleting it unilaterally
would be the wrong call. Closing the gap properly means either implementing the notification
client or amending `ARCHITECTURE.md` — a scope decision, not cleanup.

---

## 8. Testing gaps

The suite is **199 tests at baseline (201 after this review), all passing**, and its coverage
of pure logic is genuinely good: 41 tests on HDR classification, 18 on clip ranges, 17 on the
play queue, 13 on timeline geometry, 12 each on recent-files policy and audio coordination, 11
on viewport math. `docs/TECH_DEBT.md §2 R2` characterizes this accurately.

The gaps are structural rather than lazy — each one is code that cannot be reached from a
test in its current location:

| Gap | Why it is untestable now | Resolved by |
|---|---|---|
| **The input keymap** (~35 arms). Zero tests. Directly responsible for M1 shipping. | Requires `HWND` + live `GetKeyState` | Stage 2 (§10) |
| **The CPU rasterizer** (~1,240 lines). Zero direct tests, including `blend_pixel`'s compositing arithmetic and `fill_rounded_rect`'s five-case coverage computation. | `SubtitleBitmap` is private to the FFI module | Stage 3 (§10) |
| **Worker lifecycle transitions.** `decode_thread.rs` had 4 tests, none covering worker death. H1 lived here. | Needed a liveness signal to assert on | **Closed** — `serves()` now has one, plus 2 tests |
| **Coordinator/FFI end-to-end paths** — open, seek, device recovery, endpoint change. | Genuinely needs a device and a file | Manual validation + `bench/`. Expected residual gap; `TECH_DEBT.md §2 R2` says so honestly, and this review agrees it should stay that way |
| **The benchmark harness is not gated.** `bench/run-bench.ps1` produces p50/p95 for every charter metric but runs only on demand. | Deliberate (`ROADMAP.md §2`) | Promote to CI once stable across machines |

One quality note in the suite's favour: the tests read like specifications of intent —
`auto_advance_does_not_wrap_at_end_of_queue`, `resume_open_reports_resume_position_not_zero`,
`classified_hlg_routes_to_pq_output_or_tone_map_never_sdr`. No tests were found that pass
without asserting meaningful behaviour.

---

## 9. Recommended target architecture

**Essentially the current one.** `ARCHITECTURE.md` is a locked charter, `AGENTS.md` forbids
revising it, and — more to the point — this review found no evidence it is the source of any
problem. Both defects found were local bugs, not architectural consequences.

Two boundary corrections, and nothing else:

1. **Policy moves out of the FFI seams.** `ffi/*` holds unsafe code and the type conversions
   immediately around it. Pure decision logic — key mapping, overlay geometry, compositing —
   belongs in `platform/` and `render/` where it can be tested. This *reduces* the unsafe
   surface; it does not redraw a module boundary.
2. **Worker liveness becomes explicit.** The pattern "the control channel outlives the thread,
   so possession of a channel is not evidence of a live worker" has now produced two bugs
   (audio in `b603f6f`, video in H1) and remains latent in a third place (L3). The
   `serves()` liveness check and the retry-on-cancelled loop make it explicit in the video
   path; the same treatment should reach the audio handle.

Explicitly **not** recommended: a workspace split, a `PlaybackSession` trait, a second
coordinator, an event-sourcing or actor rewrite, a state-machine library, or any generic
cross-platform abstraction layer. See §11.

---

## 10. Staged pull-request plan

Each stage is one small, reviewable PR. Full validation (`fmt` / `clippy -D warnings` /
`test --all-targets` / `build --release`) after every stage.

### Stage 1 — Fix the confirmed defects · **DONE in this review** · Small

- **Problem:** H1, M1, L1.
- **Files/symbols:** `app/session.rs::spawn_decode_thread`; `app/decode_thread.rs::serves`;
  `ffi/dxgi.rs::window_proc` (the `0x53` arm); `ffi/ffmpeg.rs::receive_video_frames`.
- **Public interfaces:** unchanged. `serves()` keeps its signature; the added conjunct cannot
  produce a false negative.
- **Tests before:** none existed for worker death — that is why H1 shipped.
- **Tests after:** 2 new in `decode_thread.rs`. 199 → 201, all passing.
- **Behavioural impact:** a cancelled video open now reopens at the new seek target instead of
  killing the worker. Ctrl+S auto-repeat no longer toggles subtitles. No change to the
  steady-state present, clock, or queue paths.
- **Regression risk:** Low. The retry loop is a direct transcription of the audio worker's
  proven-in-production loop.
- **Depends on:** nothing.

### Stage 2 — Extract the input keymap · **DONE 2026-07-21** · Small · *highest value-per-line in the plan*

> Implemented as specified. `platform/input.rs` gained `command_for_key(vk, ctrl,
> shift, is_repeat) -> Option<InputEvent>` and `command_for_key_release(vk)`,
> both pure; `window_proc`'s `WM_KEYDOWN`/`WM_KEYUP` arms shrank from 218 lines
> to 19 and now only decode modifier state and dispatch. `ffi/dxgi.rs` went
> 2,295 → 2,096 lines with its `unsafe` block/fn count unchanged at 64, so no
> unsafe responsibility moved. 48 characterization rows plus 5 property tests
> (207 tests total, up from 201). Verified by mutation: reintroducing the
> Ctrl+S fall-through fails three of them. No shortcut behavior changed.


- **Problem:** M2, M5, and the class of bug M1 belongs to.
- **Files/symbols:** new `platform/input.rs::command_for_key(vk: u32, ctrl: bool, shift: bool,
  is_repeat: bool) -> Option<InputEvent>`; `ffi/dxgi.rs::window_proc`'s `WM_KEYDOWN`/`WM_KEYUP`
  arms reduce to reading modifier state, calling it, pushing the result.
- **Boundary:** pure function, no Win32 types in the signature.
- **Public interfaces to keep stable:** `InputEvent` (unchanged), `take_input_events`.
- **Tests before:** a characterization table asserting the *current* mapping for every VK the
  arms handle, including both repeat states and both modifier states — written against the
  extracted function as the first commit, so the move is provably behaviour-preserving.
- **Tests after:** the same table, plus explicit cases for the guard-fallthrough shape that
  produced M1 (Ctrl+S repeat must yield `None`, not `ToggleSubtitles`).
- **Behavioural impact:** none intended.
- **Regression risk:** Low-Medium — the keymap is the whole user-facing control surface, so
  the characterization table must be written *first* and must be exhaustive.
- **Depends on:** Stage 1 (avoids conflicting edits in `window_proc`).

### Stage 3 — Extract the pure overlay rasterizer · **DONE 2026-07-21** · Medium

- **Problem:** M3, M5, L2.
- **Files/symbols:** new `render/overlay_raster.rs` taking `fill_rect`, `fill_circle_aa`,
  `fill_rounded_rect`, `blend_pixel`, and `render_timeline_bitmap`; `SubtitleBitmap` (or an
  equivalent `OverlayBitmap`) becomes `pub(crate)`. The GDI text functions
  (`render_subtitle_bitmap`, `render_help_bitmap`, `render_recent_bitmap`,
  `render_volume_bitmap`, `render_idle_bitmap`, `draw_timeline_label`) **stay** in
  `ffi/d3d11.rs` — they call GDI and belong in the seam.
- **Tests after:** `blend_pixel` source-over identities (opaque source replaces; zero alpha is
  a no-op; over-a-transparent-destination copies); `fill_rect` clipping at every edge;
  `fill_rounded_rect` corner coverage monotonicity; `render_timeline_bitmap` played-width and
  marker positions across representative viewports. Add a `debug_assert!` for the L2 minimum
  width rather than a silent clamp.
- **Behavioural impact:** none, and this was *proved* rather than argued. A throwaway
  equivalence harness lifted the pre-refactor shape code verbatim out of `be68f9d` and
  compared it against the extracted version across **1,500 model permutations** (5 widths ×
  5 played spans × 5 handle positions × 6 marker configurations × 2 preview/loop states):
  all byte-identical, including the degenerate and out-of-range cases. The harness was then
  deleted rather than committed — it is a frozen duplicate of code that now has one home.
- **Regression risk:** realized as none. 210 → 230 tests. `ffi/d3d11.rs` 4,594 → 4,322 lines
  with its `unsafe` block/fn count unchanged at 40; `render/overlay_raster.rs` contains no
  `unsafe` and no Win32.
- **Depends on:** Stage 1.

### Stage 4 — De-duplicate the worker plumbing · **DONE 2026-07-21** · Small

- **Problem:** L4 — `worker_send` (~35 lines) duplicated verbatim across both worker bodies;
  the `device removed → DeviceLost else PlaybackFailed` mapping written three times
  (`session.rs:1608`, `:1718`, `:1748`); the SAR guard six times in `ffmpeg.rs`.
- **Boundary:** private free functions, as prescribed — no trait, no generic worker type.
  - `send_to_ui(event, tx, cancelled)` in `session.rs`; both worker bodies keep a two-argument
    `worker_send` closure over it, so the ~20 call sites are untouched.
  - `worker_failure_event(...)` for the two identical device-lost mappings, and
    `is_device_lost(error, device)` for the predicate, which the open path shares — it keeps
    its own shape because it has a third outcome (`OpenFailed`).
  - `frame_sample_aspect_ratio(frame)` in `ffmpeg.rs`, replacing three copies of the
    "zero or negative means unknown, fall back to square" guard.
- **Result:** each of the three now has exactly one definition (was 2 / 3 / 3 copies).
- **Regression risk:** realized as none — 230 tests unchanged and passing.
- **Depends on:** Stages 1-3.

### Stage 5 — Retire the blanket `allow(dead_code)` · **DONE 2026-07-21** · Small

- **Problem:** M4. Seven modules disable `dead_code` file-wide: `app/commands.rs`,
  `app/events.rs`, `app/media_ext.rs`, `app/play_queue.rs`, `app/recent.rs`,
  `playback/generations.rs`, `playback/queues.rs`.
- **Done.** All seven blanket allows removed. The compiler then flagged exactly five items,
  which is the argument for the change — a module-wide allow cannot tell reserved API from rot:
  - **Deleted:** `SessionCommand::Tick`, constructed nowhere and backed only by a no-op match
    arm.
  - **Kept, with a per-item allow stating why:** `SessionEvent::AudioEndpointChanged` (see the
    new finding L9 below), `media_ext::is_subtitle`, `PlayQueue::{is_empty, items, cursor}`,
    `RecentFiles::{is_empty, clear}`. `PlayQueue::is_empty` in particular *cannot* be deleted:
    `clippy::len_without_is_empty` requires it alongside `len`, which the auto-advance planner
    uses.
  - `playback/generations.rs` and `playback/queues.rs` were hiding nothing at all — their
    allows were pure noise.
  - Two stale module comments corrected: `media_ext.rs` and `play_queue.rs` both still claimed
    the play queue was "not yet wired into the open flow", long after `main.rs` began driving
    it.
- **The 12 crate-wide clippy allows were swept too**, after measuring each one individually
  rather than judging it by name: remove the allow, count what clippy reports, and note which
  files those reports land in. `explicit_auto_deref` was hiding nothing and was deleted. Three
  (`upper_case_acronyms`, `useless_transmute`, `type_complexity`) fire *only* on the bindgen
  output and moved to `ffi/ffmpeg.rs`, the module that `include!`s it. Five more are Win32/COM
  idioms and moved to the specific seams that use them. Two — `manual_is_multiple_of` and
  `unnecessary_map_or` — fired only in **safe application code**, three sites in total, and
  were fixed rather than allowed. Only `too_many_arguments` remains crate-wide, because it
  genuinely spans `app/`, `render/` and `ffi/`. **12 crate-wide allows became 1.** Verified by
  injecting an `unnecessary_map_or` into `app/drop_stats.rs` and confirming it now fails
  `-D warnings`, where the blanket allow used to absorb it silently.
- **Regression risk:** realized as none — 210 tests still pass, `clippy -D warnings` clean.
- **Depends on:** nothing; can run in parallel.

### Stage 6 — Extend worker-liveness discipline to audio · **DONE 2026-07-21** · Small

- **Problem:** L3, L6, and a regression Stage 1 introduced (below).
- Both handles now share `DecodeThreadHandle::seek_delivery`, returning a three-way
  `SeekDelivery { InPlace, Respawn, Retired }` instead of the boolean `serves`. L6's redundant
  double-check is gone with it.
- **The third state is not tidiness.** Stage 1 gated `serves` on `worker_count() > 0`, which
  fixed the wedge but broke audio-only files: the video worker exits after reporting
  `NoVideoStream`, so every subsequent seek tore down and respawned a worker that reopened and
  re-demuxed the file just to rediscover there is no video. Measured on an audio-only `.m4a`
  driven with 8 `PostMessageW` right-arrow seeks — **9 `[spawn_decode_thread]` before the fix,
  1 after**, with `[execute_seek]` at 8 in both runs. The workers now set a retirement flag
  before their permanent-exit returns; `prepare_spawn` clears it so the verdict never outlives
  its open.
- **Not done:** a retry on a transient audio *open error*. The liveness gate already means the
  next seek respawns audio, and an open error is usually permanent for that file, so an
  automatic retry risks spinning on a genuinely broken stream for no gain.
- **Tests:** 9 in `decode_thread.rs` (210 total, up from 207), covering each `SeekDelivery`
  state, live-worker precedence over retirement, and retirement clearing on respawn.
- **Depends on:** Stage 1.

---

## 11. Changes that should not be made

- **Do not split the crate into a workspace.** `AGENTS.md` mandates a single crate;
  `TECH_DEBT.md §5` lists this as an explicit non-goal. Nothing found here argues otherwise.
- **Do not make `PlaybackSession` a trait, and do not introduce a second coordinator.**
  `ARCHITECTURE.md §29` and `AGENTS.md` forbid it. Its width is the design, not a symptom.
- **Do not split `render/hdr.rs`.** 41 tests, pure logic, typed errors — the best file in the
  repo.
- **Do not split the FFI seams to reduce line count.** `d3d11.rs`, `dxgi.rs`, `ffmpeg.rs`, and
  `wasapi.rs` are the four designated unsafe boundaries. Extract *policy* out of them (Stages
  2-3); do not shard the unsafe code itself.
- **Do not "fix" the latent `clamp` panics (L2) with defensive clamping.** They are unreachable
  because a real invariant holds. Silently clamping would hide a genuine invariant violation
  if that guarantee ever broke. Use a `debug_assert!` when the code moves in Stage 3.
- **Do not touch the steady-state present, clock, or queue paths.** The dual-channel drain,
  the WASAPI padding smoothing, the audio-anchor handoff on underrun, and the video drop
  policy are all load-bearing and were arrived at by fixing real stutter bugs. `AGENTS.md`:
  "Any change to steady-state playback behavior in the name of 'cleanup'" is a non-goal.
- **Do not change the shutdown discipline.** `session.shutdown()` deliberately does not
  release D3D11 objects; `main` hard-exits. This is documented at length and exists because
  in-process teardown faulted in the graphics driver and triggered multi-second WER hangs.
- **Do not refactor `window_proc`'s modal-tick machinery** while extracting the keymap. The
  `SetTimer`/`SetCapture` work exists to defeat specific blocking `DefWindowProcW` loops.
- **Do not centralize the `PlaybackState` machine.** Attractive on paper; it would rewrite the
  most timing-sensitive code in the program for reviewability alone.
- **Do not add abstractions with one consumer.** In particular, no generic "worker" trait in
  Stage 4 — there are two workers and they differ meaningfully.

---

## 12. Final recommendation

**The codebase is healthy and safe to keep developing on. It does not need a refactor before
further feature work.**

The framing that prompted this review — oversized files needing decomposition — does not match
what is in the tree. There is no 5,000-line file. The four largest files are large for
defensible, documented reasons, and the one previous large-file problem (`PlaybackSession`)
was already solved in v0.3.0. The codebase has CI-enforced formatting and lints, 199 passing
tests concentrated on exactly the pure logic that benefits from them, typed errors, disciplined
resource ownership, and comments that consistently explain *why*.

What it actually had was one High-severity latent defect — a known bug class, previously
diagnosed and fixed in the audio path, never carried across to video — plus a keyboard bug and
a small FFI inconsistency. All three are fixed, with 201 tests now passing and a clean
release build.

**Recommended first PR: Stage 2, extracting the input keymap into `platform/input.rs`.** It is
small, mechanical, protected by a characterization table written first, and it converts the
single most defect-prone untested surface in the program into a pure function with exhaustive
tests. M1 existed because that code was untestable; Stage 2 is what stops the next one.

Beyond that, the plan in §10 should be paced by observed defects rather than run to
completion. `docs/TECH_DEBT.md §4` already says this — *"Next maintenance work should be
driven by observed defects, difficult review areas, or benchmark regressions rather than a
line-count target"* — and this review's evidence supports it.

---

## Appendix A — Repository hygiene

**Stale git worktree.** `.claude/worktrees/agent-a3a550c6` is a registered worktree on branch
`worktree-agent-a3a550c6` @ `b17fe45`, roughly 106 files and 20,000 lines behind `main`, with
uncommitted modifications to `session.rs`, `main.rs`, `metrics.rs`, `app/mod.rs` and untracked
`app/overlay.rs` / `app/timeline_ui.rs`.

It was diffed as part of this review. The uncommitted work is an **early draft of the v0.3.0
`OverlayManager` / `MetricsCollector` / `timeline_ui` extraction — all three of which already
shipped on `main`**. Nothing in it is salvageable. It is safe to `git worktree remove` at the
owner's discretion; this review deliberately did not remove it. (`.claude/` is gitignored, so
it does not affect the tracked tree.)

**Documentation drift to reconcile** (L8, M4):

- `docs/TECH_DEBT.md §3` claims "There is **no baseline allow-list**; no `#![allow(...)]` debt
  is being hidden." `src/main.rs:6-17` disables 12 clippy categories crate-wide, and 7 modules
  disable `dead_code` file-wide. Several of the crate allows are legitimate for a Win32/FFI
  codebase (`too_many_arguments`, `upper_case_acronyms`, `useless_transmute`); the claim of
  having none is what needs correcting.
- `docs/TECH_DEBT.md §1` largest-file table is stale: `d3d11.rs` 3036 → **4594**,
  `session.rs` 2413 → **2892**, `dxgi.rs` 1842 → **2295**, `ffmpeg.rs` 1563 → **2190**, and
  `render/hdr.rs` (**1516**) is absent.
- `ARCHITECTURE.md §5`'s repo-shape listing omits five shipped modules:
  `app/play_queue.rs`, `app/media_ext.rs`, `render/hdr.rs`, `render/hdr_validate.rs`,
  `playback/audio_diag.rs`.

## Appendix B — Areas reviewed and found sound

Recorded so a future reviewer knows these were checked rather than skipped.

- **Path handling.** `MediaSource` paths flow to FFmpeg via `to_str()` + `CString::new`, with
  explicit errors for non-UTF-8 and embedded NUL (`ffmpeg.rs:268-273`). No shell or command
  construction exists anywhere in the crate, so there is no injection surface.
- **Drag-and-drop input** (`dxgi.rs::extract_drop_paths`). `CF_HDROP` is queried correctly,
  buffers are sized from `DragQueryFileW`'s own length query, non-existent entries are dropped,
  and `ReleaseStgMedium` is called. Sound. Minor: `*pdweffect` is written without a null check
  (`dxgi.rs:1597`), which OLE never violates in practice.
- **Recent-files persistence.** Tab-separated, path last, corrupt lines skipped on load, list
  truncated to 20. Non-atomic write (L7) is the only weakness and is bounded by the tolerant
  loader.
- **Integer/overflow handling.** `encode_bgra_bmp` uses `checked_mul`/`checked_add` for BMP
  sizing and validates the buffer length against the dimensions (`session.rs:2757-2794`).
  `Duration` arithmetic uses `saturating_add`/`saturating_sub` throughout.
- **Backpressure.** The two-channel drain with per-queue gating is correct and the comment
  explaining why unconditional draining broke playback (`session.rs:770-781`) is accurate.
- **Device-loss recovery.** `recover_device` tears the worker down with `wait = true` before
  rebuilding, with the ordering rationale documented in `decode_thread.rs:41-48`.
- **Drop-order discipline.** Both `PlaybackSession` (presenter before window) and `Presenter`
  (device declared last) order fields for correct COM teardown, each with a comment explaining
  the failure mode that motivated it.
- **`unwrap`/`expect` usage.** 42 occurrences total; those in non-test code are on
  provably-non-empty containers (`video_queue.pop_front().expect("front frame existed")` after
  a `front()` check) or on infallible conversions. No unchecked indexing of externally-supplied
  data was found.
