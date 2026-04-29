use crate::{
    ffi::dxgi::{NativeWindowInner, ResizeRequest},
    platform::input::InputEvent,
};

pub struct NativeWindow {
    inner: NativeWindowInner,
}

impl NativeWindow {
    pub fn create(
        title: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: NativeWindowInner::create(title, width, height)?,
        })
    }

    pub fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    pub fn pump_messages(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.pump_messages()?;
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    pub fn take_resize_request(&self) -> Option<ResizeRequest> {
        self.inner.take_resize_request()
    }

    pub fn clear_modal_tick(&self) {
        self.inner.clear_modal_tick();
    }

    pub fn take_input_events(&self, out: &mut Vec<InputEvent>) {
        self.inner.take_input_events(out);
    }

    pub fn resize_for_content(&self, content_width: u32, content_height: u32, center: bool) {
        self.inner.resize_for_content(content_width, content_height, center);
    }

    pub fn fit_window_to_content(&self, content_width: u32, content_height: u32) {
        self.inner.fit_window_to_content(content_width, content_height);
    }

    pub fn set_window_client_size(&self, content_width: u32, content_height: u32) {
        self.inner.set_window_client_size(content_width, content_height);
    }

    pub fn toggle_borderless_fullscreen(&self) {
        self.inner.toggle_borderless_fullscreen();
    }

    pub fn is_borderless(&self) -> bool {
        self.inner.is_borderless()
    }

    pub fn client_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        self.inner.client_size()
    }

    pub fn cursor_client_position(&self) -> Result<Option<(i32, i32)>, Box<dyn std::error::Error>> {
        self.inner.cursor_client_position()
    }

    pub fn is_left_button_down(&self) -> bool {
        self.inner.is_left_button_down()
    }

    pub fn is_ctrl_held(&self) -> bool {
        self.inner.is_ctrl_held()
    }

    pub(crate) fn raw_window(&self) -> &NativeWindowInner {
        &self.inner
    }
}
