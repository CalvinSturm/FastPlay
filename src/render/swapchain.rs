use crate::{
    ffi::{
        d3d11::{D3D11Device, VideoSurface},
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.render(device, clear_color)?;
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.render_surface(device, surface)?;
        Ok(())
    }
}
