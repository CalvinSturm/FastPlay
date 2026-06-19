# FastPlay v0.2.2

FastPlay `0.2.2` is a clip-range playback stability hotfix over `0.2.1`. It fixes freezes, crashes, lag, and inconsistent Space behavior when timeline seeks land before an I marker or after an O marker.

## Highlights

- fixed a D3D11 access violation caused by ending an I/O clip while the presenter retained a decoded frame
- fixed lag and jitter when clicking the timeline to the right of the O marker during playback
- fixed Space being ignored or producing inconsistent playback while a timeline seek was still in flight
- resuming from outside the active I/O range now restarts at the I marker, or at the beginning when no I marker is set

## Fixes and Improvements

### Clip ranges and timeline seeking

- clip boundaries now stop playback logically while keeping the persistent decoder parked for safe, low-latency replay
- audio and video submission remain stopped in the ended state instead of consuming work beyond the clip boundary
- outside-range timeline clicks land paused for inspection rather than issuing a second resume seek that immediately collides with out-point enforcement
- pause/resume intent is preserved while a seek is in `Seeking` or seek-related `Priming`
- replay and resume reuse the persistent decoder through an in-place seek instead of asynchronously tearing down D3D11VA

## Validation

- repeated timeline clicks before I and after O
- Space pause/resume during and after timeline seeks
- replay from the I marker after landing outside the active range
- 27 automated tests pass
- release build and WiX MSI packaging pass

## Upgrade Notes

- existing MSI installs upgrade in place through the WiX `MajorUpgrade` path
- no settings or configuration changes are required
