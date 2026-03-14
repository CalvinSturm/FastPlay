use crate::{
    ffi::{
        d3d11::{D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::DxgiSwapChain,
    },
    platform::window::NativeWindow,
};

pub struct SwapChainPresenter {
    swap_chain: DxgiSwapChain,
}

impl SwapChainPresenter {
    pub fn new(
        window: &NativeWindow,
        device: &D3D11Device,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            swap_chain: DxgiSwapChain::create(window.raw_window(), device)?,
        })
    }

    pub fn render(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain
            .render(device, clear_color, subtitle_overlay, timeline_overlay, volume_overlay)?;
        Ok(())
    }

    pub fn resize(
        &mut self,
        device: &D3D11Device,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.resize(device, width, height)?;
        Ok(())
    }

    pub fn render_surface(
        &mut self,
        device: &D3D11Device,
        surface: &VideoSurface,
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        view: &crate::render::ViewTransform,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain
            .render_surface(device, surface, subtitle_overlay, timeline_overlay, volume_overlay, view)?;
        Ok(())
    }

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        self.swap_chain.viewport_size()
    }
}
