# FastPlay Pro Foundation

FastPlay Pro is an app-level review workflow layer. It must not gate normal local playback, seeking, queue playback, resume, screenshots, subtitles, in/out range, or loop range.

## License and capability boundary

`src/license.rs` owns the tier and feature capability checks. Feature code should call methods such as `can_save_markers()` or `can_export_markers()` instead of scattering tier comparisons.

Current activation is intentionally minimal:

- default tier: FastPlay Free
- development-only override: `FASTPLAY_PRO_DEV=1`
- no license secrets are stored
- no telemetry, account creation, DRM, or server dependency

The Lemon Squeezy activation PR should replace or extend the tier source while preserving the centralized capability methods.

## Marker storage

Review markers are owned outside `PlaybackSession`, beside Recent and PlayQueue state in the main event loop. This keeps playback coordination inside the concrete session and avoids touching the D3D11 decode/present hot path.

Markers are persisted under `%APPDATA%\FastPlay\review_markers.tsv`. Each marker records:

- media path
- timestamp in milliseconds
- optional note field

The TSV fields are escaped so notes can later contain tabs, newlines, quotes, or commas without adding a JSON dependency. Marker exports are written to the same default area as screenshots: `%USERPROFILE%\Pictures\FastPlay` when available.

## Review queue storage

Saved review queue storage is introduced as a foundation under `%APPDATA%\FastPlay\review_queues.tsv`. The current PR does not add a full saved-queue UI. When a saved queue is materialized, missing media files should be skipped gracefully.

## Adding Pro features

New Pro review tools should follow these rules:

- keep playback useful in Free
- gate through `LicenseState` capability methods
- keep app-data sidecar files out of the media directory
- keep workflow state outside decode, present, and worker code unless playback correctness requires otherwise
- prefer keyboard-first, non-modal status messages over nags
- do not add subscriptions, DRM, telemetry, accounts, streaming, media-library behavior, or plugin support
