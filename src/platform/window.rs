use std::ffi::c_void;

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

    /// # Safety
    ///
    /// `ctx` must remain valid for as long as the callback is installed.
    pub unsafe fn install_modal_tick(
        &self,
        ctx: *mut c_void,
        tick_fn: unsafe fn(*mut c_void),
    ) {
        self.inner.install_modal_tick(ctx, tick_fn);
    }

    pub fn clear_modal_tick(&self) {
        self.inner.clear_modal_tick();
    }

    pub fn take_input_events(&self) -> Vec<InputEvent> {
        self.inner.take_input_events()
    }

    pub fn toggle_borderless_fullscreen(&self) {
        self.inner.toggle_borderless_fullscreen();
    }

    pub fn is_borderless(&self) -> bool {
        self.inner.is_borderless()
    }

    pub(crate) fn raw_window(&self) -> &NativeWindowInner {
        &self.inner
    }
}
