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
mod license;
mod media;
mod platform;
mod playback;
mod render;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use app::commands::SessionCommand;
use app::play_queue::PlayQueue;
use app::recent::RecentFiles;
use app::review_markers::{
    bounded_note_text, default_export_directory, format_marker_timestamp_ms, MarkerExportFormat,
    ReviewMarkers, MAX_MARKER_NOTE_CHARS,
};
use app::review_queue::{bounded_queue_name, SavedReviewQueues, MAX_REVIEW_QUEUE_NAME_CHARS};
use app::session::PlaybackSession;
use app::timeline_ui::TimelineUiState;
use license::LicenseState;
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
    let license = LicenseState::detect();

    ffi::dxgi::install_modal_tick(&mut session);

    // Shared backend for the Recent overlay and resume-on-open.
    let mut recent = RecentFiles::load();
    let mut review_markers = ReviewMarkers::load();
    let mut review_queues = SavedReviewQueues::load();
    // Path of the currently-open file, so its progress can be saved when we
    // switch files or exit. `None` until the first file opens.
    let mut current: Option<PathBuf> = None;
    // Recent overlay selection; `Some(index)` while the overlay is open.
    let mut recent_selected: Option<usize> = None;
    // Marker overlay selection; `Some(index)` while the overlay is open.
    let mut marker_selected: Option<usize> = None;
    // Review queue overlay selection; `Some(index)` while the overlay is open.
    let mut review_queue_selected: Option<usize> = None;
    let mut active_review_queue_name: Option<String> = None;
    let mut text_edit: Option<TextEditState> = None;
    // The play queue. Owned here, beside `RecentFiles`; `PlaybackSession` stays a
    // single-file coordinator and is unaware of it. Empty until the first open.
    let mut play_queue: PlayQueue = PlayQueue::from_paths(Vec::<PathBuf>::new());

    if let Some(source) = media_path {
        open_single_file(
            &mut session,
            &mut recent,
            &mut current,
            &mut play_queue,
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
            if text_edit.is_some() {
                handle_text_edit_input(
                    &input,
                    &mut text_edit,
                    &mut session,
                    &mut review_markers,
                    &mut review_queues,
                    &mut marker_selected,
                    &mut review_queue_selected,
                    &mut active_review_queue_name,
                    &play_queue,
                )?;
                continue;
            }
            // The Recent overlay is modal: while open it captures navigation and
            // confirm/remove keys (and swallows playback keys).
            if recent_selected.is_some() {
                handle_recent_overlay_input(
                    &input,
                    &mut session,
                    &mut recent,
                    &mut recent_selected,
                    &mut current,
                    &mut play_queue,
                    &mut active_review_queue_name,
                    now,
                )?;
                continue;
            }
            if marker_selected.is_some() {
                handle_marker_overlay_input(
                    &input,
                    &license,
                    &mut session,
                    &mut review_markers,
                    &mut marker_selected,
                    &mut text_edit,
                    now,
                )?;
                continue;
            }
            if review_queue_selected.is_some() {
                handle_review_queue_overlay_input(
                    &input,
                    &license,
                    &mut session,
                    &mut recent,
                    &mut current,
                    &mut play_queue,
                    &mut review_queues,
                    &mut review_queue_selected,
                    &mut active_review_queue_name,
                    now,
                )?;
                continue;
            }
            if matches!(input, InputEvent::ToggleRecentOverlay) {
                recent_selected = Some(0);
                show_recent_overlay(&mut session, &recent, 0)?;
                continue;
            }
            if matches!(input, InputEvent::ToggleMarkerOverlay) {
                toggle_marker_overlay(
                    &license,
                    &mut session,
                    &review_markers,
                    &mut marker_selected,
                )?;
                continue;
            }
            if matches!(input, InputEvent::AddMarker) {
                add_marker_at_current_position(
                    &license,
                    &mut session,
                    &mut review_markers,
                    &marker_selected,
                    now,
                )?;
                continue;
            }
            if matches!(input, InputEvent::SaveReviewQueue) {
                begin_save_review_queue(
                    &license,
                    &mut session,
                    &mut text_edit,
                    &play_queue,
                    active_review_queue_name.as_deref(),
                );
                continue;
            }
            if matches!(input, InputEvent::ToggleReviewQueueOverlay) {
                toggle_review_queue_overlay(
                    &license,
                    &mut session,
                    &review_queues,
                    &mut review_queue_selected,
                    active_review_queue_name.as_deref(),
                )?;
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
                InputEvent::FilesDropped(paths) => {
                    open_dropped_paths(
                        &mut session,
                        &mut recent,
                        &mut current,
                        &mut play_queue,
                        paths,
                        now,
                    )?;
                    active_review_queue_name = None;
                }
                InputEvent::OpenFileDialog => {
                    let hwnd = session.window().raw_window().hwnd();
                    if let Some(path) = show_open_file_dialog(hwnd) {
                        open_single_file(
                            &mut session,
                            &mut recent,
                            &mut current,
                            &mut play_queue,
                            MediaSource::new(path),
                            now,
                        )?;
                        active_review_queue_name = None;
                    }
                }
                InputEvent::QueuePrevious => {
                    navigate_queue(
                        &mut session,
                        &mut recent,
                        &mut current,
                        &mut play_queue,
                        QueueDirection::Previous,
                        now,
                    )?;
                }
                InputEvent::QueueNext => {
                    navigate_queue(
                        &mut session,
                        &mut recent,
                        &mut current,
                        &mut play_queue,
                        QueueDirection::Next,
                        now,
                    )?;
                }
                // All other events are context-free and handled by
                // `command_for` above (or are Recent-overlay keys ignored while
                // the overlay is closed).
                _ => {}
            }
        }
        timeline_ui.update(&mut session, now)?;
        session.tick(now)?;
        // A natural end-of-file (no loop/auto-replay/out-point) latches the
        // ended signal exactly once; auto-advance to the next queued item.
        if session.take_ended_signal() {
            auto_advance_queue(
                &mut session,
                &mut recent,
                &mut current,
                &mut play_queue,
                now,
            )?;
        }
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

    // Stop the background workers (decode + audio) so no thread is executing
    // inside the graphics/codec drivers when we exit.
    session.window().clear_modal_tick();
    session.shutdown();

    // Flush the buffered trace, then hard-exit the process. We deliberately do
    // NOT release the D3D11 device, swap chain, or HW-decode surfaces: doing so
    // in-process at exit intermittently faults inside the graphics driver
    // (access violation through a dangling vtable), which hands an unhandled
    // exception to Windows Error Reporting. WER then freezes the still-visible
    // window for several seconds while it writes a multi-megabyte crash dump
    // before the process dies — the "lag on close". Letting the OS reclaim GPU
    // resources on process teardown is instant and crash-free. State is
    // persisted and workers are stopped above, so there is nothing left to do.
    logging::dump_to_session_log();
    std::process::exit(0);
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

const PRO_REVIEW_UPSELL: &str =
    "FastPlay Pro unlocks review tools: saved queues, timestamp markers, marker notes, exports, and batch screenshots. Playback stays free.";

enum TextEditState {
    MarkerNote {
        marker_index: usize,
        buffer: String,
        ignore_first_char: Option<char>,
    },
    ReviewQueueName {
        buffer: String,
    },
}

impl TextEditState {
    fn push_char(&mut self, ch: char) {
        match self {
            TextEditState::MarkerNote {
                buffer,
                ignore_first_char,
                ..
            } => {
                if ignore_first_char
                    .take()
                    .is_some_and(|ignored| ignored.eq_ignore_ascii_case(&ch))
                {
                    return;
                }
                if buffer.chars().count() < MAX_MARKER_NOTE_CHARS {
                    buffer.push(ch);
                }
            }
            TextEditState::ReviewQueueName { buffer } => {
                if buffer.chars().count() < MAX_REVIEW_QUEUE_NAME_CHARS {
                    buffer.push(ch);
                }
            }
        }
    }

    fn pop_char(&mut self) {
        match self {
            TextEditState::MarkerNote {
                buffer,
                ignore_first_char,
                ..
            } => {
                *ignore_first_char = None;
                buffer.pop();
            }
            TextEditState::ReviewQueueName { buffer } => {
                buffer.pop();
            }
        }
    }

    fn clear_pending_shortcut_char(&mut self) {
        if let TextEditState::MarkerNote {
            ignore_first_char, ..
        } = self
        {
            *ignore_first_char = None;
        }
    }

    fn prompt(&self) -> String {
        match self {
            TextEditState::MarkerNote { buffer, .. } => {
                format!("Note: {buffer}_")
            }
            TextEditState::ReviewQueueName { buffer } => {
                format!("Queue name: {buffer}_")
            }
        }
    }
}

fn marker_rows(markers: &ReviewMarkers, current: Option<&Path>) -> Vec<(String, String)> {
    let Some(path) = current else {
        return Vec::new();
    };
    markers
        .markers_for(path)
        .iter()
        .enumerate()
        .map(|(index, marker)| {
            let label = marker
                .note
                .as_deref()
                .filter(|note| !note.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Marker {}", index + 1));
            (label, format_marker_timestamp_ms(marker.timestamp_ms))
        })
        .collect()
}

fn show_marker_overlay(
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
    selected: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let rows = marker_rows(markers, session.current_media_path());
    let selected = if rows.is_empty() {
        0
    } else {
        selected.min(rows.len() - 1)
    };
    let footer = marker_overlay_footer(markers, session.current_media_path(), selected);
    session.show_marker_overlay(&rows, selected, &footer)?;
    Ok(selected)
}

fn marker_overlay_footer(
    markers: &ReviewMarkers,
    current: Option<&Path>,
    selected: usize,
) -> String {
    const EMPTY: &str = "M Add marker   Esc Close";
    let Some(path) = current else {
        return EMPTY.to_string();
    };
    let file_markers = markers.markers_for(path);
    if file_markers.is_empty() {
        return EMPTY.to_string();
    }
    let count = file_markers.len();
    let marker = file_markers[selected.min(count - 1)];
    let note = marker
        .note
        .as_deref()
        .filter(|note| !note.trim().is_empty())
        .unwrap_or("No note");
    format!(
        "{count} marker{} | {} | {} | N Note  E Export  B Batch  Esc",
        if count == 1 { "" } else { "s" },
        format_marker_timestamp_ms(marker.timestamp_ms),
        truncate_for_status(note, 54)
    )
}

fn truncate_for_status(value: &str, max_chars: usize) -> String {
    let mut out: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn toggle_marker_overlay(
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
    selected: &mut Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !license.can_save_markers() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return Ok(());
    }
    if session.current_media_path().is_none() {
        session.show_status_message("Open a file to use review markers");
        return Ok(());
    }
    let next = show_marker_overlay(session, markers, selected.unwrap_or(0))?;
    *selected = Some(next);
    Ok(())
}

fn add_marker_at_current_position(
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &mut ReviewMarkers,
    marker_selected: &Option<usize>,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    if !license.can_save_markers() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return Ok(());
    }
    let Some(path) = session.current_media_path().map(Path::to_path_buf) else {
        session.show_status_message("Open a file to add a marker");
        return Ok(());
    };
    let timestamp_ms = session.snapshot(now).position.as_millis() as u64;
    markers.add_marker(&path, timestamp_ms);
    markers.save();
    session.show_status_message(&format!(
        "Marker added {}",
        format_marker_timestamp_ms(timestamp_ms)
    ));
    if let Some(selected) = marker_selected {
        let _ = show_marker_overlay(session, markers, *selected)?;
    }
    Ok(())
}

fn export_current_markers(
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
) -> Result<(), Box<dyn std::error::Error>> {
    if !license.can_export_markers() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return Ok(());
    }
    let Some(path) = session.current_media_path().map(Path::to_path_buf) else {
        session.show_status_message("Open a file to export markers");
        return Ok(());
    };
    if markers.markers_for(&path).is_empty() {
        session.show_status_message("No markers to export");
        return Ok(());
    }
    let directory = default_export_directory()?;
    let txt = markers.export_for_file(&path, &directory, MarkerExportFormat::Txt)?;
    let csv = markers.export_for_file(&path, &directory, MarkerExportFormat::Csv)?;
    flog!(
        "markers_exported txt={} csv={}",
        txt.display(),
        csv.display()
    );
    session.show_status_message("Markers exported to Pictures\\FastPlay");
    Ok(())
}

