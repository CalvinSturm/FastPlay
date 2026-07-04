#![allow(dead_code)]

use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedReviewQueue {
    pub name: String,
    pub items: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct SavedReviewQueues {
    queues: Vec<SavedReviewQueue>,
}

pub const MAX_REVIEW_QUEUE_NAME_CHARS: usize = 80;

fn storage_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("FastPlay")
            .join("review_queues.tsv"),
    )
}

impl SavedReviewQueues {
    pub fn load() -> Self {
        let Some(path) = storage_path() else {
            return Self::default();
        };
        Self::load_from_path(&path).unwrap_or_default()
    }

    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let contents = fs::read_to_string(path)?;
        Ok(Self::parse(&contents))
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

    pub fn queues(&self) -> &[SavedReviewQueue] {
        &self.queues
    }

    pub fn upsert(&mut self, name: impl AsRef<str>, items: Vec<PathBuf>) {
        let Some(name) = normalize_queue_name(name.as_ref()) else {
            return;
        };
        if items.is_empty() {
            return;
        }
        if let Some(existing) = self
            .queues
            .iter_mut()
            .find(|queue| queue.name.eq_ignore_ascii_case(&name))
        {
            existing.name = name;
            existing.items = items;
            return;
        }
        self.queues.push(SavedReviewQueue { name, items });
    }

    pub fn remove_index(&mut self, index: usize) -> bool {
        if index >= self.queues.len() {
            return false;
        }
        self.queues.remove(index);
        true
    }

    pub fn get(&self, name: &str) -> Option<&SavedReviewQueue> {
        self.queues
            .iter()
            .find(|queue| queue.name.eq_ignore_ascii_case(name))
    }

    pub fn existing_items(queue: &SavedReviewQueue) -> Vec<PathBuf> {
        queue
            .items
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect()
    }

    fn parse(contents: &str) -> Self {
        let mut queues = Vec::new();
        let mut current: Option<SavedReviewQueue> = None;
        for line in contents.lines() {
            let Some((kind, value)) = line.split_once('\t') else {
                continue;
            };
            match kind {
                "Q" => {
                    if let Some(queue) = current.take() {
                        if !queue.items.is_empty() {
                            queues.push(queue);
                        }
                    }
                    if let Some(name) = unescape_field(value) {
                        if !name.trim().is_empty() {
                            current = Some(SavedReviewQueue {
                                name,
                                items: Vec::new(),
                            });
                        }
                    }
                }
                "I" => {
                    if let (Some(queue), Some(item)) = (current.as_mut(), unescape_field(value)) {
                        if !item.is_empty() {
                            queue.items.push(PathBuf::from(item));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(queue) = current {
            if !queue.items.is_empty() {
                queues.push(queue);
            }
        }
        Self { queues }
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        for queue in &self.queues {
            if queue.items.is_empty() {
                continue;
            }
            out.push_str(&format!("Q\t{}\n", escape_field(&queue.name)));
            for item in &queue.items {
                out.push_str(&format!("I\t{}\n", escape_field(&item.to_string_lossy())));
            }
        }
        out
    }
}

pub fn bounded_queue_name(name: &str) -> String {
    name.chars().take(MAX_REVIEW_QUEUE_NAME_CHARS).collect()
}

fn normalize_queue_name(name: &str) -> Option<String> {
    let bounded = bounded_queue_name(name);
    let trimmed = bounded.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_file(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "fastplay_review_queue_{tag}_{}_{}.tsv",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn queue_storage_roundtrips() {
        let path = unique_temp_file("roundtrip");
        let mut queues = SavedReviewQueues::default();
        queues.upsert(
            "Client notes",
            vec![PathBuf::from(r"C:\v\a.mp4"), PathBuf::from(r"C:\v\b.mp4")],
        );
        queues.save_to_path(&path).unwrap();
        let loaded = SavedReviewQueues::load_from_path(&path).unwrap();
        assert_eq!(loaded.queues(), queues.queues());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn upsert_replaces_case_insensitively() {
        let mut queues = SavedReviewQueues::default();
        queues.upsert("Review", vec![PathBuf::from("a.mp4")]);
        queues.upsert("review", vec![PathBuf::from("b.mp4")]);
        assert_eq!(queues.queues().len(), 1);
        assert_eq!(queues.queues()[0].name, "review");
        assert_eq!(queues.queues()[0].items, vec![PathBuf::from("b.mp4")]);
    }

    #[test]
    fn queue_name_is_bounded() {
        let mut queues = SavedReviewQueues::default();
        queues.upsert(
            "x".repeat(MAX_REVIEW_QUEUE_NAME_CHARS + 20),
            vec![PathBuf::from("a.mp4")],
        );
        assert_eq!(
            queues.queues()[0].name.chars().count(),
            MAX_REVIEW_QUEUE_NAME_CHARS
        );
    }

    #[test]
    fn remove_index_deletes_selected_queue() {
        let mut queues = SavedReviewQueues::default();
        queues.upsert("a", vec![PathBuf::from("a.mp4")]);
        queues.upsert("b", vec![PathBuf::from("b.mp4")]);
        assert!(queues.remove_index(0));
        assert_eq!(queues.queues().len(), 1);
        assert_eq!(queues.queues()[0].name, "b");
        assert!(!queues.remove_index(99));
    }

    #[test]
    fn existing_items_filters_missing_files() {
        let dir = std::env::temp_dir().join(format!(
            "fastplay_review_queue_existing_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exists = dir.join("exists.mp4");
        std::fs::write(&exists, b"x").unwrap();
        let queue = SavedReviewQueue {
            name: "Review".to_string(),
            items: vec![exists.clone(), dir.join("missing.mp4")],
        };
        assert_eq!(SavedReviewQueues::existing_items(&queue), vec![exists]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
