use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct MediaSource {
    path: PathBuf,
}

impl MediaSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
