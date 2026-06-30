//! Opt-in audio-pipeline diagnostics, gated by the `FASTPLAY_AUDIO_DIAG`
//! environment variable.
//!
//! This exists to *measure* (not guess) where audio choppiness comes from on
//! heavy files. It accumulates per-interval statistics on the UI `tick()` and
//! the audio-submission path, then flushes one summary line to the log ring
//! roughly once per second. When disabled it costs a single bool check per
//! call, so it is safe to leave wired into the hot path.
//!
//! The summary line is designed to separate the two candidate causes:
//!
//! - **Audio production starvation** — the single decode worker is blocked
//!   pushing video frames into a full queue and therefore stops decoding audio.
//!   Signature: `aq_depth` (decoded audio batches waiting) drops to 0 while
//!   `present_ms` / `tick_gap_ms` stay small.
//! - **Audio submission stall** — the UI thread is stuck in a slow 4K present,
//!   so `submit_due_audio` does not run often enough and WASAPI drains dry.
//!   Signature: `present_ms` / `tick_gap_ms` spike past the WASAPI buffer
//!   (~200 ms) and `wasapi_pad` hits 0.

use std::time::{Duration, Instant};

/// How often the rolling summary is flushed to the log.
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

pub struct AudioDiag {
    enabled: bool,

    interval_start: Option<Instant>,
    last_tick_at: Option<Instant>,
    ticks: u64,

    /// Wall-clock gap between consecutive `tick()` entries — captures UI stalls
    /// (e.g. a long present) that delay the next audio submission.
    max_tick_gap: Duration,

    /// GPU present (render) time measured on the UI thread.
    present_sum: Duration,
    present_max: Duration,
    present_count: u64,

    /// `submit_due_audio` wall time.
    submit_sum: Duration,
    submit_max: Duration,

    /// Decoded-audio queue depth (batches awaiting submission), sampled per tick.
    depth_min: u32,
    depth_max: u32,
    depth_sum: u64,
    depth_samples: u64,

    /// WASAPI buffered frames (`GetCurrentPadding`), sampled per tick.
    pad_min: u32,
    pad_max: u32,
    pad_sum: u64,
    pad_samples: u64,
    /// Ticks where the sink was started but its buffer hit 0 — an actual or
    /// imminent underrun.
    pad_zero_ticks: u64,

    /// Audio frames written to the sink this interval.
    frames_written: u64,

    /// Snapshots of cumulative session counters, for per-interval deltas.
    underruns_base: u64,
    presented_base: u64,
    dropped_base: u64,
}

