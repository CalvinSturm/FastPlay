use std::{error::Error, ffi::c_void, fmt, mem::{size_of, ManuallyDrop}, ptr::null_mut};

use windows::{
    core::{Interface, PCSTR},
    Win32::{
        Foundation::{BOOL, COLORREF, RECT},
        Graphics::{
            Direct3D::{Fxc::D3DCompile, ID3DBlob, D3D_DRIVER_TYPE_HARDWARE, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST},
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
                ID3D11BlendState, ID3D11Buffer, ID3D11InputLayout, ID3D11PixelShader,
                ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
                ID3D11Texture2D, ID3D11VertexShader, ID3D11VideoContext, ID3D11VideoDevice,
                D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER,
                D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE,
                D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA, D3D11_BUFFER_DESC,
                D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_INPUT_ELEMENT_DESC,
                D3D11_INPUT_PER_VERTEX_DATA, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
                D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
                D3D11_TEX2D_VPIV,
                D3D11_TEX2D_VPOV, D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
            },
            Dxgi::Common::{
                DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R32G32_FLOAT,
                DXGI_FORMAT_R32G32B32_FLOAT, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
            },
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
                SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER,
                DT_NOPREFIX, DT_WORDBREAK, FF_DONTCARE, FW_SEMIBOLD, HGDIOBJ,
                OUT_DEFAULT_PRECIS, TRANSPARENT, DEFAULT_CHARSET, DEFAULT_PITCH,
            },
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

pub(crate) struct SubtitleOverlay {
    shader_resource_view: ID3D11ShaderResourceView,
    vertex_buffer: ID3D11Buffer,
}

pub(crate) struct SubtitleRenderer {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    sampler: ID3D11SamplerState,
    blend_state: ID3D11BlendState,
}

#[derive(Clone, Debug)]
pub(crate) struct VideoSurface {
    texture: ID3D11Texture2D,
    subresource_index: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SubtitleVertex {
    position: [f32; 3],
    texcoord: [f32; 2],
}

struct SubtitleBitmap {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
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

    pub(crate) fn create_subtitle_renderer(&self) -> Result<SubtitleRenderer, Box<dyn Error>> {
        let vertex_shader_source = b"
struct VSInput {
    float3 pos : POSITION;
    float2 uv : TEXCOORD0;
};
struct PSInput {
    float4 pos : SV_POSITION;
    float2 uv : TEXCOORD0;
};
PSInput main(VSInput input) {
    PSInput output;
    output.pos = float4(input.pos, 1.0f);
    output.uv = input.uv;
    return output;
}
\0";
        let pixel_shader_source = b"
Texture2D subtitle_tex : register(t0);
SamplerState subtitle_sampler : register(s0);
float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    return subtitle_tex.Sample(subtitle_sampler, uv);
}
\0";
        let vertex_blob = compile_shader(vertex_shader_source, b"main\0", b"vs_4_0\0")?;
        let pixel_blob = compile_shader(pixel_shader_source, b"main\0", b"ps_4_0\0")?;

        let mut vertex_shader = None;
        let mut pixel_shader = None;
        let mut input_layout = None;
        let input_elements = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(b"POSITION\0".as_ptr()),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(b"TEXCOORD\0".as_ptr()),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 12,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];
        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: Default::default(),
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        let blend_desc = D3D11_BLEND_DESC {
            AlphaToCoverageEnable: BOOL(0),
            IndependentBlendEnable: BOOL(0),
            RenderTarget: [windows::Win32::Graphics::Direct3D11::D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL(1),
                SrcBlend: D3D11_BLEND_SRC_ALPHA,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            }; 8],
        };
        let mut blend_state = None;
        let vertex_bytecode = shader_blob_bytes(&vertex_blob);
        let pixel_bytecode = shader_blob_bytes(&pixel_blob);

        unsafe {
            self.device.CreateVertexShader(
                vertex_bytecode,
                None,
                Some(&mut vertex_shader),
            )?;
            self.device.CreatePixelShader(
                pixel_bytecode,
                None,
                Some(&mut pixel_shader),
            )?;
            self.device.CreateInputLayout(
                &input_elements,
                vertex_bytecode,
                Some(&mut input_layout),
            )?;
            self.device
                .CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            self.device
                .CreateBlendState(&blend_desc, Some(&mut blend_state))?;
        }

        Ok(SubtitleRenderer {
            vertex_shader: vertex_shader.ok_or(D3D11Error("CreateVertexShader returned no shader"))?,
            pixel_shader: pixel_shader.ok_or(D3D11Error("CreatePixelShader returned no shader"))?,
            input_layout: input_layout.ok_or(D3D11Error("CreateInputLayout returned no layout"))?,
            sampler: sampler.ok_or(D3D11Error("CreateSamplerState returned no sampler"))?,
            blend_state: blend_state.ok_or(D3D11Error("CreateBlendState returned no blend state"))?,
        })
    }

    pub(crate) fn create_subtitle_overlay(
        &self,
        text: &str,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_subtitle_bitmap(text, viewport_width, viewport_height)? else {
            return Ok(None);
        };
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: bitmap.pixels.as_ptr().cast(),
            SysMemPitch: bitmap.width.saturating_mul(4),
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        let mut shader_resource_view = None;
        let vertices = subtitle_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
        let vertex_buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: (size_of::<SubtitleVertex>() * vertices.len()) as u32,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let vertex_buffer_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: vertices.as_ptr().cast(),
            SysMemPitch: 0,
            SysMemSlicePitch: 0,
        };
        let mut vertex_buffer = None;

        unsafe {
            self.device
                .CreateTexture2D(&texture_desc, Some(&initial_data), Some(&mut texture))?;
            self.device.CreateShaderResourceView(
                texture
                    .as_ref()
                    .ok_or(D3D11Error("CreateTexture2D returned no subtitle texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device
                .CreateBuffer(&vertex_buffer_desc, Some(&vertex_buffer_data), Some(&mut vertex_buffer))?;
        }

        Ok(Some(SubtitleOverlay {
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no subtitle view",
            ))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no subtitle vertex buffer"))?,
        }))
    }

    pub(crate) fn render_subtitle_overlay(
        &self,
        renderer: &SubtitleRenderer,
        overlay: &SubtitleOverlay,
        render_target: &RenderTargetView,
    ) -> Result<(), Box<dyn Error>> {
        let stride = size_of::<SubtitleVertex>() as u32;
        let offset = 0u32;

        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(render_target.view.clone())]), None);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context
                .IASetInputLayout(Some(&renderer.input_layout));
            let vertex_buffers = [Some(overlay.vertex_buffer.clone())];
            let strides = [stride];
            let offsets = [offset];
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(vertex_buffers.as_ptr()),
                Some(strides.as_ptr()),
                Some(offsets.as_ptr()),
            );
            self.context
                .VSSetShader(Some(&renderer.vertex_shader), None);
            self.context
                .PSSetShader(Some(&renderer.pixel_shader), None);
            self.context.PSSetSamplers(0, Some(&[Some(renderer.sampler.clone())]));
            self.context.PSSetShaderResources(
                0,
                Some(&[Some(overlay.shader_resource_view.clone())]),
            );
            self.context.OMSetBlendState(
                Some(&renderer.blend_state),
                Some(&[0.0, 0.0, 0.0, 0.0]),
                u32::MAX,
            );
            self.context.Draw(6, 0);
            self.context.PSSetShaderResources(0, Some(&[None]));
            self.context.OMSetBlendState(None, Some(&[0.0, 0.0, 0.0, 0.0]), u32::MAX);
        }

        Ok(())
    }

    // IMPORTANT:
    // Software-fallback NV12 upload textures must be created as decoder-compatible
    // video surfaces for the existing D3D11 video-processor present path.
    // Using only a generic texture here can compile but fail at runtime in the
    // present path.
    // Required bind flags for the current design:
    // - D3D11_BIND_SHADER_RESOURCE
    // - D3D11_BIND_DECODER
    //
    // If the present path changes in the future, re-validate this assumption.

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

