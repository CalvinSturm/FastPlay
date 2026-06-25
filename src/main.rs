#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Baseline allow-list for CI: these categories are pervasive in the Win32/FFI
// shims (self-transmutes through typed HANDLEs, Win32 naming conventions,
// high-arity GPU render calls) or represent stylistic debt we haven't paid
// down yet. New violations in other categories still fail `-D warnings`.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::useless_transmute)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::type_complexity)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::manual_dangling_ptr)]
#![allow(clippy::cmp_null)]

#[macro_use]
mod logging;
mod app;
mod audio;
mod ffi;
mod media;
mod platform;
mod playback;
mod render;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use app::commands::SessionCommand;
use app::recent::RecentFiles;
use app::session::PlaybackSession;
use app::timeline_ui::TimelineUiState;
use media::{seek::SeekTarget, source::MediaSource, video::VideoDecodePreference};
use platform::input::InputEvent;
use platform::open_dialog::show_open_file_dialog;
use platform::window::NativeWindow;

/// How long the UI loop blocks on the message queue when the player is
/// quiescent. Short enough that asynchronous worker events and timed overlays
/// stay responsive (~10 wakeups/sec is negligible CPU), long enough that an
/// idle player does not busy-spin.
const IDLE_MESSAGE_WAIT_MS: u32 = 100;

