# FastPlay v0.3.0 - Recent Files, Resume Playback, and Seek Reliability

FastPlay `0.3.0` adds the first daily-use product basics: recent files and resume playback position. It also fixes important seek/scrub reliability issues and completes a major internal cleanup of the playback coordinator.

## Highlights

- Added Recent Files overlay with `Ctrl+Shift+O`
- Added resume playback position across CLI, dialog, drag/drop, and recent-file opens
- Fixed A/V desync after backward seeks
- Fixed resumed-file scrubbing mapping that could make playback stop near EOF
- Added a local benchmark harness for open, seek, pause/resume, drops, underruns, and hardware fallback metrics
- Refactored the large playback coordinator into focused tested helpers

## New Features

### Recent Files

- Press `Ctrl+Shift+O` to open the Recent Files overlay
- Use `Up` / `Down` to select a file
- Press `Enter` to reopen
- Press `Delete` to remove an entry
- Recent files are capped at 20 and deduplicated

### Resume Playback Position

FastPlay now remembers the last playback position for opened files and resumes when appropriate.

Resume applies to:

- CLI path open
- File dialog open
- Drag/drop open
- Recent Files overlay open

FastPlay avoids resuming near the end of a video.

## Fixes and Improvements

### Seeking and Scrubbing

- Fixed backward-seek A/V desync caused by stale pre-seek audio batch timestamps
- Fixed resumed-file seek/scrub mapping where resume-origin offset could push seeks past EOF
- Improved repeated seek/scrub reliability
- Preserved A/V sync after backward seeks

### Architecture and Maintainability

- Enforced `cargo fmt --check` in CI
- Added technical debt and roadmap documentation
- Extracted focused modules for:
  - viewport state
  - clip-range state
  - overlay management
  - audio coordination
  - video frame queue
  - input dispatch
  - decode-thread handle lifecycle

### Benchmarking

- Added local benchmark harness under `bench/`
- Captures p50/p95 latency for open, seek, pause/resume
- Captures frame drops, audio underruns, and hardware fallback counts
- Benchmark output supports JSON and CSV

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`
- `git diff --check`
- Manual playback smoke:
  - open
  - play/pause/resume
  - seek forward/backward
  - repeated scrub
  - recent-file reopen
  - resume playback
  - close while playing/paused
  - A/V sync after backward seek

## Upgrade Notes

- Existing installs should upgrade in place through the MSI major-upgrade path
- Recent/resume data is stored under `%APPDATA%\FastPlay`
- No account, network, or cloud storage is required
