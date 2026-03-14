use std::{
    cell::Cell,
    collections::VecDeque,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
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
    ffi::ffmpeg,
    media::{
        audio::{AudioStreamFormat, DecodedAudioFrame},
        source::MediaSource,
        video::DecodedVideoFrame,
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

const SLIGHTLY_LATE_VIDEO_THRESHOLD: Duration = Duration::from_millis(30);
const VERY_LATE_VIDEO_THRESHOLD: Duration = Duration::from_millis(400);

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
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: PlaybackMetrics,
    video_clock: Option<PlaybackClock>,
    paused_clock_position: Option<Duration>,
    audio_clock_anchor_pts: Option<Duration>,
    audio_submitted_frames: u64,
    queued_video_frames: VecDeque<DecodedVideoFrame>,
    queued_audio_frames: VecDeque<QueuedAudioFrame>,
    queued_video_capacity: usize,
    queued_audio_capacity: usize,
    pending_first_frame_metric: bool,
    pending_first_audio_metric: bool,
    video_stream_ended: bool,
    audio_stream_ended: bool,
    audio_stream_seen: bool,
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
            event_tx,
            event_rx,
            metrics: PlaybackMetrics::default(),
            video_clock: None,
            paused_clock_position: None,
            audio_clock_anchor_pts: None,
            audio_submitted_frames: 0,
            queued_video_frames: VecDeque::with_capacity(queue_defaults.decoded_video_frames),
            queued_audio_frames: VecDeque::with_capacity(queue_defaults.decoded_audio_frames),
            queued_video_capacity: queue_defaults.decoded_video_frames,
            queued_audio_capacity: queue_defaults.decoded_audio_frames,
            pending_first_frame_metric: false,
            pending_first_audio_metric: false,
            video_stream_ended: false,
            audio_stream_ended: false,
            audio_stream_seen: false,
            last_error: None,
        })
    }

    pub fn window(&self) -> &NativeWindow {
        &self.window
    }

    pub fn window_mut(&mut self) -> &mut NativeWindow {
        &mut self.window
    }

    pub fn open(
        &mut self,
        source: MediaSource,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let open_gen = self.generations.bump_open();
        let seek_gen = self.generations.seek();
        let op_id = self.operation_clock.next();
        let sender = self.event_tx.clone();
        let device = self.presenter.device().clone();

        self.presenter.reset_surfaces();
        self.audio_sink = match AudioSink::create_shared_default() {
            Ok(sink) => Some(sink),
            Err(error) => {
                self.audio_sink_error = Some(error.to_string());
                None
            }
        };
        if self.audio_sink.is_some() {
            self.audio_sink_error = None;
        }
        let audio_format = self
            .audio_sink
            .as_ref()
            .map(|sink| sink.format())
            .unwrap_or_else(AudioStreamFormat::stereo_f32_48khz);

        self.queued_video_frames.clear();
        self.queued_audio_frames.clear();
        self.video_clock = None;
        self.paused_clock_position = None;
        self.audio_clock_anchor_pts = None;
        self.audio_submitted_frames = 0;
        self.pending_first_frame_metric = false;
        self.pending_first_audio_metric = false;
        self.video_stream_ended = false;
        self.audio_stream_ended = false;
        self.audio_stream_seen = false;
        self.state = PlaybackState::Opening;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.metrics.note_open_requested(now);

        thread::spawn(move || {
            let mut produced_video = false;
            let mut produced_audio = false;
            let decode_result = ffmpeg::stream_media(
                &source,
                &device,
                audio_format,
                open_gen,
                seek_gen,
                op_id,
                |frame| {
                    produced_video = true;
                    sender
                        .send(SessionEvent::VideoFrameReady(frame))
                        .map_err(|_| "session event channel closed".to_string())
                },
                |frame| {
                    produced_audio = true;
                    sender
                        .send(SessionEvent::AudioFrameReady(frame))
                        .map_err(|_| "session event channel closed".to_string())
                },
            );

            match decode_result {
                Ok(summary) => {
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
                Err(error) if !produced_video && !produced_audio => {
                    let _ = sender.send(SessionEvent::OpenFailed {
                        open_gen,
                        op_id,
                        error,
                    });
                }
                Err(error) => {
                    let _ = sender.send(SessionEvent::PlaybackFailed {
                        open_gen,
                        seek_gen,
                        op_id,
                        error,
                    });
                }
            }
        });

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
        }
        Ok(())
    }

    /// UI-thread-only coordinator entrypoint.
    ///
    /// Safety and ownership contract:
    /// - must only run on the thread that owns the window, presenter, and WASAPI sink
    /// - must not be entered recursively
    /// - must stay non-blocking and avoid worker waits or disk I/O
    /// - M3 drains media completions, submits due audio, and schedules video
    ///   against the audio master clock when audio exists
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
                Ok(event) => {
                    self.handle_event(event)?;
                    if self.queued_video_frames.len() >= self.queued_video_capacity
                        || self.queued_audio_frames.len() >= self.queued_audio_capacity
                    {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        if let Some(size) = self.window.take_resize_request() {
            self.presenter.resize(size.width, size.height)?;
            self.metrics.note_resize(now);
        }

        self.submit_due_audio(now)?;
        self.advance_video_playback(now);

        self.presenter.render()?;
        self.metrics.note_present(now);

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
        }

        if self.can_finish_playback()? {
            self.state = PlaybackState::Ended;
            self.metrics.note_ended(now);
            eprintln!(
                "playback_summary presented_frames={} dropped_video_frames={} audio_underruns={}",
                self.metrics.presented_video_frames(),
                self.metrics.dropped_video_frames(),
                self.metrics.audio_underruns()
            );
        }

        Ok(())
    }

    fn handle_event(&mut self, event: SessionEvent) -> Result<(), Box<dyn std::error::Error>> {
        match event {
            SessionEvent::VideoFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen, frame.seek_gen, frame.op_id) {
                    return Ok(());
                }

                let handle = self
                    .presenter
                    .register_surface(frame.open_gen, frame.seek_gen, frame.surface);
                let decoded = DecodedVideoFrame::D3D11 {
                    open_gen: frame.open_gen,
                    seek_gen: frame.seek_gen,
                    op_id: frame.op_id,
                    pts: frame.pts,
                    width: frame.width,
                    height: frame.height,
                    surface: handle,
                };
                self.push_video_frame(decoded);
                if self.state == PlaybackState::Opening {
                    self.state = PlaybackState::Priming;
                }
            }
            SessionEvent::AudioFrameReady(frame) => {
                if !self.is_current_frame(frame.open_gen, frame.seek_gen, frame.op_id) {
                    return Ok(());
                }

                self.audio_stream_seen = true;
                if self.audio_sink.is_none() {
                    let message = self
                        .audio_sink_error
                        .clone()
                        .unwrap_or_else(|| "WASAPI sink was not available for decoded audio".to_string());
                    self.fail_playback(message);
                    return Ok(());
                }

                let decoded = DecodedAudioFrame {
                    open_gen: frame.open_gen,
                    seek_gen: frame.seek_gen,
                    op_id: frame.op_id,
                    pts: frame.pts,
                    format: frame.format,
                    frame_count: frame.frame_count,
                    data: frame.data,
                };
                self.push_audio_frame(decoded);
                if self.state == PlaybackState::Opening {
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
            SessionEvent::DeviceLost { .. } | SessionEvent::AudioEndpointChanged { .. } => {}
        }

        Ok(())
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

    fn submit_due_audio(&mut self, now: Instant) -> Result<(), Box<dyn std::error::Error>> {
        if self.state == PlaybackState::Paused {
            return Ok(());
        }

        let Some(sink) = self.audio_sink.as_mut() else {
            return Ok(());
        };

        let mut wrote_any_audio = false;
        let mut first_written_pts = None;

        loop {
            let Some(front) = self.queued_audio_frames.front_mut() else {
                break;
            };

            let written = sink.write_frame(&front.frame, front.submitted_frames)?;
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
            self.pending_first_audio_metric = true;
        }

        if wrote_any_audio && !sink.is_started() {
            sink.resume()?;
        }

        if self.audio_stream_seen
            && self.state != PlaybackState::Paused
            && sink.is_started()
            && self.queued_audio_frames.is_empty()
            && sink.buffered_frames()? == 0
            && !self.can_finish_playback()?
        {
            self.metrics.note_audio_underrun();
        }

        if self.audio_stream_seen && self.audio_clock_anchor_pts.is_some() && self.state == PlaybackState::Priming {
            self.state = PlaybackState::Playing;
        }

        let _ = now;
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

                if lateness > SLIGHTLY_LATE_VIDEO_THRESHOLD && self.metrics.audio_underruns() == 0 {
                    // M3 tolerates modest lateness while the audio sink is stable to avoid
                    // over-dropping on normal scheduling jitter.
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

        let handle = frame.surface();
        if let Some(previous) = self.presenter.select_surface(handle) {
            if previous != handle {
                self.presenter.release_surface(previous);
            }
        }
        self.metrics.note_video_frame_presented();
        self.pending_first_frame_metric |= self.metrics.presented_video_frames() == 1;
        if !self.audio_stream_seen || self.audio_clock_anchor_pts.is_some() {
            self.state = PlaybackState::Playing;
        }
    }

    fn drop_video_frame(&mut self, frame: DecodedVideoFrame) {
        self.presenter.release_surface(frame.surface());
        self.metrics.note_video_frame_dropped();
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
                let sample_rate = self.audio_sample_rate();
                if sample_rate > 0 {
                    let played = Duration::from_secs_f64(played_frames as f64 / sample_rate as f64);
                    return Some(anchor_pts.saturating_add(played));
                }
            }
        }

        self.video_clock.map(|clock| clock.position_at(now))
    }

    fn audio_sample_rate(&self) -> u32 {
        self.audio_sink
            .as_ref()
            .map(|sink| sink.format().sample_rate)
            .unwrap_or(0)
    }

    fn can_finish_playback(&self) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.video_stream_ended || !self.queued_video_frames.is_empty() {
            return Ok(false);
        }

        if self.audio_stream_seen {
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
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
        self.state = PlaybackState::Error;
        eprintln!("open failed: {error}");
    }

    fn fail_playback(&mut self, error: String) {
        self.last_error = Some(error.clone());
        self.active_operation_id = None;
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
