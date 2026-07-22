# ARCHITECTURE.md

## FastPlay Architecture Charter

Windows-first, latency-focused media player architecture.

This document is the implementation charter for v1.  
The architecture is considered **locked** unless a change is required to fix a correctness or performance bug.

---

## 1. Product Goal

Build a **Windows-only local media player** that feels materially faster than legacy/general-purpose players on:

- open-to-first-frame latency
- short seek latency
- pause/resume immediacy
- resize/fullscreen smoothness
- steady-state playback responsiveness
- robustness under reopen/seek/device churn

The player is optimized for **perceived latency** and **hot-path discipline**, not broad feature coverage.

---

## 2. Non-Goals for v1

The following are explicitly **out of scope** for initial implementation:

- streaming
- playlists / media library
- browser/web UI
- plugin system
- advanced subtitle styling engine
- metadata-driven tone mapping (HDR presents natively on HDR displays and
  tone-maps to SDR otherwise — see §21; the curve/OOTF are fixed)
- frame interpolation
- AI enhancement during playback
- cross-platform support
- multiple hardware decode backends
- exclusive fullscreen support

---

## 3. Core Thesis

The fastest-feeling Windows player is built around:

- **FFmpeg** for demux/probe/decode
- **D3D11 hardware surfaces** for video decode output
- **DXGI flip-model swap chain** for presentation
- **WASAPI / `IAudioClient3`** for low-latency shared audio
- **native Win32 windowing**
- **small bounded queues**
- **single coordinator ownership model**
- **generation-based stale-work dropping**

### Non-negotiable hot path

```text
disk -> FFmpeg demux -> FFmpeg hw decode -> AVFrame(AV_PIX_FMT_D3D11)
     -> opaque surface handle -> D3D11 presenter -> DXGI flip-model Present

audio -> FFmpeg decode -> WASAPI shared-mode sink
````

### Normal path invariant

No CPU copy-back in steady-state playback.

If video frames leave the D3D11 path during normal supported playback, that is considered a bug unless the session is explicitly in fallback mode.

---

## 4. Technology Decisions

### Video

* FFmpeg for:

  * file open
  * probing
  * demux
  * stream selection
  * timestamps
  * hardware decode integration
* Preferred decode output:

  * `AV_PIX_FMT_D3D11`

### Presentation

* D3D11 device/context
* DXGI flip-model swap chain
* borderless fullscreen windowed mode only for v1

### Audio

* FFmpeg audio decode
* WASAPI shared mode
* `IAudioClient3` seam for low-latency negotiation

### UI

* Native Win32 window
* Minimal custom controls
* No heavy retained-mode UI framework
* No webview/Electron shell

---

## 5. Repo Shape

Start with a **single Rust crate**, not a workspace.

```text
fastplay/
  Cargo.toml
  README.md
  ARCHITECTURE.md
  src/
    main.rs
    logging.rs

    app/
      mod.rs
      audio_controller.rs
      clip_range.rs
      commands.rs
      decode_thread.rs
      drop_stats.rs
      events.rs
      input_dispatch.rs
      media_ext.rs
      overlay.rs
      play_queue.rs
      recent.rs
      session.rs
      settings.rs
      state.rs
      timeline_ui.rs
      video_queue.rs
      viewport.rs

    playback/
      mod.rs
      audio_diag.rs
      clock.rs
      decode_control.rs
      generations.rs
      metrics.rs
      queues.rs

    media/
      mod.rs
      audio.rs
      seek.rs
      source.rs
      subtitle.rs
      video.rs

    render/
      mod.rs
      hdr.rs
      hdr_validate.rs
      presenter.rs
      surface_registry.rs
      swapchain.rs
      timeline.rs

    audio/
      mod.rs
      sink.rs

    platform/
      mod.rs
      input.rs
      open_dialog.rs
      window.rs

    ffi/
      mod.rs
      d3d11.rs
      dxgi.rs
      ffmpeg.rs
      ffmpeg_shim.c
      ffmpeg_shim.h
      runtime.rs
      wasapi.rs
