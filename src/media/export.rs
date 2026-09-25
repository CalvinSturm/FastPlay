//! Fast, lossless export of a marked media range through FFmpeg stream copy.
//!
//! Export is deliberately independent of playback. The caller owns the worker
//! thread and completion routing; this module only validates/plans/runs one
//! file operation.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRange {
    pub source: PathBuf,
    pub start: Duration,
    pub end: Duration,
}

impl ExportRange {
    pub fn new(
        source: impl Into<PathBuf>,
        start: Option<Duration>,
        end: Option<Duration>,
    ) -> Result<Self, &'static str> {
        let start = start.ok_or("Set In and Out points before exporting.")?;
        let end = end.ok_or("Set In and Out points before exporting.")?;
        if end <= start {
            return Err("Out point must be after the In point.");
        }
        Ok(Self {
            source: source.into(),
            start,
            end,
        })
    }

    pub fn duration(&self) -> Duration {
        self.end - self.start
    }
}

pub fn suggested_filename(source: &Path, start: Duration, end: Duration) -> String {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("clip");
    format!(
        "{stem}_clip_{}-{}.mp4",
        filename_timestamp(start),
        filename_timestamp(end)
    )
}

fn filename_timestamp(value: Duration) -> String {
    let total_seconds = value.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours:02}h{minutes:02}m{seconds:02}s")
    } else {
        format!("{minutes:02}m{seconds:02}s")
    }
}

fn ffmpeg_timestamp(value: Duration) -> String {
    format!("{}.{:06}", value.as_secs(), value.subsec_micros())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportStrategy {
    StreamCopy,
    AccurateTranscode,
}

pub fn ffmpeg_arguments(
    range: &ExportRange,
    temporary: &Path,
    strategy: ExportStrategy,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-ss".into(),
        ffmpeg_timestamp(range.start).into(),
        "-i".into(),
        range.source.as_os_str().to_owned(),
        "-t".into(),
        ffmpeg_timestamp(range.duration()).into(),
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "0:a?".into(),
        "-map_metadata".into(),
        "0".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-fflags".into(),
        "+genpts".into(),
    ];
    match strategy {
        ExportStrategy::StreamCopy => args.extend(["-c".into(), "copy".into()]),
        ExportStrategy::AccurateTranscode => args.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "18".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-movflags".into(),
            "+faststart".into(),
        ]),
    }
    args.extend(["-y".into(), temporary.as_os_str().to_owned()]);
    args
}

pub fn export_range(range: &ExportRange, destination: &Path) -> Result<(), String> {
    if same_file_path(&range.source, destination) {
        return Err("Choose a different filename from the source media.".into());
    }
    if destination.exists() {
        return Err("That file already exists. Choose a new filename.".into());
    }
    if !destination
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("mp4"))
    {
        return Err("Export clips must use the .mp4 filename extension.".into());
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        return Err("The selected export folder no longer exists.".into());
    }

    let temporary = temporary_path(destination);
    let ffmpeg = find_tool("ffmpeg.exe").ok_or_else(|| {
        "FFmpeg was not found. Install FFmpeg or place ffmpeg.exe beside FastPlay.".to_string()
    })?;
    let strategy = choose_strategy(range);
    let output = Command::new(ffmpeg)
        .args(ffmpeg_arguments(range, &temporary, strategy))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("Could not start FFmpeg: {error}"));

    let result = match output {
        Ok(output) if output.status.success() => fs::rename(&temporary, destination)
            .map_err(|error| format!("Could not finalize the exported clip: {error}")),
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("FFmpeg could not copy this media range")
                .trim()
                .to_string();
            Err(format!("Clip export failed: {detail}"))
        }
        Err(error) => Err(error),
    };

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn find_tool(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FASTPLAY_FFMPEG_PATH").map(PathBuf::from) {
        if name == "ffmpeg.exe" && path.is_file() {
            return Some(path);
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let adjacent = directory.join(name);
            if adjacent.is_file() {
                return Some(adjacent);
            }
        }
    }
    Some(PathBuf::from(name))
}

fn choose_strategy(range: &ExportRange) -> ExportStrategy {
    let mp4_family = range
        .source
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "mp4" | "mov" | "m4v"));
    if !mp4_family || range.start == Duration::ZERO {
        return if mp4_family && range.start == Duration::ZERO {
            ExportStrategy::StreamCopy
        } else {
            ExportStrategy::AccurateTranscode
        };
    }
    if in_point_is_keyframe(range) {
        ExportStrategy::StreamCopy
    } else {
        ExportStrategy::AccurateTranscode
    }
}

