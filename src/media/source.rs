use std::path::{Path, PathBuf};

use crate::media::video::VideoDecodePreference;

#[derive(Clone, Debug)]
pub struct MediaSource {
    path: PathBuf,
    decode_preference: VideoDecodePreference,
}

impl MediaSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            decode_preference: VideoDecodePreference::Auto,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn with_decode_preference(mut self, decode_preference: VideoDecodePreference) -> Self {
        self.decode_preference = decode_preference;
        self
    }

    pub fn decode_preference(&self) -> VideoDecodePreference {
        self.decode_preference
    }
}