```

---

## 6. Ownership Model

This architecture is intentionally strict.

### `PlaybackSession`

`PlaybackSession` is the **single coordinator** and **concrete orchestration nucleus**.

It is the only subsystem allowed to:

* change playback state
* coordinate open/close
* coordinate seek/flush
* coordinate resize/fullscreen transitions
* coordinate device/audio recovery
* consume worker completions
* decide stale-work rejection
* own metrics timing boundaries

`PlaybackSession` is **not** a trait.

### Decoder owns

* FFmpeg codec state
* FFmpeg packet/frame lifetime
* hw device context
* decode-side queue fill
* seek flush behavior inside decoder boundary

### Presenter owns

* D3D11 device/context
* swap chain
* backbuffer/RTV lifecycle
* viewport/scissor state
* present scheduling execution

### Audio sink owns

* WASAPI client lifetime
* shared-mode stream initialization
* buffer submission
* audio clock reporting

The sink does **not** own endpoint-change detection. It is bound to one
`IMMDevice` for its lifetime and cannot observe the default moving away from it;
see §19.

### Workers do **not**

* mutate session state directly
* call `Present`
* initiate cross-subsystem resets
* decide global playback policy

---

## 7. Session Event Model

All asynchronous completions must flow through `SessionEvent`.

Workers never mutate `PlaybackSession` fields directly.

### Internal event pattern

```rust
enum SessionEvent {
    DecodeModeSelected { open_gen, seek_gen, op_id, mode, hw_fallback_count, rotation_quarter_turns },
    MediaDurationKnown { open_gen, seek_gen, op_id, duration },
    VideoFrameReady(PendingVideoFrame),
    AudioFrameReady(PendingAudioFrame),
    VideoStreamEnded { open_gen, seek_gen, op_id },
    AudioStreamEnded { open_gen, seek_gen, op_id },
    OpenFailed { open_gen, op_id, error },
    PlaybackFailed { open_gen, seek_gen, op_id, error },
    DeviceLost { open_gen, seek_gen, op_id },
}
```

This preserves the **single-coordinator rule**.

### Not a `SessionEvent`: default audio endpoint changes

`SessionEvent` carries **worker output**, which is why every variant is stamped
with generations — the coordinator rejects work belonging to a superseded open
or seek before acting on it.

A default-endpoint change is not worker output. It is a global fact about the
machine, delivered by Windows on an MMDevice callback thread, and it is never
"stale" in the sense generations model. Stamping it would be actively wrong: an
endpoint change arriving mid-seek would carry pre-seek generations, be rejected
by the staleness check, and be lost.

It is therefore polled, not routed — the same shape as a window resize request
(§17). The COM callback sets a flag and nothing else; `tick(now)` consumes it on
the UI thread and acts with its own current state. See §19.

---

## 8. Generations and Operation IDs

Every async completion path must carry:

* `OpenGeneration`
* `SeekGeneration`
* `OperationId`

### Purpose

#### `OpenGeneration`

Invalidates work from prior opens / prior files.

#### `SeekGeneration`

Invalidates work from prior timeline operations within the same open.

#### `OperationId`

Provides total ordering and debugging identity for operations and completions.

### Rule

Stale work is dropped **before side effects**, not after.

### Required behavior

* `open()` increments `OpenGeneration`
* `seek()` increments `SeekGeneration`
* all worker outputs carry generations + op id
* stale video/audio/events are silently discarded
* logs/metrics include generation/op information where relevant

---

## 9. Public Safety Contract

No raw pointers or COM interfaces may escape `ffi::*`.

### Allowed public pattern

Public D3D11-backed frames are represented by **opaque handles**, not raw pointers.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoSurfaceHandle(u64);
```

### Not allowed

* `*mut c_void` in public structs
* raw COM pointers in public APIs
* `ID3D11Texture2D*` outside FFI

---

## 10. Surface Registry

`VideoSurfaceHandle` must resolve through a generation-safe registry.

### Internal registry entry

```rust
struct SurfaceEntry {
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    // hidden texture/view refs
}
```

### Rules

* `epoch_base` on the registry increments on device rebuild / presenter reset
* handles from prior epochs resolve to `None` via arithmetic underflow check
* stale handles must never become valid again
* presenter rejects unknown/stale handles
* no handle reuse across incompatible epochs

This prevents accidental reuse of invalid surfaces after device loss or rebuild.

---

## 11. State Machine

State is explicit. No “flags plus vibes”.

