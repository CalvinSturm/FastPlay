use std::{
    cell::Cell,
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, ThreadId},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app::{
        audio_controller::AudioController,
        clip_range::ClipRangeState,
        commands::SessionCommand,
        decode_thread::DecodeThreadHandle,
        drop_stats::{VideoDropBuckets, VideoDropCause},
        events::SessionEvent,
        overlay::OverlayManager,
        state::PlaybackState,
        video_queue::VideoFrameQueue,
        viewport::ViewportState,
    },
    audio::sink::AudioSink,
    ffi::{
        d3d11::BgraFrameCapture,
        dxgi::{ModalTickTarget, NativeWindowInner, ResizeRequest},
        ffmpeg::{self, AudioDecodeSession, DecodeSession, StreamStatus},
    },
    media::{
        audio::{AudioStreamFormat, DecodedAudioFrame},
        seek::{PlaybackSnapshot, PositionKind, SeekTarget},
        source::MediaSource,
        subtitle::SubtitleTrack,
        video::{DecodedVideoFrame, VideoDecodeMode, VideoDecodePreference},
    },
    platform::window::NativeWindow,
    playback::{
        audio_diag::AudioDiag,
        clock::PlaybackClock,
        decode_control::{DecodeCommand, DecodeControl},
        generations::{
            GenerationState, OpenGeneration, OperationClock, OperationId, SeekGeneration,
        },
        metrics::MetricsCollector,
        queues::QueueDefaults,
    },
    render::presenter::Presenter,
};

const VERY_LATE_VIDEO_THRESHOLD: Duration = Duration::from_millis(400);
const MAX_OBSERVED_FRAME_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_FRAME_STEP: Duration = Duration::from_nanos(33_333_333);
const WORKER_CANCELLED: &str = "fastplay operation cancelled";
const VOLUME_OVERLAY_TIMEOUT: Duration = Duration::from_millis(900);

/// Decrements a live decode-worker counter on every exit path (normal, error,
/// or panic), so a blocking teardown can wait for the worker to fully release
/// its resources. Used by the audio worker; the video worker has an equivalent
/// inline guard.
struct WorkerExitGuard(Arc<AtomicU32>);

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let remaining = self.0.fetch_sub(1, Ordering::Release).saturating_sub(1);
        crate::flog!("[audio_thread_exit] workers_remaining={}", remaining);
    }
}

struct QueuedAudioFrame {
    frame: DecodedAudioFrame,
    submitted_frames: u32,
}

pub struct PlaybackSession {
    ui_thread_id: ThreadId,
    tick_active: Cell<bool>,
    // `presenter` is declared before `window` so it drops first: the swap chain
    // and D3D11 device are released while the window HWND is still alive
    // (correct DXGI teardown order), then `window` drops and destroys the HWND.
    presenter: Presenter,
    window: NativeWindow,
    audio_sink: Option<AudioSink>,
    audio_sink_error: Option<String>,
    state: PlaybackState,
    generations: GenerationState,
    operation_clock: OperationClock,
    active_operation_id: Option<OperationId>,
    current_source: Option<Arc<MediaSource>>,
    decode_preference: VideoDecodePreference,
    /// Persistent decode worker lifecycle (control channel, preference,
    /// live-worker count, join handle). See [`DecodeThreadHandle`].
    decode_thread: DecodeThreadHandle,
    /// Handle to the independent audio-only decode worker. Audio is decoded on
    /// its own thread with its own demuxer so it is never gated behind
    /// sub-realtime video work (e.g. software-decoded 4K60), which would
    /// otherwise starve the audio sink and cause choppy audio. Respawned per
    /// open/seek rather than seeked in place (audio open is cheap and uses no
    /// GPU device).
    audio_decode_thread: DecodeThreadHandle,
    /// Worker→UI channel for video frames and all control/error events.
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    /// Separate worker→UI channel for audio frames and audio-stream-end.
    /// Decoupled from `event_rx` so a full video queue can't stall the drain
    /// loop from delivering audio (which would underrun WASAPI and stutter).
    audio_event_tx: SyncSender<SessionEvent>,
    audio_event_rx: Receiver<SessionEvent>,
    metrics: MetricsCollector,
    video_clock: Option<PlaybackClock>,
    media_time_origin_pts: Option<Duration>,
    /// Normalized position the current open began decoding at (resume offset).
    /// Subtracted when the media-time origin is first established so that a
    /// resume open anchors the origin at the file start, not at the resume PTS
    /// (otherwise the whole normalized timeline shifts by the resume amount).
    open_start_position: Duration,
    paused_clock_position: Option<Duration>,
    audio: AudioController,
    media_duration: Option<Duration>,
    pending_seek_target: Option<SeekTarget>,
    seek_discard_before_pts: Option<Duration>,
    seek_frame_presented_since_request: bool,
    audio_stream_expected: bool,
    overlay: OverlayManager,
    video_queue: VideoFrameQueue,
    queued_audio_frames: VecDeque<QueuedAudioFrame>,
    queued_audio_capacity: usize,
    drop_buckets: VideoDropBuckets,
    video_stream_ended: bool,
    audio_stream_ended: bool,
    active_decode_mode: Option<VideoDecodeMode>,
    last_error: Option<String>,
    present_needed: bool,
    screenshot_pending: bool,
    observed_frame_interval: Option<Duration>,
    last_presented_media_position: Option<Duration>,
    viewport: ViewportState,
    needs_initial_resize: bool,
    has_shown_content: bool,
    pause_after_seek: bool,
    playback_rate: f64,
    clip_range: ClipRangeState,
    /// One-shot "playback just reached its natural end" signal, consumed by the
    /// event loop to drive play-queue auto-advance. See [`EndedSignal`].
    ended_signal: EndedSignal,
    idle_pace_requested: bool,
    /// Audio sink buffered-frame count cached for the duration of one `tick`.
    /// `GetCurrentPadding` is a driver/COM call and the master clock is
    /// recomputed several times per tick; the value is stable across those
    /// reads because no audio is submitted between them.
    cached_buffered_frames: Cell<Option<u32>>,
    /// Opt-in audio-pipeline diagnostics (env `FASTPLAY_AUDIO_DIAG`). No-op when
    /// disabled; never on the always-on path.
    audio_diag: AudioDiag,
}

/// A one-shot, edge-triggered "playback just ended" latch.
///
/// [`latch`](EndedSignal::latch) marks it on the single transition into the
/// ended state; [`take`](EndedSignal::take) consumes it once and returns `false`
/// thereafter until the next `latch`. This is what keeps play-queue auto-advance
/// from firing every tick while the session sits in the ended state.
#[derive(Default)]
struct EndedSignal {
    pending: bool,
}

impl EndedSignal {
    fn latch(&mut self) {
        self.pending = true;
    }

    fn take(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let saved_volume = super::settings::load_volume();
        let queue_defaults = QueueDefaults::default();
        // Independent bounded channels for video/control vs audio events. A
        // single shared channel meant the drain loop stopped pulling *both*
        // streams as soon as *either* queue filled — so a full video queue
        // starved audio delivery, draining WASAPI to an underrun that reset
        // the A/V clock and stalled video (the "stutter"). Separate channels
        // let each stream be drained on its own gate. The `+ 4` is headroom
        // for the infrequent control/end/error events sharing the video path.
        let video_event_capacity = queue_defaults.decoded_video_frames + 4;
        let audio_event_capacity = queue_defaults.decoded_audio_frames + 4;
        let (event_tx, event_rx) = mpsc::sync_channel(video_event_capacity);
        let (audio_event_tx, audio_event_rx) = mpsc::sync_channel(audio_event_capacity);

        Ok(Self {
            ui_thread_id: thread::current().id(),
            tick_active: Cell::new(false),
            window,
            presenter,
            audio_sink: None,
            audio_sink_error: None,
            state: PlaybackState::Idle,
            generations: GenerationState::default(),
            operation_clock: OperationClock::default(),
            active_operation_id: None,
            current_source: None,
            decode_preference: VideoDecodePreference::Auto,
            decode_thread: DecodeThreadHandle::new(),
            audio_decode_thread: DecodeThreadHandle::new(),
            event_tx,
            event_rx,
            audio_event_tx,
            audio_event_rx,
            metrics: MetricsCollector::new(),
            video_clock: None,
            media_time_origin_pts: None,
            open_start_position: Duration::ZERO,
            paused_clock_position: None,
            audio: AudioController::new(saved_volume),
            media_duration: None,
            pending_seek_target: None,
            seek_discard_before_pts: None,
            seek_frame_presented_since_request: false,
            audio_stream_expected: false,
            overlay: OverlayManager::new(),
            video_queue: VideoFrameQueue::with_capacity(queue_defaults.decoded_video_frames),
            queued_audio_frames: VecDeque::with_capacity(queue_defaults.decoded_audio_frames),
            queued_audio_capacity: queue_defaults.decoded_audio_frames,
            drop_buckets: VideoDropBuckets::default(),
            video_stream_ended: false,
            audio_stream_ended: false,
            active_decode_mode: None,
            last_error: None,
            present_needed: true,
            screenshot_pending: false,
            observed_frame_interval: None,
            last_presented_media_position: None,
            viewport: ViewportState::new(),
            needs_initial_resize: false,
            has_shown_content: false,
            pause_after_seek: false,
            playback_rate: 1.0,
            clip_range: ClipRangeState::new(),
            ended_signal: EndedSignal::default(),
            idle_pace_requested: false,
            cached_buffered_frames: Cell::new(None),
            audio_diag: AudioDiag::from_env(),
        })
    }

    pub fn window(&self) -> &NativeWindow {
        &self.window
    }

    pub fn snapshot(&self, now: Instant) -> PlaybackSnapshot {
        if let Some(target) = self.pending_seek_target {
            return PlaybackSnapshot {
                position: target.position(),
                kind: PositionKind::PendingSeekTarget,
            };
        }

        PlaybackSnapshot {
            position: self.master_clock_position(now).unwrap_or(Duration::ZERO),
            kind: PositionKind::SettledPlaybackClock,
        }
    }

    pub fn is_paused(&self) -> bool {
        self.state == PlaybackState::Paused
    }

    pub fn media_duration(&self) -> Option<Duration> {
        self.media_duration
    }

    pub fn current_media_path(&self) -> Option<&Path> {
        self.current_source.as_ref().map(|source| source.path())
    }

    pub fn auto_replay(&self) -> bool {
        self.clip_range.auto_replay()
    }

    pub fn in_point(&self) -> Option<Duration> {
        self.clip_range.in_point()
    }

    pub fn out_point(&self) -> Option<Duration> {
        self.clip_range.out_point()
    }

    pub fn loop_range(&self) -> bool {
        self.clip_range.loop_range()
    }

    pub fn position_is_in_active_range(&self, position: Duration) -> bool {
        self.clip_range.position_is_in_active_range(position)
    }

