use std::{
    cell::Cell,
    collections::VecDeque,
    thread::{self, ThreadId},
    time::Instant,
};

use crate::{
    app::{events::SessionEvent, state::PlaybackState},
    platform::window::NativeWindow,
    playback::{
        generations::{GenerationState, OperationClock},
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
    metrics: PlaybackMetrics,
}

impl PlaybackSession {
    pub fn new(window: NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let presenter = Presenter::new(&window)?;

        Ok(Self {
            ui_thread_id: thread::current().id(),
            tick_active: Cell::new(false),
            window,
            presenter,
            state: PlaybackState::Idle,
            generations: GenerationState::default(),
            operation_clock: OperationClock::default(),
            events: VecDeque::new(),
            metrics: PlaybackMetrics::default(),
        })
    }

    pub fn window(&self) -> &NativeWindow {
        &self.window
    }

    pub fn window_mut(&mut self) -> &mut NativeWindow {
        &mut self.window
    }

    /// UI-thread-only coordinator entrypoint.
    ///
    /// Safety and ownership contract:
    /// - must only run on the thread that owns the window and presenter
    /// - must not be entered recursively
    /// - must stay non-blocking and avoid worker waits or disk I/O
    /// - M0 only drains local events, applies resize, and issues a clear/present
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
        while let Some(_event) = self.events.pop_front() {}

        if let Some(size) = self.window.take_resize_request() {
            self.presenter.resize(size.width, size.height)?;
            self.metrics.note_resize(now);
        }

        self.presenter.render()?;
        self.metrics.note_present(now);

        let _ = (&self.state, &self.generations, &self.operation_clock);

        Ok(())
    }
}