fn batch_marker_screenshots(
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
) {
    if !license.can_batch_export_marker_screenshots() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return;
    }
    let Some(path) = session.current_media_path() else {
        session.show_status_message("Open a file to batch export marker screenshots");
        return;
    };
    if markers.markers_for(path).is_empty() {
        session.show_status_message("No markers to capture");
        return;
    }
    session.show_status_message("Batch marker screenshots are not available yet");
}

fn review_queue_rows(queues: &SavedReviewQueues) -> Vec<(String, String)> {
    queues
        .queues()
        .iter()
        .map(|queue| {
            let count = queue.items.len();
            (
                queue.name.clone(),
                format!("{count} file{}", if count == 1 { "" } else { "s" }),
            )
        })
        .collect()
}

fn show_review_queue_overlay(
    session: &mut PlaybackSession,
    queues: &SavedReviewQueues,
    selected: usize,
    active_name: Option<&str>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let rows = review_queue_rows(queues);
    let selected = if rows.is_empty() {
        0
    } else {
        selected.min(rows.len() - 1)
    };
    let footer = match active_name {
        Some(name) => format!(
            "Enter Load   Del Delete   Esc Close   Current: {}",
            truncate_for_status(name, 32)
        ),
        None => "Enter Load   Del Delete   Esc Close".to_string(),
    };
    session.show_review_queue_overlay(&rows, selected, &footer)?;
    Ok(selected)
}

