use std::{
    cell::Cell,
    collections::VecDeque,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::{self, ThreadId},
    time::Instant,
};

use crate::{
    app::{events::SessionEvent, state::PlaybackState},
    ffi::ffmpeg,
    media::{source::MediaSource, video::DecodedVideoFrame},
    platform::window::NativeWindow,
    playback::{
        generations::{
            GenerationState, OpenGeneration, OperationClock, OperationId, SeekGeneration,
        },
        metrics::PlaybackMetrics,
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
    events: VecDeque<SessionEvent>,
    event_tx: Sender<SessionEvent>,
    event_rx: Receiver<SessionEvent>,
    metrics: PlaybackMetrics,
    pending_first_frame_metric: bool,
    last_error: Option<String>,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;
        let (event_tx, event_rx) = mpsc::channel();

        Ok(Self {
            ui_thread_id: thread::current().id(),
            tick_active: Cell::new(false),
            window,
            presenter,
            state: PlaybackState::Idle,
            generations: GenerationState::default(),
            operation_clock: OperationClock::default(),
            events: VecDeque::new(),
            event_tx,
            event_rx,
            metrics: PlaybackMetrics::default(),
            pending_first_frame_metric: false,
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

        self.state = PlaybackState::Opening;
        self.last_error = None;
        self.metrics.note_open_requested(now);

        thread::spawn(move || {
            let event =
                match ffmpeg::decode_first_video_frame(&source, &device, open_gen, seek_gen, op_id)
                {
                    Ok(frame) => SessionEvent::VideoFrameReady(frame),
                    Err(error) => SessionEvent::OpenFailed {
                        open_gen,
                        op_id,
                        error,
                    },
                };

            let _ = sender.send(event);
        });

        Ok(())
    }

    /// UI-thread-only coordinator entrypoint.
    ///
    /// Safety and ownership contract:
    /// - must only run on the thread that owns the window and presenter
    /// - must not be entered recursively
    /// - must stay non-blocking and avoid worker waits or disk I/O
    /// - M1 drains worker completions, applies resize, and presents the first
    ///   selected frame once it becomes available
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
                Ok(event) => self.events.push_back(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        while let Some(event) = self.events.pop_front() {
            self.handle_event(event)?;
        }

        if let Some(size) = self.window.take_resize_request() {
            self.presenter.resize(size.width, size.height)?;
            self.metrics.note_resize(now);
        }

        self.presenter.render()?;
        self.metrics.note_present(now);
        if self.pending_first_frame_metric && self.presenter.has_selected_surface() {
            if let Some(elapsed) = self.metrics.note_first_frame_presented(now) {
                eprintln!("open_to_first_frame_ms={}", elapsed.as_millis());
            }
            self.pending_first_frame_metric = false;
        }

        let _ = (&self.state, &self.generations, &self.operation_clock);

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
                self.presenter.select_surface(handle);
                let decoded = DecodedVideoFrame::D3D11 {
                    open_gen: frame.open_gen,
                    seek_gen: frame.seek_gen,
                    op_id: frame.op_id,
                    pts: frame.pts,
                    width: frame.width,
                    height: frame.height,
                    surface: handle,
                };
                let _ = decoded;

                self.state = PlaybackState::Playing;
                self.pending_first_frame_metric = true;
            }
            SessionEvent::OpenFailed {
                open_gen,
                op_id: _,
                error,
            } => {
                if open_gen != self.generations.open() {
                    return Ok(());
                }

                self.last_error = Some(error.clone());
                self.state = PlaybackState::Error;
                eprintln!("open failed: {error}");
            }
            SessionEvent::DeviceLost { .. } | SessionEvent::AudioEndpointChanged { .. } => {}
        }

        Ok(())
    }

    fn is_current_frame(
        &self,
        open_gen: OpenGeneration,
        seek_gen: SeekGeneration,
        _op_id: OperationId,
    ) -> bool {
        open_gen == self.generations.open() && seek_gen == self.generations.seek()
    }
}
