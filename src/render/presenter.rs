use crate::{
    ffi::{
        d3d11::{D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::PresentResult,
    },
    platform::window::NativeWindow,
    render::{
        surface_registry::{SurfaceRegistry, VideoSurfaceHandle},
        swapchain::SwapChainPresenter,
        timeline::TimelineOverlayModel,
    },
};

pub struct Presenter {
    device: D3D11Device,
    swap_chain: Option<SwapChainPresenter>,
    surfaces: SurfaceRegistry,
    current_surface: Option<VideoSurfaceHandle>,
    subtitle_overlay: Option<SubtitleOverlay>,
    timeline_overlay: Option<SubtitleOverlay>,
    timeline_model: Option<TimelineOverlayModel>,
    volume_overlay: Option<SubtitleOverlay>,
    volume_text: Option<String>,
    idle_overlay: Option<SubtitleOverlay>,
    help_overlay: Option<SubtitleOverlay>,
    has_ever_shown_content: bool,
}

impl Presenter {
    pub fn new(window: &NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let device = D3D11Device::create()?;
        let swap_chain = SwapChainPresenter::new(window, &device)?;

        let idle_overlay = device.create_idle_overlay(1280, 720).ok().flatten();

        Ok(Self {
            device,
            swap_chain: Some(swap_chain),
            surfaces: SurfaceRegistry::default(),
            current_surface: None,
            subtitle_overlay: None,
            timeline_overlay: None,
            timeline_model: None,
            volume_overlay: None,
            volume_text: None,
            idle_overlay,
            help_overlay: None,
            has_ever_shown_content: false,
        })
    }

    pub fn render(
        &mut self,
        view: &crate::render::ViewTransform,
    ) -> Result<PresentResult, Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        if let Some(handle) = self.current_surface {
            if let Some(entry) = self.surfaces.get(handle) {
                if !self.has_ever_shown_content {
                    self.has_ever_shown_content = true;
                    self.idle_overlay = None;
                }
                return sc.render_surface(
                    &self.device,
                    &entry.surface,
                    self.subtitle_overlay.as_ref(),
                    self.timeline_overlay.as_ref(),
                    self.volume_overlay.as_ref(),
                    self.help_overlay.as_ref(),
                    view,
                );
            }
        }

        sc.render(
            &self.device,
            [0.08, 0.10, 0.14, 1.0],
            self.idle_overlay.as_ref(),
            self.timeline_overlay.as_ref(),
            self.volume_overlay.as_ref(),
            self.help_overlay.as_ref(),
        )
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        sc.resize(&self.device, width, height)?;
        if !self.has_ever_shown_content {
            self.idle_overlay = self.device.create_idle_overlay(width, height).ok().flatten();
        }
        Ok(())
    }

    pub fn rebuild_swap_chain(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Release backbuffer / render-target references, flush the device
        // context, then drop the swap chain — DXGI only allows one per HWND.
        self.drop_swap_chain();
        self.swap_chain = Some(SwapChainPresenter::new(window, &self.device)?);
        Ok(())
    }

    pub fn rebuild_device(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Release everything tied to the old device before creating a new
        // one — DXGI only allows one swap chain per HWND.
        self.reset_surfaces();
        self.drop_swap_chain();
        self.device = D3D11Device::create()?;
        self.swap_chain = Some(SwapChainPresenter::new(window, &self.device)?);
        Ok(())
    }

    fn drop_swap_chain(&mut self) {
        if let Some(sc) = self.swap_chain.as_mut() {
            sc.release_resources();
        }
        self.device.flush();
        self.swap_chain = None;
    }

    pub fn device(&self) -> &D3D11Device {
        &self.device
    }

    pub fn register_surface(
        &mut self,
        open_gen: crate::playback::generations::OpenGeneration,
        seek_gen: crate::playback::generations::SeekGeneration,
        surface: VideoSurface,
    ) -> VideoSurfaceHandle {
        self.surfaces.insert(open_gen, seek_gen, surface)
    }

    /// Validates that `handle` exists and matches the given generations, then
    /// selects it as the current surface in one registry lookup.
    /// Returns `Ok(previous_handle)` on success or `Err(())` on mismatch.
    pub fn validate_and_select_surface(
        &mut self,
        handle: VideoSurfaceHandle,
        open_gen: crate::playback::generations::OpenGeneration,
        seek_gen: crate::playback::generations::SeekGeneration,
    ) -> Result<Option<VideoSurfaceHandle>, ()> {
        if !matches!(
            self.surfaces.get(handle),
            Some(entry) if entry.open_gen == open_gen && entry.seek_gen == seek_gen
        ) {
            return Err(());
        }
        Ok(self.current_surface.replace(handle))
    }

    pub fn has_selected_surface(&self) -> bool {
        self.current_surface.is_some()
    }

    pub fn current_surface_size(&self) -> Option<(u32, u32)> {
        let handle = self.current_surface?;
        let entry = self.surfaces.get(handle)?;
        Some((entry.surface.width, entry.surface.height))
    }

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_ref() else {
            return Err("swap chain unavailable".into());
        };
        sc.viewport_size()
    }

    pub fn set_subtitle_overlay(
        &mut self,
        text: Option<&str>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.subtitle_overlay = match text {
            Some(text) => self
                .device
                .create_subtitle_overlay(text, viewport_width, viewport_height)?,
            None => None,
        };
        Ok(())
    }

    pub fn clear_subtitle_overlay(&mut self) {
        self.subtitle_overlay = None;
    }

    pub fn set_timeline_overlay(
        &mut self,
        model: Option<TimelineOverlayModel>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.timeline_model == model {
            return Ok(false);
        }

        let existing = self.timeline_overlay.take();
        self.timeline_overlay = match model {
            Some(ref m) => self.device.create_timeline_overlay(m, existing)?,
            None => None,
        };
        self.timeline_model = model;
        Ok(true)
    }

    pub fn set_volume_overlay(
        &mut self,
        text: Option<&str>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let next_text = text.map(str::to_owned);
        if self.volume_text == next_text {
            return Ok(false);
        }

        let existing = self.volume_overlay.take();
        self.volume_overlay = match text {
            Some(text) => self
                .device
                .create_volume_overlay(text, viewport_width, viewport_height, existing)?,
            None => None,
        };
        self.volume_text = next_text;
        Ok(true)
    }

    pub fn show_help_overlay(
        &mut self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.help_overlay.is_none() {
            self.help_overlay = self.device.create_help_overlay(viewport_width, viewport_height)?;
        }
        Ok(())
    }

    pub fn clear_help_overlay(&mut self) {
        self.help_overlay = None;
    }

    pub fn flush_video_processor_input_cache(&mut self) {
        if let Some(sc) = self.swap_chain.as_mut() {
            sc.flush_video_processor_input_cache(&self.device);
        }
    }

    pub fn release_surface(&mut self, handle: VideoSurfaceHandle) {
        if self.current_surface == Some(handle) {
            self.current_surface = None;
        }
        if let Some(entry) = self.surfaces.remove(handle) {
            if let Some(sc) = self.swap_chain.as_mut() {
                sc.invalidate_video_processor_input_view(&self.device, &entry.surface);
            }
        }
    }

    /// Replace the idle overlay with a custom message (e.g. for error state).
    pub fn set_idle_overlay(
        &mut self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.idle_overlay = self.device.create_idle_overlay(viewport_width, viewport_height).ok().flatten();
        self.has_ever_shown_content = false;
        Ok(())
    }

    /// Returns true if the idle (no-content) overlay is currently showing.
    pub fn is_showing_idle(&self) -> bool {
        !self.has_ever_shown_content
    }

    pub fn reset_surfaces(&mut self) {
        self.current_surface = None;
        self.surfaces.clear_for_new_epoch();
        if let Some(sc) = self.swap_chain.as_mut() {
            sc.flush_video_processor_input_cache(&self.device);
        }
        self.subtitle_overlay = None;
        self.timeline_overlay = None;
        self.timeline_model = None;
        self.volume_overlay = None;
        self.volume_text = None;
    }

    pub fn surfaces_alive(&self) -> usize {
        self.surfaces.count_alive()
    }
}
