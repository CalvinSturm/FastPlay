# AGENTS.md

## Repo intent

Windows-first, latency-focused media player in Rust.

Read `ARCHITECTURE.md` before making changes. Treat it as the source of truth for architecture, ownership, invariants, milestones, and scope.

## Working rules

- Do not revise architecture unless explicitly asked.
- Implement one milestone at a time.
- Do not jump ahead to future milestones.
- Prefer minimal, high-confidence changes over broad rewrites.
- Preserve existing module boundaries unless the task explicitly requires a change.
- Keep the repository as a single Rust crate unless explicitly told otherwise.

## Safety and API boundaries

- Keep unsafe code boxed inside `src/ffi/*`.
- Do not expose raw pointers or COM interfaces in public Rust APIs.
- Public GPU/video surfaces must remain opaque-handle based.
- `PlaybackSession` must remain a concrete type.
- Workers must not mutate playback session state directly.
- Route async completions through `SessionEvent`.

## Performance priorities

Optimize in this order:

1. open-to-first-frame latency
2. seek responsiveness
3. present-path stability
4. pause/resume immediacy
5. robustness under resize, reopen, and device churn

Do not add features that do not clearly improve first-frame, seek, present, or robustness unless explicitly requested.

## Windows playback constraints

- Primary video path: FFmpeg -> `AV_PIX_FMT_D3D11` -> D3D11 present
- Presentation path: DXGI flip-model swap chain
- Audio path: WASAPI / `IAudioClient3`
- No CPU copy-back in the normal steady-state playback path unless the session is explicitly in fallback mode.
- No exclusive fullscreen support in v1.
- Do not use `DXGI_PRESENT_RESTART` in the normal windowed or borderless resize path.

## Scope discipline

Do not add any of the following unless explicitly requested:

- streaming
- playlists or media library
- browser/web UI
- plugin system
- HDR tone mapping
- frame interpolation
- CUDA/NVDEC split path
- extra hardware decode backends
- subtitle styling engine
- cross-platform abstractions

## Validation

After code changes:

- build the project
- report any compiler errors clearly
- summarize file-by-file changes
- list assumptions, deferred work, and known TODOs
- call out any conflict between the task and `ARCHITECTURE.md`

## When uncertain

- Prefer the simpler implementation that preserves the architecture contract.
- Ask for clarification only when a decision would violate architecture or materially change scope.
- If correcting a repeated mistake, update `AGENTS.md` so the rule persists.