fn in_point_is_keyframe(range: &ExportRange) -> bool {
    let Some(ffprobe) = find_tool("ffprobe.exe") else {
        return false;
    };
    let interval = format!("{}%+0.250000", ffmpeg_timestamp(range.start));
    let output = Command::new(ffprobe)
        .args([
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-select_streams"),
            OsString::from("v:0"),
            OsString::from("-skip_frame"),
            OsString::from("nokey"),
            OsString::from("-read_intervals"),
            interval.into(),
            OsString::from("-show_entries"),
            OsString::from("frame=best_effort_timestamp_time"),
            OsString::from("-of"),
            OsString::from("default=noprint_wrappers=1:nokey=1"),
            range.source.as_os_str().to_owned(),
        ])
        .stdin(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .any(|timestamp| (timestamp - range.start.as_secs_f64()).abs() <= 0.050)
}

fn temporary_path(destination: &Path) -> PathBuf {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let stem = destination
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("clip");
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp4");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{stem}.fastplay-part-{}-{sequence}.{extension}",
        std::process::id()
    ))
}

fn same_file_path(left: &Path, right: &Path) -> bool {
    let absolute = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    };
    absolute(left)
        .to_string_lossy()
        .eq_ignore_ascii_case(&absolute(right).to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn range_requires_both_marks_and_strict_order() {
        assert!(ExportRange::new("a.mp4", None, Some(seconds(2))).is_err());
        assert!(ExportRange::new("a.mp4", Some(seconds(1)), None).is_err());
        assert!(ExportRange::new("a.mp4", Some(seconds(2)), Some(seconds(2))).is_err());
        assert!(ExportRange::new("a.mp4", Some(seconds(3)), Some(seconds(2))).is_err());
        assert!(ExportRange::new("a.mp4", Some(seconds(1)), Some(seconds(2))).is_ok());
    }

    #[test]
    fn filename_keeps_source_container_and_formats_marks() {
        assert_eq!(
            suggested_filename(Path::new("video.mp4"), seconds(32), seconds(47)),
            "video_clip_00m32s-00m47s.mp4"
        );
        assert_eq!(
            suggested_filename(Path::new("review.mkv"), seconds(3661), seconds(3723)),
            "review_clip_01h01m01s-01h02m03s.mp4"
        );
    }

    #[test]
    fn ffmpeg_timestamps_preserve_microseconds() {
        assert_eq!(
            ffmpeg_timestamp(Duration::new(32, 123_456_789)),
            "32.123456"
        );
    }

    #[test]
    fn command_uses_input_seek_stream_copy_and_duration() {
        let range = ExportRange::new(
            "source.mp4",
            Some(Duration::from_millis(1_250)),
            Some(Duration::from_millis(3_750)),
        )
        .unwrap();
        let args = ffmpeg_arguments(&range, Path::new("part.mp4"), ExportStrategy::StreamCopy);
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|pair| pair == ["-ss", "1.250000"]));
        assert!(args.windows(2).any(|pair| pair == ["-t", "2.500000"]));
        assert!(args.windows(2).any(|pair| pair == ["-c", "copy"]));
        assert_eq!(args.last().map(String::as_str), Some("part.mp4"));
    }

    #[test]
    fn accurate_strategy_uses_fast_high_quality_h264_and_aac() {
        let range = ExportRange::new("source.mkv", Some(seconds(1)), Some(seconds(2))).unwrap();
        assert_eq!(choose_strategy(&range), ExportStrategy::AccurateTranscode);
        let args = ffmpeg_arguments(
            &range,
            Path::new("part.mp4"),
            ExportStrategy::AccurateTranscode,
        );
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
        assert!(!args.windows(2).any(|pair| pair == ["-c", "copy"]));
    }

    #[test]
    fn mp4_range_from_start_is_safe_to_stream_copy() {
        let range = ExportRange::new("source.mp4", Some(Duration::ZERO), Some(seconds(2))).unwrap();
        assert_eq!(choose_strategy(&range), ExportStrategy::StreamCopy);
    }

    #[test]
    fn source_cannot_be_export_destination() {
        assert!(same_file_path(
            Path::new(r"C:\Videos\Clip.mp4"),
            Path::new(r"c:\videos\clip.mp4")
        ));
    }

    #[test]
    fn temporary_file_stays_beside_destination_and_keeps_extension() {
        let destination = Path::new(r"C:\Videos\clip.mp4");
        let temporary = temporary_path(destination);
        assert_eq!(temporary.parent(), destination.parent());
        assert_eq!(
            temporary.extension().and_then(|value| value.to_str()),
            Some("mp4")
        );
        assert!(temporary
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("fastplay-part"));
    }
}