fn subtitle_quad_vertices(
    overlay_width: u32,
    overlay_height: u32,
    viewport_width: u32,
    viewport_height: u32,
) -> [SubtitleVertex; 6] {
    let margin = (viewport_height / 18).max(24) as f32;
    let left_px = ((viewport_width.saturating_sub(overlay_width)) / 2) as f32;
    let right_px = left_px + overlay_width as f32;
    let top_px = (viewport_height as f32 - margin - overlay_height as f32).max(0.0);
    let bottom_px = (top_px + overlay_height as f32).min(viewport_height as f32);

    let left = left_px / viewport_width as f32 * 2.0 - 1.0;
    let right = right_px / viewport_width as f32 * 2.0 - 1.0;
    let top = 1.0 - top_px / viewport_height as f32 * 2.0;
    let bottom = 1.0 - bottom_px / viewport_height as f32 * 2.0;

    [
        SubtitleVertex { position: [left, top, 0.0], texcoord: [0.0, 0.0] },
        SubtitleVertex { position: [right, top, 0.0], texcoord: [1.0, 0.0] },
        SubtitleVertex { position: [left, bottom, 0.0], texcoord: [0.0, 1.0] },
        SubtitleVertex { position: [left, bottom, 0.0], texcoord: [0.0, 1.0] },
        SubtitleVertex { position: [right, top, 0.0], texcoord: [1.0, 0.0] },
        SubtitleVertex { position: [right, bottom, 0.0], texcoord: [1.0, 1.0] },
    ]
}