fn toggle_review_queue_overlay(
    license: &LicenseState,
    session: &mut PlaybackSession,
    queues: &SavedReviewQueues,
    selected: &mut Option<usize>,
    active_name: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !license.can_load_review_queues() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return Ok(());
    }
    let next = show_review_queue_overlay(session, queues, selected.unwrap_or(0), active_name)?;
    *selected = Some(next);
    Ok(())
}

fn begin_save_review_queue(
    license: &LicenseState,
    session: &mut PlaybackSession,
    text_edit: &mut Option<TextEditState>,
    play_queue: &PlayQueue,
    active_name: Option<&str>,
) {
    if !license.can_save_review_queues() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return;
    }
    if play_queue.is_empty() {
        session.show_status_message("No queue to save");
        return;
    }
    let buffer = active_name
        .map(str::to_string)
        .or_else(|| {
            play_queue.current().map(|path| {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| file_label(path))
            })
        })
        .unwrap_or_else(|| "Review queue".to_string());
    *text_edit = Some(TextEditState::ReviewQueueName { buffer });
    if let Some(edit) = text_edit.as_ref() {
        session.show_status_message(&edit.prompt());
    }
}

fn handle_text_edit_input(
    input: &InputEvent,
    text_edit: &mut Option<TextEditState>,
    session: &mut PlaybackSession,
    markers: &mut ReviewMarkers,
    queues: &mut SavedReviewQueues,
    marker_selected: &mut Option<usize>,
    review_queue_selected: &mut Option<usize>,
    active_review_queue_name: &mut Option<String>,
    play_queue: &PlayQueue,
) -> Result<(), Box<dyn std::error::Error>> {
    match input {
        InputEvent::TextChar(ch) => {
            if let Some(edit) = text_edit.as_mut() {
                edit.push_char(*ch);
                session.show_status_message(&edit.prompt());
            }
        }
        InputEvent::BackspaceKey => {
            if let Some(edit) = text_edit.as_mut() {
                edit.pop_char();
                session.show_status_message(&edit.prompt());
            }
        }
        InputEvent::Confirm => {
            if let Some(edit) = text_edit.take() {
                finish_text_edit(
                    edit,
                    session,
                    markers,
                    queues,
                    marker_selected,
                    review_queue_selected,
                    active_review_queue_name,
                    play_queue,
                )?;
            }
        }
        InputEvent::EscapeKey => {
            if let Some(edit) = text_edit.take() {
                cancel_text_edit(edit, session, markers, marker_selected)?;
            }
        }
        _ => {
            if let Some(edit) = text_edit.as_mut() {
                edit.clear_pending_shortcut_char();
                session.show_status_message(&edit.prompt());
            }
        }
    }
    Ok(())
}

