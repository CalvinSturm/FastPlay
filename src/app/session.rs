use std::{
    cell::Cell,
    collections::VecDeque,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread::{self, ThreadId},
    time::Instant,
};

use crate::{
    app::{events::SessionEvent, state::PlaybackState},
    ffi::ffmpeg,
    media::{source::MediaSource, video::DecodedVideoFrame},
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

pub struct PlaybackSession {
    ui_thread_id: ThreadId,
    tick_active: Cell<bool>,
    window: NativeWindow,
    presenter: Presenter,
    state: PlaybackState,
    generations: GenerationState,
    operation_clock: OperationClock,
    active_operation_id: Option<OperationId>,
    event_tx: SyncSender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: PlaybackMetrics,
    playback_clock: Option<PlaybackClock>,
    queued_video_frames: VecDeque<DecodedVideoFrame>,
    queued_video_capacity: usize,
    pending_first_frame_metric: bool,
    video_stream_ended: bool,
    last_error: Option<String>,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let queue_defaults = QueueDefaults::default();
        let (event_tx, event_rx) = mpsc::sync_channel(queue_defaults.decoded_video_frames);

        Ok(Self {
            ui_thread_id: thread::current().id(),
            tick_active: Cell::new(false),
            window,
            presenter,
            state: PlaybackState::Idle,
            generations: GenerationState::default(),
            operation_clock: OperationClock::default(),
            active_operation_id: None,
            event_tx,
            event_rx,
            metrics: PlaybackMetrics::default(),
            playback_clock: None,
            queued_video_frames: VecDeque::with_capacity(queue_defaults.decoded_video_frames),
            queued_video_capacity: queue_defaults.decoded_video_frames,
            pending_first_frame_metric: false,
            video_stream_ended: false,
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
        self.queued_video_frames.clear();
        self.playback_clock = None;
        self.pending_first_frame_metric = false;
        self.video_stream_ended = false;
        self.state = PlaybackState::Opening;
        self.active_operation_id = Some(op_id);
        self.last_error = None;
        self.metrics.note_open_requested(now);

        thread::spawn(move || {
            let mut produced_any_frame = false;
            let decode_result =
                ffmpeg::stream_video_frames(&source, &device, open_gen, seek_gen, op_id, |frame| {
                    produced_any_frame = true;
                    sender
                        .send(SessionEvent::VideoFrameReady(frame))
                        .map_err(|_| "session event channel closed".to_string())
                });

            let final_event = match decode_result {
                Ok(()) => SessionEvent::VideoStreamEnded {
                    open_gen,
                    seek_gen,
                    op_id,
                },
                Err(error) if !produced_any_frame => SessionEvent::OpenFailed {
                    open_gen,
                    op_id,
                    error,
                },
                Err(error) => SessionEvent::PlaybackFailed {
                    open_gen,
                    seek_gen,
                    op_id,
                    error,
                },
            };

            let _ = sender.send(final_event);
        });

        Ok(())
    }

    /// UI-thread-only coordinator entrypoint.
    ///
    /// Safety and ownership contract:
    /// - must only run on the thread that owns the window and presenter
    /// - must not be entered recursively
    /// - must stay non-blocking and avoid worker waits or disk I/O
    /// - M2 drains worker completions, drops stale work before side effects,
    ///   advances the video-only clock, and presents due frames
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
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    let is_frame_event = matches!(event, SessionEvent::VideoFrameReady(_));
                    self.handle_event(event)?;
                    if is_frame_event && self.queued_video_frames.len() >= self.queued_video_capacity {
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

        self.advance_video_playback(now);

        self.presenter.render()?;
        self.metrics.note_present(now);
        if self.pending_first_frame_metric && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_first_frame_presented(now) {
                eprintln!("open_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.pending_first_frame_metric = false;
        }

        if self.video_stream_ended
            && self.queued_video_frames.is_empty()
            && matches!(self.state, PlaybackState::Priming | PlaybackState::Playing | PlaybackState::Draining)
        {
            self.state = PlaybackState::Ended;
            self.metrics.note_ended(now);
            eprintln!(
                "video_playback_summary presented_frames={} dropped_video_frames={}",
                self.metrics.presented_video_frames(),
                self.metrics.dropped_video_frames()
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

                let handle =
                    self.presenter
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
            SessionEvent::VideoStreamEnded {
                open_gen,
                seek_gen,
                op_id,
            } => {
                if !self.is_current_frame(open_gen, seek_gen, op_id) {
                    return Ok(());
                }

                self.video_stream_ended = true;
                if !self.queued_video_frames.is_empty() || self.presenter.has_selected_surface() {
                    self.state = PlaybackState::Draining;
                }
            }
            SessionEvent::OpenFailed {
                open_gen,
                op_id,
                error,
            } => {
                if open_gen != self.generations.open() || Some(op_id) != self.active_operation_id {
                    return Ok(());
                }

                self.last_error = Some(error.clone());
                self.state = PlaybackState::Error;
                eprintln!("open failed: {error}");
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

                self.last_error = Some(error.clone());
                self.state = PlaybackState::Error;
                eprintln!("playback failed: {error}");
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
                self.drop_frame(dropped);
            }
        }
    }

    fn advance_video_playback(&mut self, now: Instant) {
        loop {
            let Some(next_frame) = self.queued_video_frames.front() else {
                return;
            };

            if let Some(clock) = self.playback_clock {
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
        if self.playback_clock.is_none() {
            self.playback_clock = Some(PlaybackClock::new(now, frame.pts()));
        }

        let handle = frame.surface();
        if let Some(previous) = self.presenter.select_surface(handle) {
            if previous != handle {
                self.presenter.release_surface(previous);
            }
        }
        self.metrics.note_video_frame_presented();
        self.pending_first_frame_metric |= self.metrics.presented_video_frames() == 1;
        self.state = PlaybackState::Playing;
    }

    fn drop_frame(&mut self, frame: DecodedVideoFrame) {
        self.presenter.release_surface(frame.surface());
        self.metrics.note_video_frame_dropped();
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
