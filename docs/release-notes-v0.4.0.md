# FastPlay v0.4.0 - Lightweight Play Queue and Folder Playback

FastPlay `0.4.0` adds a lightweight, in-memory play queue: drop several files or
a folder, step through them with `PageUp` / `PageDown`, and have FastPlay
auto-advance to the next file when the current one reaches its natural end. It
stays true to the charter — `PlaybackSession` remains the single, concrete,
single-file coordinator, and the queue is owned by the event loop next to the
recent-files state. No playlists, no media library.

## Highlights

- Added a lightweight play queue from multi-file and folder drops
- Added manual previous/next navigation with `PageUp` / `PageDown`
- Added auto-advance to the next queued file at the natural end of playback
- Preserved single-file behavior: a normal open never scans the parent folder
- Kept `PlaybackSession` a single-file coordinator; the queue lives in the event loop

## New Features

### Play queue

- Drop **multiple files** onto the window to build a multi-item queue
- Drop a **folder** to queue its supported media files (non-recursive)
- Entries are filtered to supported media (subtitles excluded), de-duplicated
  case-insensitively, and ordered naturally (`Episode 2` before `Episode 10`)
- Opening a single file (CLI, file dialog, drag/drop, or Recent Files) creates a
  single-file queue and does **not** scan the parent folder

### Manual navigation

- `PageDown` opens the next queued file; `PageUp` opens the previous one
- Navigation only acts when the queue has more than one item and never wraps at
  the first or last entry
- A brief status overlay reports the result (`Next: …`, `Previous: …`,
  `No next file`, `No previous file`, `Open failed`)

### Auto-advance

- When a file in a multi-item queue reaches its natural end, FastPlay opens the
  next queued file automatically
- Auto-advance does not run for a single-file queue and does not wrap at the end
  of the queue (it shows `End of queue` instead)
- Looping, auto-replay, and in/out range stops are respected: auto-advance does
  **not** fire when you are intentionally looping or replaying a range

## Architecture Notes

- `PlayQueue` is a pure, unit-tested data structure owned by the event loop;
  `PlaybackSession` holds no queue state
- All opens — manual and automatic — flow through the existing
  `open_media -> session.open_at` path, so resume, recent files, subtitles,
  metrics, and in/out reset behave identically to a manual open
- `PlaybackSession` exposes a single new, minimal, edge-triggered
  `take_ended_signal()` that fires once per natural end-of-file transition and is
  consumed exactly once, so auto-advance cannot fire every frame while ended
- Candidate/commit discipline: next/previous lookups never move the cursor; the
  cursor commits only after the target file opens, so a failed open leaves the
  queue position unchanged

## Controls

| Key | Action |
|-----|--------|
| `PageDown` | Next file in the play queue |
| `PageUp` | Previous file in the play queue |

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets` (140 passing)
- `git diff --check`
- Manual playback smoke:
  - drop multiple videos; let the first end; confirm the second opens
  - let the last queued video end; confirm it does not wrap
  - use `PageUp` / `PageDown`; confirm manual navigation works
  - open one normal file; let it end; confirm no parent-folder auto-advance
  - confirm recent files and resume still work
  - confirm loop / auto-replay / in-out range behavior is unaffected

## Upgrade Notes

- Existing installs upgrade in place through the MSI major-upgrade path
- The play queue is in-memory and session-only; nothing new is written to disk
- No account, network, or cloud storage is required

## Deferred (intentionally out of scope)

- Persistent playlist files, shuffle, repeat modes, recursive folder scanning
- A multi-select open dialog (multi-item queues are built via drag-and-drop)
- An in-window queue list / overlay, preloading, and gapless playback
- Media-library behavior