fn finish_text_edit(
    edit: TextEditState,
    session: &mut PlaybackSession,
    markers: &mut ReviewMarkers,
    queues: &mut SavedReviewQueues,
    marker_selected: &mut Option<usize>,
    review_queue_selected: &mut Option<usize>,
    active_review_queue_name: &mut Option<String>,
    play_queue: &PlayQueue,
) -> Result<(), Box<dyn std::error::Error>> {
    match edit {
        TextEditState::MarkerNote {
            marker_index,
            buffer,
            ..
        } => {
            let Some(path) = session.current_media_path().map(Path::to_path_buf) else {
                session.show_status_message("Open a file to edit marker notes");
                return Ok(());
            };
            if markers.set_note_for_file_index(&path, marker_index, &buffer) {
                markers.save();
                *marker_selected = Some(show_marker_overlay(session, markers, marker_index)?);
                session.show_status_message("Marker note saved");
            }
        }
        TextEditState::ReviewQueueName { buffer } => {
            save_named_review_queue(
                session,
                queues,
                active_review_queue_name,
                play_queue,
                &buffer,
            );
            if let Some(selected) = *review_queue_selected {
                *review_queue_selected = Some(show_review_queue_overlay(
                    session,
                    queues,
                    selected,
                    active_review_queue_name.as_deref(),
                )?);
            }
        }
    }
    Ok(())
}

