use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use crate::{
    app::{
        commands::SessionCommand,
        drop_stats::{VideoDropBuckets, VideoDropCause},
        events::SessionEvent,
        overlay::OverlayManager,
        state::PlaybackState,
    },
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
        metrics::MetricsCollector,
        queues::QueueDefaults,
    },
    render::presenter::Presenter,
};

const VERY_LATE_VIDEO_THRESHOLD: Duration = Duration::from_millis(400);
const WORKER_CANCELLED: &str = "fastplay operation cancelled";
const VOLUME_OVERLAY_TIMEOUT: Duration = Duration::from_millis(900);

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
    active_worker_count: Arc<AtomicU32>,
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: MetricsCollector,
    video_clock: Option<PlaybackClock>,
    media_time_origin_pts: Option<Duration>,
    paused_clock_position: Option<Duration>,
    audio_clock_anchor_pts: Option<Duration>,
    audio_submitted_frames: u64,
    media_duration: Option<Duration>,
    pending_seek_target: Option<SeekTarget>,
    seek_discard_before_pts: Option<Duration>,
    seek_frame_presented_since_request: bool,
    audio_stream_expected: bool,
    overlay: OverlayManager,
    queued_video_frames: VecDeque<DecodedVideoFrame>,
    queued_audio_frames: VecDeque<QueuedAudioFrame>,
    queued_video_capacity: usize,
    queued_audio_capacity: usize,
    drop_buckets: VideoDropBuckets,
    video_stream_ended: bool,
    audio_stream_ended: bool,
    active_decode_mode: Option<VideoDecodeMode>,
    last_error: Option<String>,
    present_needed: bool,
    view_zoom: f32,
    view_pan_x: f32,
    view_pan_y: f32,
    view_rotation_quarter_turns: u8,
    stream_rotation_quarter_turns: u8,
    needs_initial_resize: bool,
    has_shown_content: bool,
    auto_replay: bool,
    pause_after_seek: bool,
    deferred_seek: Option<SeekTarget>,
    last_worker_spawned_at: Option<Instant>,
    playback_rate: f64,
    in_point: Option<Duration>,
    out_point: Option<Duration>,
    loop_range: bool,
    saved_volume: f32,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let saved_volume = super::settings::load_volume();
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
            active_worker_count: Arc::new(AtomicU32::new(0)),
            event_tx,
            event_rx,
            metrics: MetricsCollector::new(),
            video_clock: None,
            media_time_origin_pts: None,
            paused_clock_position: None,
            audio_clock_anchor_pts: None,
            audio_submitted_frames: 0,
            media_duration: None,
            pending_seek_target: None,
            seek_discard_before_pts: None,
            seek_frame_presented_since_request: false,
            audio_stream_expected: false,
            overlay: OverlayManager::new(),
            queued_video_frames: VecDeque::with_capacity(queue_defaults.decoded_video_frames),
            queued_audio_frames: VecDeque::with_capacity(queue_defaults.decoded_audio_frames),
            queued_video_capacity: queue_defaults.decoded_video_frames,
            queued_audio_capacity: queue_defaults.decoded_audio_frames,
            drop_buckets: VideoDropBuckets::default(),
            video_stream_ended: false,
            audio_stream_ended: false,
            active_decode_mode: None,
            last_error: None,
            present_needed: true,
            view_zoom: 1.0,
            view_pan_x: 0.0,
            view_pan_y: 0.0,
            view_rotation_quarter_turns: 0,
            stream_rotation_quarter_turns: 0,
            needs_initial_resize: false,
            has_shown_content: false,
            auto_replay: false,
            pause_after_seek: false,
            deferred_seek: None,
            last_worker_spawned_at: None,
            playback_rate: 1.0,
            in_point: None,
            out_point: None,
            loop_range: false,
            saved_volume,
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

    pub fn in_point(&self) -> Option<Duration> {
        self.in_point
    }

    pub fn out_point(&self) -> Option<Duration> {
        self.out_point
    }

    pub fn loop_range(&self) -> bool {
        self.loop_range
    }

    pub fn decode_preference(&self) -> VideoDecodePreference {
        self.decode_preference
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
        if self.overlay.volume_overlay_until.is_some_and(|until| now > until) {
            if self.presenter.set_volume_overlay(None, 0, 0)? {
                self.present_needed = true;
            }
            self.overlay.volume_overlay_until = None;
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
        self.overlay.subtitle_track = match SubtitleTrack::load_sidecar(&source) {
            Ok(track) => track,
            Err(error) => {
                eprintln!("subtitle load failed: {error}");
                None
            }
        };
        self.overlay.subtitles_enabled = self.overlay.subtitle_track.is_some();
        self.overlay.subtitle_clock_base = None;
        self.overlay.active_subtitle_cue = None;
        self.overlay.active_subtitle_viewport = None;
        self.playback_rate = 1.0;
        self.in_point = None;
        self.out_point = None;
        self.loop_range = false;
        let source = Arc::new(source);
        self.current_source = Some(Arc::clone(&source));
        self.media_duration = None;
        self.active_decode_mode = None;
        self.view_rotation_quarter_turns = 0;
        self.stream_rotation_quarter_turns = 0;
        if let Some(track) = self.overlay.subtitle_track.as_ref() {
            eprintln!(
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
        if self.decode_preference != VideoDecodePreference::ForceSoftware
            && self.active_decode_mode == Some(VideoDecodeMode::HardwareD3D11)
        {
            // This file is reproducibly crashing in d3d11.dll under timeline
            // stress-scrub while the hardware path is active. Switch the rest
            // of the session to software decode before spawning the scrub seek.
            self.decode_preference = VideoDecodePreference::ForceSoftware;
            eprintln!("[scrub_seek] forcing software decode for session");
            if self.overlay.show_decode_info {
                self.update_window_title();
            }
        }
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
            SessionCommand::SetInPoint => {
                self.in_point = Some(self.snapshot(now).position);
                // If the new in-point is at or past the out-point, clear the out-point.
                if let (Some(i), Some(o)) = (self.in_point, self.out_point) {
                    if i >= o {
                        self.out_point = None;
                    }
                }
            }
            SessionCommand::ClearInPoint => {
                self.in_point = None;
                // Clearing the in-point while loop_range is active with only an in-point
                // set would leave an invalid range — disable looping too.
                if self.out_point.is_none() {
                    self.loop_range = false;
                }
            }
            SessionCommand::SetOutPoint => {
                let pos = self.snapshot(now).position;
                // Out-point must be strictly after the in-point (or after 0 if none set).
                if pos > self.in_point.unwrap_or(Duration::ZERO) {
                    self.out_point = Some(pos);
                }
            }
            SessionCommand::ClearOutPoint => {
                self.out_point = None;
                // Clearing the out-point while loop_range is active with only an out-point
                // set would leave an invalid range — disable looping too.
                if self.in_point.is_none() {
                    self.loop_range = false;
                }
            }
            SessionCommand::ToggleLoopRange => {
                if self.in_point.is_some() || self.out_point.is_some() {
                    self.loop_range = !self.loop_range;
                } else {
                    self.auto_replay = !self.auto_replay;
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
                    self.audio_clock_anchor_pts = None;
                    self.audio_submitted_frames = 0;
                    self.queued_audio_frames.clear();
                    self.update_window_title();
                }
            }
            SessionCommand::PanBy { dx, dy } => {
                if self.view_zoom > 1.0 {
                    self.view_pan_x += dx;
                    self.view_pan_y += dy;
                    self.clamp_pan();
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
        sink.adjust_volume_steps(steps);
        self.saved_volume = sink.volume();
        super::settings::save_volume(self.saved_volume);
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
        self.overlay.volume_overlay_until = Some(Instant::now() + VOLUME_OVERLAY_TIMEOUT);
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

            // Execute any deferred seek once the throttle interval has
            // elapsed, old worker events have been drained above, and the
            // number of live worker threads is below the concurrency cap.
            if self.deferred_seek.is_some()
                && !self.last_worker_spawned_at.is_some_and(|t| {
                    now.duration_since(t) < Self::SEEK_WORKER_MIN_INTERVAL
                })
                && self.active_worker_count.load(Ordering::Acquire) < 2
            {
                if let Some(target) = self.deferred_seek.take() {
                    if let Some(source) = self.current_source.clone() {
                        self.execute_seek(source, target, now)?;
                    }
                }
            }

            self.submit_due_audio(now)?;
            if self.advance_video_playback(now)? {
                return Ok(());
            }
            self.update_subtitle_overlay(now)?;
            self.refresh_volume_overlay(now)?;
        }

        if self.present_needed {
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

        if self.metrics.pending_first_frame && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_first_frame_presented(now) {
                eprintln!("open_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_first_frame = false;
        }

        if self.metrics.pending_first_audio {
            if let Some(elapsed) = self.metrics.note_first_audio_started(now) {
                eprintln!("open_to_first_audio_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_first_audio = false;
            self.metrics.disable_open_audio_metric();
        }

        if self.metrics.pending_seek_first_frame && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_seek_first_frame_presented(now) {
                eprintln!("seek_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_seek_first_frame = false;
        }

        if self.metrics.pending_seek_settled && self.seek_is_settled() {
            if let Some(elapsed) = self.metrics.note_seek_av_settled(now) {
                eprintln!("seek_to_av_settled_ms={}", elapsed.as_millis());
            }
            self.metrics.pending_seek_settled = false;
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
            if self.auto_replay || self.loop_range {
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
                // Apply stream rotation on the initial decode mode event.
                // On mid-stream HW→SW fallback the rotation is the same stream
                // so this is idempotent.
                self.stream_rotation_quarter_turns = rotation_quarter_turns;
                self.view_rotation_quarter_turns = rotation_quarter_turns;
                if self.overlay.show_decode_info {
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
                    eprintln!(
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
                    let ffmpeg::PendingVideoFrame::D3D11 { width, height, .. } = &frame;
                    self.window.resize_for_content(*width, *height, center);
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
                eprintln!("[video_stream_ended] seek={} op={:?}", seek_gen.0, op_id);
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
                eprintln!(
                    "playback failed: {error_msg}"
                );
            }
            SessionEvent::DeviceLost {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }
                eprintln!("[DEVICE_LOST] seek={} op={:?} workers={}", seek_gen.0, op_id, self.active_worker_count.load(Ordering::Acquire));
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

    /// Minimum interval between spawning decoder worker threads.  Rapid
    /// scrubbing can request hundreds of seeks per second; each one opens the
    /// file and allocates a hardware decoder on the GPU.  Most GPUs only
    /// support 8-16 concurrent decode sessions, so spawning without throttling
    /// can exhaust GPU resources and crash the driver.
    const SEEK_WORKER_MIN_INTERVAL: Duration = Duration::from_millis(30);

    fn seek(&mut self, target: SeekTarget, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if self.current_source.is_none() {
            return Ok(());
        }
        // Don't seek during states where it makes no sense or would break
        // the open/recovery sequence.
        if matches!(self.state, PlaybackState::Idle | PlaybackState::Opening | PlaybackState::Error) {
            eprintln!("[seek] rejected: state={:?}", self.state);
            return Ok(());
        }

        let clamped_position = match self.media_duration {
            Some(dur) if target.position() > dur => dur,
            _ => target.position(),
        };
        let target = SeekTarget::new(clamped_position);

        // Defer if a worker was spawned very recently or if too many worker
        // threads are still alive.  Each worker holds a hardware decoder on
        // the GPU; allowing more than 2 concurrent workers risks exhausting
        // the GPU's session limit (typically 8-16), especially when multiple
        // FastPlay instances are running.
        let workers_alive = self.active_worker_count.load(Ordering::Acquire);
        let throttled = self.last_worker_spawned_at.is_some_and(|t| now.duration_since(t) < Self::SEEK_WORKER_MIN_INTERVAL);
        let should_defer = throttled || workers_alive >= 2;
        if should_defer {
            if self.decode_preference != VideoDecodePreference::ForceSoftware
                && self.active_decode_mode == Some(VideoDecodeMode::HardwareD3D11)
            {
                // Repeated scrub seeks are still provoking d3d11.dll crashes
                // on some files in the hardware path. Keep the rest of the
                // session on software decode once seek churn is detected.
                self.decode_preference = VideoDecodePreference::ForceSoftware;
                eprintln!(
                    "[seek] forcing software decode for session after hardware seek churn"
                );
                if self.overlay.show_decode_info {
                    self.update_window_title();
                }
            }
            eprintln!(
                "[seek] DEFERRED pos={:.3}s workers={} throttled={}",
                clamped_position.as_secs_f64(),
                workers_alive,
                throttled
            );
            self.deferred_seek = Some(target);
            // Update the visible seek state so the timeline preview stays
            // responsive even while the worker spawn is deferred.
            self.pending_seek_target = Some(target);
            return Ok(());
        }

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
        let workers_before = self.active_worker_count.load(Ordering::Acquire);
        eprintln!(
            "[execute_seek] pos={:.3}s abs={:.3}s open={} seek={} op={:?} workers={} surfaces={} vq={} aq={} device_removed={}",
            target.position().as_secs_f64(),
            absolute_target.as_secs_f64(),
            open_gen.0,
            seek_gen.0,
            op_id,
            workers_before,
            self.surfaces_alive(),
            self.queued_video_frames.len(),
            self.queued_audio_frames.len(),
            self.presenter.device().is_device_removed()
        );
        // Keep the previously selected surface visible until the new frame
        // arrives — `validate_and_select_surface` will swap it atomically
        // when the first frame of the new seek generation is presented.
        // The hardware-churn motivation for dropping it has moved to the
        // software-decode fallback above.
        self.prepare_runtime_for_operation_inner(false, false, false)?;
        self.state = PlaybackState::Seeking;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.deferred_seek = None;
        self.spawn_stream_worker(source, Some(absolute_target), open_gen, seek_gen, op_id);
        self.last_worker_spawned_at = Some(now);
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
        self.prepare_runtime_for_operation(rebuild_audio_sink, reset_audio_expectation)?;
        self.state = next_state;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.deferred_seek = None;
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
        eprintln!("[prepare_runtime] cancelled worker, nonce now={}", self.worker_nonce.load(Ordering::Acquire));
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
        self.audio_clock_anchor_pts = None;
        self.audio_submitted_frames = 0;
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
                    sink.set_volume(self.saved_volume);
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
        let worker_count = self.active_worker_count.clone();
        worker_count.fetch_add(1, Ordering::Release);

        eprintln!(
            "[spawn_worker] nonce={} workers_now={}",
            expected_nonce,
            worker_count.load(Ordering::Acquire)
        );

        thread::spawn(move || {
            // Ensure the counter is decremented when this thread exits,
            // regardless of the exit path (success, cancel, or error).
            struct WorkerGuard(Arc<AtomicU32>, u64);
            impl Drop for WorkerGuard {
                fn drop(&mut self) {
                    let remaining = self.0.fetch_sub(1, Ordering::Release).saturating_sub(1);
                    eprintln!("[worker_exit] nonce={} workers_remaining={}", self.1, remaining);
                }
            }
            let _guard = WorkerGuard(worker_count, expected_nonce);

            // Non-blocking send with cancellation check.  When the bounded
            // channel is full the worker yields and re-checks the nonce.
            // This prevents stale workers from blocking indefinitely on a
            // full channel, holding GPU decoder sessions open and causing
            // TDR crashes during rapid seeking.
            let worker_send = {
                let full_count = std::sync::atomic::AtomicU32::new(0);
                move |event: SessionEvent,
                      nonce: &AtomicU64,
                      expected: u64,
                      tx: &SyncSender<SessionEvent>| -> Result<(), String> {
                    let mut event = event;
                    loop {
                        match tx.try_send(event) {
                            Ok(()) => {
                                let prev = full_count.swap(0, Ordering::Relaxed);
                                if prev > 0 {
                                    eprintln!("[worker_send] nonce={} unblocked after {} full retries", expected, prev);
                                }
                                return Ok(());
                            }
                            Err(TrySendError::Full(returned)) => {
                                if nonce.load(Ordering::Acquire) != expected {
                                    eprintln!("[worker_send] nonce={} CANCELLED while channel full", expected);
                                    return Err(WORKER_CANCELLED.to_string());
                                }
                                full_count.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(std::time::Duration::from_millis(1));
                                event = returned;
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                return Err(WORKER_CANCELLED.to_string());
                            }
                        }
                    }
                }
            };

            let decode_result = ffmpeg::stream_media(
                &*source,
                &device,
                audio_format,
                start_position,
                decode_preference,
                open_gen,
                seek_gen,
                op_id,
                |mode, hw_fallback_count, rotation_quarter_turns| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    worker_send(
                        SessionEvent::DecodeModeSelected {
                            open_gen,
                            seek_gen,
                            op_id,
                            mode,
                            hw_fallback_count,
                            rotation_quarter_turns,
                        },
                        &worker_nonce,
                        expected_nonce,
                        &sender,
                    )
                },
                |duration| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    worker_send(
                        SessionEvent::MediaDurationKnown {
                            open_gen,
                            seek_gen,
                            op_id,
                            duration,
                        },
                        &worker_nonce,
                        expected_nonce,
                        &sender,
                    )
                },
                || worker_nonce.load(Ordering::Acquire) != expected_nonce,
                |frame| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    worker_send(
                        SessionEvent::VideoFrameReady(frame),
                        &worker_nonce,
                        expected_nonce,
                        &sender,
                    )
                },
                |frame| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    worker_send(
                        SessionEvent::AudioFrameReady(frame),
                        &worker_nonce,
                        expected_nonce,
                        &sender,
                    )
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
                    // Device-removed errors from the worker thread should
                    // trigger the normal device recovery path rather than
                    // entering the terminal Error state.
                    let event = if error.contains("device removed")
                        || device.is_device_removed()
                    {
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
                    let _ = sender.send(event);
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
        if self.state == PlaybackState::Paused || self.pause_after_seek || self.playback_rate != 1.0 {
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
        if self.audio_stream_expected
            && self.state != PlaybackState::Paused
            && self.audio_sink.as_ref().map_or(false, |s| s.is_started())
            && self.queued_audio_frames.is_empty()
            && buffered == 0
            && !self.can_finish_with_buffered(0)?
        {
            self.metrics.note_audio_underrun();
            // Re-anchor the audio clock so the next submission re-establishes
            // A/V sync instead of drifting permanently after the gap.
            self.audio_clock_anchor_pts = None;
            self.audio_submitted_frames = 0;
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

    fn advance_video_playback(&mut self, now: Instant) -> Result<bool, Box<dyn std::error::Error>> {
        if self.state == PlaybackState::Paused {
            return Ok(false);
        }

        // Compute the master clock once — it is constant within this call since `now`
        // is fixed and audio state does not change mid-function.
        let audio_clock = if self.audio_clock_anchor_pts.is_some() {
            let Some(clock) = self.master_clock_position(now) else {
                return Ok(false);
            };
            Some(clock)
        } else {
            None
        };

        loop {
            let Some(next_frame) = self.queued_video_frames.front() else {
                return Ok(false);
            };

            if let Some(audio_clock) = audio_clock {
                let next_frame_time = self.media_time_for_pts(next_frame.pts());
                if next_frame_time > audio_clock {
                    return Ok(false);
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
                    return Ok(false);
                }
            }

            let frame = self
                .queued_video_frames
                .pop_front()
                .expect("front frame existed");

            // Check out-point by frame PTS before presenting, so we stop exactly
            // on the right frame rather than relying on the lagging audio clock.
            if let Some(out_pt) = self.out_point {
                if matches!(
                    self.state,
                    PlaybackState::Playing | PlaybackState::Priming | PlaybackState::Draining
                ) {
                    let frame_pos = self.media_time_for_pts(frame.pts());
                    if frame_pos >= out_pt {
                        self.presenter.release_surface(frame.surface());
                        if self.loop_range {
                            let target = self.in_point.unwrap_or(Duration::ZERO);
                            self.seek(SeekTarget::new(target), now)?;
                        } else {
                            self.cancel_active_worker();
                            // Clear op_id so residual events in the channel
                            // don't pass is_current_frame and restart playback.
                            self.active_operation_id = None;
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
        if self.video_clock.is_none() && self.audio_clock_anchor_pts.is_none() {
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

        self.present_needed = true;
        self.metrics.note_video_frame_presented();
        if let Some(elapsed) = self.metrics.note_resume_first_frame(now) {
            eprintln!("play_to_motion_ms={}", elapsed.as_millis());
        }
        self.metrics.pending_first_frame |= self.metrics.presented_video_frames() == 1;
        self.seek_frame_presented_since_request |= self.metrics.pending_seek_settled;
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
        // After releasing surfaces their D3D11 texture memory may be
        // returned to the allocator and reused for new textures at the
        // same address.  The VideoProcessorCache keeps a raw-pointer
        // identity cache of input views keyed by texture address — if
        // we don't flush it here, a new texture at the same old address
        // matches a stale cached view, and VideoProcessorBlt reads
        // through a dangling COM pointer → ACCESS_VIOLATION at 0x...00D8.
        self.presenter.flush_video_processor_input_cache();
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
            if let Some(dropped) = self.queued_video_frames.pop_front() {
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
                    self.video_clock = Some(PlaybackClock::new(now, resume_pts, self.playback_rate));
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
        self.seek(SeekTarget::new(self.in_point.unwrap_or(Duration::ZERO)), now)
    }


    fn toggle_subtitles(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.overlay.subtitle_track.is_none() {
            eprintln!("subtitle toggle ignored: no external .srt sidecar was loaded");
            return Ok(());
        }

        self.overlay.subtitles_enabled = !self.overlay.subtitles_enabled;
        self.overlay.subtitle_clock_base = None;
        self.overlay.active_subtitle_cue = None;
        self.overlay.active_subtitle_viewport = None;
        if !self.overlay.subtitles_enabled {
            self.presenter.clear_subtitle_overlay();
        }
        eprintln!("subtitles_enabled={}", self.overlay.subtitles_enabled);
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
            self.clamp_pan();
        }

        self.present_needed = true;
    }

    /// Clamp pan so that at least 25% of the content remains visible on each
    /// axis. Without this the user can drag the video entirely off-screen.
    fn clamp_pan(&mut self) {
        let (vw, vh) = self.presenter.viewport_size().unwrap_or((1, 1));
        let max_pan_x = vw as f32 * self.view_zoom * 0.75;
        let max_pan_y = vh as f32 * self.view_zoom * 0.75;
        self.view_pan_x = self.view_pan_x.clamp(-max_pan_x, max_pan_x);
        self.view_pan_y = self.view_pan_y.clamp(-max_pan_y, max_pan_y);
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
        self.view_rotation_quarter_turns = self.stream_rotation_quarter_turns;
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
        self.audio_clock_anchor_pts = None;
        self.audio_submitted_frames = 0;
        self.queued_audio_frames.clear();
        self.update_window_title();
    }

    fn update_window_title(&self) {
        let base = self
            .current_source
            .as_ref()
            .and_then(|s| s.path().file_name())
            .and_then(|n| n.to_str())
            .map(|n| format!("{n} - FastPlay"))
            .unwrap_or_else(|| "FastPlay".to_owned());

        let mut suffixes: Vec<String> = Vec::new();
        if (self.playback_rate - 1.0).abs() >= 0.01 {
            let rate_str = if self.playback_rate.fract() == 0.0 {
                format!("{}x", self.playback_rate as u32)
            } else {
                format!("{:.2}", self.playback_rate).trim_end_matches('0').trim_end_matches('.').to_owned() + "x"
            };
            suffixes.push(rate_str);
        }
        if self.overlay.show_decode_info {
            if let Some(mode) = self.active_decode_mode {
                suffixes.push(mode.label().to_owned());
            }
        }
        let title = if suffixes.is_empty() {
            base
        } else {
            format!("{base} [{}]", suffixes.join("  "))
        };

        self.window.set_title(&title);
    }

    fn update_subtitle_overlay(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let subtitle_position = self.subtitle_position(now);
        let Some(track) = self.overlay.subtitle_track.as_ref() else {
            self.overlay.active_subtitle_cue = None;
            self.overlay.active_subtitle_viewport = None;
            self.presenter.clear_subtitle_overlay();
            return Ok(());
        };
        if !self.overlay.subtitles_enabled {
            self.overlay.active_subtitle_cue = None;
            self.overlay.active_subtitle_viewport = None;
            self.presenter.clear_subtitle_overlay();
            return Ok(());
        }

        let viewport = self.presenter.viewport_size()?;
        if viewport.0 == 0 || viewport.1 == 0 {
            return Ok(());
        }

        let cue = track.cue_at(subtitle_position, self.overlay.active_subtitle_cue);
        let next_index = cue.map(|(index, _)| index);
        if self.overlay.active_subtitle_cue == next_index && self.overlay.active_subtitle_viewport == Some(viewport)
        {
            return Ok(());
        }

        self.present_needed = true;
        match cue {
            Some((index, cue)) => {
                self.presenter
                    .set_subtitle_overlay(Some(&cue.text), viewport.0, viewport.1)?;
                self.overlay.active_subtitle_cue = Some(index);
                self.overlay.active_subtitle_viewport = Some(viewport);
                eprintln!(
                    "subtitle_cue index={} start_ms={} end_ms={}",
                    index,
                    cue.start.as_millis(),
                    cue.end.as_millis()
                );
            }
            None => {
                if self.overlay.active_subtitle_cue.take().is_some() {
                    eprintln!("subtitle_cue cleared");
                }
                self.overlay.active_subtitle_viewport = Some(viewport);
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
            (self.audio_clock_anchor_pts, self.audio_sink.as_ref())
        {
            if sink.is_started() {
                let buffered_frames = sink.buffered_frames().unwrap_or(0) as u64;
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