    pub fn replay_indicator_until(&self) -> Option<Instant> {
        self.overlay.replay_indicator_until()
    }

    pub fn set_timeline_overlay(
        &mut self,
        model: Option<crate::render::timeline::TimelineOverlayModel>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.presenter.set_timeline_overlay(model)? {
            self.present_needed = true;
        }
        Ok(())
    }

    pub fn refresh_volume_overlay(
        &mut self,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.overlay.refresh_volume(now, &mut self.presenter)? {
            self.present_needed = true;
        }
        Ok(())
    }

    /// Show the Recent-files overlay with the given (filename, position) rows
    /// and highlighted selection. Owned by the event loop, which holds the
    /// recent-files state; the session just renders it.
    pub fn show_recent_overlay(
        &mut self,
        rows: &[(String, String)],
        selected: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (vw, vh) = self.presenter.viewport_size().unwrap_or((1280, 720));
        self.presenter.show_recent_overlay(rows, selected, vw, vh)?;
        self.present_needed = true;
        Ok(())
    }

    pub fn clear_recent_overlay(&mut self) {
        self.presenter.clear_recent_overlay();
        self.present_needed = true;
    }

    pub fn show_marker_overlay(
        &mut self,
        rows: &[(String, String)],
        selected: usize,
        footer: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (vw, vh) = self.presenter.viewport_size().unwrap_or((1280, 720));
        self.presenter
            .show_marker_overlay(rows, selected, footer, vw, vh)?;
        self.present_needed = true;
        Ok(())
    }

    pub fn clear_marker_overlay(&mut self) {
        self.presenter.clear_recent_overlay();
        self.present_needed = true;
    }

    pub fn show_review_queue_overlay(
        &mut self,
        rows: &[(String, String)],
        selected: usize,
        footer: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (vw, vh) = self.presenter.viewport_size().unwrap_or((1280, 720));
        self.presenter
            .show_review_queue_overlay(rows, selected, footer, vw, vh)?;
        self.present_needed = true;
        Ok(())
    }

    pub fn clear_review_queue_overlay(&mut self) {
        self.presenter.clear_recent_overlay();
        self.present_needed = true;
    }

    /// Show a brief status message using the transient overlay — the same slot
    /// and auto-timeout as the volume indicator. Used by the event loop for
    /// play-queue navigation feedback. This is a render-only convenience (like
    /// [`Self::show_recent_overlay`]); it adds no queue state to the coordinator.
    pub fn show_status_message(&mut self, text: &str) {
        if let Ok((viewport_width, viewport_height)) = self.presenter.viewport_size() {
            if self
                .presenter
                .set_volume_overlay(Some(text), viewport_width, viewport_height)
                .unwrap_or(false)
            {
                self.present_needed = true;
            }
        }
        self.overlay.volume_overlay_until = Some(Instant::now() + VOLUME_OVERLAY_TIMEOUT);
    }

    /// Consume the edge-triggered "playback reached its natural end" signal.
    /// Returns `true` exactly once per real transition into the ended state
    /// (natural end-of-file, with no loop/auto-replay active), and `false` on
    /// every later tick until another such transition. The event loop polls this
    /// after `tick` to drive play-queue auto-advance without firing every frame
    /// while the session sits in the ended state.
    pub fn take_ended_signal(&mut self) -> bool {
        self.ended_signal.take()
    }

    /// Open `source`, beginning playback at `start_position` when given (used to
    /// resume a previously-watched file). `None` starts from the beginning.
    pub fn open_at(
        &mut self,
        source: MediaSource,
        start_position: Option<Duration>,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let open_gen = self.generations.bump_open();
        let seek_gen = self.generations.seek();
        let op_id = self.operation_clock.next();
        // A freshly opened file is not ended: discard any unconsumed signal so a
        // stale latch can never auto-advance the new file.
        self.ended_signal.take();
        self.decode_preference = source.decode_preference();
        self.overlay.subtitle_track = match SubtitleTrack::load_sidecar(&source) {
            Ok(track) => track,
            Err(error) => {
                flog!("subtitle load failed: {error}");
                None
            }
        };
        self.overlay.subtitles_enabled = self.overlay.subtitle_track.is_some();
        self.overlay.subtitle_clock_base = None;
        self.overlay.active_subtitle_cue = None;
        self.overlay.active_subtitle_viewport = None;
        self.playback_rate = 1.0;
        // Resume opens begin decoding at `start_position`; record it so the
        // media-time origin is anchored at the file start rather than the
        // resume PTS (see observe_media_time_origin).
        self.open_start_position = start_position.unwrap_or(Duration::ZERO);
        self.clip_range.reset_for_open();
        let source = Arc::new(source);
        self.current_source = Some(Arc::clone(&source));
        self.media_duration = None;
        self.active_decode_mode = None;
        self.observed_frame_interval = None;
        self.last_presented_media_position = None;
        self.viewport.reset_for_open();
        if let Some(track) = self.overlay.subtitle_track.as_ref() {
            flog!(
                "subtitle_track_loaded path={} cues={}",
                track.path().display(),
                track.len()
            );
        }
        self.needs_initial_resize = true;
        self.metrics.note_open_requested(now);
        self.metrics.enable_open_audio_metric();
        self.begin_operation(
            source,
            start_position,
            open_gen,
            seek_gen,
            op_id,
            PlaybackState::Opening,
            true,
            true,
        )?;
        Ok(())
    }

    pub fn scrub_seek(
        &mut self,
        target: SeekTarget,
        pause_after: bool,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Scrubbing stays on the hardware path: the persistent decoder seeks in
        // place (av_seek_frame + flush) with no per-seek decoder spawn/teardown,
        // so the d3d11 scrub-crash that once forced a software fallback can no
        // longer occur, and latest-command-wins coalescing keeps the preview
        // responsive under rapid churn.
        self.pause_after_seek = pause_after;
        self.seek(target, now)
    }

    pub fn apply_command(
        &mut self,
        command: SessionCommand,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match command {
            SessionCommand::Tick => {}
            SessionCommand::TogglePause => self.toggle_pause(now)?,
            SessionCommand::ToggleSubtitles => self.toggle_subtitles()?,
            SessionCommand::SaveScreenshot => {
                self.screenshot_pending = true;
                self.present_needed = true;
            }
            SessionCommand::Seek(target) => {
                self.pause_after_seek = false;
                self.seek(target, now)?;
            }
            SessionCommand::StepFrameForward => self.step_frame(1, now)?,
            SessionCommand::StepFrameBackward => self.step_frame(-1, now)?,
            SessionCommand::AdjustVolumeSteps(steps) => self.adjust_volume_steps(steps),
            SessionCommand::RotateClockwise => self.rotate_view(1),
            SessionCommand::RotateCounterClockwise => self.rotate_view(3),
            SessionCommand::ToggleBorderlessFullscreen => {
                self.metrics.note_fullscreen_toggle_started(now);
                self.window.toggle_borderless_fullscreen();
                if let Some(elapsed) = self
                    .metrics
                    .note_fullscreen_toggle_completed(Instant::now())
                {
                    flog!("fullscreen_toggle_ms={}", elapsed.as_millis());
                }
            }
            SessionCommand::ZoomAtCursor {
                delta,
                cursor_x,
                cursor_y,
            } => {
                self.zoom_at_cursor(delta, cursor_x, cursor_y);
            }
            SessionCommand::ResetView => {
                self.reset_view();
            }
            SessionCommand::SetInPoint => {
                self.clip_range.set_in_point(self.snapshot(now).position);
            }
            SessionCommand::ClearInPoint => {
                self.clip_range.clear_in_point();
            }
            SessionCommand::SetOutPoint => {
                self.clip_range.set_out_point(self.snapshot(now).position);
            }
            SessionCommand::ClearOutPoint => {
                self.clip_range.clear_out_point();
            }
            SessionCommand::ToggleLoopRange => {
                if self.clip_range.toggle_loop_or_replay() {
                    self.overlay.replay_indicator_until = Some(now + Duration::from_millis(1500));
                }
            }
            SessionCommand::FitWindow => {
                self.fit_window();
            }
            SessionCommand::HalfSizeWindow => {
                self.half_size_window();
            }
            SessionCommand::ToggleDecodeInfo => {
                self.overlay.show_decode_info = !self.overlay.show_decode_info;
                self.update_window_title();
            }
            SessionCommand::StepPlaybackRate(step) => {
                self.step_playback_rate(step);
            }
            SessionCommand::ResetPlaybackRate => {
                if (self.playback_rate - 1.0).abs() >= 0.01 {
                    self.playback_rate = 1.0;
                    self.video_clock = None;
                    self.audio.reset_clock();
                    self.queued_audio_frames.clear();
                    self.update_window_title();
                }
            }
            SessionCommand::PanBy { dx, dy } => {
                let viewport = self.presenter.viewport_size().unwrap_or((1, 1));
                if self.viewport.pan_by(dx, dy, viewport) {
                    self.present_needed = true;
                }
            }
            SessionCommand::ShowHelp => {
                if let Ok((vw, vh)) = self.presenter.viewport_size() {
                    self.presenter.show_help_overlay(vw, vh)?;
                    self.present_needed = true;
                }
            }
            SessionCommand::HideHelp => {
                self.presenter.clear_help_overlay();
                self.present_needed = true;
            }
        }
        Ok(())
    }

    fn adjust_volume_steps(&mut self, steps: i16) {
        let Some(sink) = self.audio_sink.as_mut() else {
            return;
        };
        let volume_percent = self.audio.adjust_volume(steps, sink);
        if let Ok((viewport_width, viewport_height)) = self.presenter.viewport_size() {
            if self
                .presenter
                .set_volume_overlay(
                    Some(&format!("{volume_percent}%")),
                    viewport_width,
                    viewport_height,
                )
                .unwrap_or(false)
            {
                self.present_needed = true;
            }
        }
        self.overlay.volume_overlay_until = Some(Instant::now() + VOLUME_OVERLAY_TIMEOUT);
        flog!("volume={volume_percent}");
    }

    fn step_frame(
        &mut self,
        direction: i8,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.current_source.is_none() {
            return Ok(());
        }
        if !matches!(self.state, PlaybackState::Paused | PlaybackState::Ended) {
            self.toggle_pause(now)?;
        }

        let current = self
            .pending_seek_target
            .map(SeekTarget::position)
            .or(self.last_presented_media_position)
            .unwrap_or_else(|| self.snapshot(now).position);
        let step = self.frame_step_duration();
        let target = if direction >= 0 {
            current.saturating_add(step)
        } else {
            current.saturating_sub(step)
        };
        let target = match self.media_duration {
            Some(duration) if target > duration => duration,
            _ => target,
        };

        self.pause_after_seek = true;
        self.seek(SeekTarget::new(target), now)
    }