fn cancel_text_edit(
    edit: TextEditState,
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
    marker_selected: &mut Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    match edit {
        TextEditState::MarkerNote { marker_index, .. } => {
            *marker_selected = Some(show_marker_overlay(session, markers, marker_index)?);
            session.show_status_message("Marker note edit canceled");
        }
        TextEditState::ReviewQueueName { .. } => {
            session.show_status_message("Review queue save canceled");
        }
    }
    Ok(())
}

fn save_named_review_queue(
    session: &mut PlaybackSession,
    queues: &mut SavedReviewQueues,
    active_review_queue_name: &mut Option<String>,
    play_queue: &PlayQueue,
    name: &str,
) {
    let name = name.trim();
    if name.is_empty() {
        session.show_status_message("Review queue name required");
        return;
    }
    let items = play_queue.items().to_vec();
    if items.is_empty() {
        session.show_status_message("No queue to save");
        return;
    }
    queues.upsert(name, items);
    queues.save();
    let lookup_name = bounded_queue_name(name);
    let lookup_name = lookup_name.trim();
    let saved_name = queues
        .get(lookup_name)
        .map(|queue| queue.name.clone())
        .unwrap_or_else(|| lookup_name.to_string());
    *active_review_queue_name = Some(saved_name.clone());
    session.show_status_message(&format!(
        "Saved review queue: {}",
        truncate_for_status(&saved_name, 48)
    ));
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

/// Direction of a manual play-queue navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueDirection {
    Previous,
    Next,
}

/// Status shown after a queue navigation opens a file.
fn nav_opened_message(direction: QueueDirection, file_name: &str) -> String {
    match direction {
        QueueDirection::Previous => format!("Previous: {file_name}"),
        QueueDirection::Next => format!("Next: {file_name}"),
    }
}

/// Status shown when there is no item to navigate to in `direction`.
fn nav_none_message(direction: QueueDirection) -> &'static str {
    match direction {
        QueueDirection::Previous => "No previous file",
        QueueDirection::Next => "No next file",
    }
}

/// Status shown when the candidate could not be opened; the cursor is unchanged.
const OPEN_FAILED_MESSAGE: &str = "Open failed";

/// Short display label for a path (its file name, or the whole path if none).
fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("(unknown)"))
        .to_string()
}

/// How a single drop resolves into a queue. Owned (not borrowed) so callers can
/// reuse the dropped paths freely. Classifying is pure aside from the `is_dir`
/// probe, which lets a dropped folder become a multi-item queue.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DropTarget {
    /// Nothing usable was dropped.
    Empty,
    /// A single file: becomes a single-file queue.
    SingleFile(PathBuf),
    /// A single folder: becomes a multi-item queue from its media files.
    Folder(PathBuf),
    /// Several dropped paths: become a multi-item queue (filtered to media).
    MultipleFiles(Vec<PathBuf>),
}

/// Classify the paths from one drop. A lone directory is a folder open; a lone
/// file is a single open; anything else is a multi-file queue. Multi-file drops
/// keep any folders in the list — [`PlayQueue::from_paths`] simply ignores
/// non-media entries (recursive expansion is intentionally out of scope).
fn classify_dropped_paths(paths: &[PathBuf]) -> DropTarget {
    match paths {
        [] => DropTarget::Empty,
        [one] if one.is_dir() => DropTarget::Folder(one.clone()),
        [one] => DropTarget::SingleFile(one.clone()),
        many => DropTarget::MultipleFiles(many.to_vec()),
    }
}

/// Open `source` as a fresh single-file queue. Used for every explicit single
/// open (CLI argument, single drop, file dialog, Recent pick) so normal
/// single-file behavior — including no parent-folder scan — is preserved.
fn open_single_file(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    source: MediaSource,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = source.path().to_path_buf();
    open_media(session, recent, current, source, now)?;
    *play_queue = PlayQueue::single(path);
    Ok(())
}

/// Make `queue` the active queue and open its first item. No-op (with a status
/// message) when the queue has no playable media. Only replaces the live queue
/// after the first open is initiated.
fn open_queue(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    queue: PlayQueue,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    match queue.current() {
        Some(first) => {
            let first = first.to_path_buf();
            open_media(session, recent, current, MediaSource::new(first), now)?;
            *play_queue = queue;
        }
        None => session.show_status_message("No playable media"),
    }
    Ok(())
}

