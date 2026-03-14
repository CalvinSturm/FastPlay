use std::{error::Error, fmt};

use windows::Win32::Graphics::{
    Direct3D::D3D_DRIVER_TYPE_HARDWARE,
    Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
        ID3D11Texture2D, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    },
};

#[derive(Debug)]
pub struct D3D11Error(&'static str);

impl fmt::Display for D3D11Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for D3D11Error {}

pub struct D3D11Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

pub struct RenderTargetView {
    view: ID3D11RenderTargetView,
}

impl D3D11Device {
    pub fn create() -> Result<Self, Box<dyn Error>> {
        let mut device = None;
        let mut context = None;

        // SAFETY:
        // - all out-pointers point to stack locals owned by this function
        // - no optional software rasterizer handle is supplied
        // - the chosen flags and feature-level slice are valid for D3D11CreateDevice
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }

        Ok(Self {
            device: device.ok_or(D3D11Error("D3D11CreateDevice returned no device"))?,
            context: context.ok_or(D3D11Error("D3D11CreateDevice returned no context"))?,
        })
    }

    pub(crate) fn create_render_target_view(
        &self,
        texture: &ID3D11Texture2D,
    ) -> Result<RenderTargetView, Box<dyn Error>> {
        let mut view = None;

        // SAFETY:
        // - `texture` is a valid backbuffer texture from the active swap chain
        // - descriptor is omitted so D3D11 derives the default RTV for the texture
        // - `view` points to a stack local that lives for the duration of the call
        unsafe {
            self.device
                .CreateRenderTargetView(texture, None, Some(&mut view))?;
        }

        Ok(RenderTargetView {
            view: view.ok_or(D3D11Error("CreateRenderTargetView returned no view"))?,
        })
    }

    pub fn clear_render_target(&self, render_target: &RenderTargetView, clear_color: [f32; 4]) {
        // SAFETY:
        // - `render_target` is owned by the active swap-chain state
        // - the context belongs to the same D3D11 device that created the RTV
        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(render_target.view.clone())]), None);
            self.context
                .ClearRenderTargetView(&render_target.view, &clear_color);
        }
    }

    pub(crate) fn raw_device(&self) -> &ID3D11Device {
        &self.device
    }
}
