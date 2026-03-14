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

    pub fn pump_messages(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.pump_messages()?;
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    pub fn take_resize_request(&self) -> Option<ResizeRequest> {
        self.inner.take_resize_request()
    }

    pub fn take_input_events(&self) -> Vec<InputEvent> {
        self.inner.take_input_events()
    }

    pub(crate) fn raw_window(&self) -> &NativeWindowInner {
        &self.inner
    }
}
