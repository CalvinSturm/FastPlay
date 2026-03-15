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
                D3D11_TEX2D_VPIV, D3D11_VIEWPORT,
                D3D11_TEX2D_VPOV, D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_180,
                D3D11_VIDEO_PROCESSOR_ROTATION_270, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
                ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
                ID3D11VideoProcessorOutputView,
            },
            Dxgi::Common::{
                DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R32G32_FLOAT,
                DXGI_FORMAT_R32G32B32_FLOAT, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
            },
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
                SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER,
                DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
                FF_DONTCARE, FW_MEDIUM, FW_SEMIBOLD, HGDIOBJ,
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

/// Cached D3D11 video processor objects reused across frames when the
/// input/output dimensions and backbuffer identity haven't changed.
/// Avoids per-frame kernel-mode allocations that stress the GPU driver.
pub(crate) struct VideoProcessorCache {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output_view: ID3D11VideoProcessorOutputView,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    /// Raw pointer used only for identity comparison — never dereferenced.
    backbuffer_identity: *mut c_void,
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

    pub(crate) fn flush(&self) {
        unsafe {
            self.context.ClearState();
            self.context.Flush();
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
        let source = ID3D11Texture2D::from_raw_borrowed(&texture)
            .ok_or(D3D11Error("decoded frame exposed a null D3D11 texture"))?;

        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        source.GetDesc(&mut source_desc);

        let copy_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: source_desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_DECODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut owned_texture = None;
        self.device
            .CreateTexture2D(&copy_desc, None, Some(&mut owned_texture))?;
        let owned_texture = owned_texture
            .ok_or(D3D11Error("CreateTexture2D returned no copy texture"))?;

        // SAFETY: both textures belong to the same D3D11 device. The source
        // subresource index selects one slice from the decoder's texture
        // array; the destination is a standalone single-slice texture.
        self.context.CopySubresourceRegion(
            &owned_texture,
            0,
            0,
            0,
            0,
            source,
            subresource_index,
            None,
        );

        Ok(VideoSurface {
            texture: owned_texture,
            subresource_index: 0,
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
        view: &crate::render::ViewTransform,
        vp_cache: &mut Option<VideoProcessorCache>,
    ) -> Result<(), Box<dyn Error>> {
        // SAFETY:
        // - the enumerator and processor are created from the active device
        // - the input and output views reference live D3D11 textures
        // - the immediate context is multithread-protected for worker/UI sharing
        // - backbuffer_identity is used only for pointer comparison, never
        //   dereferenced
        unsafe {
            let bb_identity = backbuffer.as_raw();

            // Reuse or recreate the cached enumerator, processor, and output
            // view.  These are keyed on (input dims, output dims, backbuffer
            // identity).  Input views are per-texture and created fresh each
            // frame, but the heavy kernel-mode objects are reused.
            let cache = match vp_cache {
                Some(c)
                    if c.input_width == surface.width
                        && c.input_height == surface.height
                        && c.output_width == output_width
                        && c.output_height == output_height
                        && c.backbuffer_identity == bb_identity =>
                {
                    c
                }
                slot => {
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
                    let enumerator = self
                        .video_device
                        .CreateVideoProcessorEnumerator(&content_desc)?;
                    let processor =
                        self.video_device.CreateVideoProcessor(&enumerator, 0)?;

                    let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    };
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

                    *slot = Some(VideoProcessorCache {
                        enumerator,
                        processor,
                        output_view,
                        input_width: surface.width,
                        input_height: surface.height,
                        output_width,
                        output_height,
                        backbuffer_identity: bb_identity,
                    });
                    slot.as_mut().unwrap()
                }
            };

            let rotation_quarter_turns = view.rotation_quarter_turns % 4;
            let (display_width, display_height) = if rotation_quarter_turns % 2 == 1 {
                (surface.height, surface.width)
            } else {
                (surface.width, surface.height)
            };
            let base_rect = aspect_fit_rect(
                display_width,
                display_height,
                output_width,
                output_height,
            );
            let (source_rect, dest_rect) = compute_zoomed_rects(
                &base_rect,
                view,
                surface.width,
                surface.height,
                output_width,
                output_height,
            );

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

            let mut input_view = None;
            self.video_device.CreateVideoProcessorInputView(
                &surface.texture,
                &cache.enumerator,
                &input_desc,
                Some(&mut input_view),
            )?;
            let input_view =
                input_view.ok_or(D3D11Error("CreateVideoProcessorInputView returned no view"))?;

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
                &cache.processor,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                BOOL(0),
                None,
            );
            self.video_context.VideoProcessorSetStreamRotation(
                &cache.processor,
                0,
                BOOL(1),
                match rotation_quarter_turns {
                    1 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
                    2 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
                    3 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
                    _ => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
                },
            );
            self.video_context.VideoProcessorSetStreamSourceRect(
                &cache.processor,
                0,
                BOOL(1),
                Some(&source_rect),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &cache.processor,
                0,
                BOOL(1),
                Some(&dest_rect),
            );
            self.video_context.VideoProcessorSetOutputTargetRect(
                &cache.processor,
                BOOL(1),
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: output_width as i32,
                    bottom: output_height as i32,
                }),
            );
            // VideoProcessorBlt borrows the stream array. The pInputSurface
            // field is ManuallyDrop so its COM reference is never released
            // on drop — we explicitly drop it afterwards so the kernel-mode
            // input view is freed every frame.
            let mut streams = [stream];
            self.video_context
                .VideoProcessorBlt(&cache.processor, &cache.output_view, 0, &streams)?;
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
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

    pub(crate) fn create_timeline_overlay(
        &self,
        model: &crate::render::timeline::TimelineOverlayModel,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_timeline_bitmap(model)? else {
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
        let vertices = timeline_quad_vertices(bitmap.width, bitmap.height, model.viewport_height);
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
                    .ok_or(D3D11Error("CreateTexture2D returned no timeline texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        Ok(Some(SubtitleOverlay {
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no timeline view",
            ))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no timeline vertex buffer"))?,
        }))
    }

    pub(crate) fn create_volume_overlay(
        &self,
        text: &str,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_volume_bitmap(text)? else {
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
        let vertices = volume_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
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
                    .ok_or(D3D11Error("CreateTexture2D returned no volume texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device
                .CreateBuffer(&vertex_buffer_desc, Some(&vertex_buffer_data), Some(&mut vertex_buffer))?;
        }

        Ok(Some(SubtitleOverlay {
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no volume view",
            ))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no volume vertex buffer"))?,
        }))
    }

    pub(crate) fn render_subtitle_overlay(
        &self,
        renderer: &SubtitleRenderer,
        overlay: &SubtitleOverlay,
        render_target: &RenderTargetView,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn Error>> {
        let stride = size_of::<SubtitleVertex>() as u32;
        let offset = 0u32;
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: viewport_width.max(1) as f32,
            Height: viewport_height.max(1) as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };

        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(render_target.view.clone())]), None);
            self.context.RSSetViewports(Some(&[viewport]));
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

