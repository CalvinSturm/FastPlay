## Summary

<!-- What user-visible problem does this change solve, and how? -->

## Scope

<!-- List the focused changes. Link the issue with "Closes #123" when applicable. -->

## Architecture and Risk

- [ ] I read `ARCHITECTURE.md`.
- [ ] This change preserves existing module and ownership boundaries.
- [ ] Unsafe code remains contained in `src/ffi/*`.
- [ ] Public GPU/video APIs remain opaque-handle based.
- [ ] `PlaybackSession` remains the concrete single coordinator.
- [ ] Async completions flow through `SessionEvent`, with stale work rejected before side effects.
- [ ] The normal video path remains GPU-resident without CPU copy-back.
- [ ] This change does not add an out-of-scope feature.

<!-- If an item is not applicable, explain why. Call out any architecture conflict explicitly. -->

## Validation

- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all-targets`
- [ ] `cargo build --release`
- [ ] Relevant Windows playback scenarios were tested manually.

<!-- List media characteristics, decode modes, and scenarios tested. -->

## Performance

<!-- For latency or hot-path changes, include before/after p50 and p95 measurements plus environment details. Otherwise write "Not applicable." -->

## Assumptions and Deferred Work

<!-- List assumptions, known limitations, deferred work, and TODOs. -->

## Documentation

- [ ] User-facing behavior, controls, setup, and limitations are documented.
- [ ] No documentation change is required.