```text
Idle
Opening
Priming
Playing
Paused
Seeking
Draining
Ended
Error
```

### State transition intent

* `Idle -> Opening`

  * file open requested

* `Opening -> Priming`

  * streams selected, decoders initialized, queues warming

* `Priming -> Playing`

  * first usable frame/audio path established

* `Playing -> Paused`

  * user pause

* `Paused -> Playing`

  * user resume

* `Playing/Paused -> Seeking`

  * seek requested

* `Seeking -> Priming`

  * flush complete, new target established

* `Playing -> Ended`

  * end-of-stream reached and drained

* `Any -> Error`

  * fatal error

---

## 12. Threading Model

v1 keeps the thread model small.

### Threads

* UI/render thread
* video demux/decode worker
* independent audio demux/decode worker (own `AVFormatContext`)

### Independent audio worker

Audio is decoded on its **own** demuxer/thread, separate from the video
demux/decode worker. Some files cannot use D3D11VA/NVDEC hardware decode — e.g.
true H.264 **Baseline** profile at 4K60, which the hardware rejects — and fall
back to software video decode that runs *below* realtime. A single worker
demuxing both streams in file order then produces audio only as fast as the
slow video, starving the WASAPI sink (choppy audio). Keeping audio on an
independent worker protects it from video decode stalls: audio stays realtime
while heavy/late video degrades by **dropping/lagging video frames** at the
presenter (per §15), never by starving audio. Both workers seek in place via
their own `DecodeControl` and stamp frames with generations for stale-drop.

### Hard rule

The coordinator entrypoint is `tick(now)` on the **UI/render thread only**.

---

## 13. `tick(now)` Contract

`tick(now)` is the session coordinator entrypoint.

### Contract

* UI-thread only
* non-reentrant
* non-blocking
* must not perform disk I/O
* must not block waiting for worker completion
* must not recurse
* owns state transitions, stale-work rejection, present scheduling, and due audio submission

### Canonical order of operations

```text
1. drain SessionEvent queue
2. drop stale events by (open_gen, seek_gen, op_id)
3. apply state transitions
4. submit due audio
5. select next video frame
6. present selected frame
7. schedule refill / worker nudges
8. emit metrics
```

This order is part of the contract and should not drift casually.

---

## 14. Queue Policy

Queue sizes are **defaults**, not architecture constants.

### Initial defaults

* video packet queue: 48
* audio packet queue: 96
* decoded video frame queue: 4
* decoded audio frame queue: 12

### Queue goals

* low latency
* minimal stale work
* predictable seek behavior
* bounded memory usage

### Backpressure rules

* demux pauses when packet queues are full
* decode pauses when output frame queues are full
* seek clears relevant queues before new target work is admitted
* large queues are not allowed “for smoothness” without measurement justification

---

## 15. Clock Ownership

### v1 policy

* audio is master clock when audio exists
* video is master clock for silent video

The audio position is derived from WASAPI's buffered-frames counter, which
shared mode only updates once per audio engine period (~10 ms) — a
staircase. It is smoothed by wall-time extrapolation between counter
advances (`AudioController::smooth_played`: 12 ms clamp, monotonic, reset
with the clock) so frame cadences shorter than one tread — anything above
100 fps — schedule one frame at a time instead of two-due-at-once, which
the catch-up path would otherwise thin by ~17%.

### Video behavior

* early frame: hold
* slightly late frame: may present
* very late frame: drop

### Audio behavior

* no time-stretch in v1
* no fancy sync correction beyond sane drift handling

### Seek behavior

During seek, UI should reflect the **requested target**, not stale displayed PTS.

---

## 16. Snapshot Semantics

UI-facing playback position needs explicit semantics.

### Rule

`PlaybackSnapshot.position` means:

* normal playback: current master-clock position
* seeking: pending seek target

### Recommended shape

```rust
pub enum PositionKind {
    SettledPlaybackClock,
    PendingSeekTarget,
}
```

This avoids scrubber snap-back and stale-frame-driven UI jitter.

---

## 17. Resize and Fullscreen Behavior

### v1 fullscreen mode

Use **borderless fullscreen windowed**, not exclusive fullscreen.

### Normal resize / borderless fullscreen path