    fn frame_step_duration(&self) -> Duration {
        if let Some(interval) = self.observed_frame_interval {
            return interval;
        }

        let mut previous = None;
        for frame in self.video_queue.iter() {
            let position = self.media_time_for_pts(frame.pts());
            if let Some(previous) = previous {
                let delta = position.saturating_sub(previous);
                if !delta.is_zero() && delta <= MAX_OBSERVED_FRAME_INTERVAL {
                    return delta;
                }
            }
            previous = Some(position);
        }

        DEFAULT_FRAME_STEP
    }

    fn save_screenshot(
        &self,
        capture: BgraFrameCapture,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let directory = screenshot_directory()?;
        fs::create_dir_all(&directory)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = directory.join(format!("fastplay-screenshot-{stamp}.bmp"));
        fs::write(&path, encode_bgra_bmp(&capture)?)?;
        Ok(path)
    }

    pub fn tick(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            self.ui_thread_id,
            thread::current().id(),
            "tick(now) must run on the UI thread",
        );
        assert!(
            !self.tick_active.replace(true),
            "tick(now) must be non-reentrant"
        );

        let result = self.tick_inner(now);
        self.tick_active.set(false);
        result
    }

    pub fn take_idle_pace_request(&mut self) -> bool {
        std::mem::take(&mut self.idle_pace_requested)
    }

    /// Whether the player is quiescent — idle, paused, ended, or errored — so
    /// the UI loop may block waiting for input instead of spin-pacing. In every
    /// other state playback is actively advancing (frames are scheduled against
    /// a clock, or an open/seek is in flight) and the loop must keep ticking to
    /// hit frame deadlines. Worker events that can still arrive in these states
    /// (device loss, audio endpoint change) are picked up on the next wakeup,
    /// which the bounded message-wait timeout guarantees happens promptly.
    pub fn is_idle_for_input(&self) -> bool {
        matches!(
            self.state,
            PlaybackState::Idle
                | PlaybackState::Paused
                | PlaybackState::Ended
                | PlaybackState::Error
        )
    }

    /// Stop the background workers before the process exits, *without* releasing
    /// any D3D11/GPU resources.
    ///
    /// Joining the decode worker first guarantees no thread is executing inside
    /// the graphics/codec drivers when the process subsequently calls
    /// `ExitProcess` — otherwise a terminated worker holding a driver/loader
    /// lock could deadlock the DLL-detach path.
    ///
    /// We deliberately do **not** tear down the swap chain, device, or
    /// HW-decode surfaces here. Releasing them in-process at exit intermittently
    /// faults inside the graphics driver (an access violation through a dangling
    /// vtable), which hands an unhandled exception to Windows Error Reporting;
    /// WER then freezes the still-visible window for several seconds while it
    /// writes a multi-megabyte crash dump before the process dies — the "lag on
    /// close". The caller [`crate::main`] flushes logs and then exits the
    /// process, letting the OS reclaim every GPU resource instantly and
    /// crash-free.
    pub fn shutdown(&mut self) {
        self.decode_thread.teardown(true);
        self.audio_decode_thread.teardown(true);
        self.clear_video_queue();
        self.queued_audio_frames.clear();
        if let Some(sink) = self.audio_sink.as_mut() {
            let _ = sink.pause();
        }
        self.audio_sink = None;
    }

    fn tick_inner(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        self.idle_pace_requested = false;
        self.audio_diag.note_tick_start(now);
        // Invalidate the per-tick audio-buffer cache; it is repopulated by the
        // first WASAPI query within this tick and reused thereafter.
        self.cached_buffered_frames.set(None);

        if self.state == PlaybackState::Error {
            self.show_error_idle_overlay()?;
            // Still handle resize so the error overlay re-renders at new size.
            if let Some(size) = self.window.take_resize_request() {
                self.handle_resize(size, now)?;
            }
        }

        if self.state != PlaybackState::Error {
            // At non-1.0x rates, audio is not submitted so the queue fills up
            // and would block the drain loop from processing video and control
            // events. Discard queued audio proactively so backpressure only
            // depends on the video queue.
            if self.playback_rate != 1.0 {
                self.queued_audio_frames.clear();
            }

            // Drain audio and video independently. Each loop gates only on its
            // own queue, so a full video queue no longer stops audio from being
            // pulled and submitted to WASAPI (and vice versa) — the coupling
            // that periodically starved audio and stuttered playback.
            loop {
                if self.queued_audio_frames.len() >= self.queued_audio_capacity {
                    break;
                }
                match self.audio_event_rx.try_recv() {
                    Ok(event) => self.handle_event(event, now)?,
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
            // Gate the video drain on the decoded-video queue, exactly like the
            // audio loop above. Draining unconditionally and letting
            // `push_video_frame` drop the overflow removes all backpressure on
            // the decoder: with nothing to block its sends it free-runs to EOF,
            // overflow-dropping the whole stream while the audio-clock-gated
            // presenter can only show frames at or below the 1x audio clock —
            // every one of which has already been dropped, so the picture
            // freezes on the first few frames. Stopping here when the queue is
            // full lets the bounded video channel backpressure the worker, so
            // the decoder is paced to playback. Audio lives on its own channel
            // and queue (drained above) and keeps flowing regardless, so a full
            // video queue cannot starve it.
            loop {
                if self.video_queue.is_full() {
                    break;
                }
                match self.event_rx.try_recv() {
                    Ok(event) => self.handle_event(event, now)?,
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }

            if let Some(size) = self.window.take_resize_request() {
                self.handle_resize(size, now)?;
            }

            if self.audio_diag.enabled() {
                let t = Instant::now();
                self.submit_due_audio(now)?;
                self.audio_diag.note_submit(t.elapsed());
                // Sample the decoded-audio backlog and WASAPI buffer right after
                // submission (submit_due_audio populated the buffered cache).
                let buffered = self.cached_buffered_frames.get().unwrap_or(0);
                let started = self.audio_sink.as_ref().is_some_and(|s| s.is_started());
                self.audio_diag
                    .sample(self.queued_audio_frames.len(), buffered, started);
            } else {
                self.submit_due_audio(now)?;
            }
            if self.advance_video_playback(now)? {
                self.audio_diag.maybe_flush(
                    now,
                    self.metrics.audio_underruns(),
                    self.metrics.presented_video_frames(),
                    self.metrics.dropped_video_frames(),
                );
                return Ok(());
            }
            self.update_subtitle_overlay(now)?;
            self.refresh_volume_overlay(now)?;
        }

        if self.present_needed || self.screenshot_pending {
            let view = self.viewport.transform();
            let render_result = if self.screenshot_pending {
                self.screenshot_pending = false;
                self.presenter
                    .render_with_capture(&view)
                    .map(|(result, capture)| {
                        if !matches!(result, crate::ffi::dxgi::PresentResult::DeviceLost) {
                            match self.save_screenshot(capture) {
                                Ok(path) => flog!("screenshot_saved path={}", path.display()),
                                Err(error) => flog!("screenshot_failed error={error}"),
                            }
                        }
                        result
                    })
            } else {
                let present_timer = self.audio_diag.enabled().then(Instant::now);
                let result = self.presenter.render(&view);
                if let Some(t) = present_timer {
                    self.audio_diag.note_present(t.elapsed());
                }
                result
            };
            match render_result {
                Ok(crate::ffi::dxgi::PresentResult::Ok) => {
                    self.present_needed = false;
                    self.metrics.note_present(now);
                }
                Ok(crate::ffi::dxgi::PresentResult::Occluded) => {
                    // Window is fully covered or minimized — the frame
                    // wasn't shown. Mark as presented to avoid re-rendering
                    // the same content, but skip the metrics note.
                    self.present_needed = false;
                }
                Ok(crate::ffi::dxgi::PresentResult::DeviceLost) => {
                    self.recover_device(now, "Present returned device-lost".to_string())?;
                    return Ok(());
                }
                Err(error) => {
                    self.recover_device(now, format!("present failed: {error}"))?;
                    return Ok(());
                }
            }
        } else {
            self.idle_pace_requested = true;
        }

        if self.metrics.pending_first_frame && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_first_frame_presented(now) {
                flog!("open_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_first_frame = false;
        }

        if self.metrics.pending_first_audio {
            if let Some(elapsed) = self.metrics.note_first_audio_started(now) {
                flog!("open_to_first_audio_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_first_audio = false;
            self.metrics.disable_open_audio_metric();
        }

        if self.metrics.pending_seek_first_frame && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_seek_first_frame_presented(now) {
                flog!("seek_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_seek_first_frame = false;
        }

        if self.metrics.pending_seek_settled && self.seek_is_settled() {
            if let Some(elapsed) = self.metrics.note_seek_av_settled(now) {
                flog!("seek_to_av_settled_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_seek_settled = false;
            self.pending_seek_target = None;
        }

        if self.can_finish_playback()? {
            self.metrics.note_ended(now);
            flog!(
                "playback_summary decode_mode={} hw_fallback_count={} presented_frames={} dropped_video_frames={} audio_underruns={} drop_queue_overflow={} drop_surface_mismatch={} drop_scheduler_late={}",
                self.metrics
                    .decode_mode()
                    .map(VideoDecodeMode::label)
                    .unwrap_or("unknown"),
                self.metrics.hw_fallback_count(),
                self.metrics.presented_video_frames(),
                self.metrics.dropped_video_frames(),
                self.metrics.audio_underruns(),
                self.drop_buckets.queue_overflow,
                self.drop_buckets.surface_mismatch,
                self.drop_buckets.scheduler_late
            );
            if self.clip_range.should_replay_at_end() {
                self.replay(now)?;
            } else {
                self.state = PlaybackState::Ended;
                // Natural end of the file with no loop/auto-replay active: latch
                // the edge-triggered signal so the event loop can auto-advance to
                // the next queued item. Deliberately NOT latched on the out-point
                // clip-stop path (an intentional range end, not a natural EOF).
                self.ended_signal.latch();
            }
        }

        self.audio_diag.maybe_flush(
            now,
            self.metrics.audio_underruns(),
            self.metrics.presented_video_frames(),
            self.metrics.dropped_video_frames(),
        );
        Ok(())
    }

    fn handle_event(
        &mut self,
        event: SessionEvent,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match event {
            SessionEvent::DecodeModeSelected {
                open_gen,
                seek_gen,
                op_id,
                mode,
                hw_fallback_count,
                rotation_quarter_turns,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                self.active_decode_mode = Some(mode);
                self.metrics
                    .note_decode_mode_selected(mode, hw_fallback_count);
                if mode == VideoDecodeMode::Software {
                    self.decode_preference = VideoDecodePreference::ForceSoftware;
                }
                // Apply stream rotation to the view only when the stream
                // rotation actually changes (initial open, or a new media
                // file). Mid-stream re-inits — HW→SW fallback, scrub seeks —
                // fire this event again with the same stream rotation; in
                // that case the user may have rotated the view manually, and
                // we must not clobber their choice.
                self.viewport.apply_stream_rotation(rotation_quarter_turns);
                if self.overlay.show_decode_info {
                    self.update_window_title();
                }
                flog!(
                    "decode_mode={} hw_fallback_count={}",
                    mode.label(),
                    self.metrics.hw_fallback_count()
                );
            }
            SessionEvent::MediaDurationKnown {
                open_gen,
                seek_gen,
                op_id,
                duration,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                self.media_duration = Some(duration);
            }
            SessionEvent::VideoFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen(), frame.seek_gen(), frame.op_id()) {
                    flog!(
                        "[stale_video] frame_seek={} current_seek={} frame_op={:?} current_op={:?}",
                        frame.seek_gen().0,
                        self.generations.seek().0,
                        frame.op_id(),
                        self.active_operation_id
                    );
                    return Ok(());
                }

                // Precise seek: discard pre-keyframe frames decoded before
                // the actual seek target so playback starts at the right spot.
                if let Some(discard_pts) = self.seek_discard_before_pts {
                    if frame.pts() < discard_pts {
                        return Ok(());
                    }
                }

                // On the first frame of a new file, resize the window to
                // match the video's native aspect ratio (portrait or landscape).
                if self.needs_initial_resize {
                    self.needs_initial_resize = false;
                    let center = !self.has_shown_content;
                    self.has_shown_content = true;
                    let ffmpeg::PendingVideoFrame::D3D11 { surface, .. } = &frame;
                    let (disp_w, disp_h) = surface.display_size();
                    self.window.resize_for_content(disp_w, disp_h, center);
                }

                let ffmpeg::PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    pts,
                    surface,
                    ..
                } = frame;
                self.observe_media_time_origin(pts);
                let handle = self.presenter.register_surface(open_gen, seek_gen, surface);
                self.push_video_frame(DecodedVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    pts,
                    surface: handle,
                });
                if matches!(self.state, PlaybackState::Opening | PlaybackState::Seeking) {
                    self.state = PlaybackState::Priming;
                }
            }
            SessionEvent::AudioFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen, frame.seek_gen, frame.op_id) {
                    return Ok(());
                }

                // Precise seek: discard audio decoded before the seek target.
                if let Some(discard_pts) = self.seek_discard_before_pts {
                    if frame.pts < discard_pts {
                        return Ok(());
                    }
                }

                self.audio_stream_expected = true;
                self.observe_media_time_origin(frame.pts);
                if self.audio_sink.is_none() {
                    let message = self.audio_sink_error.clone().unwrap_or_else(|| {
                        "WASAPI sink was not available for decoded audio".to_string()
                    });
                    self.fail_playback(message);
                    return Ok(());
                }

                self.push_audio_frame(DecodedAudioFrame {
                    open_gen: frame.open_gen,
                    seek_gen: frame.seek_gen,
                    op_id: frame.op_id,
                    pts: frame.pts,
                    format: frame.format,
                    frame_count: frame.frame_count,
                    data: frame.data,
                });
                if matches!(self.state, PlaybackState::Opening | PlaybackState::Seeking) {
                    self.state = PlaybackState::Priming;
                }
            }
            SessionEvent::VideoStreamEnded {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                flog!("[video_stream_ended] seek={} op={:?}", seek_gen.0, op_id);
                self.video_stream_ended = true;
                if matches!(self.state, PlaybackState::Playing | PlaybackState::Priming) {
                    self.state = PlaybackState::Draining;
                }
            }
            SessionEvent::AudioStreamEnded {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                self.audio_stream_expected = true;
                self.audio_stream_ended = true;
            }
            SessionEvent::OpenFailed {
                open_gen,
                op_id,
                error,
            } => {
                if open_gen != self.generations.open() || Some(op_id) != self.active_operation_id {
                    return Ok(());
                }
                self.fail_open(error);
            }
            SessionEvent::PlaybackFailed {
                open_gen,
                seek_gen,
                op_id,
                error,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                let error_msg = error.clone();
                self.fail_playback(error);
                flog!("playback failed: {error_msg}");
            }
            SessionEvent::DeviceLost {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                flog!(
                    "[DEVICE_LOST] seek={} op={:?} workers={}",
                    seek_gen.0,
                    op_id,
                    self.decode_thread.worker_count()
                );
                self.recover_device(now, "worker reported device-lost".to_string())?;
            }
            SessionEvent::AudioEndpointChanged {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                self.recover_audio_endpoint(now, "audio endpoint changed event".to_string())?;
            }
        }

        Ok(())
    }

    fn seek(&mut self, target: SeekTarget, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if self.current_source.is_none() {
            return Ok(());
        }
        // Don't seek during states where it makes no sense or would break
        // the open/recovery sequence.
        if matches!(
            self.state,
            PlaybackState::Idle | PlaybackState::Opening | PlaybackState::Error
        ) {
            flog!("[seek] rejected: state={:?}", self.state);
            return Ok(());
        }

        let clamped_position = match self.media_duration {
            Some(dur) if target.position() > dur => dur,
            _ => target.position(),
        };
        let target = SeekTarget::new(clamped_position);

        // No throttle/defer: the persistent decoder serves the seek in place and
        // coalesces rapid scrub seeks to the latest target, so there is no
        // per-seek decoder spawn to rate-limit.
        let source = self.current_source.clone().unwrap();
        self.execute_seek(source, target, now)
    }

    fn execute_seek(
        &mut self,
        source: Arc<MediaSource>,
        target: SeekTarget,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        self.metrics.note_seek_requested(now);
        self.metrics.disable_open_audio_metric();
        let absolute_target = self.absolute_media_position(target.position());
        let workers_before = self.decode_thread.worker_count();
        flog!(
            "[execute_seek] pos={:.3}s abs={:.3}s open={} seek={} op={:?} workers={} surfaces={} vq={} aq={} device_removed={}",
            target.position().as_secs_f64(),
            absolute_target.as_secs_f64(),
            open_gen.0,
            seek_gen.0,
            op_id,
            workers_before,
            self.surfaces_alive(),
            self.video_queue.len(),
            self.queued_audio_frames.len(),
            self.presenter.device().is_device_removed()
        );
        // Keep the previously selected surface visible until the new frame
        // arrives — `validate_and_select_surface` will swap it atomically
        // when the first frame of the new seek generation is presented.
        self.prepare_runtime_for_operation_inner(false, false, false)?;
        self.state = PlaybackState::Seeking;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        if self.decode_thread.serves(self.decode_preference) {
            // Reuse the open file: tell the running video thread to seek in
            // place. Bumping seek_gen above already gates out its old in-flight
            // frames. `serves` implies a live control; if it is somehow absent,
            // skip rather than panic — the next operation will respawn a worker.
            if let Some(control) = self.decode_thread.control() {
                control.send_seek(absolute_target, seek_gen, op_id);
            }
        } else {
            // No thread, or the decode preference changed (HW↔SW): reopen.
            self.decode_thread.teardown(false);
            self.spawn_decode_thread(
                source.clone(),
                Some(absolute_target),
                open_gen,
                seek_gen,
                op_id,
            );
        }
        // Audio seeks in place on its own persistent worker (no reopen), exactly
        // like the video worker above — so rapid scrubbing never churns the
        // audio demuxer. Stale pre-seek audio carries the old seek_gen and is
        // dropped by the coordinator's generation check. Respawn only if no
        // audio worker is alive (e.g. it exited after an open/decode error).
        if self.audio_decode_thread.control().is_some() {
            if let Some(control) = self.audio_decode_thread.control() {
                control.send_seek(absolute_target, seek_gen, op_id);
            }
        } else {
            self.audio_decode_thread.teardown(false);
            self.spawn_audio_thread(source, Some(absolute_target), open_gen, seek_gen, op_id);
        }
        self.pending_seek_target = Some(target);
        self.seek_discard_before_pts = Some(absolute_target);
        self.overlay.subtitle_clock_base = Some(target.position());
        self.metrics.note_seek_pending();
        self.seek_frame_presented_since_request = false;
        Ok(())
    }

    fn begin_operation(
        &mut self,
        source: Arc<MediaSource>,
        start_position: Option<Duration>,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
        next_state: PlaybackState,
        rebuild_audio_sink: bool,
        reset_audio_expectation: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // A new file/operation: stop the previous decode thread and open fresh.
        // Async (wait=false) is fine — the old thread shares the same device and
        // exits promptly on the shutdown signal; recover_device waits explicitly
        // before rebuilding the device.
        self.decode_thread.teardown(false);
        self.audio_decode_thread.teardown(false);
        self.prepare_runtime_for_operation(rebuild_audio_sink, reset_audio_expectation)?;
        self.state = next_state;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.spawn_decode_thread(source.clone(), start_position, open_gen, seek_gen, op_id);
        self.spawn_audio_thread(source, start_position, open_gen, seek_gen, op_id);
        Ok(())
    }

    fn prepare_runtime_for_operation(
        &mut self,
        rebuild_audio_sink: bool,
        reset_audio_expectation: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.prepare_runtime_for_operation_inner(rebuild_audio_sink, reset_audio_expectation, true)
    }

    fn prepare_runtime_for_operation_inner(
        &mut self,
        rebuild_audio_sink: bool,
        reset_audio_expectation: bool,
        reset_surfaces: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Note: the decode thread is NOT torn down here. begin_operation tears
        // it down before a reopen; execute_seek keeps it and sends a seek
        // command. This only resets UI-side runtime state (queues, clocks).
        self.clear_video_queue();
        self.queued_audio_frames.clear();
        if reset_surfaces {
            self.presenter.reset_surfaces();
            self.presenter.set_timeline_overlay(None)?;
            self.presenter.set_volume_overlay(None, 0, 0)?;
        }
        self.presenter.clear_subtitle_overlay();
        self.video_clock = None;
        if reset_audio_expectation {
            self.media_time_origin_pts = None;
            self.seek_discard_before_pts = None;
        }
        self.paused_clock_position = None;
        self.audio.reset_clock();
        self.drop_buckets = VideoDropBuckets::default();
        self.metrics.reset_for_operation();
        self.video_stream_ended = false;
        self.audio_stream_ended = false;
        self.active_decode_mode = None;
        self.overlay.volume_overlay_until = None;
        self.overlay.subtitle_clock_base = None;
        self.overlay.active_subtitle_cue = None;
        self.overlay.active_subtitle_viewport = None;
        if reset_audio_expectation {
            self.audio_stream_expected = false;
        }

        if rebuild_audio_sink {
            self.audio_sink = match AudioSink::create_shared_default() {
                Ok(mut sink) => {
                    sink.set_volume(self.audio.saved_volume());
                    self.audio_sink_error = None;
                    Some(sink)
                }
                Err(error) => {
                    self.audio_sink_error = Some(error.to_string());
                    None
                }
            };
        } else if self.pause_after_seek {
            // Scrub-seeking while paused — just stop the audio sink without
            // a full reset to avoid rapid WASAPI buffer churn (0x88890005).
            if let Some(sink) = self.audio_sink.as_mut() {
                let _ = sink.pause();
            }
        } else if let Some(sink) = self.audio_sink.as_mut() {
            if let Err(error) = sink.reset() {
                self.audio_sink_error = Some(error.to_string());
                self.audio_sink = None;
            }
        }

        Ok(())
    }

    fn spawn_decode_thread(
        &mut self,
        source: Arc<MediaSource>,
        start_position: Option<Duration>,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    ) {
        let sender = self.event_tx.clone();
        let audio_sender = self.audio_event_tx.clone();
        let device = self.presenter.device().clone();
        let audio_format = self
            .audio_sink
            .as_ref()
            .map(|sink| sink.format())
            .unwrap_or_else(AudioStreamFormat::stereo_f32_48khz);
        let decode_preference = self.decode_preference;
        let control = Arc::new(DecodeControl::new());
        let worker_count = self
            .decode_thread
            .prepare_spawn(control.clone(), decode_preference);

        flog!(
            "[spawn_decode_thread] open={} seek={} op={:?} workers_now={}",
            open_gen.0,
            seek_gen.0,
            op_id,
            self.decode_thread.worker_count()
        );

        let handle = thread::spawn(move || {
            // Decrement the live-thread counter on any exit path.
            struct WorkerGuard(Arc<AtomicU32>);
            impl Drop for WorkerGuard {
                fn drop(&mut self) {
                    let remaining = self.0.fetch_sub(1, Ordering::Release).saturating_sub(1);
                    flog!("[decode_thread_exit] workers_remaining={}", remaining);
                }
            }
            let _guard = WorkerGuard(worker_count);

            // (seek_gen, op_id) the thread currently decodes for; updated on each
            // seek command so produced frames and events carry the right stamps.
            let gen_cell = Cell::new((seek_gen, op_id));
            // The command sequence the thread is currently serving.
            let serving = Cell::new(0u64);

            // A newer command has arrived (or shutdown requested): the in-flight
            // decode should abort so the new target can be applied.
            let cancelled = || control.seq() != serving.get() || control.is_shutdown();

            // Non-blocking send that retries when the bounded channel is full,
            // aborting if a newer command arrives so a stale decode cannot block
            // forever on a full channel.
            let worker_send =
                |event: SessionEvent, tx: &SyncSender<SessionEvent>| -> Result<(), String> {
                    let mut event = event;
                    let mut full_retries: u32 = 0;
                    loop {
                        match tx.try_send(event) {
                            Ok(()) => return Ok(()),
                            Err(TrySendError::Full(returned)) => {
                                if cancelled() {
                                    return Err(WORKER_CANCELLED.to_string());
                                }
                                // Poll at 1ms while the consumer is actively
                                // draining — this is the brief, transient
                                // backpressure of normal playback, where a freed
                                // slot must be refilled promptly to avoid audio
                                // starvation. Once the queue has stayed full well
                                // past any frame interval, the UI is paused or
                                // stalled and is not draining at all; back off to
                                // a longer sleep so a paused player does not spin
                                // the decode thread. Shutdown and superseded-seek
                                // are still observed every iteration, so cancel
                                // latency stays within one sleep interval.
                                let sleep_ms = if full_retries < 64 { 1 } else { 10 };
                                full_retries = full_retries.saturating_add(1);
                                thread::sleep(std::time::Duration::from_millis(sleep_ms));
                                event = returned;
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Err(WORKER_CANCELLED.to_string());
                            }
                        }
                    }
                };

            // Cancellation signal for FFmpeg's interrupt callback: aborts a
            // blocking open/read/seek when the worker is asked to shut down, so
            // a worker wedged in FFmpeg I/O (dead network share, vanished drive)
            // still exits promptly and never hangs teardown. Owned + 'static so
            // it can live inside the format context for the session's lifetime.
            let io_cancel: Box<dyn Fn() -> bool> = {
                let control = control.clone();
                Box::new(move || control.is_shutdown())
            };

            // Open the file and decoders once. Errors map to open/playback
            // failure exactly as the per-operation worker did.
            let open_result = unsafe {
                DecodeSession::open(
                    &source,
                    &device,
                    audio_format,
                    start_position,
                    decode_preference,
                    // Audio is decoded by the independent audio worker (see
                    // `spawn_audio_thread`); this worker is video-only.
                    false,
                    io_cancel,
                    &cancelled,
                    &mut |mode, hw_fallback_count, rotation_quarter_turns| {
                        let (seek_gen, op_id) = gen_cell.get();
                        worker_send(
                            SessionEvent::DecodeModeSelected {
                                open_gen,
                                seek_gen,
                                op_id,
                                mode,
                                hw_fallback_count,
                                rotation_quarter_turns,
                            },
                            &sender,
                        )
                    },
                    &mut |duration| {
                        let (seek_gen, op_id) = gen_cell.get();
                        worker_send(
                            SessionEvent::MediaDurationKnown {
                                open_gen,
                                seek_gen,
                                op_id,
                                duration,
                            },
                            &sender,
                        )
                    },
                )
            };
            let mut session = match open_result {
                Ok(Some(session)) => session,
                Ok(None) => return,
                Err(error) => {
                    if !control.is_shutdown() {
                        let (seek_gen, op_id) = gen_cell.get();
                        let event =
                            if error.contains("device removed") || device.is_device_removed() {
                                SessionEvent::DeviceLost {
                                    open_gen,
                                    seek_gen,
                                    op_id,
                                }
                            } else if start_position.is_some() {
                                SessionEvent::PlaybackFailed {
                                    open_gen,
                                    seek_gen,
                                    op_id,
                                    error,
                                }
                            } else {
                                SessionEvent::OpenFailed {
                                    open_gen,
                                    op_id,
                                    error,
                                }
                            };
                        let _ = worker_send(event, &sender);
                    }
                    return;
                }
            };

            let mut on_decode_mode = |mode, hw_fallback_count, rotation_quarter_turns| {
                let (seek_gen, op_id) = gen_cell.get();
                worker_send(
                    SessionEvent::DecodeModeSelected {
                        open_gen,
                        seek_gen,
                        op_id,
                        mode,
                        hw_fallback_count,
                        rotation_quarter_turns,
                    },
                    &sender,
                )
            };
            let mut on_video = |frame| worker_send(SessionEvent::VideoFrameReady(frame), &sender);
            let mut on_audio =
                |frame| worker_send(SessionEvent::AudioFrameReady(frame), &audio_sender);

            // Persistent decode loop: decode the current position to EOF, then
            // block for the next seek command (or shutdown). A seek arriving
            // mid-decode trips `cancelled`, aborting run_to_eof so the new
            // target is applied via seek_and_flush without reopening the file.
            loop {
                let (cur_seek_gen, cur_op_id) = gen_cell.get();
                let result = unsafe {
                    session.run_to_eof(
                        &device,
                        open_gen,
                        cur_seek_gen,
                        cur_op_id,
                        &cancelled,
                        &mut on_decode_mode,
                        &mut on_video,
                        &mut on_audio,
                    )
                };
                if control.is_shutdown() {
                    break;
                }
                match result {
                    Ok(StreamStatus::Completed(_)) => {
                        // Only announce end-of-stream if no newer command has
                        // superseded this run.
                        if control.seq() == serving.get() {
                            let (seek_gen, op_id) = gen_cell.get();
                            let _ = worker_send(
                                SessionEvent::VideoStreamEnded {
                                    open_gen,
                                    seek_gen,
                                    op_id,
                                },
                                &sender,
                            );
                            if session.had_audio_stream() {
                                let _ = worker_send(
                                    SessionEvent::AudioStreamEnded {
                                        open_gen,
                                        seek_gen,
                                        op_id,
                                    },
                                    &audio_sender,
                                );
                            }
                        }
                    }
                    Ok(StreamStatus::Cancelled) => {}
                    Err(ref error) if error == WORKER_CANCELLED => {}
                    Err(error) => {
                        let (seek_gen, op_id) = gen_cell.get();
                        let event =
                            if error.contains("device removed") || device.is_device_removed() {
                                SessionEvent::DeviceLost {
                                    open_gen,
                                    seek_gen,
                                    op_id,
                                }
                            } else {
                                SessionEvent::PlaybackFailed {
                                    open_gen,
                                    seek_gen,
                                    op_id,
                                    error,
                                }
                            };
                        let _ = worker_send(event, &sender);
                    }
                }

                let (command, new_seq) = control.wait_next();
                match command {
                    DecodeCommand::Shutdown => break,
                    DecodeCommand::Seek {
                        target,
                        seek_gen,
                        op_id,
                    } => {
                        serving.set(new_seq);
                        gen_cell.set((seek_gen, op_id));
                        if let Err(error) = unsafe { session.seek(target) } {
                            let event =
                                if error.contains("device removed") || device.is_device_removed() {
                                    SessionEvent::DeviceLost {
                                        open_gen,
                                        seek_gen,
                                        op_id,
                                    }
                                } else {
                                    SessionEvent::PlaybackFailed {
                                        open_gen,
                                        seek_gen,
                                        op_id,
                                        error,
                                    }
                                };
                            let _ = worker_send(event, &sender);
                        }
                    }
                }
            }
        });
        self.decode_thread.set_join(handle);
    }

    /// Spawn the persistent, independent audio-only decode worker for `source`,
    /// decoding from `start_position` and stamping frames with the given
    /// generations.
    ///
    /// ## Why a separate audio worker exists
    ///
    /// Some files cannot use D3D11VA/NVDEC hardware decode — e.g. true H.264
    /// **Baseline** profile at 4K60, which the hardware rejects — and fall back
    /// to software video decode that runs *below* realtime. With a single worker
    /// demuxing and decoding both streams sequentially, audio was produced only
    /// as fast as the slow video, starving the WASAPI sink and causing choppy
    /// audio. Decoding audio on its own demuxer/thread keeps the audio sink fed
    /// at realtime regardless of video decode speed; when video can't keep up it
    /// simply lags and the presenter drops late *video* frames (see
    /// [`Self::advance_video_playback`]), never the audio.
    ///
    /// ## Lifecycle
    ///
    /// Mirrors the video worker: it decodes to EOF, then parks on
    /// [`DecodeControl::wait_next`] for an in-place [`DecodeCommand::Seek`] (so
    /// rapid scrubbing reuses the open demuxer instead of reopening the file) or
    /// [`DecodeCommand::Shutdown`]. It uses no GPU device, so there is no
    /// hardware fallback to coordinate. Audio-side failures degrade to silent
    /// (no audio) rather than failing the whole playback, and still emit
    /// `AudioStreamEnded` so end-of-playback can complete. Stale frames from a
    /// superseded seek carry the old generations and are dropped by the
    /// coordinator's `is_current_frame` check.
    fn spawn_audio_thread(
        &mut self,
        source: Arc<MediaSource>,
        start_position: Option<Duration>,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    ) {
        let audio_sender = self.audio_event_tx.clone();
        let audio_format = self
            .audio_sink
            .as_ref()
            .map(|sink| sink.format())
            .unwrap_or_else(AudioStreamFormat::stereo_f32_48khz);
        let control = Arc::new(DecodeControl::new());
        // Preference is irrelevant for audio (no hardware path); the handle is
        // tracked so the coordinator can send in-place seeks and shutdown.
        let worker_count = self
            .audio_decode_thread
            .prepare_spawn(control.clone(), VideoDecodePreference::Auto);

        let handle = thread::spawn(move || {
            let _guard = WorkerExitGuard(worker_count);

            // (seek_gen, op_id) the worker currently decodes for; updated on each
            // seek command so produced frames carry the right stamps.
            let gen_cell = Cell::new((seek_gen, op_id));
            // The command sequence the worker is currently serving.
            let serving = Cell::new(0u64);
            // A newer command arrived (or shutdown): abort the in-flight decode.
            let cancelled = || control.seq() != serving.get() || control.is_shutdown();
            let worker_send =
                |event: SessionEvent, tx: &SyncSender<SessionEvent>| -> Result<(), String> {
                    let mut event = event;
                    let mut full_retries: u32 = 0;
                    loop {
                        match tx.try_send(event) {
                            Ok(()) => return Ok(()),
                            Err(TrySendError::Full(returned)) => {
                                if cancelled() {
                                    return Err(WORKER_CANCELLED.to_string());
                                }
                                let sleep_ms = if full_retries < 64 { 1 } else { 10 };
                                full_retries = full_retries.saturating_add(1);
                                thread::sleep(Duration::from_millis(sleep_ms));
                                event = returned;
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Err(WORKER_CANCELLED.to_string());
                            }
                        }
                    }
                };

            let io_cancel: Box<dyn Fn() -> bool> = {
                let control = control.clone();
                Box::new(move || control.is_shutdown())
            };

            let mut session = match unsafe {
                AudioDecodeSession::open(
                    &source,
                    audio_format,
                    start_position,
                    io_cancel,
                    &cancelled,
                )
            } {
                // No audio stream, or cancelled during open: nothing to do.
                Ok(None) => return,
                Ok(Some(session)) => session,
                Err(error) => {
                    if !control.is_shutdown() {
                        flog!("[audio_worker] open failed (continuing without audio): {error}");
                    }
                    return;
                }
            };

            let mut on_audio =
                |frame| worker_send(SessionEvent::AudioFrameReady(frame), &audio_sender);

            // Persistent decode loop: decode to EOF, then block for the next
            // seek command (or shutdown). A seek arriving mid-decode trips
            // `cancelled`, aborting `run_to_eof` so the new target is applied via
            // an in-place `seek` without reopening the file.
            loop {
                let (cur_seek_gen, cur_op_id) = gen_cell.get();
                let result = unsafe {
                    session.run_to_eof(open_gen, cur_seek_gen, cur_op_id, &cancelled, &mut on_audio)
                };
                if control.is_shutdown() {
                    break;
                }
                // Announce end-of-audio so the coordinator's end-of-playback
                // check can complete — but only if no newer command superseded
                // this run. A mid-stream decode error degrades to "audio stopped
                // here" rather than failing video.
                let announce_end = match result {
                    Ok(StreamStatus::Completed(_)) => true,
                    Ok(StreamStatus::Cancelled) => false,
                    Err(ref error) if error == WORKER_CANCELLED => false,
                    Err(error) => {
                        flog!("[audio_worker] decode error (audio stopped): {error}");
                        true
                    }
                };
                if announce_end && control.seq() == serving.get() {
                    let (seek_gen, op_id) = gen_cell.get();
                    let _ = worker_send(
                        SessionEvent::AudioStreamEnded {
                            open_gen,
                            seek_gen,
                            op_id,
                        },
                        &audio_sender,
                    );
                }

                let (command, new_seq) = control.wait_next();
                match command {
                    DecodeCommand::Shutdown => break,
                    DecodeCommand::Seek {
                        target,
                        seek_gen,
                        op_id,
                    } => {
                        serving.set(new_seq);
                        gen_cell.set((seek_gen, op_id));
                        if let Err(error) = unsafe { session.seek(target) } {
                            if !control.is_shutdown() {
                                flog!("[audio_worker] seek failed (audio stopped): {error}");
                            }
                        }
                    }
                }
            }
        });
        self.audio_decode_thread.set_join(handle);
    }

    fn handle_resize(
        &mut self,
        size: ResizeRequest,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.metrics.note_resize(now);
        self.metrics.note_resize_recovery_started(now);

        match self.presenter.resize(size.width, size.height) {
            Ok(()) => {
                self.present_needed = true;
                if let Some(elapsed) = self.metrics.note_resize_recovered(now) {
                    flog!("resize_recover_ms={}", elapsed.as_millis());
                }
                Ok(())
            }
            Err(error) => {
                if self.presenter.rebuild_swap_chain(&self.window).is_ok() {
                    self.present_needed = true;
                    if let Some(elapsed) = self.metrics.note_resize_recovered(now) {
                        flog!("resize_recover_ms={}", elapsed.as_millis());
                    }
                    return Ok(());
                }
                flog!("resize recovery deferred: {error}");
                let _ = self.metrics.note_resize_recovered(now);
                Ok(())
            }
        }
    }

    fn submit_due_audio(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if matches!(self.state, PlaybackState::Paused | PlaybackState::Ended)
            || self.pause_after_seek
            || self.playback_rate != 1.0
        {
            return Ok(());
        }

        let mut first_written_pts = None;
        let mut wrote_any_audio = false;
        let mut audio_error = None;

        {
            let Some(sink) = self.audio_sink.as_mut() else {
                return Ok(());
            };

            while let Some(front) = self.queued_audio_frames.front_mut() {
                let written = match sink.write_frame(&front.frame, front.submitted_frames) {
                    Ok(written) => written,
                    Err(error) => {
                        audio_error = Some(error.to_string());
                        break;
                    }
                };
                if written == 0 {
                    break;
                }
                self.audio_diag.add_frames_written(written);

                if first_written_pts.is_none() {
                    let offset = Duration::from_secs_f64(
                        front.submitted_frames as f64 / front.frame.format.sample_rate as f64,
                    );
                    first_written_pts = Some(front.frame.pts().saturating_add(offset));
                }

                wrote_any_audio = true;
                self.audio.record_submitted(written as u64);
                front.submitted_frames = front.submitted_frames.saturating_add(written);
                if front.submitted_frames >= front.frame.frame_count() {
                    self.queued_audio_frames.pop_front();
                }
            }

            if wrote_any_audio && !sink.is_started() {
                if let Err(error) = sink.resume() {
                    audio_error = Some(error.to_string());
                }
            }

            if wrote_any_audio && !self.audio.is_clock_anchored() && sink.is_started() {
                self.audio.set_clock_anchor(first_written_pts);
                self.video_clock = None;
                if self.metrics.measure_open_audio() {
                    self.metrics.pending_first_audio = true;
                }
            }
        }

        if let Some(error) = audio_error {
            self.recover_audio_endpoint(now, error)?;
            return Ok(());
        }

        let buffered = match self.audio_sink.as_ref() {
            Some(sink) => sink.buffered_frames().unwrap_or(1),
            None => return Ok(()),
        };
        // Seed the per-tick cache so the master clock / end-of-playback checks
        // later in this tick reuse this value instead of re-querying WASAPI.
        self.cached_buffered_frames.set(Some(buffered));
        if self.audio_stream_expected
            && self.state != PlaybackState::Paused
            && self.audio_sink.as_ref().map_or(false, |s| s.is_started())
            && self.queued_audio_frames.is_empty()
            && buffered == 0
            && !self.can_finish_with_buffered(0)?
        {
            self.metrics.note_audio_underrun();
            self.audio_diag
                .note_underrun(self.queued_audio_frames.len(), buffered);
            // Audio drained dry. Capture the current position *before* clearing
            // the anchor, then hand the master clock to a video clock seeded at
            // that position. Without this, master_clock_position falls through
            // to a None video_clock: the playhead collapses to zero and queued
            // video frames present unpaced (burst) until audio re-anchors. The
            // next successful submission clears video_clock and re-establishes
            // the audio clock (see the anchor handoff above).
            let resume_pts = self.master_clock_position(now).unwrap_or(Duration::ZERO);
            self.audio.reset_clock();
            if self.video_clock.is_none() {
                self.video_clock = Some(PlaybackClock::new(now, resume_pts, self.playback_rate));
            }
        }

        // Stop the audio sink once all audio has been submitted and the
        // WASAPI buffer has drained.  Without this, GetCurrentPadding may
        // return a small residual value indefinitely, blocking the
        // end-of-playback transition in can_finish_playback().
        if self.audio_stream_expected
            && self.audio_stream_ended
            && self.queued_audio_frames.is_empty()
            && matches!(self.state, PlaybackState::Draining | PlaybackState::Playing)
        {
            if let Some(sink) = self.audio_sink.as_mut() {
                if sink.is_started() && buffered == 0 {
                    let _ = sink.pause();
                }
            }
        }

        if self.audio_stream_expected
            && self.audio.is_clock_anchored()
            && self.state == PlaybackState::Priming
        {
            self.state = PlaybackState::Playing;
        }

        Ok(())
    }

    fn advance_video_playback(&mut self, now: Instant) -> Result<bool, Box<dyn std::error::Error>> {
        if matches!(self.state, PlaybackState::Paused | PlaybackState::Ended) {
            return Ok(false);
        }

        // Compute the master clock once — it is constant within this call since `now`
        // is fixed and audio state does not change mid-function.
        let audio_clock = if self.audio.is_clock_anchored() {
            let Some(clock) = self.master_clock_position(now) else {
                return Ok(false);
            };
            Some(clock)
        } else {
            None
        };

        loop {
            let Some(next_frame) = self.video_queue.front() else {
                return Ok(false);
            };

            if let Some(audio_clock) = audio_clock {
                let next_frame_time = self.media_time_for_pts(next_frame.pts());
                if next_frame_time > audio_clock {
                    return Ok(false);
                }

                let lateness = audio_clock.saturating_sub(next_frame_time);
                if lateness > VERY_LATE_VIDEO_THRESHOLD && self.video_queue.len() > 1 {
                    let dropped = self.video_queue.pop_front().expect("front frame existed");
                    self.drop_video_frame(dropped, VideoDropCause::SchedulerLate);
                    continue;
                }

                while self.video_queue.len() > 1 {
                    let Some(upcoming_frame) = self.video_queue.peek_second() else {
                        break;
                    };
                    let upcoming_time = self.media_time_for_pts(upcoming_frame.pts());
                    if upcoming_time > audio_clock {
                        break;
                    }

                    let dropped = self.video_queue.pop_front().expect("front frame existed");
                    self.drop_video_frame(dropped, VideoDropCause::SchedulerLate);
                }
            } else if let Some(clock) = self.video_clock {
                let due_at = clock.deadline_for(self.media_time_for_pts(next_frame.pts()));
                if now < due_at {
                    return Ok(false);
                }
            }

            let frame = self.video_queue.pop_front().expect("front frame existed");

            // Check out-point by frame PTS before presenting, so we stop exactly
            // on the right frame rather than relying on the lagging audio clock.
            if let Some(out_pt) = self.clip_range.out_point() {
                if matches!(
                    self.state,
                    PlaybackState::Playing | PlaybackState::Priming | PlaybackState::Draining
                ) {
                    let frame_pos = self.media_time_for_pts(frame.pts());
                    if frame_pos >= out_pt {
                        self.presenter.release_surface(frame.surface());
                        if self.clip_range.loop_range() {
                            let target = self.clip_range.replay_position();
                            self.seek(SeekTarget::new(target), now)?;
                        } else {
                            // This is a logical clip boundary, not a decoder
                            // lifetime boundary. Keep the persistent decoder
                            // parked so Space can replay through an in-place
                            // seek. Asynchronously tearing down the D3D11VA
                            // codec here can race the UI rendering the retained
                            // surface and invalidate the shared device.
                            self.clear_video_queue();
                            self.queued_audio_frames.clear();
                            if let Some(sink) = self.audio_sink.as_mut() {
                                if sink.is_started() {
                                    let _ = sink.pause();
                                }
                            }
                            // Freeze the display clock so the timeline playhead
                            // doesn't drift past the out-point after stopping.
                            self.video_clock = None;
                            self.paused_clock_position = Some(frame_pos);
                            self.state = PlaybackState::Ended;
                        }
                        return Ok(true);
                    }
                }
            }

            self.present_video_frame(frame, now);
            return Ok(false);
        }
    }

    fn present_video_frame(&mut self, frame: DecodedVideoFrame, now: Instant) {
        if self.video_clock.is_none() && !self.audio.is_clock_anchored() {
            self.video_clock = Some(PlaybackClock::new(
                now,
                self.media_time_for_pts(frame.pts()),
                self.playback_rate,
            ));
        }

        let handle = frame.surface();
        match self
            .presenter
            .validate_and_select_surface(handle, frame.open_gen(), frame.seek_gen())
        {
            Err(()) => {
                self.drop_video_frame(frame, VideoDropCause::SurfaceMismatch);
                return;
            }
            Ok(Some(previous)) if previous != handle => {
                self.presenter.release_surface(previous);
            }
            Ok(_) => {}
        }

        let frame_position = self.media_time_for_pts(frame.pts());
        if let Some(previous) = self.last_presented_media_position {
            let interval = frame_position.saturating_sub(previous);
            if !interval.is_zero() && interval <= MAX_OBSERVED_FRAME_INTERVAL {
                self.observed_frame_interval = Some(interval);
            }
        }
        self.last_presented_media_position = Some(frame_position);

        self.present_needed = true;
        self.metrics.note_video_frame_presented();
        if let Some(elapsed) = self.metrics.note_resume_first_frame(now) {
            flog!("play_to_motion_ms={}", elapsed.as_millis());
        }
        self.metrics.pending_first_frame |= self.metrics.presented_video_frames() == 1;
        self.seek_frame_presented_since_request |= self.metrics.pending_seek_settled;
        // Check pause_after_seek unconditionally — audio submission is already
        // blocked by the same flag so we must not wait for the audio anchor.
        if self.pause_after_seek {
            self.pause_after_seek = false;
            self.paused_clock_position = self.pending_seek_target.map(|t| t.position());
            self.state = PlaybackState::Paused;
        } else if !self.audio_stream_expected || self.audio.is_clock_anchored() {
            self.state = PlaybackState::Playing;
        }
    }

    fn drop_video_frame(&mut self, frame: DecodedVideoFrame, cause: VideoDropCause) {
        self.presenter.release_surface(frame.surface());
        self.drop_buckets.note(cause);
        self.metrics.note_video_frame_dropped();
    }

    fn clear_video_queue(&mut self) {
        while let Some(frame) = self.video_queue.pop_front() {
            self.presenter.release_surface(frame.surface());
        }
    }

    fn push_video_frame(&mut self, frame: DecodedVideoFrame) {
        self.video_queue.insert_ordered(frame);
        while let Some(dropped) = self.video_queue.evict_overflow() {
            self.drop_video_frame(dropped, VideoDropCause::QueueOverflow);
        }
    }

    fn push_audio_frame(&mut self, frame: DecodedAudioFrame) {
        if self.queued_audio_frames.len() >= self.queued_audio_capacity {
            return;
        }

        self.queued_audio_frames.push_back(QueuedAudioFrame {
            frame,
            submitted_frames: 0,
        });
    }

    fn toggle_pause(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        match self.state {
            PlaybackState::Seeking => {
                self.toggle_pending_seek_pause(now)?;
            }
            PlaybackState::Priming if self.pending_seek_target.is_some() => {
                self.toggle_pending_seek_pause(now)?;
            }
            PlaybackState::Playing | PlaybackState::Priming | PlaybackState::Draining => {
                self.metrics.note_pause_requested(now);
                self.paused_clock_position = self.master_clock_position(now);
                if let Some(sink) = self.audio_sink.as_mut() {
                    if sink.is_started() {
                        sink.pause()?;
                    }
                }
                self.state = PlaybackState::Paused;
                if let Some(elapsed) = self.metrics.note_pause_completed(Instant::now()) {
                    flog!("pause_to_stop_ms={}", elapsed.as_millis());
                }
            }
            PlaybackState::Paused => {
                let position = self.snapshot(now).position;
                if let Some(target) = self.clip_range.resume_target(position) {
                    self.pause_after_seek = false;
                    self.seek(SeekTarget::new(target), now)?;
                    return Ok(());
                }
                // If both streams ended while paused, treat as replay.
                if self.video_stream_ended
                    && self.video_queue.is_empty()
                    && (!self.audio_stream_expected
                        || (self.audio_stream_ended && self.queued_audio_frames.is_empty()))
                {
                    self.replay(now)?;
                    return Ok(());
                }
                self.metrics.note_resume_requested(now);
                if !self.audio.is_clock_anchored() {
                    let resume_pts = self.paused_clock_position.unwrap_or(Duration::ZERO);
                    self.video_clock =
                        Some(PlaybackClock::new(now, resume_pts, self.playback_rate));
                } else if let Some(sink) = self.audio_sink.as_mut() {
                    sink.resume()?;
                }
                self.paused_clock_position = None;
                self.state = if self.presenter.has_selected_surface() {
                    PlaybackState::Playing
                } else {
                    PlaybackState::Priming
                };
            }
            PlaybackState::Ended => {
                self.replay(now)?;
            }
            _ => {}
        }

        Ok(())
    }

    fn toggle_pending_seek_pause(
        &mut self,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.pause_after_seek {
            // Space arrived before the seek produced its first frame. Preserve
            // the pause request so that frame lands paused instead of ignoring
            // the key while the coordinator is in Seeking/Priming.
            self.pause_after_seek = true;
            if let Some(sink) = self.audio_sink.as_mut() {
                if sink.is_started() {
                    sink.pause()?;
                }
            }
            return Ok(());
        }

        // A second Space means resume. Reissue the pending target as the one
        // authoritative non-pausing seek so the audio sink is reset exactly as
        // it is at the end of a normal scrub. Resuming outside an active clip
        // range restarts at its beginning.
        let position = self
            .pending_seek_target
            .map(SeekTarget::position)
            .unwrap_or_else(|| self.snapshot(now).position);
        let target = self.clip_range.resume_target(position).unwrap_or(position);
        self.pause_after_seek = false;
        self.seek(SeekTarget::new(target), now)
    }

    fn replay(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        self.seek(SeekTarget::new(self.clip_range.replay_position()), now)
    }

    fn toggle_subtitles(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.overlay.toggle_subtitles(&mut self.presenter)
    }

    fn zoom_at_cursor(&mut self, delta: i16, cursor_x: i32, cursor_y: i32) {
        let viewport = self.presenter.viewport_size().unwrap_or((1, 1));
        if self
            .viewport
            .zoom_at_cursor(delta, cursor_x, cursor_y, viewport)
        {
            self.present_needed = true;
        }
    }

    fn rotate_view(&mut self, delta_quarter_turns: u8) {
        self.viewport.rotate(delta_quarter_turns);
        self.present_needed = true;
    }

    fn reset_view(&mut self) {
        self.viewport.reset();
        self.present_needed = true;
    }

    fn fit_window(&mut self) {
        let Some((w, h)) = self.presenter.current_surface_size() else {
            return;
        };
        // Account for rotation: odd quarter-turns swap width and height.
        let (w, h) = self.viewport.orient_dimensions(w, h);
        self.window.fit_window_to_content(w, h);
    }

    fn half_size_window(&mut self) {
        let Some((w, h)) = self.presenter.current_surface_size() else {
            return;
        };
        let (w, h) = self.viewport.orient_dimensions(w, h);
        self.window
            .set_window_client_size((w / 2).max(1), (h / 2).max(1));
    }

    fn step_playback_rate(&mut self, step: i8) {
        const RATES: &[f64] = &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
        let current_idx = RATES
            .iter()
            .position(|&r| (r - self.playback_rate).abs() < 0.01)
            .unwrap_or(3); // default to 1.0 index
        let new_idx = (current_idx as i8 + step).clamp(0, RATES.len() as i8 - 1) as usize;
        let new_rate = RATES[new_idx];
        if (new_rate - self.playback_rate).abs() < 0.01 {
            return;
        }
        self.playback_rate = new_rate;
        // Reset clock state — clocks re-anchor on next frame/audio submission.
        self.video_clock = None;
        self.audio.reset_clock();
        self.queued_audio_frames.clear();
        self.update_window_title();
    }

    fn update_window_title(&self) {
        let source_name = self
            .current_source
            .as_ref()
            .and_then(|s| s.path().file_name())
            .and_then(|n| n.to_str());
        let decode_label = self.active_decode_mode.map(|mode| mode.label());
        let title = self
            .overlay
            .window_title(source_name, self.playback_rate, decode_label);
        self.window.set_title(&title);
    }

    fn update_subtitle_overlay(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let subtitle_position = self.subtitle_position(now);
        if self
            .overlay
            .update_subtitle(subtitle_position, &mut self.presenter)?
        {
            self.present_needed = true;
        }
        Ok(())
    }

    fn subtitle_position(&mut self, now: Instant) -> Duration {
        if let Some(target) = self.pending_seek_target {
            return target.position();
        }

        let master = self.master_clock_position(now).unwrap_or(Duration::ZERO);
        if let Some(base) = self.overlay.subtitle_clock_base {
            if master.saturating_add(Duration::from_secs(1)) < base {
                return base;
            }
            self.overlay.subtitle_clock_base = None;
        }

        master
    }

    fn master_clock_position(&self, now: Instant) -> Option<Duration> {
        if let Some(paused) = self.paused_clock_position {
            return Some(paused);
        }

        if let (Some(anchor_pts), Some(sink)) =
            (self.audio.clock_anchor_pts(), self.audio_sink.as_ref())
        {
            if sink.is_started() {
                let buffered_frames = self.current_buffered_frames() as u64;
                let played_frames = self.audio.played_frames(buffered_frames);
                let sample_rate = sink.format().sample_rate;
                if sample_rate > 0 {
                    let played = Duration::from_secs_f64(played_frames as f64 / sample_rate as f64);
                    return Some(self.media_time_for_pts(anchor_pts).saturating_add(played));
                }
            }
        }

        self.video_clock.map(|clock| clock.position_at(now))
    }

    fn observe_media_time_origin(&mut self, pts: Duration) {
        if self.media_time_origin_pts.is_none() {
            // On a resume open the first decoded frame is at ~`file_start +
            // open_start_position`. Anchor the origin at the file start so
            // normalized positions stay aligned with the timeline (a non-resume
            // open uses offset 0, unchanged behavior).
            self.media_time_origin_pts = Some(media_time_origin(pts, self.open_start_position));
            self.open_start_position = Duration::ZERO;
        }
    }

    fn media_time_for_pts(&self, pts: Duration) -> Duration {
        pts.saturating_sub(self.media_time_origin_pts.unwrap_or(pts))
    }

    fn seek_is_settled(&self) -> bool {
        self.seek_frame_presented_since_request
            && (!self.audio_stream_expected || self.audio.is_clock_anchored())
    }

    fn desired_restart_position(&self, now: Instant) -> Duration {
        self.absolute_media_position(
            self.pending_seek_target
                .map(SeekTarget::position)
                .unwrap_or_else(|| self.snapshot(now).position),
        )
    }

    fn absolute_media_position(&self, normalized_position: Duration) -> Duration {
        self.media_time_origin_pts
            .map(|origin| origin.saturating_add(normalized_position))
            .unwrap_or(normalized_position)
    }

    fn recover_audio_endpoint(
        &mut self,
        now: Instant,
        reason: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(source) = self.current_source.clone() else {
            return Ok(());
        };
        let restart_target = self.desired_restart_position(now);
        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        flog!("audio endpoint recovery: {reason}");
        self.metrics.disable_open_audio_metric();
        self.begin_operation(
            source,
            Some(restart_target),
            open_gen,
            seek_gen,
            op_id,
            PlaybackState::Seeking,
            true,
            false,
        )?;
        self.overlay.subtitle_clock_base = Some(restart_target);
        Ok(())
    }

    fn recover_device(
        &mut self,
        now: Instant,
        reason: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(source) = self.current_source.clone() else {
            return Ok(());
        };

        self.metrics.note_device_recovery_started(now);
        flog!("device recovery: {reason}");
        // The decode thread holds a clone of the device being destroyed — wait
        // for it to exit before rebuilding so its codec/device teardown cannot
        // race the new device.
        self.decode_thread.teardown(true);
        if let Err(error) = self.presenter.rebuild_device(&self.window) {
            flog!("device recovery failed: {error}");
            self.fail_playback(format!("device recovery failed: {error}"));
            return Ok(());
        }
        let restart_target = self.desired_restart_position(now);
        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        self.metrics.disable_open_audio_metric();
        self.begin_operation(
            source,
            Some(restart_target),
            open_gen,
            seek_gen,
            op_id,
            PlaybackState::Seeking,
            false,
            false,
        )?;
        self.overlay.subtitle_clock_base = Some(restart_target);
        if let Some(elapsed) = self.metrics.note_device_recovered(now) {
            flog!("device_recovery_ms={}", elapsed.as_millis());
        }
        Ok(())
    }

    fn can_finish_playback(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let buffered = self.current_buffered_frames();
        self.can_finish_with_buffered(buffered)
    }

    /// Audio sink buffered-frame count, cached for the duration of a single
    /// `tick` (see [`Self::cached_buffered_frames`]). Outside a tick it always
    /// queries fresh so callers like `snapshot` never observe a stale value.
    fn current_buffered_frames(&self) -> u32 {
        if self.tick_active.get() {
            if let Some(cached) = self.cached_buffered_frames.get() {
                return cached;
            }
        }
        let value = self
            .audio_sink
            .as_ref()
            .map(|sink| sink.buffered_frames().unwrap_or(0))
            .unwrap_or(0);
        if self.tick_active.get() {
            self.cached_buffered_frames.set(Some(value));
        }
        value
    }

    fn can_finish_with_buffered(&self, buffered: u32) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.video_stream_ended || !self.video_queue.is_empty() {
            return Ok(false);
        }

        if self.audio_stream_expected {
            if !self.audio_stream_ended || !self.queued_audio_frames.is_empty() {
                return Ok(false);
            }
            if buffered != 0 {
                return Ok(false);
            }
        }

        Ok(matches!(
            self.state,
            PlaybackState::Priming | PlaybackState::Playing | PlaybackState::Draining
        ))
    }

    fn fail_open(&mut self, error: String) {
        self.decode_thread.teardown(false);
        self.audio_decode_thread.teardown(false);
        self.pending_seek_target = None;
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
        self.active_decode_mode = None;
        self.state = PlaybackState::Error;
        flog!("open failed: {error}");
    }

    fn fail_playback(&mut self, error: String) {
        self.decode_thread.teardown(false);
        self.audio_decode_thread.teardown(false);
        self.pending_seek_target = None;
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
        self.active_decode_mode = None;
        self.state = PlaybackState::Error;
        flog!("playback failed: {error}");
    }

    /// Show the idle "Drop a file" overlay during Error state so the user
    /// knows how to recover.  Only rebuilds the overlay once (when
    /// `has_ever_shown_content` was previously set by normal playback).
    fn show_error_idle_overlay(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.presenter.is_showing_idle() {
            let (vw, vh) = self.presenter.viewport_size().unwrap_or((1280, 720));
            self.presenter.set_idle_overlay(vw, vh)?;
            self.present_needed = true;
        }
        Ok(())
    }

    fn surfaces_alive(&self) -> usize {
        // Count how many surface slots currently hold a texture.
        // This is a diagnostic-only method; the cost is acceptable
        // since it's only called from the tracing path.
        self.presenter.surfaces_alive()
    }

    fn is_current_frame(
        &self,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    ) -> bool {
        open_gen == self.generations.open()
            && seek_gen == self.generations.seek()
            && Some(op_id) == self.active_operation_id
    }
}

fn screenshot_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile).join("Pictures").join("FastPlay"));
    }

    Ok(std::env::current_dir()?.join("FastPlay Screenshots"))
}