fn compile_shader(
    source: &[u8],
    entry_point: &[u8],
    target: &[u8],
) -> Result<ID3DBlob, Box<dyn Error>> {
    let mut blob = None;
    let mut error_blob = None;

    unsafe {
        let status = D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_point.as_ptr()),
            PCSTR(target.as_ptr()),
            0,
            0,
            &mut blob,
            Some(&mut error_blob),
        );
        if let Err(error) = status {
            if let Some(error_blob) = error_blob {
                let message = std::slice::from_raw_parts(
                    error_blob.GetBufferPointer().cast::<u8>(),
                    error_blob.GetBufferSize(),
                );
                return Err(format!(
                    "D3DCompile failed: {error}; {}",
                    String::from_utf8_lossy(message)
                )
                .into());
            }
            return Err(Box::new(error));
        }
    }

    blob.ok_or_else(|| Box::new(D3D11Error("D3DCompile returned no bytecode")) as Box<dyn Error>)
}

fn shader_blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()) }
}

fn render_subtitle_bitmap(
    text: &str,
    viewport_width: u32,
    viewport_height: u32,
) -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    if text.trim().is_empty() {
        return Ok(None);
    }

    let font_height = (viewport_height / 18).max(24) as i32;
    let padding = (font_height / 2).max(12);
    let max_text_width = ((viewport_width as i32 * 3) / 4).max(320);
    let mut text_rect = RECT {
        left: 0,
        top: 0,
        right: max_text_width,
        bottom: 0,
    };
    let mut text_wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Box::new(D3D11Error("CreateCompatibleDC returned null")));
        }

        let font = CreateFontW(
            -font_height,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            windows::core::w!("Segoe UI"),
        );
        if font.0.is_null() {
            let _ = DeleteDC(dc);
            return Err(Box::new(D3D11Error("CreateFontW returned null")));
        }

        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let _ = DrawTextW(
            dc,
            &mut text_wide,
            &mut text_rect,
            DT_CALCRECT | DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );

        let bitmap_width = (text_rect.right - text_rect.left + padding * 2).max(1) as u32;
        let bitmap_height = (text_rect.bottom - text_rect.top + padding * 2).max(1) as u32;
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: bitmap_width as i32,
            biHeight: -(bitmap_height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if bitmap.0.is_null() || bits.is_null() {
            let _ = SelectObject(dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteDC(dc);
            return Err(Box::new(D3D11Error("CreateDIBSection failed for subtitles")));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (bitmap_width * bitmap_height * 4) as usize);

        let mut draw_rect = RECT {
            left: padding,
            top: padding,
            right: bitmap_width as i32 - padding,
            bottom: bitmap_height as i32 - padding,
        };
        let _ = DrawTextW(
            dc,
            &mut text_wide,
            &mut draw_rect,
            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );

        let source: &[u8] =
            std::slice::from_raw_parts(bits.cast::<u8>(), (bitmap_width * bitmap_height * 4) as usize);
        let mut pixels = vec![0u8; source.len()];
        for (source_px, dest_px) in source.chunks_exact(4).zip(pixels.chunks_exact_mut(4)) {
            dest_px[0] = 0;
            dest_px[1] = 0;
            dest_px[2] = 0;
            dest_px[3] = 96;

            let intensity = source_px[0].max(source_px[1]).max(source_px[2]);
            if intensity > 0 {
                dest_px[0] = 255;
                dest_px[1] = 255;
                dest_px[2] = 255;
                dest_px[3] = intensity.max(180);
            }
        }

        let _ = SelectObject(dc, old_bitmap);
        let _ = SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);

        Ok(Some(SubtitleBitmap {
            width: bitmap_width,
            height: bitmap_height,
            pixels,
        }))
    }
}