```text
suspend submits
drop presentables tied to old viewport/generation
ResizeBuffers
rebuild RTV/viewport
rebind backbuffer
resume presents
```

### Important rule

`DXGI_PRESENT_RESTART` is **not** part of the normal windowed/borderless path.

If exclusive fullscreen is ever added later, restart behavior can be reconsidered there.

---

## 18. Device Loss and Recovery

### Presenter-only failure

If the presenter path is invalid but decode device state is still valid:

* rebuild presenter path
* recreate swap-chain dependent state
* continue from latest valid state

### Shared device failure

If the shared D3D11 device is removed/lost:

* rebuild decoder hw device
* rebuild presenter path
* clear/invalidate surface registry
* clear video queues
* preserve user intent if possible
* re-prime session

### Metric

Record `device_recovery_ms`.

---

## 19. Audio Endpoint Changes

Audio endpoint changes are part of v1 robustness testing. There are two
distinct cases and they are detected differently, because only one of them
produces an error to react to.

### Case 1 — the device in use goes away

Unplugged, disabled, or removed. WASAPI calls against the sink begin failing
with `AUDCLNT_E_DEVICE_INVALIDATED`. This is detected **reactively**: the failed
write surfaces in `submit_due_audio`, which calls `recover_audio_endpoint`.
Detection is within one `tick`.

### Case 2 — the default moves to a different device

The user plugs in headphones, changes output in the volume flyout, or connects a
Bluetooth sink. An `IAudioClient` is bound to one specific `IMMDevice` for its
lifetime, so **every WASAPI call keeps succeeding** and audio keeps rendering to
the old endpoint. There is no error, and no reactive scheme can see this.

This is detected **proactively**, by an `IMMNotificationClient` registered at
session construction. The callback runs on an MMDevice worker thread where COM
forbids blocking and forbids re-entering the enumerator, so it does the least
work that is correct: it filters to `(eRender, eConsole)` — the flow and role the
sink is opened with — and sets a flag. `tick(now)` polls that flag on the UI
thread (see §7). Registration failure is non-fatal and degrades to Case 1 alone.

### On endpoint change

* detect (per the two cases above)
* rebuild the sink **and** respawn the decode workers — they resample to the
  sink's mix format, captured at spawn, and the new device may differ in rate or
  channel count
* preserve session intent: a paused or finished player must not start playing
  because the output device changed
* do not let audio sink become a second coordinator

---

## 20. Fallback Matrix

v1 supports exactly one primary video path and one fallback path.

### Preferred

`FFmpeg demux -> D3D11 hw decode -> D3D11 present`

### Fallback A

`FFmpeg demux -> software decode -> D3D11 upload -> D3D11 present`

### Fallback B

Fail open with visible error when no sane path exists.

### Rules

* no silent mode switching without logging/metrics
* once a file/session falls back for stability, keep it on fallback path for that session
* surface current mode in debug info:

  * `HW:D3D11`
  * `SW`

### Explicitly deferred

* CUDA/NVDEC split path
* DXVA2 secondary path
* multiple hardware decode backends in v1

---

## 21. Color / HDR Policy

Correctness and stability take priority over ambitious HDR handling.

### Supported-first policy

* SDR correctness first
* NV12 first-class
* P010 accepted conservatively
* preserve range metadata where possible

### Shipped HDR behavior

* On an HDR-active display (Windows "Use HDR" on), HDR10 PQ and HLG present
  natively (`VideoPresentationPath::HdrPqOutput`): a per-open 10-bit
  `R10G10B10A2` swap chain committed to `RGB_FULL_G2084_NONE_P2020`
  (`SetColorSpace1`), with FastPlay's own pixel shader writing PQ — PQ input
  passes through bit-transparently, HLG is completed to display light
  (BT.2100 OOTF, 1000-nit nominal) and PQ-encoded. Overlays go through a
  PQ-aware shader variant (sRGB → BT.2020 → 203-nit reference → PQ);
  screenshots CPU-tone-map the 10-bit readback to SDR.
* On an SDR display (or when any gate bit is missing), HDR tone-maps to SDR
  through the same shader into the ordinary `B8G8R8A8` chain. In both cases
  the conversion is never the GPU video processor, whose HDR conversions
  are unavailable on real hardware (probed on NVIDIA; see
  `docs/release-notes-v0.4.2.md`).
