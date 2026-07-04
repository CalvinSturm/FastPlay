// The queue is the backend for an upcoming queue/folder playback flow; nothing
// in the shipping binary constructs or drives it yet, so its public API reads as
// dead code in a binary crate. Mirror `recent.rs` and allow it module-wide until
// the open flow is wired to it.
#![allow(dead_code)]

//! A lightweight, in-memory queue for sequential file playback.
//!
//! `PlayQueue` is the backend for the queue/folder playback flow. It is a pure
//! data structure: it holds an ordered, de-duplicated list of file paths and a
//! cursor marking the item currently playing. It performs **no** filesystem I/O
//! during navigation and knows nothing about `PlaybackSession` — like
//! [`crate::app::recent::RecentFiles`], it is owned by the event loop, which
//! drives opens. Folder enumeration lives in the free [`media_files_in_folder`]
//! helper, kept deliberately separate from cursor movement.
//!
//! ## Cursor / commit discipline
//!
//! Looking up the next/previous item ([`PlayQueue::next_path`] /
//! [`PlayQueue::previous_path`]) never moves the cursor — it only returns a
//! *candidate*. The caller opens that candidate and, only if the open succeeds,
//! commits the move ([`PlayQueue::commit_next`] / [`PlayQueue::commit_previous`]).
//! This keeps queue state from drifting if a future open fails:
//!
//! ```ignore
//! if let Some(candidate) = queue.next_path() {
//!     let candidate = candidate.to_path_buf();
//!     if open_media(&candidate).is_ok() {
//!         queue.commit_next();
//!     }
//! }
//! ```
//!
//! Navigation only ever produces a candidate when the queue holds more than one
//! item, and never wraps at the first/last entry.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::Chars;

use crate::app::media_ext::is_supported_media;

/// An ordered, de-duplicated list of media file paths with a playback cursor.
pub struct PlayQueue {
    items: Vec<PathBuf>,
    cursor: usize,
}

impl PlayQueue {
    /// A queue containing exactly one explicitly-opened file. The path is
    /// trusted as-is (no supported-extension filtering): the user chose it
    /// directly, and whether it can actually play is the open flow's call.
    pub fn single(path: PathBuf) -> Self {
        Self {
            items: vec![path],
            cursor: 0,
        }
    }

