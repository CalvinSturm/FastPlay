# FastPlay v0.1.6

FastPlay `0.1.6` is a stability and smoothness release: it fixes several crashes around hardware decoding, scrubbing, and shutdown, and removes per-frame overhead from the playback hot path.

## Highlights

- fixed a hard crash (`d3d11.dll` access violation) during rapid hardware→software scrub transitions
- fixed a crash when closing the window after a clip finished playing
- smoother hardware playback: removed a per-frame GPU stall that blocked presentation every frame
- leaner rendering: dropped a per-frame video-processor cache that added work and a dangling-pointer hazard for no benefit
- crash reports now include a symbolized backtrace, making faults far easier to diagnose

## Fixes and Improvements

### Stability and Crashes

- **HW→SW scrub crash:** the hardware decoder's teardown (`avcodec_free_context`) now runs under the shared D3D11 context lock, so a cancelled hardware worker can no longer corrupt the immediate context while the live worker or UI thread is presenting — the access violation that reproduced under timeline stress-scrubbing on some files
- **Shutdown crash:** closing the window — most reliably after a clip had finished playing — could fault while releasing D3D11 resources. The D3D11 device is now ordered to outlive the swap chain, surfaces, overlays, and contexts created from it; the window HWND is destroyed only after those resources are released (correct DXGI teardown order); and an explicit ordered shutdown idles the GPU and waits for the decode worker before exit
- **Removed a dangling-pointer hazard:** the video-processor input-view cache keyed on texture-pointer identity could match a stale entry after the allocator reused a freed texture address; it has been removed entirely (it never produced a useful cache hit)

### Performance

- removed the per-frame GPU flush after each hardware frame copy — it was a busy-wait held under the context lock, blocking UI presentation once per frame and allocating a query object each frame; immediate-context submission order already guarantees correctness
- the surface registry now reclaims its backing storage once it empties, instead of growing unbounded across long playback and heavy scrubbing

### Diagnostics

- the vectored crash handler now captures and writes a symbolized backtrace to `crash.log` for access violations, turning an opaque fault address into the exact faulting call chain

## Upgrade Notes

- existing MSI installs upgrade in place through the WiX `MajorUpgrade` path
- crash diagnostics in `%APPDATA%\FastPlay\crash.log` now include a backtrace section in addition to the fault address