* The swapchain kind is decided per open (`PresentationPathSelected` event →
  `Presenter::ensure_swapchain_for_path`); it is never changed mid-playback,
  and resize/device recovery rebuild the current kind. A surface whose
  output mode disagrees with the live chain is a typed render error.
* The capability snapshot (`HdrPresentationCapabilities`) is taken on the
  main thread at open from the live swap chain's containing output;
  `display_hdr_active` = the output's desktop color space is G2084/P2020.
* HDR10 static metadata (mastering display, MaxCLL/MaxFALL) is read from the
  first decoded frame's side data and applied to the HDR chain via
  `SetHDRMetaData` (units unit-tested against the MSDN worked example).
  Strictly advisory: absence or failure never gates playback.
* The tone map / HLG OOTF are fixed (203 cd/m² diffuse white, knee 0.75,
  1000-nit HLG peak); static metadata is forwarded to the display but does
  not drive the curve.
* The stream's colorimetry is carried as a validated shader signal
  (`HdrToneMapSignal`: transfer + range), not a DXGI color space — DXGI's
  enum has no full-range-PQ variant, but full-range PQ exists in the wild
  (Topaz Video AI "HDR Enhanced" 8-bit H.264 exports) and plays; the matrix
  validation (BT.2020 NCL or unspecified only) is unchanged.
* Software decoding may reduce 10-bit HDR to 8-bit NV12 before conversion,
  which can introduce banding.
* Classification is conservative: contradictory or incomplete HDR signalling
  is declined at open with a typed error, never guessed into a path.

### Deferred

* metadata-aware / display-peak-adaptive tone mapping
* auto-reopen when the window's monitor or HDR state changes mid-play
  (today the decision is per open; see `docs/TECH_DEBT.md` §6)
* wide gamut correctness polish
* full HDR UX

If the HDR path is uncertain, prefer a documented limitation over incorrect output.

---

## 22. Subtitle Policy

Keep subtitles narrow in v1.

### v1 subtitle scope

* optional external `.srt`
* CPU text layout
* GPU alpha composition during present
* no ASS styling engine

### Composition rule

Subtitle work must not contaminate the video decode hot path.

---

## 23. FFI Boundaries

Unsafe code is boxed into four seams only.

### `ffi::ffmpeg`

Owns:

* FFmpeg contexts
* packet/frame allocation
* probing
* decode
* seek/flush calls
* hw device context setup

### `ffi::d3d11`

Owns:

* D3D11 device/context
* render state objects
* texture/view creation
* hidden surface access

### `ffi::dxgi`

Owns:

* swap chain creation
* resize
* present calls
* frame latency waitable object wiring

### `ffi::wasapi`

Owns:

* audio endpoint/device setup
* `IAudioClient3`
* render client
* audio buffer plumbing

### Rule

No unsafe graphics/audio/media objects in the safe public API.

---

## 24. Metrics Specification

All performance claims are percentile-based and scenario-based.

### Primary metrics

* `open_to_shell_ms`
* `open_to_first_frame_ms`
* `open_to_first_audio_ms`
* `play_to_motion_ms`
* `pause_to_stop_ms`
* `seek_to_first_frame_ms`
* `seek_to_av_settled_ms`
* `resize_recover_ms`
* `fullscreen_toggle_ms`
* `device_recovery_ms`
* `dropped_video_frames`
* `audio_underruns`
* `hw_fallback_count`

### Slice dimensions

Collect by:

* codec
* resolution
* bitrate bucket
* container
* warm vs cold open
* storage class
* GPU model / driver
* display refresh rate

### Reporting style

Use:

* p50
* p95

Do **not** make universal latency promises.

---

## 25. Waitable-Object Latency Hook

This is a **v1.1 benchmark/optimization hook**, not day-one bring-up scope.

### Candidate optimization

* `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`
* `IDXGISwapChain2::GetFrameLatencyWaitableObject()`

### Policy

* default off during bring-up
* benchmark after stable playback exists
* keep only if it improves latency without destabilizing present behavior

---

## 26. Concrete API Skeleton

