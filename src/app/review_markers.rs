#![allow(dead_code)]

use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewMarker {
    pub media_path: PathBuf,
    pub timestamp_ms: u64,
    pub note: Option<String>,
}

#[derive(Debug, Default)]
pub struct ReviewMarkers {
    markers: Vec<ReviewMarker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerExportFormat {
    Txt,
    Csv,
}

pub const MAX_MARKER_NOTE_CHARS: usize = 240;

fn key_for(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

fn storage_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("FastPlay")
            .join("review_markers.tsv"),
    )
}

pub fn default_export_directory() -> io::Result<PathBuf> {
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile).join("Pictures").join("FastPlay"));
    }
    Ok(std::env::current_dir()?.join("FastPlay Screenshots"))
}

impl ReviewMarkers {
    pub fn load() -> Self {
        let Some(path) = storage_path() else {
            return Self::default();
        };
        Self::load_from_path(&path).unwrap_or_default()
    }

    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let contents = fs::read_to_string(path)?;
        let markers = contents.lines().filter_map(parse_line).collect();
        Ok(Self { markers })
    }

    pub fn save(&self) {
        let Some(path) = storage_path() else {
            return;
        };
        let _ = self.save_to_path(&path);
    }

    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.serialize())
    }

    pub fn markers(&self) -> &[ReviewMarker] {
        &self.markers
    }

    pub fn markers_for<'a>(&'a self, media_path: &Path) -> Vec<&'a ReviewMarker> {
        let key = key_for(media_path);
        let mut markers: Vec<&ReviewMarker> = self
            .markers
            .iter()
            .filter(|marker| key_for(&marker.media_path) == key)
            .collect();
        markers.sort_by_key(|marker| marker.timestamp_ms);
        markers
    }

    pub fn add_marker(&mut self, media_path: &Path, timestamp_ms: u64) {
        let key = key_for(media_path);
        if self
            .markers
            .iter()
            .any(|marker| key_for(&marker.media_path) == key && marker.timestamp_ms == timestamp_ms)
        {
            return;
        }
        self.markers.push(ReviewMarker {
            media_path: media_path.to_path_buf(),
            timestamp_ms,
            note: None,
        });
        self.sort();
    }

    pub fn remove_for_file_index(&mut self, media_path: &Path, selected: usize) -> bool {
        let Some(index) = self.storage_index_for_file_index(media_path, selected) else {
            return false;
        };
        self.markers.remove(index);
        true
    }

    pub fn set_note_for_file_index(
        &mut self,
        media_path: &Path,
        selected: usize,
        note: &str,
    ) -> bool {
        let Some(index) = self.storage_index_for_file_index(media_path, selected) else {
            return false;
        };
        self.markers[index].note = normalize_note(note);
        true
    }

    pub fn export_for_file(
        &self,
        media_path: &Path,
        directory: &Path,
        format: MarkerExportFormat,
    ) -> io::Result<PathBuf> {
        fs::create_dir_all(directory)?;
        let extension = match format {
            MarkerExportFormat::Txt => "txt",
            MarkerExportFormat::Csv => "csv",
        };
        let file_stem = sanitized_file_stem(media_path);
        let path = directory.join(format!("{file_stem}-markers.{extension}"));
        let markers = self.markers_for(media_path);
        let contents = match format {
            MarkerExportFormat::Txt => export_txt(media_path, &markers),
            MarkerExportFormat::Csv => export_csv(&markers),
        };
        fs::write(&path, contents)?;
        Ok(path)
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        for marker in &self.markers {
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                marker.timestamp_ms,
                escape_field(&marker.media_path.to_string_lossy()),
                escape_field(marker.note.as_deref().unwrap_or(""))
            ));
        }
        out
    }

    fn sort(&mut self) {
        self.markers.sort_by(|a, b| {
            key_for(&a.media_path)
                .cmp(&key_for(&b.media_path))
                .then(a.timestamp_ms.cmp(&b.timestamp_ms))
        });
    }

    fn storage_index_for_file_index(&self, media_path: &Path, selected: usize) -> Option<usize> {
        let key = key_for(media_path);
        let mut matches: Vec<(usize, u64)> = self
            .markers
            .iter()
            .enumerate()
            .filter(|(_, marker)| key_for(&marker.media_path) == key)
            .map(|(index, marker)| (index, marker.timestamp_ms))
            .collect();
        matches.sort_by_key(|(_, timestamp_ms)| *timestamp_ms);
        matches.get(selected).map(|(index, _)| *index)
    }
}

pub fn bounded_note_text(note: &str) -> String {
    note.chars().take(MAX_MARKER_NOTE_CHARS).collect()
}

fn normalize_note(note: &str) -> Option<String> {
    let bounded = bounded_note_text(note);
    let trimmed = bounded.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_line(line: &str) -> Option<ReviewMarker> {
    let mut parts = line.splitn(3, '\t');
    let timestamp_ms = parts.next()?.parse::<u64>().ok()?;
    let media_path = PathBuf::from(unescape_field(parts.next()?)?);
    if media_path.as_os_str().is_empty() {
        return None;
    }
    let note = unescape_field(parts.next()?)?;
    Some(ReviewMarker {
        media_path,
        timestamp_ms,
        note: if note.is_empty() { None } else { Some(note) },
    })
}

fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            _ => return None,
        }
    }
    Some(out)
}

pub fn format_marker_timestamp_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes}:{seconds:02}.{millis:03}")
    }
}

pub fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn export_txt(media_path: &Path, markers: &[&ReviewMarker]) -> String {
    let filename = media_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| media_path.to_str().unwrap_or("(unknown)"));
    let mut out = format!("File: {filename}\n");
    for marker in markers {
        out.push_str(&format_marker_timestamp_ms(marker.timestamp_ms));
        if let Some(note) = marker.note.as_deref() {
            out.push_str(&format!("\t{note}"));
        }
        out.push('\n');
    }
    out
}

