//! Shared knowledge of which file extensions FastPlay treats as playable media.
//!
//! This is the single source of truth for the supported-extension list. The
//! native open dialog builds its file-type filter from it
//! ([`media_dialog_filter_spec`]), and the play queue uses [`is_supported_media`]
//! to decide which dropped/selected files become queue entries. Keeping one list
//! means the dialog filter and the queue's idea of "media" cannot drift apart.
//!
//! Matching is case-insensitive (Windows extensions are not case-significant).
//! Subtitles are tracked separately: they belong in the open dialog (so a user
//! can browse to one) but must never become standalone queue entries.

use std::path::Path;

/// Video and audio container/codec extensions FastPlay will attempt to play.
/// Order is preserved when building the dialog filter spec.
pub const SUPPORTED_MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v", "ts", "mpg", "mpeg", "mp3", "flac",
    "wav", "ogg", "aac", "m4a", "opus",
];

/// Subtitle sidecar extensions. These appear in the open dialog filter but are
/// excluded from play-queue entries (a subtitle is not a thing to "play next").
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt"];

/// Whether `path`'s extension is a supported playable-media extension
/// (case-insensitive). Subtitles are intentionally *not* supported media.
pub fn is_supported_media(path: &Path) -> bool {
    extension_matches(path, SUPPORTED_MEDIA_EXTENSIONS)
}

/// Whether `path`'s extension is a subtitle sidecar (case-insensitive).
///
/// Not called from non-test code: the sidecar loader (`SubtitleTrack::load_sidecar`)
/// derives its own path rather than classifying one. Kept as the tested
/// counterpart to [`is_supported_media`] — together they are what documents and
/// enforces "a subtitle is browsable but never a queue entry".
#[allow(dead_code)]
pub fn is_subtitle(path: &Path) -> bool {
    extension_matches(path, SUBTITLE_EXTENSIONS)
}

fn extension_matches(path: &Path, extensions: &[&str]) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => extensions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

/// Build the semicolon-separated `*.ext` filter spec for the native open dialog,
/// e.g. `*.mp4;*.mkv;...;*.srt`. Includes subtitles so they remain browsable.
pub fn media_dialog_filter_spec() -> String {
    SUPPORTED_MEDIA_EXTENSIONS
        .iter()
        .chain(SUBTITLE_EXTENSIONS.iter())
        .map(|ext| format!("*.{ext}"))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_media_matches_case_insensitively() {
        assert!(is_supported_media(Path::new("clip.mp4")));
        assert!(is_supported_media(Path::new("clip.MP4")));
        assert!(is_supported_media(Path::new("song.Flac")));
        assert!(is_supported_media(Path::new(r"C:\Videos\Movie.MKV")));
    }

    #[test]
    fn unsupported_and_extensionless_are_rejected() {
        assert!(!is_supported_media(Path::new("notes.txt")));
        assert!(!is_supported_media(Path::new("archive.zip")));
        assert!(!is_supported_media(Path::new("no_extension")));
    }

    #[test]
    fn subtitles_are_not_supported_media() {
        assert!(!is_supported_media(Path::new("episode.srt")));
        assert!(is_subtitle(Path::new("episode.srt")));
        assert!(is_subtitle(Path::new("episode.SRT")));
        assert!(!is_subtitle(Path::new("episode.mp4")));
    }

    #[test]
    fn dialog_filter_spec_matches_legacy_literal() {
        // Must stay byte-identical to the string previously hardcoded in
        // `open_dialog.rs` so the dialog's file-type filter is unchanged.
        assert_eq!(
            media_dialog_filter_spec(),
            "*.mp4;*.mkv;*.avi;*.mov;*.webm;*.wmv;*.flv;*.m4v;*.ts;*.mpg;*.mpeg;*.mp3;\
             *.flac;*.wav;*.ogg;*.aac;*.m4a;*.opus;*.srt"
        );
    }
}
