//! Clip export: writes the marked in/out range to a new file next to the
//! source.
//!
//! `PlaybackSession` owns one [`ClipExporter`]. The export itself runs on a
//! dedicated worker thread with its own demuxer (a stream-copy remux, see
//! `ffi::ffmpeg::export_clip`), so it never touches the playback decode
//! workers or the present path. The worker reports back only through a
//! [`SessionEvent::ClipExportFinished`] on its own channel, which the session
//! drains every tick — independently of the video queue, so a paused player
//! still hears about a finished export promptly.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{app::events::SessionEvent, ffi::ffmpeg};

/// Containers FFmpeg can mux by extension that a clip may keep. Anything else
/// (including extensions whose guessed muxer is a raw elementary stream, like
/// `.m4v`) falls back to a container that holds the same codecs.
const KEPT_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "avi", "ts", "m2ts", "mts", "flv", "wmv", "asf", "mpg", "mpeg",
    "m4a", "mp3", "flac", "wav", "ogg", "opus",
];

pub struct ClipExporter {
    worker: Option<ExportWorker>,
    event_tx: Sender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
}

struct ExportWorker {
    handle: JoinHandle<()>,
    cancel: Arc<AtomicBool>,
}

impl Default for ClipExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipExporter {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            worker: None,
            event_tx,
            event_rx,
        }
    }

    pub fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    /// Start exporting `[start, end)` of `source` (source presentation times;
    /// `end == None` runs to the end of the file) to `destination`. The caller
    /// checks [`Self::is_running`] first; only one export runs at a time.
    pub fn start(
        &mut self,
        source: PathBuf,
        destination: PathBuf,
        start: Duration,
        end: Option<Duration>,
    ) -> Result<(), String> {
        debug_assert!(self.worker.is_none(), "one clip export at a time");
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let event_tx = self.event_tx.clone();
        let handle = thread::Builder::new()
            .name("fastplay-clip-export".into())
            .spawn(move || {
                let io_cancel = Arc::clone(&worker_cancel);
                let result = ffmpeg::export_clip(
                    &source,
                    &destination,
                    start,
                    end,
                    Box::new(move || io_cancel.load(Ordering::Relaxed)),
                )
                .map(|summary| {
                    crate::flog!(
                        "clip_export_done path={} actual_start_ms={} packets={}",
                        destination.display(),
                        summary.actual_start.as_millis(),
                        summary.packets_written
                    );
                    destination
                });
                // The receiver only goes away with the session at shutdown.
                let _ = event_tx.send(SessionEvent::ClipExportFinished { result });
            })
            .map_err(|error| format!("could not start clip export: {error}"))?;
        self.worker = Some(ExportWorker { handle, cancel });
        Ok(())
    }

    /// The finished export's event, if the worker has reported. Reaps the
    /// worker thread when it does.
    pub fn poll(&mut self) -> Option<SessionEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.handle.join();
                }
                Some(event)
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Cancel any running export and wait for it. The worker checks the flag
    /// per packet and from FFmpeg's blocking I/O, then removes its partial
    /// output, so this returns promptly.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Relaxed);
            let _ = worker.handle.join();
        }
    }
}

/// The range to export for the given marks, or `None` when no mark is set. A
/// missing in-point starts at the beginning of the file; a missing out-point
/// (`None` end) runs to the end of the file.
pub fn export_range(
    in_point: Option<Duration>,
    out_point: Option<Duration>,
) -> Option<(Duration, Option<Duration>)> {
    if in_point.is_none() && out_point.is_none() {
        return None;
    }
    Some((in_point.unwrap_or(Duration::ZERO), out_point))
}

/// A free path next to `source` for a clip of `[start, end)`, e.g.
/// `movie_clip_00-01-23.500_00-02-10.000.mp4`. `exists` is consulted so an
/// earlier export of the same range is never overwritten.
pub fn export_destination(
    source: &Path,
    start: Duration,
    end: Option<Duration>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let extension = clip_extension(source);
    let end_label = end.map(format_stamp).unwrap_or_else(|| "end".into());
    let base = format!("{stem}_clip_{}_{end_label}", format_stamp(start));
    let directory = source.parent().unwrap_or_else(|| Path::new(""));

    let mut candidate = directory.join(format!("{base}.{extension}"));
    let mut suffix = 2u32;
    while exists(&candidate) {
        candidate = directory.join(format!("{base} ({suffix}).{extension}"));
        suffix += 1;
    }
    candidate
}

/// The clip keeps the source container when it is a known muxer, otherwise
/// uses Matroska, which can hold any codec a playable source carries.
fn clip_extension(source: &Path) -> String {
    let extension = source
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "m4v" => "mp4".into(),
        known if KEPT_EXTENSIONS.contains(&known) => known.into(),
        _ => "mkv".into(),
    }
}

/// `HH-MM-SS.mmm` — `:` is not allowed in Windows file names.
fn format_stamp(position: Duration) -> String {
    let total_millis = position.as_millis();
    let millis = total_millis % 1000;
    let total_seconds = total_millis / 1000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3600;
    format!("{hours:02}-{minutes:02}-{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn no_marks_means_nothing_to_export() {
        assert_eq!(export_range(None, None), None);
    }

    #[test]
    fn range_uses_marks_and_opens_missing_ends() {
        assert_eq!(
            export_range(Some(secs(5)), Some(secs(10))),
            Some((secs(5), Some(secs(10))))
        );
        assert_eq!(export_range(Some(secs(5)), None), Some((secs(5), None)));
        assert_eq!(
            export_range(None, Some(secs(10))),
            Some((Duration::ZERO, Some(secs(10))))
        );
    }

    #[test]
    fn stamp_is_file_name_safe() {
        assert_eq!(format_stamp(Duration::from_millis(83_500)), "00-01-23.500");
        assert_eq!(format_stamp(secs(3 * 3600 + 7)), "03-00-07.000");
    }

    #[test]
    fn destination_sits_next_to_source_and_keeps_container() {
        let path = export_destination(
            Path::new("videos/movie.mp4"),
            Duration::from_millis(83_500),
            Some(secs(130)),
            |_| false,
        );
        assert_eq!(
            path,
            Path::new("videos").join("movie_clip_00-01-23.500_00-02-10.000.mp4")
        );
    }

    #[test]
    fn destination_without_end_is_labelled_end() {
        let path = export_destination(Path::new("a.mkv"), secs(1), None, |_| false);
        assert_eq!(path, Path::new("a_clip_00-00-01.000_end.mkv"));
    }

    #[test]
    fn destination_never_overwrites() {
        let taken = [
            PathBuf::from("a_clip_00-00-01.000_00-00-02.000.mp4"),
            PathBuf::from("a_clip_00-00-01.000_00-00-02.000 (2).mp4"),
        ];
        let path = export_destination(Path::new("a.mp4"), secs(1), Some(secs(2)), |p| {
            taken.iter().any(|t| t == p)
        });
        assert_eq!(path, Path::new("a_clip_00-00-01.000_00-00-02.000 (3).mp4"));
    }

    #[test]
    fn unknown_or_raw_containers_fall_back() {
        assert_eq!(clip_extension(Path::new("a.MP4")), "mp4");
        assert_eq!(clip_extension(Path::new("a.m4v")), "mp4");
        assert_eq!(clip_extension(Path::new("a.rmvb")), "mkv");
        assert_eq!(clip_extension(Path::new("a")), "mkv");
    }
}
