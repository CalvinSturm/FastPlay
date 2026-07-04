# FastPlay Pro Foundation

FastPlay Pro is an app-level review workflow layer. It must not gate normal local playback, seeking, queue playback, resume, screenshots, subtitles, in/out range, or loop range.

## License and capability boundary

`src/license.rs` owns the tier and feature capability checks. Feature code should call methods such as `can_save_markers()` or `can_export_markers()` instead of scattering tier comparisons.

Current activation is intentionally app-local:

- default tier: FastPlay Free
- development-only override: `FASTPLAY_PRO_DEV=1`
- Lemon Squeezy license activate, validate, and deactivate calls
- local license metadata under `%APPDATA%\FastPlay\license.tsv`
- no telemetry, account creation, DRM, or server dependency for normal playback

`license.tsv` stores the license key and instance id needed by Lemon Squeezy for later validation and deactivation. This is local app metadata, not secure secret storage, and FastPlay does not present it as DRM.

License controls are intentionally keyboard-first and non-blocking to normal playback:

- `Ctrl+Shift+L`: enter and activate a license key
- `Ctrl+Shift+V`: validate the saved license
- `Ctrl+Shift+D`: deactivate the saved license

Startup validation runs in a background thread when a stored license exists so the player does not block the playback open path on a network request. Pro capability checks still flow through `LicenseState`.

## Marker storage

Review markers are owned outside `PlaybackSession`, beside Recent and PlayQueue state in the main event loop. This keeps playback coordination inside the concrete session and avoids touching the D3D11 decode/present hot path.

Markers are persisted under `%APPDATA%\FastPlay\review_markers.tsv`. Each marker records:

- media path
- timestamp in milliseconds
- optional note field, bounded to 240 characters

The TSV fields are escaped so notes can contain tabs, newlines, quotes, or commas without adding a JSON dependency. Marker exports are written to the same default area as screenshots: `%USERPROFILE%\Pictures\FastPlay` when available.

Marker notes are edited from the marker overlay: `Ctrl+M`, select a marker, `N`, type the note, `Enter` to save, or `Esc` to cancel. Free mode shows the Pro review-tools copy and does not save notes.

## Review queue storage

Saved review queues live under `%APPDATA%\FastPlay\review_queues.tsv`. The UI is intentionally minimal:

- `Ctrl+Shift+S`: save the current queue as a named review queue
- `Ctrl+Shift+Q`: open the saved review queue overlay
- arrow keys: select a saved queue
- `Enter`: load the selected queue
- `Delete`: delete the selected queue
- `Esc`: close the overlay

`Ctrl+Shift+O` remains the existing Recent Files overlay, so review queues use `Ctrl+Shift+Q` instead. Loading a saved queue skips missing files gracefully and reports how many were skipped. This is saved queue workflow, not a media library.

## Deferred Pro work

Still deferred:

- reliable batch screenshots from marker timestamps

## Adding Pro features

New Pro review tools should follow these rules:

- keep playback useful in Free
- gate through `LicenseState` capability methods
- keep app-data sidecar files out of the media directory
- keep workflow state outside decode, present, and worker code unless playback correctness requires otherwise
- prefer keyboard-first, non-modal status messages over nags
- do not add subscriptions, DRM, telemetry, accounts, streaming, media-library behavior, or plugin support