fn export_csv(markers: &[&ReviewMarker]) -> String {
    let mut out = "file,timestamp_ms,timestamp_display,note\n".to_string();
    for marker in markers {
        let file = marker
            .media_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| marker.media_path.to_str().unwrap_or("(unknown)"));
        out.push_str(&format!(
            "{},{},{},{}\n",
            csv_field(file),
            marker.timestamp_ms,
            csv_field(&format_marker_timestamp_ms(marker.timestamp_ms)),
            csv_field(marker.note.as_deref().unwrap_or(""))
        ));
    }
    out
}

fn sanitized_file_stem(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("fastplay");
    let sanitized: String = stem
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ if ch.is_control() => '_',
            _ => ch,
        })
        .collect();
    if sanitized.trim().is_empty() {
        "fastplay".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_file(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "fastplay_review_markers_{tag}_{}_{}.tsv",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn timestamp_format_includes_milliseconds() {
        assert_eq!(format_marker_timestamp_ms(83_456), "1:23.456");
        assert_eq!(format_marker_timestamp_ms(3_723_004), "1:02:03.004");
    }

    #[test]
    fn csv_escapes_quotes_commas_and_newlines() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
    }

    #[test]
    fn marker_storage_roundtrips_notes_with_escapes() {
        let path = unique_temp_file("roundtrip");
        let mut markers = ReviewMarkers::default();
        markers.markers.push(ReviewMarker {
            media_path: PathBuf::from(r"C:\Videos\clip.mp4"),
            timestamp_ms: 42_000,
            note: Some("quote, tab\tnewline\nslash\\".to_string()),
        });
        markers.save_to_path(&path).unwrap();
        let loaded = ReviewMarkers::load_from_path(&path).unwrap();
        assert_eq!(loaded.markers, markers.markers);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn markers_for_sorts_by_time_and_matches_case_insensitively() {
        let mut markers = ReviewMarkers::default();
        markers.add_marker(Path::new(r"C:\V\Clip.MP4"), 2_000);
        markers.add_marker(Path::new(r"c:\v\clip.mp4"), 1_000);
        let got: Vec<u64> = markers
            .markers_for(Path::new(r"C:\v\CLIP.mp4"))
            .iter()
            .map(|marker| marker.timestamp_ms)
            .collect();
        assert_eq!(got, vec![1_000, 2_000]);
    }

    #[test]
    fn remove_selected_marker_uses_visible_sorted_order() {
        let mut markers = ReviewMarkers::default();
        let media = Path::new("clip.mp4");
        markers.add_marker(media, 3_000);
        markers.add_marker(media, 1_000);
        markers.add_marker(media, 2_000);
        assert!(markers.remove_for_file_index(media, 1));
        let got: Vec<u64> = markers
            .markers_for(media)
            .iter()
            .map(|marker| marker.timestamp_ms)
            .collect();
        assert_eq!(got, vec![1_000, 3_000]);
    }

    #[test]
    fn marker_note_is_bounded_and_persisted() {
        let path = unique_temp_file("bounded_note");
        let media = Path::new("clip.mp4");
        let mut markers = ReviewMarkers::default();
        markers.add_marker(media, 1_000);
        let long_note = "x".repeat(MAX_MARKER_NOTE_CHARS + 20);
        assert!(markers.set_note_for_file_index(media, 0, &long_note));
        assert_eq!(
            markers.markers_for(media)[0]
                .note
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            MAX_MARKER_NOTE_CHARS
        );

        markers.save_to_path(&path).unwrap();
        let loaded = ReviewMarkers::load_from_path(&path).unwrap();
        assert_eq!(
            loaded.markers_for(media)[0]
                .note
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            MAX_MARKER_NOTE_CHARS
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn blank_marker_note_clears_note() {
        let media = Path::new("clip.mp4");
        let mut markers = ReviewMarkers::default();
        markers.add_marker(media, 1_000);
        assert!(markers.set_note_for_file_index(media, 0, "needs review"));
        assert!(markers.set_note_for_file_index(media, 0, "   "));
        assert_eq!(markers.markers_for(media)[0].note, None);
    }

    #[test]
    fn export_txt_includes_marker_notes() {
        let dir = std::env::temp_dir().join(format!(
            "fastplay_review_markers_txt_export_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let media = Path::new(r"C:\Videos\clip.mp4");
        let mut markers = ReviewMarkers::default();
        markers.add_marker(media, 1_234);
        markers.set_note_for_file_index(media, 0, "tighten this cut");
        let out = markers
            .export_for_file(media, &dir, MarkerExportFormat::Txt)
            .unwrap();
        let contents = std::fs::read_to_string(&out).unwrap();
        assert!(contents.contains("0:01.234\ttighten this cut"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_csv_writes_expected_columns() {
        let dir = std::env::temp_dir().join(format!(
            "fastplay_review_markers_export_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut markers = ReviewMarkers::default();
        markers.markers.push(ReviewMarker {
            media_path: PathBuf::from(r"C:\Videos\clip, one.mp4"),
            timestamp_ms: 1_234,
            note: Some("needs \"review\"".to_string()),
        });
        let out = markers
            .export_for_file(
                Path::new(r"C:\Videos\clip, one.mp4"),
                &dir,
                MarkerExportFormat::Csv,
            )
            .unwrap();
        let contents = std::fs::read_to_string(&out).unwrap();
        assert!(contents.contains("file,timestamp_ms,timestamp_display,note"));
        assert!(contents.contains("\"clip, one.mp4\",1234,0:01.234,\"needs \"\"review\"\"\""));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
