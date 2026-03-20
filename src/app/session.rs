use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc,
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use crate::{
    app::{commands::SessionCommand, events::SessionEvent, state::PlaybackState},
    audio::sink::AudioSink,
    ffi::{
        dxgi::ResizeRequest,
        ffmpeg::{self, StreamStatus},
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
        clock::PlaybackClock,
        generations::{
            GenerationState, OpenGeneration, OperationClock, OperationId, SeekGeneration,
        },
        metrics::PlaybackMetrics,
        queues::QueueDefaults,
    },
    render::presenter::Presenter,
};

const VERY_LATE_VIDEO_THRESHOLD: Duration = Duration::from_millis(400);
const WORKER_CANCELLED: &str = "fastplay operation cancelled";
const VOLUME_OVERLAY_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug)]
enum VideoDropCause {
    QueueOverflow,
    SurfaceMismatch,
    SchedulerLate,
}

#[derive(Clone, Copy, Debug, Default)]
struct VideoDropBuckets {
    queue_overflow: u64,
    surface_mismatch: u64,
    scheduler_late: u64,
}

impl VideoDropBuckets {
    fn note(&mut self, cause: VideoDropCause) {
        match cause {
            VideoDropCause::QueueOverflow => {
                self.queue_overflow = self.queue_overflow.saturating_add(1);
            }
            VideoDropCause::SurfaceMismatch => {
                self.surface_mismatch = self.surface_mismatch.saturating_add(1);
            }
            VideoDropCause::SchedulerLate => {
                self.scheduler_late = self.scheduler_late.saturating_add(1);
            }
        }
    }
}

struct QueuedAudioFrame {
    frame: DecodedAudioFrame,
    submitted_frames: u32,
}

pub struct PlaybackSession {
    ui_thread_id: ThreadId,
    tick_active: Cell<bool>,
    window: NativeWindow,
    presenter: Presenter,
    audio_sink: Option<AudioSink>,
    audio_sink_error: Option<String>,
    state: PlaybackState,
    generations: GenerationState,
    operation_clock: OperationClock,
    active_operation_id: Option<OperationId>,
    current_source: Option<Arc<MediaSource>>,
    decode_preference: VideoDecodePreference,
    worker_nonce: Arc<AtomicU64>,
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: PlaybackMetrics,
    video_clock: Option<PlaybackClock>,
    media_time_origin_pts: Option<Duration>,
    paused_clock_position: Option<Duration>,
    audio_clock_anchor_pts: Option<Duration>,
    audio_submitted_frames: u64,
    media_duration: Option<Duration>,
    pending_seek_target: Option<SeekTarget>,
    seek_discard_before_pts: Option<Duration>,
    pending_seek_first_frame_metric: bool,
    pending_seek_settled_metric: bool,
    seek_frame_presented_since_request: bool,
    audio_stream_expected: bool,
    subtitle_track: Option<SubtitleTrack>,
    subtitles_enabled: bool,
    subtitle_clock_base: Option<Duration>,
    active_subtitle_cue: Option<usize>,
    active_subtitle_viewport: Option<(u32, u32)>,
    queued_video_frames: VecDeque<DecodedVideoFrame>,
    queued_audio_frames: VecDeque<QueuedAudioFrame>,
    queued_video_capacity: usize,
    queued_audio_capacity: usize,
    drop_buckets: VideoDropBuckets,
    measure_open_audio_metric: bool,
    pending_first_frame_metric: bool,
    pending_first_audio_metric: bool,
    video_stream_ended: bool,
    audio_stream_ended: bool,
    active_decode_mode: Option<VideoDecodeMode>,
    last_error: Option<String>,
    present_needed: bool,
    volume_overlay_until: Option<Instant>,
    view_zoom: f32,
    view_pan_x: f32,
    view_pan_y: f32,
    view_rotation_quarter_turns: u8,
    needs_initial_resize: bool,
    has_shown_content: bool,
    auto_replay: bool,
    replay_indicator_until: Option<Instant>,
    pause_after_seek: bool,
    show_decode_info: bool,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let queue_defaults = QueueDefaults::default();
        let event_capacity =
            queue_defaults.decoded_video_frames + queue_defaults.decoded_audio_frames + 4;
        let (event_tx, event_rx) = mpsc::sync_channel(event_capacity);

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
            worker_nonce: Arc::new(AtomicU64::new(0)),
            event_tx,
            event_rx,
            metrics: PlaybackMetrics::default(),
            video_clock: None,
            media_time_origin_pts: None,
            paused_clock_position: None,
            audio_clock_anchor_pts: None,
            audio_submitted_frames: 0,
            media_duration: None,
            pending_seek_target: None,
            seek_discard_before_pts: None,
            pending_seek_first_frame_metric: false,
            pending_seek_settled_metric: false,
            seek_frame_presented_since_request: false,
            audio_stream_expected: false,
            subtitle_track: None,
            subtitles_enabled: true,
            subtitle_clock_base: None,
            active_subtitle_cue: None,
            active_subtitle_viewport: None,
            queued_video_frames: VecDeque::with_capacity(queue_defaults.decoded_video_frames),
            queued_audio_frames: VecDeque::with_capacity(queue_defaults.decoded_audio_frames),
            queued_video_capacity: queue_defaults.decoded_video_frames,
            queued_audio_capacity: queue_defaults.decoded_audio_frames,
            drop_buckets: VideoDropBuckets::default(),
            measure_open_audio_metric: false,
            pending_first_frame_metric: false,
            pending_first_audio_metric: false,
            video_stream_ended: false,
            audio_stream_ended: false,
            active_decode_mode: None,
            last_error: None,
            present_needed: true,
            volume_overlay_until: None,
            view_zoom: 1.0,
            view_pan_x: 0.0,
            view_pan_y: 0.0,
            view_rotation_quarter_turns: 0,
            needs_initial_resize: false,
            has_shown_content: false,
            auto_replay: false,
            replay_indicator_until: None,
            pause_after_seek: false,
            show_decode_info: false,
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

