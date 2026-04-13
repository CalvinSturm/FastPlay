# FastPlay v0.1.4

FastPlay `0.1.4` focuses on seek-path robustness and Windows playback stability.

## Highlights

- improved stability during rapid timeline seeking and scrub-heavy playback
- fixed a crash path triggered by active timeline scrub overlay rendering on some D3D11 systems
- hardened hardware playback around rapid seek churn, surface release, and presenter cache invalidation
- improved fullscreen and timeline interaction stability during fast input
- added Windows app settings persistence for user-facing playback state

## Fixes and Improvements

### Seek and Scrub Stability

- reduced crash risk during repeated timeline clicks and drag scrubbing
- disabled the timeline overlay during active scrub drag to avoid a confirmed D3D11 crash path
- dropped stale present surfaces more aggressively during seeks
- improved handling of seek-worker backpressure and rapid seek churn

### D3D11 / Present Path Hardening

- tightened D3D11 context coordination between decode and present work
- invalidated stale video-processor input-view cache entries when surfaces are released
- flushed relevant presenter-side cache state during surface resets
- added more defensive synchronization around hardware surface copy behavior

### Windows Playback Polish

- improved fullscreen-toggle stability during fast timeline interaction
- kept the release MSI/app version aligned at `0.1.4`
- updated release documentation for MSI packaging and release flow

## Upgrade Notes

- existing MSI installs should upgrade in place through the existing WiX `MajorUpgrade` path
- the timeline overlay is intentionally hidden while the mouse is actively scrubbing; it returns once scrubbing ends
