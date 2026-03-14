use std::{
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

        let contents = std::fs::read_to_string(&sidecar_path)
            .map_err(|error| format!("failed to read subtitle sidecar {}: {error}", sidecar_path.display()))?;
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

    pub fn cue_at(&self, position: Duration) -> Option<(usize, &SubtitleCue)> {
        self.cues
            .iter()
            .enumerate()
            .find(|(_, cue)| cue.start <= position && position < cue.end)
    }
}

fn parse_srt(contents: &str) -> Result<Vec<SubtitleCue>, String> {
    let normalized = contents.replace("\r\n", "\n");
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
        let text = lines
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            continue;
        }

        cues.push(SubtitleCue { start, end, text });
    }

    cues.sort_by_key(|cue| cue.start);
    Ok(cues)
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
    let (clock, millis) = value
        .split_once(',')
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
        return Err(format!("subtitle timestamp had too many components: {value}"));
    }

    Ok(Duration::from_millis(
        hours.saturating_mul(3_600_000)
            .saturating_add(minutes.saturating_mul(60_000))
            .saturating_add(seconds.saturating_mul(1_000))
            .saturating_add(millis.min(999)),
    ))
}