    pub fn auto_replay(&self) -> bool {
        self.auto_replay
    }

    pub fn decode_preference(&self) -> VideoDecodePreference {
        self.decode_preference
    }

    pub fn replay_indicator_until(&self) -> Option<Instant> {
        self.replay_indicator_until
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
        if self.volume_overlay_until.is_some_and(|until| now > until) {
            if self.presenter.set_volume_overlay(None, 0, 0)? {
                self.present_needed = true;
            }
            self.volume_overlay_until = None;
        }
        Ok(())
    }

    pub fn open(
        &mut self,
        source: MediaSource,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let open_gen = self.generations.bump_open();
        let seek_gen = self.generations.seek();
        let op_id = self.operation_clock.next();
        self.decode_preference = source.decode_preference();
        self.subtitle_track = match SubtitleTrack::load_sidecar(&source) {
            Ok(track) => track,
            Err(error) => {
                eprintln!("subtitle load failed: {error}");
                None
            }
        };
        self.subtitles_enabled = self.subtitle_track.is_some();
        self.subtitle_clock_base = None;
        self.active_subtitle_cue = None;
        self.active_subtitle_viewport = None;
        let source = Arc::new(source);
        self.current_source = Some(Arc::clone(&source));
        self.media_duration = None;
        self.active_decode_mode = None;
        if let Some(track) = self.subtitle_track.as_ref() {
            eprintln!(
                "subtitle_track_loaded path={} cues={}",
                track.path().display(),
                track.len()
            );
        }
        self.needs_initial_resize = true;
        self.metrics.note_open_requested(now);
        self.measure_open_audio_metric = true;
        self.begin_operation(
            source,
            None,
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
            SessionCommand::Seek(target) => self.seek(target, now)?,
            SessionCommand::AdjustVolumeSteps(steps) => self.adjust_volume_steps(steps),
            SessionCommand::RotateClockwise => self.rotate_view(1),
            SessionCommand::RotateCounterClockwise => self.rotate_view(3),
            SessionCommand::ToggleBorderlessFullscreen => {
                self.metrics.note_fullscreen_toggle_started(now);
                self.window.toggle_borderless_fullscreen();
                if let Some(elapsed) = self.metrics.note_fullscreen_toggle_completed(Instant::now()) {
                    eprintln!("fullscreen_toggle_ms={}", elapsed.as_millis());
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
            SessionCommand::ToggleAutoReplay => {
                self.auto_replay = !self.auto_replay;
                self.replay_indicator_until = Some(now + Duration::from_millis(1500));
            }
            SessionCommand::FitWindow => {
                self.fit_window();
            }
            SessionCommand::HalfSizeWindow => {
                self.half_size_window();
            }
            SessionCommand::ToggleDecodeInfo => {
                self.show_decode_info = !self.show_decode_info;
                self.update_window_title();
            }
        }
        Ok(())
    }

    fn adjust_volume_steps(&mut self, steps: i16) {
        let Some(sink) = self.audio_sink.as_mut() else {
            return;
        };
        sink.adjust_volume_steps(steps);
        let volume_percent = sink.volume_percent();
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
        self.volume_overlay_until = Some(Instant::now() + VOLUME_OVERLAY_TIMEOUT);
        eprintln!("volume={volume_percent}");
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

    fn tick_inner(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if self.state == PlaybackState::Error {
            thread::sleep(Duration::from_millis(1));
            return Ok(());
        }

        loop {
            if self.queued_video_frames.len() >= self.queued_video_capacity
                || self.queued_audio_frames.len() >= self.queued_audio_capacity
            {
                break;
            }

            match self.event_rx.try_recv() {
                Ok(event) => self.handle_event(event, now)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        if let Some(size) = self.window.take_resize_request() {
            self.handle_resize(size, now)?;
        }

        self.submit_due_audio(now)?;
        self.advance_video_playback(now);
        self.update_subtitle_overlay(now)?;
        self.refresh_volume_overlay(now)?;

        if self.present_needed && self.state != PlaybackState::Error {
            let view = crate::render::ViewTransform {
                zoom: self.view_zoom,
                pan_x: self.view_pan_x,
                pan_y: self.view_pan_y,
                rotation_quarter_turns: self.view_rotation_quarter_turns,
            };
            match self.presenter.render(&view) {
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
            thread::sleep(Duration::from_millis(1));
        }

        if self.pending_first_frame_metric && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_first_frame_presented(now) {
                eprintln!("open_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.pending_first_frame_metric = false;
        }

        if self.pending_first_audio_metric {
            if let Some(elapsed) = self.metrics.note_first_audio_started(now) {
                eprintln!("open_to_first_audio_ms={}", elapsed.as_millis());
            }
            self.pending_first_audio_metric = false;
            self.measure_open_audio_metric = false;
        }

        if self.pending_seek_first_frame_metric && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_seek_first_frame_presented(now) {
                eprintln!("seek_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.pending_seek_first_frame_metric = false;
        }

        if self.pending_seek_settled_metric && self.seek_is_settled() {
            if let Some(elapsed) = self.metrics.note_seek_av_settled(now) {
                eprintln!("seek_to_av_settled_ms={}", elapsed.as_millis());
            }
            self.pending_seek_settled_metric = false;
            self.pending_seek_target = None;
        }

        if self.can_finish_playback()? {
            self.metrics.note_ended(now);
            eprintln!(
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
            if self.auto_replay {
                self.replay(now)?;
            } else {
                self.state = PlaybackState::Ended;
            }
        }

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
                if self.show_decode_info {
                    self.update_window_title();
                }
                eprintln!(
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
                    let ffmpeg::PendingVideoFrame::D3D11 { width, height, .. } = &frame;
                    self.window.resize_for_content(*width, *height, center);
                }

                let ffmpeg::PendingVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts,
                    width,
                    height,
                    surface,
                } = frame;
                self.observe_media_time_origin(pts);
                let handle = self.presenter.register_surface(open_gen, seek_gen, surface);
                self.push_video_frame(DecodedVideoFrame::D3D11 {
                    open_gen,
                    seek_gen,
                    op_id,
                    pts,
                    width,
                    height,
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
                self.fail_playback(error);
            }
            SessionEvent::DeviceLost {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                self.recover_device(now, "device lost event".to_string())?;
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
        let Some(source) = self.current_source.clone() else {
            return Ok(());
        };

        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        self.metrics.note_seek_requested(now);
        self.measure_open_audio_metric = false;
        let absolute_target = self.absolute_media_position(target.position());
        // Keep the last presented surface alive during seeks so the user sees
        // the previous frame until the new one arrives, avoiding a grey flash.
        self.prepare_runtime_for_operation_inner(false, false, false)?;
        self.state = PlaybackState::Seeking;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.spawn_stream_worker(source, Some(absolute_target), open_gen, seek_gen, op_id);
        self.pending_seek_target = Some(target);
        self.seek_discard_before_pts = Some(absolute_target);
        self.subtitle_clock_base = Some(target.position());
        self.pending_seek_first_frame_metric = true;
        self.pending_seek_settled_metric = true;
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
        self.prepare_runtime_for_operation(rebuild_audio_sink, reset_audio_expectation)?;
        self.state = next_state;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.spawn_stream_worker(source, start_position, open_gen, seek_gen, op_id);
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
        self.cancel_active_worker();
        self.clear_video_queue();
        self.queued_audio_frames.clear();
        if reset_surfaces {
            self.presenter.reset_surfaces();
        }
        self.presenter.clear_subtitle_overlay();
        self.presenter.set_timeline_overlay(None)?;
        self.presenter.set_volume_overlay(None, 0, 0)?;
        self.video_clock = None;
        if reset_audio_expectation {
            self.media_time_origin_pts = None;
            self.seek_discard_before_pts = None;
        }
        self.paused_clock_position = None;
        self.audio_clock_anchor_pts = None;
        self.audio_submitted_frames = 0;
        self.drop_buckets = VideoDropBuckets::default();
        self.pending_first_frame_metric = false;
        self.pending_first_audio_metric = false;
        self.video_stream_ended = false;
        self.audio_stream_ended = false;
        self.active_decode_mode = None;
        self.volume_overlay_until = None;
        self.subtitle_clock_base = None;
        self.active_subtitle_cue = None;
        self.active_subtitle_viewport = None;
        if reset_audio_expectation {
            self.audio_stream_expected = false;
        }

        if rebuild_audio_sink {
            self.audio_sink = match AudioSink::create_shared_default() {
                Ok(sink) => {
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

    fn spawn_stream_worker(
        &mut self,
        source: Arc<MediaSource>,
        start_position: Option<Duration>,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        op_id: OperationId,
    ) {
        let sender = self.event_tx.clone();
        let device = self.presenter.device().clone();
        let audio_format = self
            .audio_sink
            .as_ref()
            .map(|sink| sink.format())
            .unwrap_or_else(AudioStreamFormat::stereo_f32_48khz);
        let decode_preference = self.decode_preference;
        let worker_nonce = self.worker_nonce.clone();
        let expected_nonce = self.reserve_worker_nonce();

        thread::spawn(move || {
            let decode_result = ffmpeg::stream_media(
                &*source,
                &device,
                audio_format,
                start_position,
                decode_preference,
                open_gen,
                seek_gen,
                op_id,
                |mode, hw_fallback_count| {
                    if worker_nonce.load(Ordering::Relaxed) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    sender
                        .send(SessionEvent::DecodeModeSelected {
                            open_gen,
                            seek_gen,
                            op_id,
                            mode,
                            hw_fallback_count,
                        })
                        .map_err(|_| WORKER_CANCELLED.to_string())
                },
                |duration| {
                    if worker_nonce.load(Ordering::Relaxed) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    sender
                        .send(SessionEvent::MediaDurationKnown {
                            open_gen,
                            seek_gen,
                            op_id,
                            duration,
                        })
                        .map_err(|_| WORKER_CANCELLED.to_string())
                },
                || worker_nonce.load(Ordering::Relaxed) != expected_nonce,
                |frame| {
                    if worker_nonce.load(Ordering::Relaxed) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    sender
                        .send(SessionEvent::VideoFrameReady(frame))
                        .map_err(|_| WORKER_CANCELLED.to_string())
                },
                |frame| {
                    if worker_nonce.load(Ordering::Relaxed) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    sender
                        .send(SessionEvent::AudioFrameReady(frame))
                        .map_err(|_| WORKER_CANCELLED.to_string())
                },
            );

            match decode_result {
                Ok(StreamStatus::Completed(summary)) => {
                    let _ = sender.send(SessionEvent::VideoStreamEnded {
                        open_gen,
                        seek_gen,
                        op_id,
                    });
                    if summary.had_audio_stream {
                        let _ = sender.send(SessionEvent::AudioStreamEnded {
                            open_gen,
                            seek_gen,
                            op_id,
                        });
                    }
                }
                Ok(StreamStatus::Cancelled) => {}
                Err(error) if error == WORKER_CANCELLED => {}
                Err(error) => {
                    let _ = sender.send(if start_position.is_some() {
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
                    });
                }
            }
        });
    }

    fn reserve_worker_nonce(&self) -> u64 {
        self.worker_nonce
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    fn cancel_active_worker(&self) {
        let _ = self.worker_nonce.fetch_add(1, Ordering::AcqRel);
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
                    eprintln!("resize_recover_ms={}", elapsed.as_millis());
                }
                Ok(())
            }
            Err(error) => {
                if self.presenter.rebuild_swap_chain(&self.window).is_ok() {
                    self.present_needed = true;
                    if let Some(elapsed) = self.metrics.note_resize_recovered(now) {
                        eprintln!("resize_recover_ms={}", elapsed.as_millis());
                    }
                    return Ok(());
                }
                eprintln!("resize recovery deferred: {error}");
                let _ = self.metrics.note_resize_recovered(now);
                Ok(())
            }
        }
    }

    fn submit_due_audio(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if self.state == PlaybackState::Paused || self.pause_after_seek {
            return Ok(());
        }

        let mut first_written_pts = None;
        let mut wrote_any_audio = false;
        let mut audio_error = None;

        {
            let Some(sink) = self.audio_sink.as_mut() else {
                return Ok(());
            };

            loop {
                let Some(front) = self.queued_audio_frames.front_mut() else {
                    break;
                };

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

                if first_written_pts.is_none() {
                    let offset = Duration::from_secs_f64(
                        front.submitted_frames as f64 / front.frame.format.sample_rate as f64,
                    );
                    first_written_pts = Some(front.frame.pts().saturating_add(offset));
                }

                wrote_any_audio = true;
                self.audio_submitted_frames =
                    self.audio_submitted_frames.saturating_add(written as u64);
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

            if wrote_any_audio && self.audio_clock_anchor_pts.is_none() && sink.is_started() {
                self.audio_clock_anchor_pts = first_written_pts;
                self.video_clock = None;
                if self.measure_open_audio_metric {
                    self.pending_first_audio_metric = true;
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
        if self.audio_stream_expected
            && self.state != PlaybackState::Paused
            && self.audio_sink.as_ref().map_or(false, |s| s.is_started())
            && self.queued_audio_frames.is_empty()
            && buffered == 0
            && !self.can_finish_with_buffered(0)?
        {
            self.metrics.note_audio_underrun();
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
            && self.audio_clock_anchor_pts.is_some()
            && self.state == PlaybackState::Priming
        {
            self.state = PlaybackState::Playing;
        }

        Ok(())
    }

    fn advance_video_playback(&mut self, now: Instant) {
        if self.state == PlaybackState::Paused {
            return;
        }

        // Compute the master clock once — it is constant within this call since `now`
        // is fixed and audio state does not change mid-function.
        let audio_clock = if self.audio_clock_anchor_pts.is_some() {
            let Some(clock) = self.master_clock_position(now) else {
                return;
            };
            Some(clock)
        } else {
            None
        };

        loop {
            let Some(next_frame) = self.queued_video_frames.front() else {
                return;
            };

            if let Some(audio_clock) = audio_clock {
                let next_frame_time = self.media_time_for_pts(next_frame.pts());
                if next_frame_time > audio_clock {
                    return;
                }

                let lateness = audio_clock.saturating_sub(next_frame_time);
                if lateness > VERY_LATE_VIDEO_THRESHOLD && self.queued_video_frames.len() > 1 {
                    let dropped = self
                        .queued_video_frames
                        .pop_front()
                        .expect("front frame existed");
                    self.drop_video_frame(dropped, VideoDropCause::SchedulerLate);
                    continue;
                }

                while self.queued_video_frames.len() > 1 {
                    let Some(upcoming_frame) = self.queued_video_frames.get(1) else {
                        break;
                    };
                    let upcoming_time = self.media_time_for_pts(upcoming_frame.pts());
                    if upcoming_time > audio_clock {
                        break;
                    }

                    let dropped = self
                        .queued_video_frames
                        .pop_front()
                        .expect("front frame existed");
                    self.drop_video_frame(dropped, VideoDropCause::SchedulerLate);
                }
            } else if let Some(clock) = self.video_clock {
                let due_at = clock.deadline_for(self.media_time_for_pts(next_frame.pts()));
                if now < due_at {
                    return;
                }
            }

            let frame = self
                .queued_video_frames
                .pop_front()
                .expect("front frame existed");
            self.present_video_frame(frame, now);
            return;
        }
    }

    fn present_video_frame(&mut self, frame: DecodedVideoFrame, now: Instant) {
        if self.video_clock.is_none() && self.audio_clock_anchor_pts.is_none() {
            self.video_clock = Some(PlaybackClock::new(
                now,
                self.media_time_for_pts(frame.pts()),
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

        self.present_needed = true;
        self.metrics.note_video_frame_presented();
        if let Some(elapsed) = self.metrics.note_resume_first_frame(now) {
            eprintln!("play_to_motion_ms={}", elapsed.as_millis());
        }
        self.pending_first_frame_metric |= self.metrics.presented_video_frames() == 1;
        self.seek_frame_presented_since_request |= self.pending_seek_settled_metric;
        // Check pause_after_seek unconditionally — audio submission is already
        // blocked by the same flag so we must not wait for the audio anchor.
        if self.pause_after_seek {
            self.pause_after_seek = false;
            self.paused_clock_position = self.pending_seek_target.map(|t| t.position());
            self.state = PlaybackState::Paused;
        } else if !self.audio_stream_expected || self.audio_clock_anchor_pts.is_some() {
            self.state = PlaybackState::Playing;
        }
    }

    fn drop_video_frame(&mut self, frame: DecodedVideoFrame, cause: VideoDropCause) {
        self.presenter.release_surface(frame.surface());
        self.drop_buckets.note(cause);
        self.metrics.note_video_frame_dropped();
    }

    fn clear_video_queue(&mut self) {
        while let Some(frame) = self.queued_video_frames.pop_front() {
            self.presenter.release_surface(frame.surface());
        }
    }

    fn push_video_frame(&mut self, frame: DecodedVideoFrame) {
        let insert_at = if self.queued_video_frames.back().map_or(true, |last| frame.pts() >= last.pts()) {
            self.queued_video_frames.len()
        } else {
            self.queued_video_frames
                .binary_search_by(|queued| queued.pts().cmp(&frame.pts()))
                .unwrap_or_else(|pos| pos)
        };
        self.queued_video_frames.insert(insert_at, frame);

        while self.queued_video_frames.len() > self.queued_video_capacity {
            if let Some(dropped) = self.queued_video_frames.pop_back() {
                self.drop_video_frame(dropped, VideoDropCause::QueueOverflow);
            }
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
                    eprintln!("pause_to_stop_ms={}", elapsed.as_millis());
                }
            }
            PlaybackState::Paused => {
                // If both streams ended while paused, treat as replay.
                if self.video_stream_ended
                    && self.queued_video_frames.is_empty()
                    && (!self.audio_stream_expected
                        || (self.audio_stream_ended && self.queued_audio_frames.is_empty()))
                {
                    self.replay(now)?;
                    return Ok(());
                }
                self.metrics.note_resume_requested(now);
                if self.audio_clock_anchor_pts.is_none() {
                    let resume_pts = self.paused_clock_position.unwrap_or(Duration::ZERO);
                    self.video_clock = Some(PlaybackClock::new(now, resume_pts));
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

    fn replay(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        self.seek(SeekTarget::new(Duration::ZERO), now)
    }

    fn toggle_subtitles(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.subtitle_track.is_none() {
            eprintln!("subtitle toggle ignored: no external .srt sidecar was loaded");
            return Ok(());
        }

        self.subtitles_enabled = !self.subtitles_enabled;
        self.subtitle_clock_base = None;
        self.active_subtitle_cue = None;
        self.active_subtitle_viewport = None;
        if !self.subtitles_enabled {
            self.presenter.clear_subtitle_overlay();
        }
        eprintln!("subtitles_enabled={}", self.subtitles_enabled);
        Ok(())
    }

    fn zoom_at_cursor(&mut self, delta: i16, cursor_x: i32, cursor_y: i32) {
        let factor = if delta > 0 { 1.125f32 } else { 1.0 / 1.125 };
        let new_zoom = (self.view_zoom * factor).clamp(1.0, 8.0);

        if (new_zoom - self.view_zoom).abs() < f32::EPSILON {
            return;
        }

        // Compute the viewport size for cursor-centered zoom.
        let (vw, vh) = self.presenter.viewport_size().unwrap_or((1, 1));
        let cx = vw as f32 * 0.5;
        let cy = vh as f32 * 0.5;

        // Pixel under cursor in content space: content_pt = (cursor - center - pan) / zoom
        // New pan keeps that content point under the cursor.
        let dx = cursor_x as f32 - cx;
        let dy = cursor_y as f32 - cy;
        let content_x = (dx - self.view_pan_x) / self.view_zoom;
        let content_y = (dy - self.view_pan_y) / self.view_zoom;
        let new_pan_x = dx - content_x * new_zoom;
        let new_pan_y = dy - content_y * new_zoom;

        self.view_zoom = new_zoom;

        // Clamp at zoom == 1.0: no pan drift.
        if new_zoom <= 1.0 {
            self.view_pan_x = 0.0;
            self.view_pan_y = 0.0;
        } else {
            self.view_pan_x = new_pan_x;
            self.view_pan_y = new_pan_y;
        }

        self.present_needed = true;
    }

    fn rotate_view(&mut self, delta_quarter_turns: u8) {
        self.view_rotation_quarter_turns = self
            .view_rotation_quarter_turns
            .wrapping_add(delta_quarter_turns)
            % 4;
        self.present_needed = true;
    }

    fn reset_view(&mut self) {
        self.view_zoom = 1.0;
        self.view_pan_x = 0.0;
        self.view_pan_y = 0.0;
        self.view_rotation_quarter_turns = 0;
        self.present_needed = true;
    }

    fn fit_window(&mut self) {
        let Some((mut w, mut h)) = self.presenter.current_surface_size() else {
            return;
        };
        // Account for rotation: odd quarter-turns swap width and height.
        if self.view_rotation_quarter_turns % 2 != 0 {
            std::mem::swap(&mut w, &mut h);
        }
        self.window.fit_window_to_content(w, h);
    }

    fn half_size_window(&mut self) {
        let Some((mut w, mut h)) = self.presenter.current_surface_size() else {
            return;
        };
        if self.view_rotation_quarter_turns % 2 != 0 {
            std::mem::swap(&mut w, &mut h);
        }
        self.window.set_window_client_size((w / 2).max(1), (h / 2).max(1));
    }

    fn update_window_title(&self) {
        let base = self
            .current_source
            .as_ref()
            .and_then(|s| s.path().file_name())
            .and_then(|n| n.to_str())
            .map(|n| format!("{n} - FastPlay"))
            .unwrap_or_else(|| "FastPlay".to_owned());

        let title = if self.show_decode_info {
            match self.active_decode_mode {
                Some(mode) => format!("{base} [{}]", mode.label()),
                None => base,
            }
        } else {
            base
        };

        self.window.set_title(&title);
    }

    fn update_subtitle_overlay(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let subtitle_position = self.subtitle_position(now);
        let Some(track) = self.subtitle_track.as_ref() else {
            self.active_subtitle_cue = None;
            self.active_subtitle_viewport = None;
            self.presenter.clear_subtitle_overlay();
            return Ok(());
        };
        if !self.subtitles_enabled {
            self.active_subtitle_cue = None;
            self.active_subtitle_viewport = None;
            self.presenter.clear_subtitle_overlay();
            return Ok(());
        }

        let viewport = self.presenter.viewport_size()?;
        if viewport.0 == 0 || viewport.1 == 0 {
            return Ok(());
        }

        let cue = track.cue_at(subtitle_position, self.active_subtitle_cue);
        let next_index = cue.map(|(index, _)| index);
        if self.active_subtitle_cue == next_index && self.active_subtitle_viewport == Some(viewport)
        {
            return Ok(());
        }

        self.present_needed = true;
        match cue {
            Some((index, cue)) => {
                self.presenter
                    .set_subtitle_overlay(Some(&cue.text), viewport.0, viewport.1)?;
                self.active_subtitle_cue = Some(index);
                self.active_subtitle_viewport = Some(viewport);
                eprintln!(
                    "subtitle_cue index={} start_ms={} end_ms={}",
                    index,
                    cue.start.as_millis(),
                    cue.end.as_millis()
                );
            }
            None => {
                if self.active_subtitle_cue.take().is_some() {
                    eprintln!("subtitle_cue cleared");
                }
                self.active_subtitle_viewport = Some(viewport);
                self.presenter.clear_subtitle_overlay();
            }
        }

        Ok(())
    }

    fn subtitle_position(&mut self, now: Instant) -> Duration {
        if let Some(target) = self.pending_seek_target {
            return target.position();
        }

        let master = self.master_clock_position(now).unwrap_or(Duration::ZERO);
        if let Some(base) = self.subtitle_clock_base {
            if master.saturating_add(Duration::from_secs(1)) < base {
                return base.saturating_add(master);
            }
            self.subtitle_clock_base = None;
        }

        master
    }

    fn master_clock_position(&self, now: Instant) -> Option<Duration> {
        if let Some(paused) = self.paused_clock_position {
            return Some(paused);
        }

        if let (Some(anchor_pts), Some(sink)) =
            (self.audio_clock_anchor_pts, self.audio_sink.as_ref())
        {
            if sink.is_started() {
                let buffered_frames = sink.buffered_frames().ok()? as u64;
                let played_frames = self.audio_submitted_frames.saturating_sub(buffered_frames);
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
            self.media_time_origin_pts = Some(pts);
        }
    }

    fn media_time_for_pts(&self, pts: Duration) -> Duration {
        pts.saturating_sub(self.media_time_origin_pts.unwrap_or(pts))
    }

    fn seek_is_settled(&self) -> bool {
        self.seek_frame_presented_since_request
            && (!self.audio_stream_expected || self.audio_clock_anchor_pts.is_some())
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
        eprintln!("audio endpoint recovery: {reason}");
        self.measure_open_audio_metric = false;
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
        self.subtitle_clock_base = Some(restart_target);
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
        eprintln!("device recovery: {reason}");
        if let Err(error) = self.presenter.rebuild_device(&self.window) {
            eprintln!("device recovery failed: {error}");
            self.fail_playback(format!("device recovery failed: {error}"));
            return Ok(());
        }
        let restart_target = self.desired_restart_position(now);
        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        self.measure_open_audio_metric = false;
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
        self.subtitle_clock_base = Some(restart_target);
        if let Some(elapsed) = self.metrics.note_device_recovered(now) {
            eprintln!("device_recovery_ms={}", elapsed.as_millis());
        }
        Ok(())
    }

    fn can_finish_playback(&self) -> Result<bool, Box<dyn std::error::Error>> {
        let buffered = self
            .audio_sink
            .as_ref()
            .map(|s| s.buffered_frames().unwrap_or(0))
            .unwrap_or(0);
        self.can_finish_with_buffered(buffered)
    }

    fn can_finish_with_buffered(&self, buffered: u32) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.video_stream_ended || !self.queued_video_frames.is_empty() {
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
        self.cancel_active_worker();
        self.pending_seek_target = None;
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
        self.active_decode_mode = None;
        self.state = PlaybackState::Error;
        eprintln!("open failed: {error}");
    }

    fn fail_playback(&mut self, error: String) {
        self.cancel_active_worker();
        self.pending_seek_target = None;
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
        self.active_decode_mode = None;
        self.state = PlaybackState::Error;
        eprintln!("playback failed: {error}");
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
