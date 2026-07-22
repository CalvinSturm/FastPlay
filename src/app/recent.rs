//! Recent-files history and resume-position persistence.
//!
//! This is the shared backend for two product features: the Recent overlay and
//! automatic resume-on-open. Both read the same per-file records, so opening a
//! file (from the CLI, a drag/drop, the file dialog, or the Recent overlay)
//! resumes where the user left off, and the Recent overlay lists the same
//! files with their saved positions.
//!
//! `PlaybackSession` is unaware of this; the event loop in `main` owns a
//! [`RecentFiles`], records progress when switching files or exiting, and feeds
//! rows to the overlay. It is intentionally backend-first so a later command
//! palette or "open with" flow can reuse the same state.
//!
//! Storage is a small tab-separated file under `%APPDATA%\FastPlay` (no extra
//! dependency); each line is `last_position_ms\tduration_ms\tlast_opened_ms\tpath`
//! with the path last so it may contain spaces (Windows paths cannot contain
//! tabs or newlines).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of files retained in the history.
pub const MAX_RECENT: usize = 20;

/// Only resume if the user got more than this far in.
const MIN_RESUME_MS: u64 = 30_000;
/// Do not resume if the saved position is at/after this fraction of the
/// duration (treat the file as "finished" and start from the beginning).
const RESUME_BEFORE_FRACTION: f64 = 0.95;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub last_position_ms: u64,
    pub duration_ms: u64,
    pub last_opened_unix_ms: u64,
}

/// The recent-files list, newest first.
#[derive(Debug, Default)]
pub struct RecentFiles {
    entries: Vec<RecentEntry>,
}

/// Windows paths are case-insensitive; dedupe on a lowercased key.
fn key_for(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// The resume target for a saved (position, duration), or `None` to start from
/// the beginning. Pure so it can be unit-tested without any I/O.
///
/// Resume only when the user watched more than [`MIN_RESUME_MS`] and stopped
/// before [`RESUME_BEFORE_FRACTION`] of the (known) duration.
pub fn resume_target_ms(last_position_ms: u64, duration_ms: u64) -> Option<u64> {
    if last_position_ms <= MIN_RESUME_MS {
        return None;
    }
    if duration_ms == 0 {
        // Unknown duration: cannot apply the near-end rule, so be conservative.
        return None;
    }
    if (last_position_ms as f64) >= RESUME_BEFORE_FRACTION * duration_ms as f64 {
        return None;
    }
    Some(last_position_ms)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn storage_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("FastPlay").join("recent.tsv"))
}

impl RecentFiles {
    /// Load the history from disk, tolerating a missing or partially corrupt
    /// file (unparseable lines are skipped).
    pub fn load() -> Self {
        let Some(path) = storage_path() else {
            return Self::default();
        };
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut entries = Vec::new();
        for line in contents.lines() {
            if let Some(entry) = parse_line(line) {
                entries.push(entry);
            }
        }
        entries.truncate(MAX_RECENT);
        Self { entries }
    }

    /// Persist the history, best-effort (errors are ignored, like volume).
    ///
    /// Writes a sibling temporary file and renames it over the real one. A plain
    /// `fs::write` truncates first, so a crash or power loss between the
    /// truncate and the write leaves an empty or half-written history — and this
    /// is called on the way out of the process, where a hard exit is the norm
    /// rather than the exception. `fs::rename` replaces an existing file
    /// atomically on Windows for same-volume paths, which these always are.
    pub fn save(&self) {
        let Some(path) = storage_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                e.last_position_ms,
                e.duration_ms,
                e.last_opened_unix_ms,
                e.path.to_string_lossy()
            ));
        }

        let temp = path.with_extension("tsv.tmp");
        if fs::write(&temp, &out).is_err() {
            // Could not stage the replacement; leave the existing history alone
            // rather than destroying it.
            let _ = fs::remove_file(&temp);
            return;
        }
        if fs::rename(&temp, &path).is_err() {
            // Rename failed (e.g. the target is locked). Fall back to a direct
            // write so the history still updates, and clean up the staging file.
            let _ = fs::write(&path, &out);
            let _ = fs::remove_file(&temp);
        }
    }

    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }

    /// Not called from non-test code; kept as the natural companion to
    /// [`entries`] for callers that only need emptiness.
    ///
    /// [`entries`]: Self::entries
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mark `path` as just opened: move it to the front (preserving any saved
    /// position/duration) or insert it new, then cap the list. `now_ms` is the
    /// wall-clock time in unix milliseconds (injected for testability).
    pub fn touch_opened(&mut self, path: &Path, now_ms: u64) {
        let key = key_for(path);
        let existing = self
            .entries
            .iter()
            .position(|e| key_for(&e.path) == key)
            .map(|i| self.entries.remove(i));
        let mut entry = existing.unwrap_or_else(|| RecentEntry {
            path: path.to_path_buf(),
            last_position_ms: 0,
            duration_ms: 0,
            last_opened_unix_ms: now_ms,
        });
        entry.last_opened_unix_ms = now_ms;
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_RECENT);
    }

    /// Convenience wrapper using the current wall clock.
    pub fn touch_opened_now(&mut self, path: &Path) {
        self.touch_opened(path, now_unix_ms());
    }

    /// Update the saved playback progress for `path` (if it is in the list).
    /// Does not reorder the list — progress is captured when leaving a file.
    pub fn update_progress(&mut self, path: &Path, position_ms: u64, duration_ms: u64) {
        let key = key_for(path);
        if let Some(entry) = self.entries.iter_mut().find(|e| key_for(&e.path) == key) {
            entry.last_position_ms = position_ms;
            if duration_ms > 0 {
                entry.duration_ms = duration_ms;
            }
        }
    }

    /// The resume position for `path`, applying [`resume_target_ms`] to its
    /// saved progress. `None` if the file is unknown or should start from 0.
    pub fn resume_target(&self, path: &Path) -> Option<u64> {
        let key = key_for(path);
        let entry = self.entries.iter().find(|e| key_for(&e.path) == key)?;
        resume_target_ms(entry.last_position_ms, entry.duration_ms)
    }

    pub fn remove_index(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
    }

    /// Reserved for a clear-history binding; exercised by tests only.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