fn main() {
    // ── Vectored Exception Handler ─────────────────────────────────────
    // Access violations from d3d11.dll kill the process instantly —
    // Rust's panic handler never fires.  A VEH runs BEFORE the default
    // handler and flushes the in-memory log ring to disk, so the trace
    // leading up to a hard crash is still captured.
    ffi::runtime::install_crash_handler();

    std::panic::set_hook(Box::new(|info| {
        let msg = format!("panic: {info}");
        flog!("{msg}");
        // Flush the buffered trace so the panic context survives.
        logging::dump_to_session_log();
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = std::path::PathBuf::from(appdata).join("FastPlay");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.log"), &msg);
        }
    }));
    if let Err(error) = run() {
        let msg = format!("fatal: {error}");
        flog!("{msg}");
        logging::dump_to_session_log();
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = std::path::PathBuf::from(appdata).join("FastPlay");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.log"), &msg);
        }
        std::process::exit(1);
    }

    // Normal exit: flush the buffered trace (metrics, mode changes, etc.).
    logging::dump_to_session_log();
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Set DPI awareness before creating any windows so the system delivers
    // physical pixel sizes and WM_DPICHANGED on monitor transitions.
    ffi::runtime::set_dpi_awareness();

    // Raise the Windows multimedia timer resolution to 1 ms so that
    // thread::sleep(1ms) wakes up on time.  Without this the default
    // resolution is ~15 ms, which causes the main loop to miss video frame
    // deadlines and produces stuttering / A-V desync.
    let _timer_resolution = ffi::runtime::MultimediaTimerResolution::begin_1ms();

    let media_path = parse_media_source_from_args()?;
    let window = NativeWindow::create("FastPlay", 1280, 720)?;
    let mut session = PlaybackSession::new(window)?;
    let mut timeline_ui = TimelineUiState::new();

    ffi::dxgi::install_modal_tick(&mut session);

    // Shared backend for the Recent overlay and resume-on-open.
    let mut recent = RecentFiles::load();
    // Path of the currently-open file, so its progress can be saved when we
    // switch files or exit. `None` until the first file opens.
    let mut current: Option<PathBuf> = None;
    // Recent overlay selection; `Some(index)` while the overlay is open.
    let mut recent_selected: Option<usize> = None;

    if let Some(source) = media_path {
        open_media(
            &mut session,
            &mut recent,
            &mut current,
            source,
            Instant::now(),
        )?;
    }

    let mut input_events: Vec<InputEvent> = Vec::new();
    while session.window().is_open() {
        session.window().pump_messages()?;
        // WM_CLOSE may have destroyed the window during pump_messages.
        if !session.window().is_open() {
            break;
        }
        let now = Instant::now();
        session.window().take_input_events(&mut input_events);
        for input in input_events.drain(..) {
            // The Recent overlay is modal: while open it captures navigation and
            // confirm/remove keys (and swallows playback keys).
            if recent_selected.is_some() {
                handle_recent_overlay_input(
                    &input,
                    &mut session,
                    &mut recent,
                    &mut recent_selected,
                    &mut current,
                    now,
                )?;
                continue;
            }
            if matches!(input, InputEvent::ToggleRecentOverlay) {
                recent_selected = Some(0);
                show_recent_overlay(&mut session, &recent, 0)?;
                continue;
            }

            // Context-free events translate directly to a command. The remaining
            // events need event-loop context (current position, scrub UI, window
            // state, file I/O) and are handled below.
            if let Some(command) = app::input_dispatch::command_for(&input) {
                session.apply_command(command, now)?;
                continue;
            }
            match input {
                InputEvent::SeekRelativeSeconds(offset_seconds) => {
                    let snapshot = session.snapshot(now);
                    let next_position = if offset_seconds >= 0 {
                        snapshot
                            .position
                            .saturating_add(std::time::Duration::from_secs(offset_seconds as u64))
                    } else {
                        snapshot
                            .position
                            .saturating_sub(std::time::Duration::from_secs(
                                (-offset_seconds) as u64,
                            ))
                    };
                    session
                        .apply_command(SessionCommand::Seek(SeekTarget::new(next_position)), now)?;
                    timeline_ui.seek_overlay_until =
                        Some(now + app::timeline_ui::SEEK_OVERLAY_DURATION);
                }
                InputEvent::StepFrameForward => {
                    session.apply_command(SessionCommand::StepFrameForward, now)?;
                    timeline_ui.seek_overlay_until =
                        Some(now + app::timeline_ui::SEEK_OVERLAY_DURATION);
                }
                InputEvent::StepFrameBackward => {
                    session.apply_command(SessionCommand::StepFrameBackward, now)?;
                    timeline_ui.seek_overlay_until =
                        Some(now + app::timeline_ui::SEEK_OVERLAY_DURATION);
                }
                InputEvent::EscapeKey => {
                    if session.window().is_borderless() {
                        session.apply_command(SessionCommand::ToggleBorderlessFullscreen, now)?;
                    }
                }
                InputEvent::BackspaceKey => {
                    if timeline_ui.is_scrubbing() {
                        timeline_ui.cancel_scrub(&mut session, now)?;
                    }
                }
                InputEvent::FileDropped(path) => {
                    open_media(
                        &mut session,
                        &mut recent,
                        &mut current,
                        MediaSource::new(path),
                        now,
                    )?;
                }
                InputEvent::OpenFileDialog => {
                    let hwnd = session.window().raw_window().hwnd();
                    if let Some(path) = show_open_file_dialog(hwnd) {
                        open_media(
                            &mut session,
                            &mut recent,
                            &mut current,
                            MediaSource::new(path),
                            now,
                        )?;
                    }
                }
                // All other events are context-free and handled by
                // `command_for` above (or are Recent-overlay keys ignored while
                // the overlay is closed).
                _ => {}
            }
        }
        timeline_ui.update(&mut session, now)?;
        session.tick(now)?;
        if session.take_idle_pace_request() {
            // The tick produced no frame to present. While playback is actively
            // advancing, pace tightly so the next frame's deadline is not missed.
            // When the player is quiescent (idle/paused/ended/error), block on
            // the message queue instead of spinning — this is what keeps an idle
            // or paused FastPlay from pinning a CPU core. The bounded timeout
            // still lets asynchronous worker events and timed overlays be picked
            // up promptly, and any input/resize/drag wakes the wait immediately.
            if session.is_idle_for_input() {
                session.window().wait_for_messages(IDLE_MESSAGE_WAIT_MS);
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    // Persist the position of whatever was playing when the user quit.
    save_current_progress(&session, &mut recent, &current, Instant::now());
    recent.save();

    session.window().clear_modal_tick();
    // Tear down GPU/worker resources in a controlled order while the window
    // HWND is still alive, before the session is dropped.
    session.shutdown();
    Ok(())
}

/// Format a millisecond position as `m:ss` (or `h:mm:ss` past an hour).
fn format_clock_ms(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Build (filename, position) rows for the Recent overlay.
fn recent_rows(recent: &RecentFiles) -> Vec<(String, String)> {
    recent
        .entries()
        .iter()
        .map(|e| {
            let name = e
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("(unknown)")
                .to_string();
            (name, format_clock_ms(e.last_position_ms))
        })
        .collect()
}

fn show_recent_overlay(
    session: &mut PlaybackSession,
    recent: &RecentFiles,
    selected: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = recent_rows(recent);
    session.show_recent_overlay(&rows, selected)
}

/// Save the current file's playback position into the recent history. No-op
/// until a file has actually opened (duration known).
fn save_current_progress(
    session: &PlaybackSession,
    recent: &mut RecentFiles,
    current: &Option<PathBuf>,
    now: Instant,
) {
    if let (Some(path), Some(duration)) = (current.as_ref(), session.media_duration()) {
        let position = session.snapshot(now).position;
        recent.update_progress(
            path,
            position.as_millis() as u64,
            duration.as_millis() as u64,
        );
    }
}

/// Open `source`: save the outgoing file's progress, record the new file in the
/// recent history (moving it to the top), resume near the saved position when
/// appropriate, and update the window title.
fn open_media(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    source: MediaSource,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    save_current_progress(session, recent, current, now);

    let path = source.path().to_path_buf();
    // Resume target is read from the saved entry before `touch_opened` (which
    // preserves the saved position) reorders the list.
    let resume = recent.resume_target(&path).map(Duration::from_millis);
    if let Some(at) = resume {
        flog!("resume path={} at_ms={}", path.display(), at.as_millis());
    }
    recent.touch_opened_now(&path);
    recent.save();

    let title = window_title_for(&source);
    session.open_at(source, resume, now)?;
    session.window().set_title(&title);
    *current = Some(path);
    Ok(())
}

/// Handle an input event while the Recent overlay is open (it is modal).
fn handle_recent_overlay_input(
    input: &InputEvent,
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    selected: &mut Option<usize>,
    current: &mut Option<PathBuf>,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let sel = selected.unwrap_or(0);
    match input {
        InputEvent::ToggleRecentOverlay | InputEvent::EscapeKey => {
            session.clear_recent_overlay();
            *selected = None;
        }
        InputEvent::NavigateUp => {
            let s = sel.saturating_sub(1);
            *selected = Some(s);
            show_recent_overlay(session, recent, s)?;
        }
        InputEvent::NavigateDown => {
            let len = recent.entries().len();
            let s = if len == 0 { 0 } else { (sel + 1).min(len - 1) };
            *selected = Some(s);
            show_recent_overlay(session, recent, s)?;
        }
        InputEvent::Confirm => {
            let path = recent.entries().get(sel).map(|e| e.path.clone());
            if let Some(path) = path {
                if path.exists() {
                    session.clear_recent_overlay();
                    *selected = None;
                    open_media(session, recent, current, MediaSource::new(path), now)?;
                } else {
                    // The file moved/was deleted: drop it and keep browsing.
                    recent.remove_index(sel);
                    recent.save();
                    refresh_after_remove(session, recent, selected, sel)?;
                }
            }
        }
        InputEvent::RemoveSelected if sel < recent.entries().len() => {
            recent.remove_index(sel);
            recent.save();
            refresh_after_remove(session, recent, selected, sel)?;
        }
        // Any other key is swallowed while the overlay is modal.
        _ => {}
    }
    Ok(())
}

/// After removing entry `removed_index`, close the overlay if the list is now
/// empty, otherwise clamp the selection and re-render.
fn refresh_after_remove(
    session: &mut PlaybackSession,
    recent: &RecentFiles,
    selected: &mut Option<usize>,
    removed_index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let len = recent.entries().len();
    if len == 0 {
        session.clear_recent_overlay();
        *selected = None;
    } else {
        let s = removed_index.min(len - 1);
        *selected = Some(s);
        show_recent_overlay(session, recent, s)?;
    }
    Ok(())
}

fn window_title_for(source: &MediaSource) -> String {
    let name = source
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("FastPlay");
    format!("{name} - FastPlay")
}

fn parse_media_source_from_args() -> Result<Option<MediaSource>, Box<dyn std::error::Error>> {
    let mut force_software = false;
    let mut media_path = None;

    for argument in std::env::args_os().skip(1) {
        if media_path.is_none() && argument == "--force-sw" {
            force_software = true;
            continue;
        }

        if media_path.is_some() {
            return Err("usage: fastplay [--force-sw] <media-path>".into());
        }

        media_path = Some(argument);
    }

    Ok(media_path.map(|path| {
        let source = MediaSource::new(path);
        if force_software {
            source.with_decode_preference(VideoDecodePreference::ForceSoftware)
        } else {
            source
        }
    }))
}
