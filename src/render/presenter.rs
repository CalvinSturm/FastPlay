use crate::{
    ffi::d3d11::{D3D11Device, SubtitleOverlay, VideoSurface},
    media::video::{DecodedVideoFrame, SoftwareVideoFrameFormat},
    platform::window::NativeWindow,
    render::{
        surface_registry::{SurfaceRegistry, VideoSurfaceHandle},
        swapchain::SwapChainPresenter,
    },
};

pub struct Presenter {
    device: D3D11Device,
    swap_chain: SwapChainPresenter,
    surfaces: SurfaceRegistry,
    current_surface: Option<VideoSurfaceHandle>,
    subtitle_overlay: Option<SubtitleOverlay>,
}

impl Presenter {
    pub fn new(window: &NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let device = D3D11Device::create()?;
        let swap_chain = SwapChainPresenter::new(window, &device)?;

        Ok(Self {
            device,
            swap_chain,
            surfaces: SurfaceRegistry::default(),
            current_surface: None,
            subtitle_overlay: None,
        })
    }

    pub fn render(
        &mut self,
        view: &crate::render::ViewTransform,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(handle) = self.current_surface {
            if let Some(entry) = self.surfaces.get(handle) {
                self.swap_chain.render_surface(
                    &self.device,
                    &entry.surface,
                    self.subtitle_overlay.as_ref(),
                    view,
                )?;
                return Ok(());
            }
        }

        self.swap_chain
            .render(&self.device, [0.08, 0.10, 0.14, 1.0], self.subtitle_overlay.as_ref())?;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.resize(&self.device, width, height)?;
        Ok(())
    }

    pub fn rebuild_swap_chain(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain = SwapChainPresenter::new(window, &self.device)?;
        Ok(())
    }

    pub fn rebuild_device(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.device = D3D11Device::create()?;
        self.swap_chain = SwapChainPresenter::new(window, &self.device)?;
        self.reset_surfaces();
        Ok(())
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

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        self.swap_chain.viewport_size()
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
    }
}