fn timeline_quad_vertices(
    _overlay_width: u32,
    overlay_height: u32,
    viewport_height: u32,
) -> [SubtitleVertex; 6] {
    let top_px = (viewport_height as i32 - overlay_height as i32 - 10).max(0) as f32;
    let bottom_px = (top_px + overlay_height as f32).min(viewport_height as f32);
    let top = 1.0 - top_px / viewport_height as f32 * 2.0;
    let bottom = 1.0 - bottom_px / viewport_height as f32 * 2.0;

    [
        SubtitleVertex { position: [-1.0, top, 0.0], texcoord: [0.0, 0.0] },
        SubtitleVertex { position: [1.0, top, 0.0], texcoord: [1.0, 0.0] },
        SubtitleVertex { position: [-1.0, bottom, 0.0], texcoord: [0.0, 1.0] },
        SubtitleVertex { position: [-1.0, bottom, 0.0], texcoord: [0.0, 1.0] },
        SubtitleVertex { position: [1.0, top, 0.0], texcoord: [1.0, 0.0] },
        SubtitleVertex { position: [1.0, bottom, 0.0], texcoord: [1.0, 1.0] },
    ]
}

fn volume_quad_vertices(
    overlay_width: u32,
    overlay_height: u32,
    viewport_width: u32,
    viewport_height: u32,
) -> [SubtitleVertex; 6] {
    let margin = 16.0f32;
    let right_px = (viewport_width as f32 - margin).max(overlay_width as f32);
    let left_px = (right_px - overlay_width as f32).max(0.0);
    let top_px = margin;
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

fn aspect_fit_rect(
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) -> RECT {
    if source_width == 0 || source_height == 0 || output_width == 0 || output_height == 0 {
        return RECT {
            left: 0,
            top: 0,
            right: output_width as i32,
            bottom: output_height as i32,
        };
    }

    let source_aspect = source_width as f32 / source_height as f32;
    let output_aspect = output_width as f32 / output_height as f32;

    let (dest_width, dest_height) = if output_aspect > source_aspect {
        let height = output_height as f32;
        let width = height * source_aspect;
        (width.round() as i32, output_height as i32)
    } else {
        let width = output_width as f32;
        let height = width / source_aspect;
        (output_width as i32, height.round() as i32)
    };

    let left = ((output_width as i32 - dest_width) / 2).max(0);
    let top = ((output_height as i32 - dest_height) / 2).max(0);
    RECT {
        left,
        top,
        right: left + dest_width.max(1),
        bottom: top + dest_height.max(1),
    }
}

/// Computes clamped source and dest rects for the D3D11 video processor.
///
/// The video processor requires both rects to stay within their respective
/// texture bounds. When the view transform would push the dest rect outside
/// the output, we clip it and adjust the source rect proportionally so only
/// the visible portion of the video is sampled.
fn compute_zoomed_rects(
    base: &RECT,
    view: &crate::render::ViewTransform,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) -> (RECT, RECT) {
    let full_source = RECT {
        left: 0,
        top: 0,
        right: source_width as i32,
        bottom: source_height as i32,
    };

    if view.zoom == 1.0 && view.pan_x == 0.0 && view.pan_y == 0.0 {
        return (full_source, *base);
    }

    let bw = (base.right - base.left) as f32;
    let bh = (base.bottom - base.top) as f32;
    let cx = base.left as f32 + bw * 0.5;
    let cy = base.top as f32 + bh * 0.5;

    // Virtual dest rect (may exceed output bounds).
    let vw = bw * view.zoom;
    let vh = bh * view.zoom;
    let vl = cx - vw * 0.5 + view.pan_x;
    let vt = cy - vh * 0.5 + view.pan_y;

    // Clip the virtual rect to the output bounds.
    let out_w = output_width as f32;
    let out_h = output_height as f32;
    let cl = vl.max(0.0);
    let ct = vt.max(0.0);
    let cr = (vl + vw).min(out_w);
    let cb = (vt + vh).min(out_h);

    if cr <= cl || cb <= ct {
        // Entirely off-screen — present nothing.
        return (
            RECT { left: 0, top: 0, right: 1, bottom: 1 },
            RECT { left: 0, top: 0, right: 0, bottom: 0 },
        );
    }

    // Map the clipped region back to source texture coordinates.
    let sw = source_width as f32;
    let sh = source_height as f32;
    let sl = ((cl - vl) / vw) * sw;
    let st = ((ct - vt) / vh) * sh;
    let sr = ((cr - vl) / vw) * sw;
    let sb = ((cb - vt) / vh) * sh;

    let source_rect = RECT {
        left: (sl.round() as i32).clamp(0, source_width as i32),
        top: (st.round() as i32).clamp(0, source_height as i32),
        right: (sr.round() as i32).clamp(1, source_width as i32),
        bottom: (sb.round() as i32).clamp(1, source_height as i32),
    };

    let dest_rect = RECT {
        left: cl.round() as i32,
        top: ct.round() as i32,
        right: cr.round() as i32,
        bottom: cb.round() as i32,
    };

    (source_rect, dest_rect)
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

fn render_timeline_bitmap(
    model: &crate::render::timeline::TimelineOverlayModel,
) -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    if model.viewport_width == 0 || model.viewport_height == 0 || model.duration_secs == 0 {
        return Ok(None);
    }

    let width = model.viewport_width;
    let height = crate::render::timeline::TIMELINE_HEIGHT_PX;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let layout = crate::render::timeline::layout(model.viewport_width, model.viewport_height);
    let track_top = (layout.track_top - layout.top).max(0) as u32;
    let track_bottom = (layout.track_bottom - layout.top).max(track_top as i32 + 1) as u32;
    let track_left = layout.track_left.max(0) as u32;
    let track_right = layout.track_right.max(layout.track_left + 1) as u32;

    fill_rect(
        &mut pixels,
        width,
        height,
        0,
        0,
        width,
        height,
        [12, 14, 18, 220],
    );
    fill_rect(
        &mut pixels,
        width,
        height,
        track_left,
        track_top,
        track_right,
        track_bottom,
        [255, 255, 255, 140],
    );
    fill_rect(
        &mut pixels,
        width,
        height,
        track_left,
        track_top,
        (track_left + model.played_px).min(track_right),
        track_bottom,
        [255, 255, 255, 255],
    );
    fill_circle(
        &mut pixels,
        width,
        height,
        model.handle_center_x.clamp(layout.track_left, layout.track_right) as u32,
        track_top + ((track_bottom - track_top) / 2),
        9,
        [12, 14, 18, 255],
    );
    fill_circle(
        &mut pixels,
        width,
        height,
        model.handle_center_x.clamp(layout.track_left, layout.track_right) as u32,
        track_top + ((track_bottom - track_top) / 2),
        7,
        [255, 255, 255, 245],
    );

    let left_label = match model.preview_position_secs {
        Some(preview_secs) => format!(
            "{} -> {}",
            crate::render::timeline::format_timestamp(model.current_position_secs),
            crate::render::timeline::format_timestamp(preview_secs)
        ),
        None => crate::render::timeline::format_timestamp(model.current_position_secs),
    };
    let right_label = crate::render::timeline::format_timestamp(model.duration_secs);

    draw_timeline_label(&mut pixels, width, height, &left_label, true)?;
    if model.loop_enabled {
        let right_label = format!("\u{27F3}  {right_label}");
        draw_timeline_label(&mut pixels, width, height, &right_label, false)?;
    } else {
        draw_timeline_label(&mut pixels, width, height, &right_label, false)?;
    }

    Ok(Some(SubtitleBitmap { width, height, pixels }))
}

fn render_volume_bitmap(text: &str) -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    if text.trim().is_empty() {
        return Ok(None);
    }

    let padding_x = 14i32;
    let padding_y = 8i32;
    let mut text_rect = RECT {
        left: 0,
        top: 0,
        right: 160,
        bottom: 0,
    };
    let mut text_wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Box::new(D3D11Error("CreateCompatibleDC returned null")));
        }

        let font = CreateFontW(
            -18,
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
            DT_CALCRECT | DT_RIGHT | DT_SINGLELINE | DT_NOPREFIX,
        );

        let bitmap_width = (text_rect.right - text_rect.left + padding_x * 2).max(1) as u32;
        let bitmap_height = (text_rect.bottom - text_rect.top + padding_y * 2).max(1) as u32;
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
            return Err(Box::new(D3D11Error("CreateDIBSection failed for volume overlay")));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (bitmap_width * bitmap_height * 4) as usize);
        let source = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (bitmap_width * bitmap_height * 4) as usize);
        fill_rect(
            source,
            bitmap_width,
            bitmap_height,
            0,
            0,
            bitmap_width,
            bitmap_height,
            [12, 14, 18, 208],
        );

        let mut draw_rect = RECT {
            left: padding_x,
            top: padding_y,
            right: bitmap_width as i32 - padding_x,
            bottom: bitmap_height as i32 - padding_y,
        };
        let _ = DrawTextW(
            dc,
            &mut text_wide,
            &mut draw_rect,
            DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        let source: &[u8] =
            std::slice::from_raw_parts(bits.cast::<u8>(), (bitmap_width * bitmap_height * 4) as usize);
        let mut pixels = vec![0u8; source.len()];
        pixels.copy_from_slice(source);
        for px in pixels.chunks_exact_mut(4) {
            let intensity = px[0].max(px[1]).max(px[2]);
            if intensity > 0 && px[3] == 0 {
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
                px[3] = intensity.max(190);
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

fn draw_timeline_label(
    destination_pixels: &mut [u8],
    width: u32,
    height: u32,
    text: &str,
    align_left: bool,
) -> Result<(), Box<dyn Error>> {
    if text.is_empty() {
        return Ok(());
    }

    let mut text_wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut draw_rect = RECT {
        left: 12,
        top: 0,
        right: width as i32 - 12,
        bottom: 18,
    };
    let draw_flags = if align_left {
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX
    } else {
        DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX
    };

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Box::new(D3D11Error("CreateCompatibleDC returned null")));
        }

        let font = CreateFontW(
            -14,
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
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
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
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
            return Err(Box::new(D3D11Error("CreateDIBSection failed for timeline label")));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (width * height * 4) as usize);
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let _ = DrawTextW(dc, &mut text_wide, &mut draw_rect, draw_flags);

        let source =
            std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        for (source_px, dest_px) in source.chunks_exact(4).zip(destination_pixels.chunks_exact_mut(4))
        {
            let intensity = source_px[0].max(source_px[1]).max(source_px[2]);
            if intensity == 0 {
                continue;
            }

            dest_px[0] = 255;
            dest_px[1] = 255;
            dest_px[2] = 255;
            dest_px[3] = dest_px[3].max(intensity.max(170));
        }

        let _ = SelectObject(dc, old_bitmap);
        let _ = SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
    }

    Ok(())
}

fn fill_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    color: [u8; 4],
) {
    let left = left.min(width);
    let right = right.min(width);
    let top = top.min(height);
    let bottom = bottom.min(height);

    for y in top..bottom {
        for x in left..right {
            let offset = ((y * width + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn fill_circle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    center_x: u32,
    center_y: u32,
    radius: u32,
    color: [u8; 4],
) {
    let radius_sq = (radius * radius) as i32;
    let min_x = center_x.saturating_sub(radius) as i32;
    let max_x = (center_x + radius).min(width.saturating_sub(1)) as i32;
    let min_y = center_y.saturating_sub(radius) as i32;
    let max_y = (center_y + radius).min(height.saturating_sub(1)) as i32;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x - center_x as i32;
            let dy = y - center_y as i32;
            if dx * dx + dy * dy > radius_sq {
                continue;
            }

            let offset = (((y as u32) * width + x as u32) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}