fn encode_bgra_bmp(capture: &BgraFrameCapture) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let width = capture.width;
    let height = capture.height;
    let pixel_bytes = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(4))
        .ok_or("screenshot dimensions overflowed BMP size")?;
    if capture.pixels.len() != pixel_bytes as usize {
        return Err("screenshot pixel buffer size did not match dimensions".into());
    }

    let header_bytes = 14u32 + 40u32;
    let file_size = header_bytes
        .checked_add(pixel_bytes)
        .ok_or("screenshot BMP file size overflowed")?;
    let top_down_height = -(height as i32);

    let mut bytes = Vec::with_capacity(file_size as usize);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&header_bytes.to_le_bytes());

    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&(width as i32).to_le_bytes());
    bytes.extend_from_slice(&top_down_height.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&pixel_bytes.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&capture.pixels);
    Ok(bytes)
}

impl ModalTickTarget for PlaybackSession {
    fn modal_tick(&mut self) {
        let _ = self.tick(Instant::now());
    }

    fn modal_tick_window(&self) -> &NativeWindowInner {
        self.window.raw_window()
    }
}

/// The media-time origin (PTS of normalized position 0) to record when the
/// first frame of an open is observed. `first_pts` is that frame's PTS;
/// `start_position` is the normalized position the open began decoding at (the
/// resume offset, or zero for a normal open). Anchoring at `first_pts -
/// start_position` keeps a resumed file's timeline aligned: the resumed frame
/// reports `start_position`, not zero.
fn media_time_origin(first_pts: Duration, start_position: Duration) -> Duration {
    first_pts.saturating_sub(start_position)
}

