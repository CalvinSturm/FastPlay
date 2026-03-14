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
- HDR tone mapping
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

    app/
      mod.rs
      session.rs
      state.rs
      events.rs
      commands.rs

    playback/
      mod.rs
      clock.rs
      metrics.rs
      queues.rs
      generations.rs

    media/
      mod.rs
      source.rs
      video.rs
      audio.rs
      seek.rs

    render/
      mod.rs
      presenter.rs
      swapchain.rs
      surface_registry.rs

    audio/
      mod.rs
      sink.rs

    platform/
      mod.rs
      window.rs
      input.rs

    ffi/
      mod.rs
      ffmpeg.rs
      d3d11.rs
      dxgi.rs
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
* audio endpoint recovery detection
* audio clock reporting

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
    VideoFrameReady(DecodedVideoFrame),
    AudioFrameReady(DecodedAudioFrame),
    OpenFailed {
        open_gen: OpenGeneration,
        op_id: OperationId,
        error: String,
    },
    DeviceLost {
        op_id: OperationId,
    },
    AudioEndpointChanged {
        op_id: OperationId,
    },
}
```

This preserves the **single-coordinator rule**.

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
    registry_epoch: u64,
    open_gen: OpenGeneration,
    seek_gen: SeekGeneration,
    // hidden texture/view refs
}
```

### Rules

* registry epoch increments on device rebuild / presenter reset
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
Closing
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

* `Any -> Closing`

  * teardown in progress

---

## 12. Threading Model

v1 keeps the thread model small.

### Threads

* UI/render thread
* demux/decode worker(s)
* audio worker or sink thread as required by implementation

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

Audio endpoint changes are part of v1 robustness testing.

### On endpoint change

* detect
* flush/rebuild sink as needed
* preserve session intent if possible
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

v1 prioritizes correctness and stability over ambitious HDR handling.

### Supported-first policy

* SDR correctness first
* NV12 first-class
* P010 accepted conservatively
* preserve range metadata where possible

### Deferred

* advanced HDR tone mapping
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
        op_id: OperationId,
        pts: std::time::Duration,
        width: u32,
        height: u32,
        surface: VideoSurfaceHandle,
    },
    Software {
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        pts: std::time::Duration,
        width: u32,
        height: u32,
        planes: Vec<Vec<u8>>,
        strides: Vec<usize>,
    },
}
```

### Edge traits

Traits are allowed at subsystem edges only.

* `VideoDecoder`
* `AudioDecoder`
* `SwapChainPresenter`
* `AudioSink`
* `MetricsSink`

`PlaybackSession` remains concrete.

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

**Architecture locked.**
Begin implementation.

Next artifact:

* repo bootstrap
* module stubs
* concrete `PlaybackSession`
* `SessionEvent`
* empty `tick(now)` loop
* D3D11 swap-chain bring-up