### Core identity types

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpenGeneration(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SeekGeneration(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OperationId(pub std::num::NonZeroU64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoSurfaceHandle(u64);
```

### Core session type

```rust
pub struct PlaybackSession {
    // concrete coordinator and policy owner
}
```

### Public frame shape

```rust
pub enum DecodedVideoFrame {
    D3D11 {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        pts: std::time::Duration,
        surface: VideoSurfaceHandle,
    },
}
```

### Edge traits

Traits are allowed at subsystem edges if needed but are not currently used.
All subsystems are concrete types. `PlaybackSession` remains concrete.

---

## 27. Build Order

Architecture revisions stop here. Build in this order.

### M0

* concrete `PlaybackSession`
* `SessionEvent`
* Win32 window
* D3D11 device/context
* DXGI flip-model swap chain
* clear/present loop
* resize handling

### M1

* FFmpeg open/probe
* D3D11 hardware decode path
* decode + present first frame
* first-frame metric

### M2

* steady video playback

### M3

* WASAPI `IAudioClient3` sink
* audio master clock
* play/pause responsiveness

### M4

* seek generations
* stale-drop enforcement
* reopen handling
* resize/device/audio-endpoint recovery

### M5

* software fallback path

### Software fallback present-path requirement

Software-decoded frames are uploaded into D3D11 and then presented through the existing video-processor path.

Current practical constraint:
- software-uploaded NV12 textures must be created as decoder-compatible video surfaces for the present path to accept them.

Current implementation detail:
- D3D11 software-upload textures use bind flags:
  - `D3D11_BIND_SHADER_RESOURCE`
  - `D3D11_BIND_DECODER`

This requirement is part of the current fallback-path contract and must be preserved unless the presentation path is explicitly redesigned.

### M6

* subtitle overlay
* polish
* optional waitable-object benchmark pass

---

## 28. First Five Commits

### Commit 1

`init: concrete PlaybackSession, SessionEvent, generations, state machine`

### Commit 2

`render: Win32 window + D3D11 device + flip swap chain`

### Commit 3

`render: opaque surface registry + presenter contract`

### Commit 4

`media: FFmpeg open/probe + D3D11 decode to first frame`

### Commit 5

`app: coordinator tick loop for open -> prime -> first frame`

---

## 29. Hard Invariants

These are blocking architectural rules.

### Invariant 1

No raw pointers or COM interfaces in public structs.

### Invariant 2

`PlaybackSession` is the only coordinator and is a concrete type.

### Invariant 3

All async results carry `(open_gen, seek_gen, op_id)` and stale work is dropped before side effects.

### Invariant 4

Normal steady-state video path is:

```text
FFmpeg -> AV_PIX_FMT_D3D11 -> opaque surface handle -> D3D11 present
```

### Invariant 5

`tick(now)` is UI-thread only, non-reentrant, non-blocking.

---

## 30. Implementation Filter

Every new feature must answer:

**Does this improve first-frame, seek, present, or robustness?**

If not, it waits.

---

## 31. Status

**Architecture locked. Milestones M0–M6 complete.**

Current release: v0.3.0

Implemented:
* single-crate Rust implementation with the module boundaries shown in §5
* concrete `PlaybackSession` and `tick(now)` loop
* focused coordinator-owned helpers for viewport, clip range, overlays, audio,
  video queueing, input dispatch, and decode-thread lifecycle
* D3D11 hw decode + DXGI flip-model present
* WASAPI shared-mode audio with audio-master clock
* seek generations, stale-drop enforcement, reopen handling
* latest-command-wins seek coalescing with in-flight decode cancellation
* software fallback path (D3D11 upload + video-processor present)
* external `.srt` subtitle overlay
* borderless fullscreen, zoom/pan, rotation, resize/device recovery
* timeline scrub overlay with cancel
* recent-files overlay and per-file resume playback position
* file associations and MSI installer
* local p50/p95 benchmark harness with JSON/CSV output
* playback speed control
* in/out point markers
* help overlay (H key)
* Ctrl+drag pan with clamping (content stays visible)
* auto-rotation from stream display matrix metadata (CCW→CW corrected)
* decode info toggle
* 1 ms Windows timer resolution for smooth playback
* audio underrun recovery (clock re-anchor)
* error state idle overlay for recovery
* Start Menu shortcut and custom install directory in MSI