#[cfg(test)]
mod tests {
    use super::{media_time_origin, EndedSignal};
    use std::time::Duration;

    #[test]
    fn ended_signal_fires_once_on_latch() {
        let mut signal = EndedSignal::default();
        assert!(!signal.take(), "no signal before any end");
        signal.latch();
        assert!(signal.take(), "fires once on the ended transition");
    }

    #[test]
    fn ended_signal_is_consumed_and_does_not_repeat() {
        let mut signal = EndedSignal::default();
        signal.latch();
        assert!(signal.take());
        // While still "ended" (no new latch), later polls return false so
        // auto-advance cannot fire every tick.
        assert!(!signal.take());
        assert!(!signal.take());
    }

    #[test]
    fn ended_signal_relatches_on_a_new_transition() {
        let mut signal = EndedSignal::default();
        signal.latch();
        assert!(signal.take());
        assert!(!signal.take());
        // A new real end-of-file transition latches again.
        signal.latch();
        assert!(signal.take());
    }

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    // Normalized position of `frame_pts` under an origin derived from a first
    // frame at `first_pts` for an open that started at `start_position`.
    fn normalized(first_pts: Duration, start_position: Duration, frame_pts: Duration) -> Duration {
        let origin = media_time_origin(first_pts, start_position);
        frame_pts.saturating_sub(origin)
    }

