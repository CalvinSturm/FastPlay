# FastPlay v0.5.0

FastPlay `0.5.0` is the **Pro Preview Foundation** release. It adds the
licensing and review-workflow base needed for paid creator tooling while keeping
normal local playback free.

This is not a broad product-feature release. Batch screenshots from markers are
still deferred.

## Highlights

- Added timestamp markers for the current media file.
- Added marker notes with local persistence.
- Added marker export to `.txt` and `.csv`.
- Added saved review queues for reopening a review set later.
- Added Lemon Squeezy FastPlay Pro activation, validation, and deactivation.
- Kept normal playback, seeking, queue playback, resume, screenshots,
  subtitles, in/out range, and loop range available in Free mode.

## Pro Preview workflow

FastPlay Pro is the review-workflow layer for creators, editors, QA testers, and
power users. The paid value is saving time while reviewing footage, not gating
local playback.

Implemented in this foundation release:

- marker creation for the current media file
- bounded marker note editing from the keyboard
- marker overlay selection, seek, and removal
- marker persistence under local app data
- marker export for the current file
- saved review queue save, load, and delete UI
- app-local Lemon Squeezy license metadata and lifecycle controls
- centralized Free/Pro capability checks

Deferred:

- reliable batch screenshots from markers

## License controls

- `Ctrl+Shift+L`: enter and activate a FastPlay Pro license key
- `Ctrl+Shift+V`: validate the saved FastPlay Pro license
- `Ctrl+Shift+D`: deactivate the saved FastPlay Pro license

Normal playback does not require an account, license key, network request, or
server availability.

## Technical notes

- `src/license.rs` owns license state and capability checks.
- Stored license metadata lives under `%APPDATA%\FastPlay\license.tsv`.
- Marker data lives under `%APPDATA%\FastPlay\review_markers.tsv`.
- Saved review queues live under `%APPDATA%\FastPlay\review_queues.tsv`.
- Startup license validation runs in a background thread when a stored license
  exists, so it does not block the playback open path.
- No architecture changes: `PlaybackSession` remains the single concrete
  playback coordinator, and Pro workflow state stays outside decode, present,
  and worker hot paths.

## Validation

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets` (169 passing)
- `cargo build --release`
- `cargo wix`

## Manual smoke before tag

- Launch app
- Open a normal video
- Open a 4K/60 video if available
- Confirm playback controls still work
- Confirm close/shutdown works cleanly
- Install MSI
- Launch from Start Menu
- Open an associated video file if association is expected
- Uninstall MSI
- Activate Pro with a real Lemon Squeezy key
- Validate Pro
- Deactivate Pro
- Try an invalid key
- Try offline/no-network behavior

## Upgrade notes

- Existing installs upgrade in place through the MSI major-upgrade path.
- Free playback remains available without Pro activation.