/// Open whatever was dropped onto the window: a single file, a folder (whose
/// media files form a queue), or several files (a multi-item queue).
fn open_dropped_paths(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    paths: Vec<PathBuf>,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    match classify_dropped_paths(&paths) {
        DropTarget::Empty => {}
        DropTarget::SingleFile(path) => {
            open_single_file(
                session,
                recent,
                current,
                play_queue,
                MediaSource::new(path),
                now,
            )?;
        }
        DropTarget::Folder(dir) => {
            open_queue(
                session,
                recent,
                current,
                play_queue,
                PlayQueue::from_folder(&dir),
                now,
            )?;
        }
        DropTarget::MultipleFiles(paths) => {
            open_queue(
                session,
                recent,
                current,
                play_queue,
                PlayQueue::from_paths(paths),
                now,
            )?;
        }
    }
    Ok(())
}

/// Open the previous/next queue item using the same path as a normal open, so
/// subtitles, resume, metrics, and in/out reset stay consistent.
///
/// Cursor discipline (see [`PlayQueue`]): look up the candidate *without* moving
/// the cursor, open it, and only commit the move if the open is initiated. A
/// missing candidate file leaves the cursor on the current item.
fn navigate_queue(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    direction: QueueDirection,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let candidate = match direction {
        QueueDirection::Previous => play_queue.previous_path(),
        QueueDirection::Next => play_queue.next_path(),
    };
    let Some(candidate) = candidate.map(Path::to_path_buf) else {
        session.show_status_message(nav_none_message(direction));
        return Ok(());
    };
    if !candidate.exists() {
        // The file moved or was deleted since enumeration: keep the cursor put.
        session.show_status_message(OPEN_FAILED_MESSAGE);
        return Ok(());
    }
    let label = file_label(&candidate);
    open_media(session, recent, current, MediaSource::new(candidate), now)?;
    match direction {
        QueueDirection::Previous => play_queue.commit_previous(),
        QueueDirection::Next => play_queue.commit_next(),
    };
    session.show_status_message(&nav_opened_message(direction, &label));
    Ok(())
}

/// Status shown when a multi-item queue's last file ends (no wrapping).
const END_OF_QUEUE_MESSAGE: &str = "End of queue";

/// What auto-advance should do when the current file reaches its natural end.
/// Pure (derives only from the queue) so the decision is unit-testable; the
/// actual open is delegated to [`navigate_queue`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum AutoAdvance {
    /// Single-file queue (or empty): nothing to advance to, stay ended quietly.
    Skip,
    /// Multi-item queue sitting on its last item: no wrapping.
    EndOfQueue,
    /// Multi-item queue with a next candidate to open.
    OpenNext(PathBuf),
}

/// Decide the auto-advance action for `play_queue` at end-of-file. Uses
/// [`PlayQueue::next_path`] so the cursor is not moved; the commit happens only
/// after a successful open, inside [`navigate_queue`].
fn plan_auto_advance(play_queue: &PlayQueue) -> AutoAdvance {
    // Auto-advance is only ever for multi-item queues.
    if play_queue.len() <= 1 {
        return AutoAdvance::Skip;
    }
    match play_queue.next_path() {
        Some(next) => AutoAdvance::OpenNext(next.to_path_buf()),
        None => AutoAdvance::EndOfQueue,
    }
}

/// Auto-advance to the next queued file when the current one ends naturally.
/// Reuses [`navigate_queue`] for the open, so the candidate/commit discipline,
/// resume, subtitles, metrics, and in/out reset all match manual navigation.
fn auto_advance_queue(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    match plan_auto_advance(play_queue) {
        // A single-file open just ended: no queue to advance, no parent-folder
        // scan, no message.
        AutoAdvance::Skip => Ok(()),
        AutoAdvance::EndOfQueue => {
            session.show_status_message(END_OF_QUEUE_MESSAGE);
            Ok(())
        }
        AutoAdvance::OpenNext(_) => navigate_queue(
            session,
            recent,
            current,
            play_queue,
            QueueDirection::Next,
            now,
        ),
    }
}