fn parse_line(line: &str) -> Option<RecentEntry> {
    // path is last so it may contain spaces; numeric fields never contain tabs.
    let mut parts = line.splitn(4, '\t');
    let last_position_ms = parts.next()?.parse::<u64>().ok()?;
    let duration_ms = parts.next()?.parse::<u64>().ok()?;
    let last_opened_unix_ms = parts.next()?.parse::<u64>().ok()?;
    let path = parts.next()?;
    if path.is_empty() {
        return None;
    }
    Some(RecentEntry {
        path: PathBuf::from(path),
        last_position_ms,
        duration_ms,
        last_opened_unix_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(r: &RecentFiles) -> Vec<String> {
        r.entries()
            .iter()
            .map(|e| e.path.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn touch_opened_moves_to_front_and_dedupes() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new("a.mp4"), 1);
        r.touch_opened(Path::new("b.mp4"), 2);
        r.touch_opened(Path::new("a.mp4"), 3); // reopen a -> front, no dupe
        assert_eq!(paths(&r), vec!["a.mp4", "b.mp4"]);
    }

    #[test]
    fn dedupe_is_case_insensitive() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new(r"C:\Videos\Clip.mp4"), 1);
        r.touch_opened(Path::new(r"c:\videos\clip.mp4"), 2);
        assert_eq!(r.entries().len(), 1);
    }

    #[test]
    fn touch_opened_preserves_saved_progress() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new("a.mp4"), 1);
        r.update_progress(Path::new("a.mp4"), 120_000, 600_000);
        r.touch_opened(Path::new("b.mp4"), 2);
        r.touch_opened(Path::new("a.mp4"), 3); // reopening must keep a's position
        let a = &r.entries()[0];
        assert_eq!(a.path, PathBuf::from("a.mp4"));
        assert_eq!(a.last_position_ms, 120_000);
        assert_eq!(a.duration_ms, 600_000);
    }

    #[test]
    fn caps_at_max_recent() {
        let mut r = RecentFiles::default();
        for i in 0..(MAX_RECENT + 5) {
            r.touch_opened(Path::new(&format!("f{i}.mp4")), i as u64);
        }
        assert_eq!(r.entries().len(), MAX_RECENT);
        // Newest (highest index) is at the front.
        assert_eq!(
            r.entries()[0].path,
            PathBuf::from(format!("f{}.mp4", MAX_RECENT + 4))
        );
    }

    #[test]
    fn update_progress_no_op_for_unknown_file() {
        let mut r = RecentFiles::default();
        r.update_progress(Path::new("ghost.mp4"), 50_000, 100_000);
        assert!(r.is_empty());
    }

    #[test]
    fn update_progress_keeps_duration_when_unknown() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new("a.mp4"), 1);
        r.update_progress(Path::new("a.mp4"), 10_000, 600_000);
        r.update_progress(Path::new("a.mp4"), 20_000, 0); // dur unknown -> keep old
        assert_eq!(r.entries()[0].duration_ms, 600_000);
        assert_eq!(r.entries()[0].last_position_ms, 20_000);
    }

    #[test]
    fn resume_only_after_30s() {
        assert_eq!(resume_target_ms(29_000, 600_000), None);
        assert_eq!(resume_target_ms(31_000, 600_000), Some(31_000));
    }

    #[test]
    fn resume_not_near_end() {
        // 95% of 100s = 95s.
        assert_eq!(resume_target_ms(94_000, 100_000), Some(94_000));
        assert_eq!(resume_target_ms(96_000, 100_000), None);
    }

    #[test]
    fn resume_none_when_duration_unknown() {
        assert_eq!(resume_target_ms(120_000, 0), None);
    }

    #[test]
    fn resume_target_reads_saved_entry() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new("a.mp4"), 1);
        r.update_progress(Path::new("a.mp4"), 120_000, 600_000);
        assert_eq!(r.resume_target(Path::new("a.mp4")), Some(120_000));
        assert_eq!(r.resume_target(Path::new("unknown.mp4")), None);
    }

    #[test]
    fn remove_and_clear() {
        let mut r = RecentFiles::default();
        r.touch_opened(Path::new("a.mp4"), 1);
        r.touch_opened(Path::new("b.mp4"), 2);
        r.remove_index(0); // removes b (front)
        assert_eq!(paths(&r), vec!["a.mp4"]);
        r.remove_index(99); // out of range -> no-op
        assert_eq!(paths(&r), vec!["a.mp4"]);
        r.clear();
        assert!(r.is_empty());
    }

    #[test]
    fn parse_line_roundtrip_and_rejects_garbage() {
        let e = parse_line("252000\t900000\t1780000000000\tC:\\Videos\\my clip.mp4").unwrap();
        assert_eq!(e.last_position_ms, 252_000);
        assert_eq!(e.duration_ms, 900_000);
        assert_eq!(e.last_opened_unix_ms, 1_780_000_000_000);
        assert_eq!(e.path, PathBuf::from("C:\\Videos\\my clip.mp4"));
        assert!(parse_line("garbage").is_none());
        assert!(parse_line("12\t34\t56\t").is_none()); // empty path
    }
}
