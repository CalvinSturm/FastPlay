use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::media::source::MediaSource;

#[derive(Clone, Debug)]
pub struct SubtitleCue {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SubtitleTrack {
    path: PathBuf,
    cues: Vec<SubtitleCue>,
}

impl SubtitleTrack {
    pub fn load_sidecar(source: &MediaSource) -> Result<Option<Self>, String> {
        let sidecar_path = source.path().with_extension("srt");
        if !sidecar_path.exists() {
            return Ok(None);
        }

        let raw_bytes = std::fs::read(&sidecar_path).map_err(|error| {
            format!(
                "failed to read subtitle sidecar {}: {error}",
                sidecar_path.display()
            )
        })?;

        let contents = decode_subtitle_bytes(&raw_bytes);
        let cues = parse_srt(&contents)?;
        Ok(Some(Self {
            path: sidecar_path,
            cues,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.cues.len()
    }

    pub fn cue_at(&self, position: Duration, hint: Option<usize>) -> Option<(usize, &SubtitleCue)> {
        // Fast path: check the hinted cue and its immediate successor first.
        // During normal monotonic playback this avoids the binary search entirely.
        if let Some(hint_idx) = hint {
            if let Some(cue) = self.cues.get(hint_idx) {
                if cue.start <= position && position < cue.end {
                    return Some((hint_idx, cue));
                }
                // Check the next cue — the most common advance in forward playback.
                if let Some(next_idx) = hint_idx.checked_add(1) {
                    if let Some(next) = self.cues.get(next_idx) {
                        if next.start > position {
                            // Position is in the gap before the next cue.
                            return None;
                        }
                        if position < next.end {
                            return Some((next_idx, next));
                        }
                    }
                }
            }
            // Hint is stale (e.g. after seek) — fall through to binary search.
        }

        // Binary search fallback.
        let idx = self.cues.partition_point(|cue| cue.start <= position);
        if idx == 0 {
            return None;
        }
        let candidate = idx - 1;
        if position < self.cues[candidate].end {
            Some((candidate, &self.cues[candidate]))
        } else {
            None
        }
    }
}

/// Decode raw subtitle bytes to a UTF-8 string.
///
/// 1. Strip a UTF-8 BOM (EF BB BF) if present.
/// 2. Attempt UTF-8 decode.
/// 3. Fall back to Windows-1252 if UTF-8 fails.
fn decode_subtitle_bytes(bytes: &[u8]) -> String {
    // Strip UTF-8 BOM if present.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);

    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_windows_1252(bytes),
    }
}

/// Decode bytes as Windows-1252 (superset of ISO-8859-1).
///
/// Bytes 0x00–0x7F map to ASCII, 0x80–0xFF map to the Windows-1252 table.
/// This is a single-byte encoding so every input byte produces exactly one
/// Unicode character.
fn decode_windows_1252(bytes: &[u8]) -> String {
    // Windows-1252 mapping for 0x80–0x9F (the only range that differs from
    // ISO-8859-1).  Index 0 corresponds to byte 0x80.
    const WIN1252_HIGH: [char; 32] = [
        '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}',
        '\u{017D}', '\u{008F}', '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
    ];

    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x00..=0x7F => out.push(b as char),
            0x80..=0x9F => out.push(WIN1252_HIGH[(b - 0x80) as usize]),
            0xA0..=0xFF => out.push(char::from(b)),
        }
    }
    out
}

fn parse_srt(contents: &str) -> Result<Vec<SubtitleCue>, String> {
    let normalized: Cow<str> = if contents.contains('\r') {
        Cow::Owned(contents.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(contents)
    };
    let mut cues = Vec::new();

    for block in normalized.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }

        let mut lines = block.lines();
        let first = lines
            .next()
            .ok_or_else(|| "subtitle block was unexpectedly empty".to_string())?;
        let timing_line = if first.contains("-->") {
            first
        } else {
            lines
                .next()
                .ok_or_else(|| format!("subtitle cue {first} was missing its timing line"))?
        };

        let (start, end) = parse_timing_line(timing_line)?;
        let mut text = String::new();
        for line in lines.map(str::trim_end).filter(|l| !l.is_empty()) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line);
        }
        if text.is_empty() {
            continue;
        }

        // Strip common SRT HTML-style tags before storing.
        let text = strip_srt_tags(&text);

        cues.push(SubtitleCue { start, end, text });
    }

    cues.sort_by_key(|cue| cue.start);
    Ok(cues)
}

