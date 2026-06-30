# Phase 3: UX & Compatibility Hardening

> [!NOTE]
> Archived historical planning note. This is not current implementation
> guidance. Use `ARCHITECTURE.md`, `docs/ROADMAP.md`, and `docs/TECH_DEBT.md`
> for live project direction.

Small, high-value UX and compatibility items: DPI awareness, Ctrl+O file dialog, subtitle encoding/tolerance, minimum window size, version mismatch cleanup.

## Key Observations from Research

- **Ctrl+O is currently bound to "clear out-point"** (line 1283 of dxgi.rs). This conflicts with the requested "open file" shortcut.
- **ARCHITECTURE.md says `v0.1.3`** (line 941); **Cargo.toml says `0.1.6`**. These are mismatched.
- **No DPI awareness** is currently set. No `WM_DPICHANGED` or `WM_GETMINMAXINFO` handling exists.
- **Subtitle parser** only accepts comma (`,`) as ms separator, only reads UTF-8 via `read_to_string`, no BOM handling, no tag stripping.
- **README.md** documents `Ctrl+O → Clear out-point`. This will need updating.

## User Review Required

> [!IMPORTANT]
> **Ctrl+O keybind conflict:** Currently `Ctrl+O` clears the out-point, `O` sets it. To add `Ctrl+O` as "open file dialog", the clear-out-point keybind must move. Options:
> 1. **(Recommended)** Move clear-out-point to `Shift+O` (mirrors typical NLE conventions), or remove it and require timeline UI interaction to clear.
> 2. Use a different shortcut for open file (e.g., `Ctrl+Shift+O`), but this is non-standard for "open file".
>
> The plan below assumes option 1: `Ctrl+O → open file`, `Shift+O → clear out-point`, `Shift+I → clear in-point` (for symmetry).

> [!WARNING]
> **Behavioral change:** The `Ctrl+O` and `Ctrl+I` keybinds will change meaning. This is a user-facing behavior change that must be reflected in README.md and the help overlay.

## Proposed Changes

### A. DPI Awareness

#### [MODIFY] [Cargo.toml](../../Cargo.toml)
- Add `"Win32_UI_HiDpi"` feature to the `windows` dependency for `SetProcessDpiAwarenessContext` and `GetDpiForWindow`.

#### [MODIFY] [runtime.rs](../../src/ffi/runtime.rs)
- Add `set_dpi_awareness()` function that calls `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` with fallback to `SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE)` for older Windows 10.
- Keep both paths behind safe wrappers. The function is called once at startup before any window creation.

#### [MODIFY] [main.rs](../../src/main.rs)
- Call `ffi::runtime::set_dpi_awareness()` at the top of `run()`, before window creation.

#### [MODIFY] [dxgi.rs](../../src/ffi/dxgi.rs)
- Handle `WM_DPICHANGED` in `window_proc`: read the suggested `RECT` from `lparam` and call `SetWindowPos` to resize/reposition the window. Post a `ResizeRequest` so the swap chain resizes on the next tick.
- Update `adjust_window_size()` to use DPI-aware `AdjustWindowRectExForDpi` when available (with fallback to current `AdjustWindowRectEx`).
- Add `GetDpiForWindow` import.

**Rendering/swap-chain impact:** The swap chain already auto-sizes from `WM_SIZE` client rect, and `ResizeBuffers` uses client-area pixels. DPI changes trigger `WM_SIZE` after `SetWindowPos`, so the existing resize path handles it. No swap-chain code changes needed.

---

### B. Ctrl+O Native Open Dialog

#### [MODIFY] [Cargo.toml](../../Cargo.toml)
- Add `"Win32_UI_Controls"` feature (needed for `IFileOpenDialog` / Common Item Dialog).

#### [MODIFY] [input.rs](../../src/platform/input.rs)
- Add `OpenFileDialog` variant to `InputEvent`.

#### [MODIFY] [dxgi.rs](../../src/ffi/dxgi.rs)
- In `WM_KEYDOWN`, change `0x4F if ctrl_held` from `ClearOutPoint` to `OpenFileDialog`.
- Change `0x49 if ctrl_held` from `ClearInPoint` to use shift detection instead.
- Add shift-key detection: `Shift+O → ClearOutPoint`, `Shift+I → ClearInPoint`.

#### [MODIFY] [main.rs](../../src/main.rs)
- Add handler for `InputEvent::OpenFileDialog`:
  - Call a new `show_open_file_dialog(hwnd)` function.
  - On success, feed the path through `session.open(source, now)` and update title (same as `FileDropped`).
  - On cancel, do nothing.

