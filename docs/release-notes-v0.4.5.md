# FastPlay v0.4.5

FastPlay `0.4.5` is a reliability release. Its headline fix is a leak that
only bit people who keep many players open at once: with a dozen instances
running, one would eventually freeze, refuse to close, and sometimes take
other applications — including Windows Explorer — down with it.

## Fixes

### Many instances open → a frozen, unclosable window

Release builds never freed a single GDI object. Every cleanup call in the
overlay rasterizers was written `debug_assert!(DeleteObject(..))`, and
`debug_assert!` does not merely skip the *check* when assertions are off — it
does not evaluate the expression at all. All 36 delete sites across the seven
rasterizers (subtitle, timeline, timeline label, idle, help, recent, volume)
were affected: roughly 8.5 handles leaked per timeline-overlay rebuild,
measured 25 → 709 over 40 seek pairs.

Windows allows 10,000 GDI handles per process but only 65,536 across a
desktop session, so a dozen instances exhausted the *shared* pool at ~5,400
each — before any single player hit its own limit. Every process in the
session then began failing GDI allocations, which is why unrelated
applications crashed alongside FastPlay.

Six `CreateDIBSection` sites also leaked the font and device context on their
error path. All deletes now route through helpers that run in every build,
with a test that fails if a raw delete reappears outside them.

### A fatal error left the window hung instead of reporting

Closing FastPlay deliberately stops its workers and exits without releasing
the D3D11 device in-process — that release intermittently faults inside the
graphics driver, and Windows Error Reporting then freezes the still-visible
window for seconds while it writes a crash dump.

Every error path bypassed that discipline. A fatal error returned straight
out of the event loop, dropping the session on the way — releasing the device
while the decode and audio workers were still running inside those very
drivers. So the GDI failure above surfaced not as an error message but as a
hung window parked in WER. Clean close and fatal error now take the same
route: persist progress, stop workers, flush the trace, exit.

### Diagnostics survived only one instance

Every player wrote the same `session.log` and `crash.log`, truncating on each
write, so concurrent instances erased each other's traces — the evidence for
the freeze above had to be reconstructed from the Windows event log instead.
Each run now writes `session-<utc-stamp>-<pid>.log` and its crash counterpart,
swept after 7 days.

## Also in this release

Work that landed since `0.4.4` and ships here for the first time:

- audio-only files reach the end-of-file state on seek, the error overlay
  displays rather than failing silently, and demuxer packet leaks are closed
- playback follows the default audio endpoint when Windows switches devices,
  with deduplicated recovery that preserves play/pause state
- COM notification teardown ordering fixed; recent-file writes are atomic
- the pure overlay rasterizer and the keyboard keymap moved out of the unsafe
  FFI seams into tested modules

## Validation

- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test`
  (257 passing, including new UTC-stamp, retention, GDI source-invariant, and
  run-log resolution tests), `cargo build --release`, `cargo wix`
- GDI growth over 40 seek pairs: +708 → 0, flat across two runs
- Fault injection at the real GDI failure site: before, no worker-exit lines
  in the trace and the device released under live workers; after, workers
  stop first, exit code 1, no Windows Error Reporting event
- **Development build**, twelve concurrent instances tiled so none is occluded,
  40 seek pairs each: GDI handles 300 → 606 and climbing on `0.4.4`, flat at
  192 on `0.4.5`; all twelve closed from the title-bar X, and each wrote its
  own intact log
- **Installed MSI**, same twelve-instance test against
  `C:\Program Files\fastplay\bin\fastplay.exe`, plus in-place upgrade over
  `0.4.4`, Start Menu shortcut, `.mp4` association, and uninstall/reinstall

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- No account, network, or cloud storage is required.
- Old `session.log` / `crash.log` files are not read any more and can be
  deleted; new per-run logs are swept automatically after a week.