    #[test]
    fn normal_open_origin_is_file_start() {
        // Non-resume open: first frame ~file start (0), offset 0.
        assert_eq!(media_time_origin(s(0), s(0)), s(0));
        // A later frame reports its true position.
        assert_eq!(normalized(s(0), s(0), s(42)), s(42));
    }

    #[test]
    fn resume_open_reports_resume_position_not_zero() {
        // Resume to 60s on a file with start_time 0: first frame ~60s.
        // Bug was: origin = 60 -> resumed frame reported position 0.
        assert_eq!(media_time_origin(s(60), s(60)), s(0));
        assert_eq!(normalized(s(60), s(60), s(60)), s(60)); // resumed frame -> 60s
        assert_eq!(normalized(s(60), s(60), s(90)), s(90)); // later frame -> 90s, in range
    }

    #[test]
    fn resume_open_with_nonzero_container_start_time() {
        // Container start_time 10s, resume to 50s -> first frame ~60s abs.
        // Origin should be the file start (10s); positions are 0-based from there.
        assert_eq!(media_time_origin(s(60), s(50)), s(10));
        assert_eq!(normalized(s(60), s(50), s(60)), s(50)); // resumed frame -> 50s
    }

    #[test]
    fn keyframe_before_target_saturates_without_underflow() {
        // A backward seek may land a hair before the target; never underflow.
        assert_eq!(media_time_origin(s(58), s(60)), s(0));
    }
}