impl AudioDiag {
    pub fn from_env() -> Self {
        let enabled = std::env::var_os("FASTPLAY_AUDIO_DIAG").is_some();
        if enabled {
            crate::flog!("[audio_diag] enabled (FASTPLAY_AUDIO_DIAG set)");
        }
        Self {
            enabled,
            interval_start: None,
            last_tick_at: None,
            ticks: 0,
            max_tick_gap: Duration::ZERO,
            present_sum: Duration::ZERO,
            present_max: Duration::ZERO,
            present_count: 0,
            submit_sum: Duration::ZERO,
            submit_max: Duration::ZERO,
            depth_min: u32::MAX,
            depth_max: 0,
            depth_sum: 0,
            depth_samples: 0,
            pad_min: u32::MAX,
            pad_max: 0,
            pad_sum: 0,
            pad_samples: 0,
            pad_zero_ticks: 0,
            frames_written: 0,
            underruns_base: 0,
            presented_base: 0,
            dropped_base: 0,
        }
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Record the start of a `tick()`. Tracks the inter-tick wall gap.
    pub fn note_tick_start(&mut self, now: Instant) {
        if !self.enabled {
            return;
        }
        self.ticks += 1;
        if let Some(last) = self.last_tick_at {
            let gap = now.saturating_duration_since(last);
            if gap > self.max_tick_gap {
                self.max_tick_gap = gap;
            }
        }
        self.last_tick_at = Some(now);
        if self.interval_start.is_none() {
            self.interval_start = Some(now);
        }
    }

    pub fn note_present(&mut self, dur: Duration) {
        if !self.enabled {
            return;
        }
        self.present_sum += dur;
        self.present_count += 1;
        if dur > self.present_max {
            self.present_max = dur;
        }
    }

    pub fn note_submit(&mut self, dur: Duration) {
        if !self.enabled {
            return;
        }
        self.submit_sum += dur;
        if dur > self.submit_max {
            self.submit_max = dur;
        }
    }

    pub fn add_frames_written(&mut self, frames: u32) {
        if !self.enabled {
            return;
        }
        self.frames_written += frames as u64;
    }

    /// Sample the decoded-audio queue depth and WASAPI buffered frames. Call
    /// once per tick while audio is expected and the sink is started.
    pub fn sample(&mut self, audio_queue_depth: usize, buffered: u32, sink_started: bool) {
        if !self.enabled {
            return;
        }
        let depth = audio_queue_depth as u32;
        self.depth_min = self.depth_min.min(depth);
        self.depth_max = self.depth_max.max(depth);
        self.depth_sum += depth as u64;
        self.depth_samples += 1;

        self.pad_min = self.pad_min.min(buffered);
        self.pad_max = self.pad_max.max(buffered);
        self.pad_sum += buffered as u64;
        self.pad_samples += 1;
        if sink_started && buffered == 0 {
            self.pad_zero_ticks += 1;
        }
    }

    /// Emit an immediate, high-signal line when the coordinator detects an
    /// underrun, with the queue/buffer context at that instant.
    pub fn note_underrun(&self, audio_queue_depth: usize, buffered: u32) {
        if !self.enabled {
            return;
        }
        crate::flog!(
            "[audio_diag] UNDERRUN aq_depth={} wasapi_pad={} tick_gap_max_ms={} present_max_ms={}",
            audio_queue_depth,
            buffered,
            self.max_tick_gap.as_millis(),
            self.present_max.as_millis(),
        );
    }

    /// Flush a rolling summary if the interval has elapsed, then reset the
    /// per-interval accumulators. `underruns`/`presented`/`dropped` are the
    /// session's cumulative counters; the line reports per-interval deltas.
    pub fn maybe_flush(&mut self, now: Instant, underruns: u64, presented: u64, dropped: u64) {
        if !self.enabled {
            return;
        }
        let Some(start) = self.interval_start else {
            return;
        };
        let win = now.saturating_duration_since(start);
        if win < FLUSH_INTERVAL {
            return;
        }

        let present_avg_ms = if self.present_count > 0 {
            self.present_sum.as_secs_f64() * 1000.0 / self.present_count as f64
        } else {
            0.0
        };
        let submit_avg_ms = if self.ticks > 0 {
            self.submit_sum.as_secs_f64() * 1000.0 / self.ticks as f64
        } else {
            0.0
        };
        let depth_avg = if self.depth_samples > 0 {
            self.depth_sum as f64 / self.depth_samples as f64
        } else {
            0.0
        };
        let pad_avg = if self.pad_samples > 0 {
            self.pad_sum as f64 / self.pad_samples as f64
        } else {
            0.0
        };
        let depth_min = if self.depth_min == u32::MAX {
            0
        } else {
            self.depth_min
        };
        let pad_min = if self.pad_min == u32::MAX {
            0
        } else {
            self.pad_min
        };

        crate::flog!(
            "[audio_diag] win_ms={} ticks={} tick_gap_max_ms={} present_ms(avg/max)={:.1}/{} \
             submit_ms(avg/max)={:.2}/{} aq_depth(min/avg/max)={}/{:.1}/{} \
             wasapi_pad(min/avg/max)={}/{:.0}/{} pad_zero_ticks={} frames_written={} \
             underruns=+{} presented=+{} dropped=+{}",
            win.as_millis(),
            self.ticks,
            self.max_tick_gap.as_millis(),
            present_avg_ms,
            self.present_max.as_millis(),
            submit_avg_ms,
            self.submit_max.as_millis(),
            depth_min,
            depth_avg,
            self.depth_max,
            pad_min,
            pad_avg,
            self.pad_max,
            self.pad_zero_ticks,
            self.frames_written,
            underruns.saturating_sub(self.underruns_base),
            presented.saturating_sub(self.presented_base),
            dropped.saturating_sub(self.dropped_base),
        );

        // Reset interval accumulators; carry counter bases forward.
        self.interval_start = Some(now);
        self.max_tick_gap = Duration::ZERO;
        self.present_sum = Duration::ZERO;
        self.present_max = Duration::ZERO;
        self.present_count = 0;
        self.submit_sum = Duration::ZERO;
        self.submit_max = Duration::ZERO;
        self.depth_min = u32::MAX;
        self.depth_max = 0;
        self.depth_sum = 0;
        self.depth_samples = 0;
        self.pad_min = u32::MAX;
        self.pad_max = 0;
        self.pad_sum = 0;
        self.pad_samples = 0;
        self.pad_zero_ticks = 0;
        self.frames_written = 0;
        self.underruns_base = underruns;
        self.presented_base = presented;
        self.dropped_base = dropped;
    }
}