/// Strip simple HTML-style formatting tags commonly found in SRT files.
///
/// Removes: `<b>`, `</b>`, `<i>`, `</i>`, `<u>`, `</u>`, `<font ...>`, `</font>`.
/// Does not attempt full HTML parsing — just matches known tag patterns.
fn strip_srt_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            // Try to match a known tag.
            if let Some(end) = find_closing_angle(&input[i..]) {
                let tag_content = &input[i + 1..i + end]; // between < and >
                if is_known_srt_tag(tag_content) {
                    // Skip past the '>' character.
                    while let Some(&(j, _)) = chars.peek() {
                        if j > i + end {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                }
            }
            output.push(ch);
        } else {
            output.push(ch);
        }
    }

    output
}

/// Find the position of the first `>` relative to the start of `s`.
fn find_closing_angle(s: &str) -> Option<usize> {
    // Limit search to a reasonable tag length to avoid scanning entire cue text.
    let limit = s.len().min(64);
    s[..limit].find('>')
}

/// Check if the content between `<` and `>` is a known SRT formatting tag.
fn is_known_srt_tag(content: &str) -> bool {
    let content = content.trim();

    // Closing tags: /b, /i, /u, /font
    if let Some(rest) = content.strip_prefix('/') {
        let rest = rest.trim();
        matches!(rest.to_ascii_lowercase().as_str(), "b" | "i" | "u" | "font")
    } else {
        // Opening tags: b, i, u, or font with optional attributes
        let tag_name = content
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(tag_name.as_str(), "b" | "i" | "u" | "font")
    }
}

fn parse_timing_line(line: &str) -> Result<(Duration, Duration), String> {
    let (start, end) = line
        .split_once("-->")
        .ok_or_else(|| format!("invalid subtitle timing line: {line}"))?;
    let start = parse_timestamp(start.trim())?;
    let end = parse_timestamp(end.trim())?;
    Ok((start, end.max(start)))
}

fn parse_timestamp(value: &str) -> Result<Duration, String> {
    // Accept both comma (standard SRT) and period (common variant) as the
    // millisecond separator: 00:00:01,500 or 00:00:01.500.
    let (clock, millis) = value
        .split_once(',')
        .or_else(|| value.split_once('.'))
        .ok_or_else(|| format!("subtitle timestamp was missing milliseconds: {value}"))?;
    let mut parts = clock.split(':');
    let hours = parts
        .next()
        .ok_or_else(|| format!("subtitle timestamp was missing hours: {value}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid subtitle hour value in {value}: {error}"))?;
    let minutes = parts
        .next()
        .ok_or_else(|| format!("subtitle timestamp was missing minutes: {value}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid subtitle minute value in {value}: {error}"))?;
    let seconds = parts
        .next()
        .ok_or_else(|| format!("subtitle timestamp was missing seconds: {value}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid subtitle second value in {value}: {error}"))?;
    let millis = millis
        .parse::<u64>()
        .map_err(|error| format!("invalid subtitle millisecond value in {value}: {error}"))?;
    if parts.next().is_some() {
        return Err(format!(
            "subtitle timestamp had too many components: {value}"
        ));
    }

    Ok(Duration::from_millis(
        hours
            .saturating_mul(3_600_000)
            .saturating_add(minutes.saturating_mul(60_000))
            .saturating_add(seconds.saturating_mul(1_000))
            .saturating_add(millis.min(999)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_comma_separator() {
        let d = parse_timestamp("00:01:02,345").unwrap();
        assert_eq!(d, Duration::from_millis(62_345));
    }

    #[test]
    fn parse_period_separator() {
        let d = parse_timestamp("00:01:02.345").unwrap();
        assert_eq!(d, Duration::from_millis(62_345));
    }

    #[test]
    fn strip_bold_italic_tags() {
        assert_eq!(strip_srt_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_srt_tags("<i>world</i>"), "world");
        assert_eq!(strip_srt_tags("<u>underline</u>"), "underline");
    }

    #[test]
    fn strip_font_tags() {
        assert_eq!(
            strip_srt_tags("<font color=\"#ffff00\">yellow</font>"),
            "yellow"
        );
    }

    #[test]
    fn preserve_unknown_tags() {
        assert_eq!(strip_srt_tags("<div>keep</div>"), "<div>keep</div>");
    }

    #[test]
    fn utf8_bom_stripped() {
        let with_bom = b"\xEF\xBB\xBF1\n00:00:01,000 --> 00:00:02,000\nHello";
        let s = decode_subtitle_bytes(with_bom);
        assert!(s.starts_with('1'));
        assert!(!s.starts_with('\u{FEFF}'));
    }

    #[test]
    fn windows_1252_fallback() {
        // 0xE9 = é in Windows-1252.
        let bytes = b"1\n00:00:01,000 --> 00:00:02,000\ncaf\xe9";
        let s = decode_subtitle_bytes(bytes);
        assert!(s.contains("café"));
    }
}
