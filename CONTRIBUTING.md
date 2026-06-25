# Contributing to FastPlay

FastPlay is a Windows-first, latency-focused media player. Contributions should
preserve its narrow scope and prioritize first-frame latency, seek
responsiveness, present-path stability, pause/resume immediacy, or robustness.

## Before You Start

Read [`ARCHITECTURE.md`](./ARCHITECTURE.md) before proposing or making changes.
It is the source of truth for architecture, ownership, invariants, and scope.
The architecture is locked unless a change is required to fix a correctness or
performance bug.

For substantial work, open an issue first so scope and approach can be agreed
before implementation. Security vulnerabilities must be reported according to
[`SECURITY.md`](./SECURITY.md), not in a public issue.

## Development Setup

You need:

- Windows 10 or later
- A stable Rust toolchain with Clippy
- FFmpeg development headers, import libraries, and runtime DLLs
- A D3D11, DXGI, and WASAPI-capable system for runtime testing

Configure FFmpeg with `FFMPEG_DIR`, or use the discovery options documented in
[`README.md`](./README.md#ffmpeg-setup).

Build and run:

```powershell
cargo build --release
cargo run --release -- <path-to-media>
```

Force the software decode fallback:

```powershell
cargo run --release -- --force-sw <path-to-media>
```

## Contribution Rules

- Keep the repository as a single Rust crate.
- Prefer small, reviewable, high-confidence changes.
- Preserve existing module and ownership boundaries.
- Keep unsafe code inside `src/ffi/*`.
- Do not expose raw pointers or COM interfaces through public Rust APIs.
- Keep public GPU/video surfaces opaque-handle based.
- Keep `PlaybackSession` concrete and as the single coordinator.
- Route worker completions through `SessionEvent`; workers must not mutate
  playback session state directly.
- Preserve `(open_gen, seek_gen, op_id)` stale-work rejection before side
  effects.
- Do not add CPU copy-back to normal steady-state playback.
- Do not add out-of-scope features listed in `ARCHITECTURE.md` unless explicitly
  approved.

Do not casually change the software-fallback texture creation flags, decoder
lifetime behavior at logical clip boundaries, or normal resize/present flags.
These details have runtime correctness constraints documented in
`ARCHITECTURE.md`.

## Testing

Before submitting a pull request, run:

```powershell
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

For playback-path changes, test affected scenarios on Windows with representative
local media. Depending on the change, include hardware decode, `--force-sw`,
seek/scrub, pause/resume, resize, borderless fullscreen, reopen, and device or
audio-endpoint recovery.

Performance changes should include before/after measurements using the relevant
FastPlay metrics. Prefer p50 and p95 results, and report the media codec,
resolution, container, storage class, GPU/driver, display refresh rate, and
whether the run was warm or cold.

The repository does not currently enforce `rustfmt` in CI because the existing
codebase is not rustfmt-clean. Avoid unrelated formatting churn.

## Commits and Pull Requests

Use concise, imperative commit subjects. Existing history generally follows
the form `area: description`, such as `fix: ...`, `render: ...`, or `media: ...`.

Pull requests should:

- Explain the user-visible problem and the chosen solution.
- Stay focused on one coherent change.
- Link the relevant issue when one exists.
- Identify architecture invariants and hot paths affected.
- Include validation commands and manual playback scenarios.
- Include before/after metrics for latency or performance claims.
- Call out assumptions, known limitations, deferred work, and risks.
- Update documentation when behavior, controls, setup, or limitations change.

By contributing, you agree that your contribution is licensed under the
project's [MIT License](./LICENSE).