#### [NEW] [open_dialog.rs](../../src/platform/open_dialog.rs)
- Implement `show_open_file_dialog(hwnd: HWND) -> Option<PathBuf>` using the native `IFileOpenDialog` COM interface.
- Filter: "Media Files" (`.mp4`, `.mkv`, `.avi`, `.mov`, `.webm`, `.wmv`, `.flv`, `.m4v`, `.ts`, `.mpg`, `.mpeg`, `.mp3`, `.flac`, `.wav`, `.ogg`, `.aac`, `.m4a`), plus "All Files (`*.*`)".
- Cancel returns `None`.
- No menu bar added.

#### [MODIFY] [mod.rs](../../src/platform/mod.rs)
- Add `pub mod open_dialog;`.

---

### C. Subtitle Tolerance

#### [MODIFY] [subtitle.rs](../../src/media/subtitle.rs)

1. **Period/comma ms separator:** Change `parse_timestamp` to accept both `,` and `.` as the millisecond separator. Try `,` first, then `.`.
2. **UTF-8 BOM:** In `load_sidecar`, read raw bytes first (`std::fs::read`), strip UTF-8 BOM (`EF BB BF`) if present, then attempt UTF-8 decode.
3. **Windows-1252 fallback:** If UTF-8 decode fails, fall back to decoding the bytes as Windows-1252 (a simple 256-entry table, no external dependency needed).
4. **HTML tag stripping:** After collecting cue text, strip `<b>`, `</b>`, `<i>`, `</i>`, `<u>`, `</u>`, `<font ...>`, `</font>` tags. Use a simple regex-free approach (scan for `<` and `>`, match known tag names).

---

### D. Minimum Window Size

#### [MODIFY] [dxgi.rs](../../src/ffi/dxgi.rs)

- Handle `WM_GETMINMAXINFO` in `window_proc`:
  - Set `ptMinTrackSize` to 640×360 (adjusted for window chrome via `adjust_window_size`).
  - This prevents the window from being resized to invalid tiny sizes.
- Add the `WM_GETMINMAXINFO` import and `MINMAXINFO` struct usage.
- The swap chain resize path already guards `width == 0 || height == 0` (line 916), so the minimum size enforcement is additive safety.

---

### E. Version Mismatch Cleanup

#### [MODIFY] [ARCHITECTURE.md](../../ARCHITECTURE.md)
- Update line 941 from `Current release: v0.1.3` to `Current release: v0.1.6` to match Cargo.toml.

---

### F. README / Help Overlay Updates

#### [MODIFY] [README.md](../../README.md)
- Update keybind table: `Ctrl+O` → Open file dialog, `Shift+O` → Clear out-point, `Shift+I` → Clear in-point.
- Add note about SRT comma/period tolerance and encoding fallback under "External subtitles".
- Document DPI awareness briefly.

#### [MODIFY] [session.rs](../../src/app/session.rs) (help overlay text only)
- Update the help overlay text to reflect the new Ctrl+O, Shift+O, Shift+I bindings. This is a string-only change, not a structural refactor.

---

## Verification Plan

### Automated Tests
```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Manual Verification
1. **Ctrl+O** opens the native Windows file dialog.
2. Canceling Ctrl+O does nothing.
3. Selecting a media file opens it through the existing path.
4. **Shift+O** clears the out-point (replacing Ctrl+O).
5. **Shift+I** clears the in-point (replacing Ctrl+I).
6. Window cannot be resized below 640×360.
7. Title bar / open behavior still works after DPI awareness is set.
8. UTF-8 BOM SRT loads correctly.
9. SRT with period milliseconds (`00:00:01.500`) loads correctly.
10. Windows-1252 SRT fallback works (testable with a Latin-1 encoded SRT file).
11. Simple SRT tags (`<b>`, `<i>`, `<font>`) do not display literally.
12. DPI awareness is enabled — honest note: without multi-monitor setups, `WM_DPICHANGED` handling is code-reviewed but not field-tested. The implementation uses the standard Windows-recommended `SetWindowPos` with the suggested rect.
13. ARCHITECTURE.md version now reads `v0.1.6`.

### Files Changed Summary
| File | Change |
|------|--------|
| `Cargo.toml` | Add `Win32_UI_HiDpi`, `Win32_UI_Controls` features |
| `src/ffi/runtime.rs` | Add `set_dpi_awareness()` |
| `src/main.rs` | Call DPI awareness, handle `OpenFileDialog` event |
| `src/ffi/dxgi.rs` | `WM_DPICHANGED`, `WM_GETMINMAXINFO`, keybind changes |
| `src/platform/input.rs` | Add `OpenFileDialog` variant |
| `src/platform/open_dialog.rs` | **[NEW]** native file open dialog |
| `src/platform/mod.rs` | Add `open_dialog` module |
| `src/media/subtitle.rs` | Period/comma, BOM, Win-1252, tag stripping |
| `ARCHITECTURE.md` | Version bump to v0.1.6 |
| `README.md` | Updated keybinds, subtitle docs, DPI note |
| `src/app/session.rs` | Help overlay text update (string only) |

