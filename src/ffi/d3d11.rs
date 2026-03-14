use std::{error::Error, ffi::c_void, fmt, mem::ManuallyDrop};

use windows::{
    core::Interface,
    Win32::{
        Foundation::BOOL,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
                ID3D11RenderTargetView, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
                D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV,
                D3D11_TEX2D_VPOV, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
            },
            Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC},
        },
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

#[derive(Clone)]
pub struct D3D11Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
}

pub struct RenderTargetView {
    view: ID3D11RenderTargetView,
}

#[derive(Clone, Debug)]
pub(crate) struct VideoSurface {
    texture: ID3D11Texture2D,
    subresource_index: u32,
    width: u32,
    height: u32,
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

        let device = device.ok_or(D3D11Error("D3D11CreateDevice returned no device"))?;
        let context = context.ok_or(D3D11Error("D3D11CreateDevice returned no context"))?;
        let multithread: ID3D11Multithread = device.cast()?;
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;

        // SAFETY:
        // - the multithread interface comes from the live D3D11 device
        // - M1 shares the D3D11 device across the decode worker and UI thread
        unsafe {
            let _ = multithread.SetMultithreadProtected(BOOL(1));
        }

        Ok(Self {
            device,
            context,
            video_device,
            video_context,
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

    pub(crate) fn raw_device_ptr(&self) -> *mut c_void {
        self.device.clone().into_raw()
    }

    pub(crate) unsafe fn surface_from_raw_texture(
        &self,
        texture: *mut c_void,
        subresource_index: u32,
        width: u32,
        height: u32,
    ) -> Result<VideoSurface, Box<dyn Error>> {
        let borrowed = ID3D11Texture2D::from_raw_borrowed(&texture)
            .ok_or(D3D11Error("decoded frame exposed a null D3D11 texture"))?;

        Ok(VideoSurface {
            texture: borrowed.clone(),
            subresource_index,
            width,
            height,
        })
    }

    pub(crate) fn render_video_surface(
        &self,
        surface: &VideoSurface,
        backbuffer: &ID3D11Texture2D,
        output_width: u32,
        output_height: u32,
    ) -> Result<(), Box<dyn Error>> {
        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 1,
                Denominator: 1,
            },
            InputWidth: surface.width.max(1),
            InputHeight: surface.height.max(1),
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 1,
                Denominator: 1,
            },
            OutputWidth: output_width.max(1),
            OutputHeight: output_height.max(1),
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };

        // SAFETY:
        // - the enumerator and processor are created from the active device
        // - the input and output views reference live D3D11 textures
        // - the immediate context is multithread-protected for worker/UI sharing
        unsafe {
            let enumerator = self
                .video_device
                .CreateVideoProcessorEnumerator(&content_desc)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;

            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: surface.subresource_index,
                    },
                },
            };
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };

            let mut input_view = None;
            self.video_device.CreateVideoProcessorInputView(
                &surface.texture,
                &enumerator,
                &input_desc,
                Some(&mut input_view),
            )?;
            let input_view =
                input_view.ok_or(D3D11Error("CreateVideoProcessorInputView returned no view"))?;

            let mut output_view = None;
            self.video_device.CreateVideoProcessorOutputView(
                backbuffer,
                &enumerator,
                &output_desc,
                Some(&mut output_view),
            )?;
            let output_view = output_view.ok_or(D3D11Error(
                "CreateVideoProcessorOutputView returned no view",
            ))?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            self.video_context.VideoProcessorSetStreamOutputRate(
                &processor,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                BOOL(0),
                None,
            );
            self.video_context
                .VideoProcessorBlt(&processor, &output_view, 0, &[stream])?;
        }

        Ok(())
    }

    pub(crate) fn upload_nv12_surface(
        &self,
        width: u32,
        height: u32,
        y_plane: &[u8],
        y_stride: usize,
        uv_plane: &[u8],
        uv_stride: usize,
    ) -> Result<VideoSurface, Box<dyn Error>> {
        if width == 0 || height == 0 {
            return Err(Box::new(D3D11Error("software upload requires non-zero dimensions")));
        }
        if width % 2 != 0 || height % 2 != 0 {
            return Err(Box::new(D3D11Error(
                "software NV12 upload currently requires even frame dimensions",
            )));
        }
        if y_stride != uv_stride {
            return Err(Box::new(D3D11Error(
                "software NV12 upload requires equal luma/chroma strides",
            )));
        }

        let expected_y_len = y_stride.saturating_mul(height as usize);
        let expected_uv_len = uv_stride.saturating_mul((height / 2) as usize);
        if y_plane.len() < expected_y_len || uv_plane.len() < expected_uv_len {
            return Err(Box::new(D3D11Error(
                "software NV12 planes were smaller than the declared stride/height",
            )));
        }

        let mut upload = Vec::with_capacity(expected_y_len.saturating_add(expected_uv_len));
        upload.extend_from_slice(&y_plane[..expected_y_len]);
        upload.extend_from_slice(&uv_plane[..expected_uv_len]);

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_DECODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: upload.as_ptr().cast(),
            SysMemPitch: y_stride as u32,
            SysMemSlicePitch: upload.len() as u32,
        };
        let mut texture = None;

        // SAFETY:
        // - the upload buffer stays alive for the duration of CreateTexture2D
        // - the NV12 planes are laid out contiguously as required for a single-subresource upload
        // - the created texture remains owned by the returned VideoSurface
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&initial_data), Some(&mut texture))?;
        }

        Ok(VideoSurface {
            texture: texture.ok_or(D3D11Error("CreateTexture2D returned no software texture"))?,
            subresource_index: 0,
            width,
            height,
        })
    }
}
