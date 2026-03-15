use crate::{
    ffi::{
        d3d11::{D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::PresentResult,
    },
    media::video::{DecodedVideoFrame, SoftwareVideoFrameFormat},
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
}

impl Presenter {
    pub fn new(window: &NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let device = D3D11Device::create()?;
        let swap_chain = SwapChainPresenter::new(window, &device)?;

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
                return sc.render_surface(
                    &self.device,
                    &entry.surface,
                    self.subtitle_overlay.as_ref(),
                    self.timeline_overlay.as_ref(),
                    self.volume_overlay.as_ref(),
                    view,
                );
            }
        }

        sc.render(
            &self.device,
            [0.08, 0.10, 0.14, 1.0],
            self.subtitle_overlay.as_ref(),
            self.timeline_overlay.as_ref(),
            self.volume_overlay.as_ref(),
        )
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        sc.resize(&self.device, width, height)?;
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

    pub fn select_surface(&mut self, handle: VideoSurfaceHandle) -> Option<VideoSurfaceHandle> {
        if !self.surfaces.contains(handle) {
            return self.current_surface;
        }
        self.current_surface.replace(handle)
    }

    pub fn upload_software_frame(
        &mut self,
        frame: &DecodedVideoFrame,
    ) -> Result<VideoSurfaceHandle, Box<dyn std::error::Error>> {
        let DecodedVideoFrame::Software {
            open_gen,
            seek_gen,
            width,
            height,
            format,
            planes,
            strides,
            ..
        } = frame
        else {
            return Err("upload_software_frame requires a software frame".into());
        };

        let surface = match format {
            SoftwareVideoFrameFormat::Nv12 => {
                if planes.len() != 2 || strides.len() != 2 {
                    return Err("NV12 software upload requires two planes and two strides".into());
                }
                self.device.upload_nv12_surface(
                    *width,
                    *height,
                    &planes[0],
                    strides[0],
                    &planes[1],
                    strides[1],
                )?
            }
        };

        Ok(self.register_surface(*open_gen, *seek_gen, surface))
    }

    pub fn surface_matches(
        &self,
        handle: VideoSurfaceHandle,
        open_gen: crate::playback::generations::OpenGeneration,
        seek_gen: crate::playback::generations::SeekGeneration,
    ) -> bool {
        matches!(
            self.surfaces.get(handle),
            Some(entry) if entry.open_gen == open_gen && entry.seek_gen == seek_gen
        )
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

        self.timeline_overlay = match model {
            Some(model) => self.device.create_timeline_overlay(&model)?,
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

        self.volume_overlay = match text {
            Some(text) => self
                .device
                .create_volume_overlay(text, viewport_width, viewport_height)?,
            None => None,
        };
        self.volume_text = next_text;
        Ok(true)
    }

    pub fn release_surface(&mut self, handle: VideoSurfaceHandle) {
        if self.current_surface == Some(handle) {
            self.current_surface = None;
        }
        let _ = self.surfaces.remove(handle);
    }

    pub fn reset_surfaces(&mut self) {
        self.current_surface = None;
        self.surfaces.clear_for_new_epoch();
        self.subtitle_overlay = None;
        self.timeline_overlay = None;
        self.timeline_model = None;
        self.volume_overlay = None;
        self.volume_text = None;
    }
}