    /// A queue built from many candidate paths (multi-select, multi-drop, or a
    /// folder enumeration). Unsupported files and subtitles are dropped, the
    /// remainder is sorted in a stable natural order (so `Episode 2` precedes
    /// `Episode 10`), and case-insensitive duplicate paths are removed. The
    /// cursor starts at the first item. An all-unsupported (or empty) input
    /// yields an empty queue.
    pub fn from_paths<I: IntoIterator<Item = PathBuf>>(paths: I) -> Self {
        let mut items: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| is_supported_media(path))
            .collect();
        items.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        let mut seen = HashSet::new();
        items.retain(|path| seen.insert(dedup_key(path)));
        Self { items, cursor: 0 }
    }

    /// A queue built from an already-user-ordered list, preserving order while
    /// still filtering unsupported files and case-insensitive duplicates. Used
    /// for saved review queues, where the saved order is the user's intent.
    pub fn from_ordered_paths<I: IntoIterator<Item = PathBuf>>(paths: I) -> Self {
        let mut seen = HashSet::new();
        let items = paths
            .into_iter()
            .filter(|path| is_supported_media(path))
            .filter(|path| seen.insert(dedup_key(path)))
            .collect();
        Self { items, cursor: 0 }
    }

    /// A queue built from the supported media files directly inside `dir`
    /// (non-recursive). A missing or unreadable folder yields an empty queue.
    pub fn from_folder(dir: &Path) -> Self {
        Self::from_paths(media_files_in_folder(dir))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The queue entries in playback order.
    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    /// Index of the item currently playing.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The item currently playing, or `None` if the queue is empty.
    pub fn current(&self) -> Option<&Path> {
        self.items.get(self.cursor).map(PathBuf::as_path)
    }

    /// The next candidate to play **without** moving the cursor. `None` at the
    /// last item or when the queue holds a single item (nothing to advance to).
    pub fn next_path(&self) -> Option<&Path> {
        if self.items.len() <= 1 {
            return None;
        }
        self.items.get(self.cursor + 1).map(PathBuf::as_path)
    }

    /// The previous candidate to play **without** moving the cursor. `None` at
    /// the first item or when the queue holds a single item.
    pub fn previous_path(&self) -> Option<&Path> {
        if self.items.len() <= 1 || self.cursor == 0 {
            return None;
        }
        self.items.get(self.cursor - 1).map(PathBuf::as_path)
    }

    /// Commit a move to the next item. No-op (returns `false`) at the last item
    /// — the queue never wraps. Call only after the next candidate opened.
    pub fn commit_next(&mut self) -> bool {
        if self.next_path().is_some() {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    /// Commit a move to the previous item. No-op (returns `false`) at the first
    /// item — the queue never wraps. Call only after the previous candidate
    /// opened.
    pub fn commit_previous(&mut self) -> bool {
        if self.previous_path().is_some() {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }
}

/// Case-insensitive normalized key for de-duplicating Windows paths. Mirrors the
/// approach in [`crate::app::recent`]: a lowercased path string. Deliberately
/// avoids `canonicalize`, which performs I/O and fails on paths that do not
/// currently exist.
fn dedup_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// Enumerate the supported media files directly inside `dir` (non-recursive),
/// in a stable natural order.
///
/// Filesystem-facing helper, intentionally separate from queue navigation.
/// Behavior at the edges (documented by tests):
/// - a missing or unreadable folder returns an empty list (not an error);
/// - unreadable individual entries are skipped rather than panicking;
/// - subdirectories and non-media files (including subtitles) are excluded.
pub fn media_files_in_folder(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported_media(path))
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    files
}

/// Compare two strings with numeric-aware ("natural") ordering, case-insensitive,
/// so `Episode 2` sorts before `Episode 10`. Runs of ASCII digits are compared by
/// numeric value (length after stripping leading zeros, then lexically); all other
/// characters compare by their lowercased value. Implemented inline to avoid
/// pulling in a natural-sort dependency for this single use.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let da = take_digits(&mut ai);
                    let db = take_digits(&mut bi);
                    match cmp_numeric(&da, &db) {
                        Ordering::Equal => {}
                        ord => return ord,
                    }
                } else {
                    match ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase()) {
                        Ordering::Equal => {
                            ai.next();
                            bi.next();
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

/// Consume and return the leading run of ASCII digits from `it`.
fn take_digits(it: &mut Peekable<Chars>) -> String {
    let mut run = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            run.push(c);
            it.next();
        } else {
            break;
        }
    }
    run
}

/// Compare two non-empty ASCII-digit runs by numeric value, without parsing
/// (so arbitrarily long runs cannot overflow). Equal values differing only in
/// leading zeros are ordered with the shorter (fewer leading zeros) run first
/// for a deterministic total order.
fn cmp_numeric(a: &str, b: &str) -> Ordering {
    let ta = a.trim_start_matches('0');
    let tb = b.trim_start_matches('0');
    match ta.len().cmp(&tb.len()) {
        Ordering::Equal => match ta.cmp(tb) {
            Ordering::Equal => a.len().cmp(&b.len()),
            ord => ord,
        },
        ord => ord,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn names(queue: &PlayQueue) -> Vec<String> {
        queue
            .items()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn three_item_queue() -> PlayQueue {
        PlayQueue::from_paths(vec![p(r"C:\v\a.mp4"), p(r"C:\v\b.mp4"), p(r"C:\v\c.mp4")])
    }

    // ── Construction ────────────────────────────────────────────────────────

    #[test]
    fn single_file_queue_holds_one_trusted_path() {
        let q = PlayQueue::single(p(r"C:\v\movie.mkv"));
        assert_eq!(q.len(), 1);
        assert!(!q.is_empty());
        assert_eq!(q.current(), Some(Path::new(r"C:\v\movie.mkv")));
    }

    #[test]
    fn single_file_queue_does_not_filter_unusual_extensions() {
        // Explicit single selection is trusted; the open flow decides playability.
        let q = PlayQueue::single(p(r"C:\v\weird.xyz"));
        assert_eq!(q.len(), 1);
        assert_eq!(q.current(), Some(Path::new(r"C:\v\weird.xyz")));
    }

    #[test]
    fn from_paths_filters_unsupported_and_subtitles() {
        let q = PlayQueue::from_paths(vec![
            p(r"C:\v\a.mp4"),
            p(r"C:\v\readme.txt"),
            p(r"C:\v\subs.srt"),
            p(r"C:\v\b.mkv"),
        ]);
        assert_eq!(names(&q), vec!["a.mp4", "b.mkv"]);
    }

    #[test]
    fn from_paths_empty_input_is_empty_queue() {
        let q = PlayQueue::from_paths(Vec::<PathBuf>::new());
        assert!(q.is_empty());
        assert_eq!(q.current(), None);
        assert_eq!(q.next_path(), None);
        assert_eq!(q.previous_path(), None);
    }

    #[test]
    fn from_paths_all_unsupported_is_empty_queue() {
        let q = PlayQueue::from_paths(vec![p("a.txt"), p("b.srt")]);
        assert!(q.is_empty());
    }

    #[test]
    fn from_paths_dedupes_case_insensitively() {
        let q = PlayQueue::from_paths(vec![p(r"C:\v\Clip.mp4"), p(r"C:\v\clip.MP4")]);
        assert_eq!(q.len(), 1);
    }

    // ── Ordering ────────────────────────────────────────────────────────────

    #[test]
    fn from_paths_sorts_naturally() {
        let q = PlayQueue::from_paths(vec![
            p(r"C:\v\Episode 10.mp4"),
            p(r"C:\v\Episode 2.mp4"),
            p(r"C:\v\Episode 1.mp4"),
        ]);
        assert_eq!(
            names(&q),
            vec!["Episode 1.mp4", "Episode 2.mp4", "Episode 10.mp4"]
        );
    }

    #[test]
    fn from_ordered_paths_preserves_user_order() {
        let q = PlayQueue::from_ordered_paths(vec![
            p(r"C:\v\Episode 10.mp4"),
            p(r"C:\v\Episode 2.mp4"),
            p(r"C:\v\ignore.txt"),
            p(r"C:\v\Episode 10.mp4"),
        ]);
        assert_eq!(names(&q), vec!["Episode 10.mp4", "Episode 2.mp4"]);
    }

    #[test]
    fn natural_cmp_orders_numbers_by_value() {
        assert_eq!(natural_cmp("Episode 2", "Episode 10"), Ordering::Less);
        assert_eq!(natural_cmp("img9", "img10"), Ordering::Less);
        assert_eq!(natural_cmp("a", "B"), Ordering::Less); // case-insensitive
        assert_eq!(natural_cmp("file", "file"), Ordering::Equal);
        // Equal numeric value, differing leading zeros: deterministic, shorter first.
        assert_eq!(natural_cmp("v2", "v02"), Ordering::Less);
    }

    // ── Navigation: candidates do not move the cursor ───────────────────────

    #[test]
    fn lookup_does_not_move_cursor() {
        let q = three_item_queue();
        assert_eq!(q.cursor(), 0);
        assert_eq!(q.next_path(), Some(Path::new(r"C:\v\b.mp4")));
        assert_eq!(q.next_path(), Some(Path::new(r"C:\v\b.mp4"))); // still b
        assert_eq!(q.cursor(), 0);
        assert_eq!(q.current(), Some(Path::new(r"C:\v\a.mp4")));
    }

    #[test]
    fn single_item_queue_has_no_navigation() {
        let mut q = PlayQueue::single(p("only.mp4"));
        assert_eq!(q.next_path(), None);
        assert_eq!(q.previous_path(), None);
        assert!(!q.commit_next());
        assert!(!q.commit_previous());
        assert_eq!(q.cursor(), 0);
    }

    // ── Navigation: commit moves, never wraps ───────────────────────────────

    #[test]
    fn commit_next_advances_then_stops_at_last() {
        let mut q = three_item_queue();
        assert!(q.commit_next());
        assert_eq!(q.current(), Some(Path::new(r"C:\v\b.mp4")));
        assert!(q.commit_next());
        assert_eq!(q.current(), Some(Path::new(r"C:\v\c.mp4")));
        // At the last item: no candidate, no wrap, cursor unchanged.
        assert_eq!(q.next_path(), None);
        assert!(!q.commit_next());
        assert_eq!(q.current(), Some(Path::new(r"C:\v\c.mp4")));
    }

    #[test]
    fn commit_previous_retreats_then_stops_at_first() {
        let mut q = three_item_queue();
        // At the first item: no candidate, no wrap.
        assert!(!q.commit_previous());
        assert_eq!(q.cursor(), 0);
        q.commit_next();
        q.commit_next();
        assert!(q.commit_previous());
        assert_eq!(q.current(), Some(Path::new(r"C:\v\b.mp4")));
    }

    #[test]
    fn candidate_matches_committed_current() {
        let mut q = three_item_queue();
        let candidate = q.next_path().unwrap().to_path_buf();
        assert!(q.commit_next());
        assert_eq!(q.current(), Some(candidate.as_path()));
    }

    // ── Folder enumeration helper (filesystem-backed) ───────────────────────

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "fastplay_play_queue_{tag}_{}_{}",
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    }

    #[test]
    fn folder_enumeration_filters_sorts_and_excludes() {
        let dir = unique_temp_dir("enum");
        touch(&dir, "Episode 10.mp4");
        touch(&dir, "Episode 2.mp4");
        touch(&dir, "notes.txt");
        touch(&dir, "subs.srt");
        let files = media_files_in_folder(&dir);
        let got: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, vec!["Episode 2.mp4", "Episode 10.mp4"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_enumeration_ignores_subdirectories() {
        let dir = unique_temp_dir("subdir");
        touch(&dir, "a.mp4");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        touch(&dir.join("nested"), "b.mp4");
        let files = media_files_in_folder(&dir);
        let got: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, vec!["a.mp4"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_enumeration_missing_folder_is_empty() {
        // Documents the chosen behavior: missing folder -> empty list, not error.
        let dir = std::env::temp_dir().join("fastplay_play_queue_definitely_missing_dir_zzz");
        assert!(media_files_in_folder(&dir).is_empty());
    }

    #[test]
    fn from_folder_builds_queue_from_supported_files() {
        let dir = unique_temp_dir("from_folder");
        touch(&dir, "b.mp4");
        touch(&dir, "a.mp4");
        touch(&dir, "ignore.txt");
        let q = PlayQueue::from_folder(&dir);
        assert_eq!(names(&q), vec!["a.mp4", "b.mp4"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
