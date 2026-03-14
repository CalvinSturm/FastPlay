use crate::{
    ffi::d3d11::D3D11Device,
    platform::window::NativeWindow,
    render::{surface_registry::SurfaceRegistry, swapchain::SwapChainPresenter},
};

pub struct Presenter {
    device: D3D11Device,
    swap_chain: SwapChainPresenter,
    #[allow(dead_code)]
    surfaces: SurfaceRegistry,
}

impl Presenter {
    pub fn new(window: &NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let device = D3D11Device::create()?;
        let swap_chain = SwapChainPresenter::new(window, &device)?;

        Ok(Self {
            device,
            swap_chain,
            surfaces: SurfaceRegistry::default(),
        })
    }

    pub fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain
            .render(&self.device, [0.08, 0.10, 0.14, 1.0])?;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.resize(&self.device, width, height)?;
        Ok(())
    }
}