/// Handle an input event while the Recent overlay is open (it is modal).
fn handle_recent_overlay_input(
    input: &InputEvent,
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    selected: &mut Option<usize>,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    active_review_queue_name: &mut Option<String>,
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
                    open_single_file(
                        session,
                        recent,
                        current,
                        play_queue,
                        MediaSource::new(path),
                        now,
                    )?;
                    *active_review_queue_name = None;
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

fn handle_marker_overlay_input(
    input: &InputEvent,
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &mut ReviewMarkers,
    selected: &mut Option<usize>,
    text_edit: &mut Option<TextEditState>,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let sel = selected.unwrap_or(0);
    match input {
        InputEvent::ToggleMarkerOverlay | InputEvent::EscapeKey => {
            session.clear_marker_overlay();
            *selected = None;
        }
        InputEvent::NavigateUp => {
            let s = sel.saturating_sub(1);
            *selected = Some(show_marker_overlay(session, markers, s)?);
        }
        InputEvent::NavigateDown => {
            let len = marker_rows(markers, session.current_media_path()).len();
            let s = if len == 0 { 0 } else { (sel + 1).min(len - 1) };
            *selected = Some(show_marker_overlay(session, markers, s)?);
        }
        InputEvent::Confirm => {
            if let Some(path) = session.current_media_path() {
                if let Some(marker) = markers.markers_for(path).get(sel) {
                    let target = Duration::from_millis(marker.timestamp_ms);
                    session.apply_command(SessionCommand::Seek(SeekTarget::new(target)), now)?;
                }
            }
        }
        InputEvent::RemoveSelected => {
            if !license.can_save_markers() {
                session.show_status_message(PRO_REVIEW_UPSELL);
                return Ok(());
            }
            if let Some(path) = session.current_media_path().map(Path::to_path_buf) {
                if markers.remove_for_file_index(&path, sel) {
                    markers.save();
                    let s = show_marker_overlay(session, markers, sel)?;
                    *selected = Some(s);
                    session.show_status_message("Marker removed");
                }
            }
        }
        InputEvent::AddMarker => {
            add_marker_at_current_position(license, session, markers, selected, now)?;
        }
        InputEvent::EditMarkerNote => {
            begin_marker_note_edit(license, session, markers, selected, text_edit)?;
        }
        InputEvent::ExportMarkers => {
            export_current_markers(license, session, markers)?;
        }
        InputEvent::BatchMarkerScreenshots => {
            batch_marker_screenshots(license, session, markers);
        }
        _ => {}
    }
    Ok(())
}

fn begin_marker_note_edit(
    license: &LicenseState,
    session: &mut PlaybackSession,
    markers: &ReviewMarkers,
    selected: &Option<usize>,
    text_edit: &mut Option<TextEditState>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !license.can_edit_marker_notes() {
        session.show_status_message(PRO_REVIEW_UPSELL);
        return Ok(());
    }
    let Some(path) = session.current_media_path() else {
        session.show_status_message("Open a file to edit marker notes");
        return Ok(());
    };
    let marker_index = selected.unwrap_or(0);
    let Some(marker) = markers.markers_for(path).get(marker_index).copied() else {
        session.show_status_message("No marker selected");
        return Ok(());
    };
    *text_edit = Some(TextEditState::MarkerNote {
        marker_index,
        buffer: bounded_note_text(marker.note.as_deref().unwrap_or("")),
        ignore_first_char: Some('n'),
    });
    if let Some(edit) = text_edit.as_ref() {
        session.show_status_message(&edit.prompt());
    }
    Ok(())
}

fn handle_review_queue_overlay_input(
    input: &InputEvent,
    license: &LicenseState,
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    queues: &mut SavedReviewQueues,
    selected: &mut Option<usize>,
    active_review_queue_name: &mut Option<String>,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let sel = selected.unwrap_or(0);
    match input {
        InputEvent::ToggleReviewQueueOverlay | InputEvent::EscapeKey => {
            session.clear_review_queue_overlay();
            *selected = None;
        }
        InputEvent::NavigateUp => {
            let s = sel.saturating_sub(1);
            *selected = Some(show_review_queue_overlay(
                session,
                queues,
                s,
                active_review_queue_name.as_deref(),
            )?);
        }
        InputEvent::NavigateDown => {
            let len = queues.queues().len();
            let s = if len == 0 { 0 } else { (sel + 1).min(len - 1) };
            *selected = Some(show_review_queue_overlay(
                session,
                queues,
                s,
                active_review_queue_name.as_deref(),
            )?);
        }
        InputEvent::Confirm => {
            if !license.can_load_review_queues() {
                session.show_status_message(PRO_REVIEW_UPSELL);
                return Ok(());
            }
            load_selected_review_queue(
                session,
                recent,
                current,
                play_queue,
                queues,
                selected,
                active_review_queue_name,
                sel,
                now,
            )?;
        }
        InputEvent::RemoveSelected => {
            if !license.can_delete_review_queues() {
                session.show_status_message(PRO_REVIEW_UPSELL);
                return Ok(());
            }
            let removed_name = queues.queues().get(sel).map(|queue| queue.name.clone());
            if queues.remove_index(sel) {
                if removed_name.as_deref() == active_review_queue_name.as_deref() {
                    *active_review_queue_name = None;
                }
                queues.save();
                let next = show_review_queue_overlay(
                    session,
                    queues,
                    sel,
                    active_review_queue_name.as_deref(),
                )?;
                *selected = Some(next);
                session.show_status_message("Review queue deleted");
            }
        }
        _ => {}
    }
    Ok(())
}

fn load_selected_review_queue(
    session: &mut PlaybackSession,
    recent: &mut RecentFiles,
    current: &mut Option<PathBuf>,
    play_queue: &mut PlayQueue,
    queues: &SavedReviewQueues,
    selected: &mut Option<usize>,
    active_review_queue_name: &mut Option<String>,
    index: usize,
    now: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(saved) = queues.queues().get(index).cloned() else {
        return Ok(());
    };
    let existing = SavedReviewQueues::existing_items(&saved);
    let skipped = saved.items.len().saturating_sub(existing.len());
    let queue = PlayQueue::from_ordered_paths(existing);
    if queue.is_empty() {
        session.show_status_message("No existing files in saved queue");
        return Ok(());
    }
    session.clear_review_queue_overlay();
    *selected = None;
    open_queue(session, recent, current, play_queue, queue, now)?;
    *active_review_queue_name = Some(saved.name);
    if skipped == 0 {
        session.show_status_message("Loaded review queue");
    } else {
        session.show_status_message(&format!("Loaded queue, skipped {skipped} missing files"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn nav_messages_are_direction_specific() {
        assert_eq!(
            nav_opened_message(QueueDirection::Next, "ep2.mp4"),
            "Next: ep2.mp4"
        );
        assert_eq!(
            nav_opened_message(QueueDirection::Previous, "ep1.mp4"),
            "Previous: ep1.mp4"
        );
        assert_eq!(nav_none_message(QueueDirection::Next), "No next file");
        assert_eq!(
            nav_none_message(QueueDirection::Previous),
            "No previous file"
        );
    }

    #[test]
    fn file_label_uses_file_name() {
        assert_eq!(
            file_label(Path::new(r"C:\videos\My Clip.mkv")),
            "My Clip.mkv"
        );
        assert_eq!(file_label(Path::new("bare.mp4")), "bare.mp4");
    }

    #[test]
    fn classify_empty_drop() {
        assert_eq!(classify_dropped_paths(&[]), DropTarget::Empty);
    }

    #[test]
    fn classify_single_file_drop() {
        let paths = vec![PathBuf::from("a.mp4")];
        assert_eq!(
            classify_dropped_paths(&paths),
            DropTarget::SingleFile(PathBuf::from("a.mp4"))
        );
    }

    #[test]
    fn classify_multiple_file_drop() {
        let paths = vec![PathBuf::from("a.mp4"), PathBuf::from("b.mkv")];
        assert_eq!(
            classify_dropped_paths(&paths),
            DropTarget::MultipleFiles(paths.clone())
        );
    }

    #[test]
    fn auto_advance_skips_single_file_queue() {
        // A natural end of a single-file queue must not auto-advance (and must
        // never scan a parent folder).
        let queue = PlayQueue::single(PathBuf::from(r"C:\v\only.mp4"));
        assert_eq!(plan_auto_advance(&queue), AutoAdvance::Skip);
    }

    #[test]
    fn auto_advance_opens_next_candidate_without_moving_cursor() {
        let queue = PlayQueue::from_paths(vec![
            PathBuf::from(r"C:\v\a.mp4"),
            PathBuf::from(r"C:\v\b.mp4"),
            PathBuf::from(r"C:\v\c.mp4"),
        ]);
        // Cursor is at a; the plan targets b without committing.
        assert_eq!(
            plan_auto_advance(&queue),
            AutoAdvance::OpenNext(PathBuf::from(r"C:\v\b.mp4"))
        );
        assert_eq!(queue.cursor(), 0, "planning does not move the cursor");
    }

    #[test]
    fn auto_advance_does_not_wrap_at_end_of_queue() {
        let mut queue = PlayQueue::from_paths(vec![
            PathBuf::from(r"C:\v\a.mp4"),
            PathBuf::from(r"C:\v\b.mp4"),
        ]);
        queue.commit_next(); // now on the last item
        assert_eq!(plan_auto_advance(&queue), AutoAdvance::EndOfQueue);
    }

    #[test]
    fn auto_advance_skips_empty_queue() {
        let queue = PlayQueue::from_paths(Vec::<PathBuf>::new());
        assert_eq!(plan_auto_advance(&queue), AutoAdvance::Skip);
    }

    #[test]
    fn classify_single_folder_drop() {
        // A lone existing directory classifies as a folder open.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fastplay_main_drop_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = vec![dir.clone()];
        assert_eq!(
            classify_dropped_paths(&paths),
            DropTarget::Folder(dir.clone())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
