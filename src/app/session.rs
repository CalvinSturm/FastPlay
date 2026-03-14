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
    app::{
        commands::SessionCommand,
        events::SessionEvent,
        state::PlaybackState,
    },
    audio::sink::AudioSink,
    ffi::{dxgi::ResizeRequest, ffmpeg::{self, StreamStatus}},
    media::{
        audio::{AudioStreamFormat, DecodedAudioFrame},
        seek::{PlaybackSnapshot, PositionKind, SeekTarget},
        source::MediaSource,
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
    current_source: Option<MediaSource>,
    decode_preference: VideoDecodePreference,
    worker_nonce: Arc<AtomicU64>,
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: PlaybackMetrics,
    video_clock: Option<PlaybackClock>,
    paused_clock_position: Option<Duration>,
    audio_clock_anchor_pts: Option<Duration>,
    audio_submitted_frames: u64,
    pending_seek_target: Option<SeekTarget>,
    pending_seek_first_frame_metric: bool,
    pending_seek_settled_metric: bool,
    seek_frame_presented_since_request: bool,
    audio_stream_expected: bool,
    queued_video_frames: VecDeque<DecodedVideoFrame>,
    queued_audio_frames: VecDeque<QueuedAudioFrame>,
    queued_video_capacity: usize,
    queued_audio_capacity: usize,
    measure_open_audio_metric: bool,
    pending_first_frame_metric: bool,
    pending_first_audio_metric: bool,
    video_stream_ended: bool,
    audio_stream_ended: bool,
    active_decode_mode: Option<VideoDecodeMode>,
    last_error: Option<String>,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let queue_defaults = QueueDefaults::default();
        let event_capacity = queue_defaults.decoded_video_frames + queue_defaults.decoded_audio_frames + 4;
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
            paused_clock_position: None,
            audio_clock_anchor_pts: None,
            audio_submitted_frames: 0,
            pending_seek_target: None,
            pending_seek_first_frame_metric: false,
            pending_seek_settled_metric: false,
            seek_frame_presented_since_request: false,
            audio_stream_expected: false,
            queued_video_frames: VecDeque::with_capacity(queue_defaults.decoded_video_frames),
            queued_audio_frames: VecDeque::with_capacity(queue_defaults.decoded_audio_frames),
            queued_video_capacity: queue_defaults.decoded_video_frames,
            queued_audio_capacity: queue_defaults.decoded_audio_frames,
            measure_open_audio_metric: false,
            pending_first_frame_metric: false,
            pending_first_audio_metric: false,
            video_stream_ended: false,
            audio_stream_ended: false,
            active_decode_mode: None,
            last_error: None,
        })
    }

    pub fn window(&self) -> &NativeWindow {
        &self.window
    }

    pub fn window_mut(&mut self) -> &mut NativeWindow {
        &mut self.window
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

    pub fn open(
        &mut self,
        source: MediaSource,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let open_gen = self.generations.bump_open();
        let seek_gen = self.generations.seek();
        let op_id = self.operation_clock.next();
        self.decode_preference = source.decode_preference();
        self.current_source = Some(source.clone());
        self.active_decode_mode = None;
        self.metrics.note_open_requested(now);
        self.measure_open_audio_metric = true;
        self.begin_operation(source, None, open_gen, seek_gen, op_id, PlaybackState::Opening, true, true)?;
        Ok(())
    }

    pub fn apply_command(
        &mut self,
        command: SessionCommand,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match command {
            SessionCommand::Tick => {}
            SessionCommand::TogglePause => self.toggle_pause(now)?,
            SessionCommand::Seek(target) => self.seek(target, now)?,
        }
        Ok(())
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
        self.submit_due_audio(now)?;

        loop {
            match self.event_rx.try_recv() {
                Ok(event) => self.handle_event(event, now)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }

            if self.queued_video_frames.len() >= self.queued_video_capacity
                || self.queued_audio_frames.len() >= self.queued_audio_capacity
            {
                break;
            }
        }

        if let Some(size) = self.window.take_resize_request() {
            self.handle_resize(size, now)?;
        }

        self.submit_due_audio(now)?;
        self.advance_video_playback(now);

        match self.presenter.render() {
            Ok(()) => self.metrics.note_present(now),
            Err(error) => {
                self.recover_device(now, format!("present failed: {error}"))?;
                return Ok(());
            }
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
            self.state = PlaybackState::Ended;
            self.metrics.note_ended(now);
            eprintln!(
                "playback_summary decode_mode={} hw_fallback_count={} presented_frames={} dropped_video_frames={} audio_underruns={}",
                self.metrics
                    .decode_mode()
                    .map(VideoDecodeMode::label)
                    .unwrap_or("unknown"),
                self.metrics.hw_fallback_count(),
                self.metrics.presented_video_frames(),
                self.metrics.dropped_video_frames(),
                self.metrics.audio_underruns()
            );
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
                eprintln!(
                    "decode_mode={} hw_fallback_count={}",
                    mode.label(),
                    self.metrics.hw_fallback_count()
                );
            }
            SessionEvent::VideoFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen(), frame.seek_gen(), frame.op_id()) {
                    return Ok(());
                }

                match frame {
                    ffmpeg::PendingVideoFrame::D3D11 {
                        open_gen,
                        seek_gen,
                        op_id,
                        pts,
                        width,
                        height,
                        surface,
                    } => {
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
                    }
                    ffmpeg::PendingVideoFrame::Software {
                        open_gen,
                        seek_gen,
                        op_id,
                        pts,
                        width,
                        height,
                        format,
                        planes,
                        strides,
                    } => {
                        let software_frame = DecodedVideoFrame::Software {
                            open_gen,
                            seek_gen,
                            op_id,
                            pts,
                            width,
                            height,
                            format,
                            planes,
                            strides,
                        };
                        let handle = self.presenter.upload_software_frame(&software_frame)?;
                        self.push_video_frame(DecodedVideoFrame::D3D11 {
                            open_gen,
                            seek_gen,
                            op_id,
                            pts,
                            width,
                            height,
                            surface: handle,
                        });
                    }
                }
                if matches!(self.state, PlaybackState::Opening | PlaybackState::Seeking) {
                    self.state = PlaybackState::Priming;
                }
            }
            SessionEvent::AudioFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen, frame.seek_gen, frame.op_id) {
                    return Ok(());
                }

                self.audio_stream_expected = true;
                if self.audio_sink.is_none() {
                    let message = self
                        .audio_sink_error
                        .clone()
                        .unwrap_or_else(|| "WASAPI sink was not available for decoded audio".to_string());
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

    fn seek(
        &mut self,
        target: SeekTarget,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(source) = self.current_source.clone() else {
            return Ok(());
        };

        let open_gen = self.generations.open();
        let seek_gen = self.generations.bump_seek();
        let op_id = self.operation_clock.next();
        self.metrics.note_seek_requested(now);
        self.measure_open_audio_metric = false;
        self.begin_operation(
            source,
            Some(target.position()),
            open_gen,
            seek_gen,
            op_id,
            PlaybackState::Seeking,
            false,
            false,
        )?;
        self.pending_seek_target = Some(target);
        self.pending_seek_first_frame_metric = true;
        self.pending_seek_settled_metric = true;
        self.seek_frame_presented_since_request = false;
        Ok(())
    }

    fn begin_operation(
        &mut self,
        source: MediaSource,
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
        self.cancel_active_worker();
        self.clear_video_queue();
        self.queued_audio_frames.clear();
        self.presenter.reset_surfaces();
        self.video_clock = None;
        self.paused_clock_position = None;
        self.audio_clock_anchor_pts = None;
        self.audio_submitted_frames = 0;
        self.pending_first_frame_metric = false;
        self.pending_first_audio_metric = false;
        self.video_stream_ended = false;
        self.audio_stream_ended = false;
        self.active_decode_mode = None;
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
        source: MediaSource,
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
                &source,
                &device,
                audio_format,
                start_position,
                decode_preference,
                open_gen,
                seek_gen,
                op_id,
                |mode, hw_fallback_count| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
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
                || worker_nonce.load(Ordering::Acquire) != expected_nonce,
                |frame| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
                        return Err(WORKER_CANCELLED.to_string());
                    }
                    sender
                        .send(SessionEvent::VideoFrameReady(frame))
                        .map_err(|_| WORKER_CANCELLED.to_string())
                },
                |frame| {
                    if worker_nonce.load(Ordering::Acquire) != expected_nonce {
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
                if let Some(elapsed) = self.metrics.note_resize_recovered(now) {
                    eprintln!("resize_recover_ms={}", elapsed.as_millis());
                }
                Ok(())
            }
            Err(error) => {
                if self.presenter.rebuild_swap_chain(&self.window).is_ok() {
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
        if self.state == PlaybackState::Paused {
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
                self.audio_submitted_frames = self.audio_submitted_frames.saturating_add(written as u64);
                front.submitted_frames = front.submitted_frames.saturating_add(written);
                if front.submitted_frames >= front.frame.frame_count() {
                    self.queued_audio_frames.pop_front();
                }
            }

            if wrote_any_audio && self.audio_clock_anchor_pts.is_none() {
                self.audio_clock_anchor_pts = first_written_pts;
                if self.measure_open_audio_metric {
                    self.pending_first_audio_metric = true;
                }
            }

            if wrote_any_audio && !sink.is_started() {
                if let Err(error) = sink.resume() {
                    audio_error = Some(error.to_string());
                }
            }
        }

        if let Some(error) = audio_error {
            self.recover_audio_endpoint(now, error)?;
            return Ok(());
        }

        let Some(sink) = self.audio_sink.as_ref() else {
            return Ok(());
        };
        if self.audio_stream_expected
            && self.state != PlaybackState::Paused
            && sink.is_started()
            && self.queued_audio_frames.is_empty()
            && sink.buffered_frames()? == 0
            && !self.can_finish_playback()?
        {
            self.metrics.note_audio_underrun();
        }

        if self.audio_stream_expected && self.audio_clock_anchor_pts.is_some() && self.state == PlaybackState::Priming {
            self.state = PlaybackState::Playing;
        }

        Ok(())
    }

    fn advance_video_playback(&mut self, now: Instant) {
        if self.state == PlaybackState::Paused {
            return;
        }

        loop {
            let Some(next_frame) = self.queued_video_frames.front() else {
                return;
            };

            if self.audio_clock_anchor_pts.is_some() {
                let Some(audio_clock) = self.master_clock_position(now) else {
                    return;
                };
                if next_frame.pts() > audio_clock {
                    return;
                }

                let lateness = audio_clock.saturating_sub(next_frame.pts());
                if lateness > VERY_LATE_VIDEO_THRESHOLD && self.queued_video_frames.len() > 1 {
                    let dropped = self
                        .queued_video_frames
                        .pop_front()
                        .expect("front frame existed");
                    self.drop_video_frame(dropped);
                    continue;
                }
            } else if let Some(clock) = self.video_clock {
                let due_at = clock.deadline_for(next_frame.pts());
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
            self.video_clock = Some(PlaybackClock::new(now, frame.pts()));
        }

        if !self
            .presenter
            .surface_matches(frame.surface(), frame.open_gen(), frame.seek_gen())
        {
            self.drop_video_frame(frame);
            return;
        }

        let handle = frame.surface();
        if let Some(previous) = self.presenter.select_surface(handle) {
            if previous != handle {
                self.presenter.release_surface(previous);
            }
        }

        self.metrics.note_video_frame_presented();
        self.pending_first_frame_metric |= self.metrics.presented_video_frames() == 1;
        self.seek_frame_presented_since_request |= self.pending_seek_settled_metric;
        if !self.audio_stream_expected || self.audio_clock_anchor_pts.is_some() {
            self.state = PlaybackState::Playing;
        }
    }

    fn drop_video_frame(&mut self, frame: DecodedVideoFrame) {
        self.presenter.release_surface(frame.surface());
        self.metrics.note_video_frame_dropped();
    }

    fn clear_video_queue(&mut self) {
        while let Some(frame) = self.queued_video_frames.pop_front() {
            self.presenter.release_surface(frame.surface());
        }
    }

    fn push_video_frame(&mut self, frame: DecodedVideoFrame) {
        let insert_at = self
            .queued_video_frames
            .iter()
            .position(|queued| frame.pts() < queued.pts())
            .unwrap_or(self.queued_video_frames.len());
        self.queued_video_frames.insert(insert_at, frame);

        while self.queued_video_frames.len() > self.queued_video_capacity {
            if let Some(dropped) = self.queued_video_frames.pop_back() {
                self.drop_video_frame(dropped);
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
                self.paused_clock_position = self.master_clock_position(now);
                if let Some(sink) = self.audio_sink.as_mut() {
                    if sink.is_started() {
                        sink.pause()?;
                    }
                }
                self.state = PlaybackState::Paused;
            }
            PlaybackState::Paused => {
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
            _ => {}
        }

        Ok(())
    }

    fn master_clock_position(&self, now: Instant) -> Option<Duration> {
        if let Some(paused) = self.paused_clock_position {
            return Some(paused);
        }

        if let (Some(anchor_pts), Some(sink)) = (self.audio_clock_anchor_pts, self.audio_sink.as_ref()) {
            if sink.is_started() {
                let buffered_frames = sink.buffered_frames().ok()? as u64;
                let played_frames = self.audio_submitted_frames.saturating_sub(buffered_frames);
                let sample_rate = sink.format().sample_rate;
                if sample_rate > 0 {
                    let played = Duration::from_secs_f64(played_frames as f64 / sample_rate as f64);
                    return Some(anchor_pts.saturating_add(played));
                }
            }
        }

        self.video_clock.map(|clock| clock.position_at(now))
    }

    fn seek_is_settled(&self) -> bool {
        self.seek_frame_presented_since_request
            && (!self.audio_stream_expected || self.audio_clock_anchor_pts.is_some())
    }

    fn desired_restart_position(&self, now: Instant) -> Duration {
        self.pending_seek_target
            .map(SeekTarget::position)
            .unwrap_or_else(|| self.snapshot(now).position)
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
        self.presenter.rebuild_device(&self.window)?;
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
        if let Some(elapsed) = self.metrics.note_device_recovered(now) {
            eprintln!("device_recovery_ms={}", elapsed.as_millis());
        }
        Ok(())
    }

    fn can_finish_playback(&self) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.video_stream_ended || !self.queued_video_frames.is_empty() {
            return Ok(false);
        }

        if self.audio_stream_expected {
            if !self.audio_stream_ended || !self.queued_audio_frames.is_empty() {
                return Ok(false);
            }
            if let Some(sink) = self.audio_sink.as_ref() {
                if sink.buffered_frames()? != 0 {
                    return Ok(false);
                }
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
