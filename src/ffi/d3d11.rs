use std::{
    error::Error,
    ffi::c_void,
    fmt,
    mem::{size_of, ManuallyDrop},
    ptr::null_mut,
    sync::{Arc, Mutex},
};

use windows::{
    core::{Interface, PCSTR},
    Win32::{
        Foundation::{BOOL, COLORREF, RECT},
        Graphics::{
            Direct3D::{
                Fxc::D3DCompile, ID3DBlob, D3D11_SRV_DIMENSION_TEXTURE2D, D3D_DRIVER_TYPE_HARDWARE,
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            },
            Direct3D11::{
                D3D11CreateDevice, ID3D11BlendState, ID3D11Buffer, ID3D11Device,
                ID3D11DeviceContext, ID3D11InputLayout, ID3D11Multithread, ID3D11PixelShader,
                ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
                ID3D11Texture2D, ID3D11VertexShader, ID3D11VideoContext, ID3D11VideoContext1,
                ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
                ID3D11VideoProcessorEnumerator1, ID3D11VideoProcessorOutputView,
                D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE,
                D3D11_BIND_VERTEX_BUFFER, D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA,
                D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA, D3D11_BUFFER_DESC,
                D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CPU_ACCESS_READ, D3D11_CPU_ACCESS_WRITE,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAPPED_SUBRESOURCE,
                D3D11_MAP_READ, D3D11_MAP_WRITE_DISCARD, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
                D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
                D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_SRV, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
                D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT,
                D3D11_USAGE_DYNAMIC, D3D11_USAGE_IMMUTABLE, D3D11_USAGE_STAGING,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270,
                D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
                D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VIEWPORT,
                D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
            },
            Dxgi::Common::{
                DXGI_COLOR_SPACE_TYPE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
                DXGI_FORMAT_P010, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16G16_UNORM,
                DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R32G32B32_FLOAT, DXGI_FORMAT_R32G32_FLOAT,
                DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
            },
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
                DrawTextW, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER,
                BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
                DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
                DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FF_DONTCARE, FW_MEDIUM, FW_SEMIBOLD,
                HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
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
    // Field order is drop order, and these device children (the immediate
    // context, video device, and video context) hold references back to the
    // ID3D11Device. The device must outlive them, so `device` is declared
    // AFTER the contexts and released last. Declaring it first releases the
    // device before its children, whose final Release then touches a destroyed
    // device — a use-after-free that only surfaces once this is the last
    // surviving device clone (decode worker exited, surfaces already freed).
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    device: ID3D11Device,
    /// Serializes all immediate-context operations across threads.
    ///
    /// `ID3D11Multithread::SetMultithreadProtected` only covers
    /// `ID3D11DeviceContext` methods.  `ID3D11VideoContext` methods
    /// (FFmpeg's D3D11VA DecoderBeginFrame/SubmitDecoderBuffers/
    /// DecoderEndFrame, and our VideoProcessorBlt/Set*) are **not**
    /// covered by that CritSec.  Without this lock, the decode worker
    /// and UI render thread race on the same underlying immediate
    /// context, causing access violations inside d3d11.dll.
    context_lock: Arc<Mutex<()>>,
}

pub struct RenderTargetView {
    view: ID3D11RenderTargetView,
}

pub(crate) struct SubtitleOverlay {
    texture: ID3D11Texture2D,
    shader_resource_view: ID3D11ShaderResourceView,
    vertex_buffer: ID3D11Buffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) struct SubtitleRenderer {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    sampler: ID3D11SamplerState,
    blend_state: ID3D11BlendState,
}

/// GPU objects for the HDR→SDR tone-map path: a full-screen-quad shader pair
/// that samples the decoded NV12/P010 frame directly and does the transfer
/// math itself (see `HDR_TONE_MAP_PIXEL_SHADER`).
///
/// This exists because the D3D11 video processor cannot do the job: NVIDIA
/// advertises no HLG (`GHLG`) input conversion at all, and offers PQ only to
/// linear-scRGB or HDR10 outputs, never to the gamma-2.2 sRGB an 8-bit SDR
/// backbuffer scans out. Doing the EOTF, tone curve, gamut, and encode in a
/// shader is both portable and exact.
///
/// The vertex and constant buffers are `DYNAMIC` and rewritten each frame
/// (geometry follows the window/zoom/rotation; the constants follow the
/// stream), so the whole path allocates nothing per frame.
pub(crate) struct HdrToneMapRenderer {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    sampler: ID3D11SamplerState,
    vertex_buffer: ID3D11Buffer,
    constant_buffer: ID3D11Buffer,
}

/// Mirror of the tone-map shader's `cbuffer`. Two `float4`s: HLSL constant
/// buffers are laid out in 16-byte registers, so the size must stay a
/// multiple of 16 and no field may straddle a register boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ToneMapParams {
    /// `[y_offset, y_scale, c_offset, c_scale]` — normalizes raw YCbCr samples
    /// to Y in [0,1] and Cb/Cr in [-0.5,0.5]. Bit-depth and range dependent,
    /// so it is computed on the CPU (see [`ToneMapParams::new`]) rather than
    /// branched on in the shader.
    range: [f32; 4],
    /// `[sample_scale, transfer, knee, unused]`.
    params: [f32; 4],
}

/// Where the tone curve stops being the identity. Below this (in units of
/// diffuse white) HDR and SDR agree, so the shader passes those values
/// through untouched and only the highlights above it get compressed. 0.75
/// keeps all of skin, sky, and midtones bit-exact and reserves the top
/// quarter of the range for specular roll-off.
const TONE_MAP_KNEE: f32 = 0.75;

impl ToneMapParams {
    /// `ten_bit` selects the studio-range levels and the P010 sample scaling;
    /// it must reflect the *texture* format, not the stream's nominal depth.
    fn new(signal: crate::render::hdr::HdrToneMapSignal, ten_bit: bool) -> Self {
        // Studio (limited) range levels, per BT.709/BT.2020, scaled to the
        // bit depth actually stored in the texture. Expressed as normalized
        // code values so the shader stays depth-agnostic.
        let (max_code, black, white_span, chroma_center, chroma_span) = if ten_bit {
            (1023.0, 64.0, 876.0, 512.0, 896.0)
        } else {
            (255.0, 16.0, 219.0, 128.0, 224.0)
        };

        let range = if signal.full_range {
            // Full range: luma already spans the code range, and chroma is
            // centered on the midpoint code with no headroom/footroom. The
            // center is `chroma_center / max_code` (128/255, 512/1023), not
            // exactly 0.5 — the same convention as full-range (JPEG) YCbCr.
            [0.0, 1.0, chroma_center / max_code, 1.0]
        } else {
            [
                black / max_code,
                max_code / white_span,
                chroma_center / max_code,
                max_code / chroma_span,
            ]
        };

        // P010 stores each 10-bit code in the *high* bits of a 16-bit word, so
        // a UNORM fetch returns code * 64 / 65535, not code / 1023. Rescale so
        // the shader sees a value normalized against the 10-bit maximum.
        // NV12 is already normalized against 255 by the UNORM fetch.
        let sample_scale = if ten_bit { 65535.0 / 65472.0 } else { 1.0 };

        let transfer = match signal.transfer {
            crate::render::hdr::HdrTransfer::Pq => 0.0,
            crate::render::hdr::HdrTransfer::Hlg => 1.0,
        };

        Self {
            range,
            params: [sample_scale, transfer, TONE_MAP_KNEE, 0.0],
        }
    }
}

/// Colorimetry of a decoded frame, reduced to what the D3D11 video processor
/// can express: which YCbCr→RGB matrix applies and whether the samples use
/// the full 0–255 range or the limited 16–235 studio range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceColor {
    /// BT.709 matrix when true, BT.601 otherwise.
    pub(crate) bt709: bool,
    /// Full-range (0–255) samples when true, limited/studio (16–235) otherwise.
    pub(crate) full_range: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct VideoSurface {
    texture: ID3D11Texture2D,
    subresource_index: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sar_num: u32,
    pub(crate) sar_den: u32,
    pub(crate) color: SurfaceColor,
    /// When `Some`, this frame carries HDR content that must be tone-mapped
    /// to SDR by the video processor: the value is the decoded stream's DXGI
    /// input color space (PQ or HLG), set through `ID3D11VideoContext1`
    /// with an sRGB output space so the driver performs HDR→SDR conversion.
    /// `None` is the pixel-verified SDR path, where `color` alone drives the
    /// legacy matrix/range configuration. Constant for every frame of one
    /// opened file (resolved once at decoder open).
    pub(crate) hdr_tone_map: Option<DXGI_COLOR_SPACE_TYPE>,
}

impl VideoSurface {
    /// Display dimensions: coded size corrected by the sample aspect ratio.
    /// Width carries the SAR scaling (`width * sar_num / sar_den`); height is
    /// unchanged. SAR 1:1 (or unknown) leaves display == coded; anamorphic
    /// content like PAL 720×576 with SAR 64:45 yields display 1024×576.
    ///
    /// This is the single source of truth for display aspect — window sizing
    /// and the render's aspect-fit both use it, so their aspect ratios stay
    /// bit-identical and never disagree by a rounding pixel.
    pub(crate) fn display_size(&self) -> (u32, u32) {
        (self.width * self.sar_num / self.sar_den, self.height)
    }
}

/// Cached D3D11 video processor objects reused across frames when the
/// input/output dimensions and backbuffer identity haven't changed.
/// Avoids per-frame kernel-mode allocations that stress the GPU driver.
///
/// Input views are deliberately *not* cached: every decoded frame is copied
/// into a freshly allocated texture (subresource 0), so a texture-identity
/// cache key never produces a correct hit — it could only ever match a stale
/// entry after the allocator reused a freed texture's address, which is a
/// crash hazard, not a win. Each frame creates and drops its own input view.
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

pub(crate) struct BgraFrameCapture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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

/// Unmaps a mapped subresource when dropped, so a successful `Map` is always
/// paired with an `Unmap` even when reading the mapped data returns early.
struct MapGuard<'a> {
    context: &'a ID3D11DeviceContext,
    resource: &'a ID3D11Texture2D,
    subresource: u32,
}

impl Drop for MapGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: paired with a successful Map of this subresource; the caller
        // holds the context lock for the lifetime of this guard.
        unsafe {
            self.context.Unmap(self.resource, self.subresource);
        }
    }
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
            context_lock: Arc::new(Mutex::new(())),
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

    pub(crate) fn capture_bgra_texture(
        &self,
        texture: &ID3D11Texture2D,
    ) -> Result<BgraFrameCapture, Box<dyn Error>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if desc.Width == 0 || desc.Height == 0 {
            return Err(Box::new(D3D11Error(
                "cannot capture an empty D3D11 texture",
            )));
        }

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut staging = None;
        unsafe {
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        }
        let staging = staging.ok_or(D3D11Error("CreateTexture2D returned no staging texture"))?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let _lock = self.context_lock.lock().unwrap_or_else(|e| e.into_inner());
        let row_bytes = desc.Width as usize * 4;
        let mut pixels = vec![0u8; row_bytes * desc.Height as usize];
        unsafe {
            self.context.CopyResource(&staging, texture);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            // From here a successful Map must be released on every path. The
            // guard's Unmap runs before `_lock` is released (reverse drop order).
            let _unmap = MapGuard {
                context: &self.context,
                resource: &staging,
                subresource: 0,
            };

            // Never assume the mapping is tightly packed as width*4: the driver
            // may align each row to a wider RowPitch. We copy row-by-row using
            // RowPitch below, but a RowPitch *smaller* than one packed row would
            // make the final row read past the mapped region — reject it.
            if (mapped.RowPitch as usize) < row_bytes {
                return Err(Box::new(D3D11Error(
                    "mapped RowPitch smaller than one packed row of pixels",
                )));
            }

            for row in 0..desc.Height as usize {
                let src = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
                let dst = pixels.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(src, dst, row_bytes);
            }
        }

        Ok(BgraFrameCapture {
            width: desc.Width,
            height: desc.Height,
            pixels,
        })
    }

    /// Returns `true` if the D3D11 device has been removed (GPU TDR or driver
    /// reset).  Must be checked **before** issuing any rendering commands so
    /// that stale COM objects in the video processor cache are never touched.
    pub(crate) fn is_device_removed(&self) -> bool {
        // SAFETY: GetDeviceRemovedReason is a pure query with no side effects.
        let hr = unsafe { self.device.GetDeviceRemovedReason() };
        hr.is_err()
    }

    /// Acquire the context lock.  Must be held around any operation that
    /// touches the `ID3D11VideoContext` (FFmpeg D3D11VA decode, video
    /// processor Blt, CopySubresourceRegion).
    pub(crate) fn lock_context(&self) -> std::sync::MutexGuard<'_, ()> {
        self.context_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn raw_device(&self) -> &ID3D11Device {
        &self.device
    }

    pub(crate) fn raw_device_ptr(&self) -> *mut c_void {
        // SAFETY: as_raw() borrows the COM pointer without incrementing
        // the reference count.  FFmpeg's av_hwdevice_ctx_init calls
        // AddRef internally on the device it receives, so handing it
        // an un-AddRef'd pointer is correct — the previous clone()
        // + into_raw() leaked one COM reference per call.
        self.device.as_raw()
    }

    /// Whether the device exposes `ID3D11VideoContext1` (required for the
    /// HDR color-space APIs). A read-only QueryInterface; never called on
    /// the SDR render path.
    pub(crate) fn video_context1_available(&self) -> bool {
        self.video_context.cast::<ID3D11VideoContext1>().is_ok()
    }

    /// HDR-only video processor configuration for the `Hdr10Passthrough`
    /// path. The verified SDR configuration in `render_video_surface` is a
    /// separate, untouched code path and never calls this.
    ///
    /// Both color spaces come from the `verified_*` helpers in
    /// `render::hdr`: resolved for standard HDR10 and pixel-validated
    /// through the identical `Set*ColorSpace1` calls in
    /// `hdr10_validation_blt` (`bench/verify-colors-pq.ps1`). Non-HDR10
    /// signals (HLG, constant-luminance / non-BT.2020 matrices, full-range
    /// PQ) still return typed errors from those helpers, never a panic.
    // Wired into the HDR render path by the passthrough commit.
    #[allow(dead_code)]
    pub(crate) fn configure_hdr10_video_processor_skeleton(
        &self,
        processor: &ID3D11VideoProcessor,
        content: &crate::render::hdr::ContentColorInfo,
    ) -> Result<(), Box<dyn Error>> {
        let video_context1: ID3D11VideoContext1 = self
            .video_context
            .cast()
            .map_err(|_| crate::render::hdr::HdrError::VideoContext1Unavailable)?;

        // Resolved for standard HDR10 (YCBCR_STUDIO_G2084_LEFT_P2020 in,
        // RGB_FULL_G2084_NONE_P2020 out); anything else is a typed error
        // from the helpers.
        let stream_color_space = crate::render::hdr::verified_hdr_stream_color_space(content)?;
        let output_color_space = crate::render::hdr::verified_hdr10_processor_output_color_space()?;

        // SAFETY: the processor belongs to this device; like the SDR Set*
        // calls, the caller must hold context_lock to serialise video
        // context access across threads.
        unsafe {
            let _lock = self.context_lock.lock().unwrap_or_else(|e| e.into_inner());
            video_context1.VideoProcessorSetStreamColorSpace1(processor, 0, stream_color_space);
            video_context1.VideoProcessorSetOutputColorSpace1(processor, output_color_space);
        }
        Ok(())
    }

    /// Structural capability check for HDR format conversion through the
    /// video processor. Takes the actual decoded input format and the
    /// intended color spaces/output format — interface availability alone
    /// never counts as support.
    ///
    /// The color-space arguments must come from the `verified_*` helpers,
    /// which are typed errors until resolved, so this cannot run with
    /// guessed values.
    // Wired into capability probing by the passthrough commit.
    #[allow(dead_code)]
    pub(crate) fn check_hdr_format_conversion(
        &self,
        enumerator: &ID3D11VideoProcessorEnumerator,
        input_format: DXGI_FORMAT,
        input_color_space: DXGI_COLOR_SPACE_TYPE,
        output_format: DXGI_FORMAT,
        output_color_space: DXGI_COLOR_SPACE_TYPE,
    ) -> Result<bool, Box<dyn Error>> {
        let enumerator1: ID3D11VideoProcessorEnumerator1 = enumerator
            .cast()
            .map_err(|_| crate::render::hdr::HdrError::VideoProcessorEnumerator1Unavailable)?;
        // SAFETY: read-only capability query on a live enumerator from this
        // device.
        let supported = unsafe {
            enumerator1.CheckVideoProcessorFormatConversion(
                input_format,
                input_color_space,
                output_format,
                output_color_space,
            )?
        };
        Ok(supported.as_bool())
    }

    /// Dev-only HDR10 validation blt (`bench/verify-colors-pq.ps1`): renders
    /// one NV12 surface into an R10G10B10A2 backbuffer with the resolved
    /// HDR10 color spaces set through `ID3D11VideoContext1`. This is the
    /// prototype of the future passthrough render path; the verified SDR
    /// `render_video_surface` above is untouched and shares no code with it.
    ///
    /// `stream_color_space_override` exists solely for the harness's
    /// negative control (deliberately wrong input space must produce a
    /// pixel FAIL). When it is set, the structural format-conversion check
    /// is logged but not enforced, so the wrong value demonstrably reaches
    /// the blt.
    // Called only by the env-gated validation entry (render::hdr_validate).
    #[allow(dead_code)]
    pub(crate) fn hdr10_validation_blt(
        &self,
        surface: &VideoSurface,
        backbuffer: &ID3D11Texture2D,
        output_width: u32,
        output_height: u32,
        content: &crate::render::hdr::ContentColorInfo,
        stream_color_space_override: Option<DXGI_COLOR_SPACE_TYPE>,
    ) -> Result<(), Box<dyn Error>> {
        let output_color_space = crate::render::hdr::verified_hdr10_processor_output_color_space()?;
        let stream_color_space = match stream_color_space_override {
            Some(wrong) => wrong,
            None => crate::render::hdr::verified_hdr_stream_color_space(content)?,
        };

        let video_context1: ID3D11VideoContext1 = self
            .video_context
            .cast()
            .map_err(|_| crate::render::hdr::HdrError::VideoContext1Unavailable)?;

        // SAFETY: same contracts as render_video_surface — all objects are
        // created from this device, the views reference live textures, and
        // every ID3D11VideoContext call runs under context_lock.
        unsafe {
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

            // Structural oracle: the driver accepts or rejects the exact
            // (format, color space) conversion pair.
            let conversion_supported = self.check_hdr_format_conversion(
                &enumerator,
                DXGI_FORMAT_NV12,
                stream_color_space,
                DXGI_FORMAT_R10G10B10A2_UNORM,
                output_color_space,
            )?;
            flog!(
                "[hdr-validate] CheckVideoProcessorFormatConversion NV12({:?}) -> \
                 R10G10B10A2({:?}): {}",
                stream_color_space,
                output_color_space,
                conversion_supported
            );
            if !conversion_supported && stream_color_space_override.is_none() {
                return Err(crate::render::hdr::HdrError::HdrFormatConversionUnsupported.into());
            }

            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;

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
                "CreateVideoProcessorOutputView returned no HDR view",
            ))?;

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
                &enumerator,
                &input_desc,
                Some(&mut input_view),
            )?;
            let input_view = input_view.ok_or(D3D11Error(
                "CreateVideoProcessorInputView returned no HDR view",
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

            let full_target = RECT {
                left: 0,
                top: 0,
                right: output_width as i32,
                bottom: output_height as i32,
            };
            let mut streams = [stream];
            let blt_result = {
                let _lock = self.context_lock.lock().unwrap_or_else(|e| e.into_inner());
                video_context1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    stream_color_space,
                );
                video_context1.VideoProcessorSetOutputColorSpace1(&processor, output_color_space);
                self.video_context.VideoProcessorSetStreamOutputRate(
                    &processor,
                    0,
                    D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                    BOOL(0),
                    None,
                );
                // Full-frame stretch: the validation window is created at the
                // video's exact size, so source and dest map 1:1.
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &processor,
                    0,
                    BOOL(1),
                    Some(&RECT {
                        left: 0,
                        top: 0,
                        right: surface.width as i32,
                        bottom: surface.height as i32,
                    }),
                );
                self.video_context.VideoProcessorSetStreamDestRect(
                    &processor,
                    0,
                    BOOL(1),
                    Some(&full_target),
                );
                self.video_context.VideoProcessorSetOutputTargetRect(
                    &processor,
                    BOOL(1),
                    Some(&full_target),
                );
                self.video_context
                    .VideoProcessorBlt(&processor, &output_view, 0, &streams)
            };
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            blt_result?;
        }
        Ok(())
    }

    pub(crate) unsafe fn surface_from_raw_texture(
        &self,
        texture: *mut c_void,
        subresource_index: u32,
        width: u32,
        height: u32,
        sar_num: u32,
        sar_den: u32,
        color: SurfaceColor,
        hdr_tone_map: Option<DXGI_COLOR_SPACE_TYPE>,
    ) -> Result<VideoSurface, Box<dyn Error>> {
        // Guard: if the device was removed (GPU TDR) bail out before touching
        // any D3D11 objects.  Without this the worker thread crashes inside
        // d3d11.dll when calling CopySubresourceRegion on a dead device.
        if self.is_device_removed() {
            return Err(Box::new(D3D11Error(
                "D3D11 device removed (TDR) during surface copy",
            )));
        }

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
        let owned_texture =
            owned_texture.ok_or(D3D11Error("CreateTexture2D returned no copy texture"))?;

        // SAFETY: both textures belong to the same D3D11 device. The source
        // subresource index selects one slice from the decoder's texture
        // array; the destination is a standalone single-slice texture.
        // Caller must hold context_lock — this method does NOT acquire it
        // internally because the lock must also cover the preceding
        // avcodec_receive_frame (which uses the video context for D3D11VA).
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
        // No GPU flush here. This copy, the next frame's decode (which reuses
        // the source pool slot after av_frame_unref), and the UI thread's
        // later VideoProcessorBlt of `owned_texture` are all submitted to the
        // same immediate context under context_lock, so the GPU executes them
        // in submission order — the copy is guaranteed to finish before either
        // the slot is overwritten or the result is read. Blocking the worker
        // (and thus the lock, and thus UI presentation) on a per-frame event
        // query was a band-aid for the decoder-teardown race now fixed in
        // CodecContext::Drop; it is unnecessary for correctness.

        Ok(VideoSurface {
            texture: owned_texture,
            subresource_index: 0,
            width,
            height,
            sar_num,
            sar_den,
            color,
            hdr_tone_map,
        })
    }

    /// Present one SDR frame through the D3D11 video processor. This is the
    /// pixel-verified path; HDR goes through
    /// [`Self::render_video_surface_tone_mapped`] instead, because the video
    /// processor cannot convert HDR to the 8-bit sRGB backbuffer (see
    /// [`HdrToneMapRenderer`]).
    pub(crate) fn render_video_surface(
        &self,
        surface: &VideoSurface,
        backbuffer: &ID3D11Texture2D,
        output_width: u32,
        output_height: u32,
        view: &crate::render::ViewTransform,
        vp_cache: &mut Option<VideoProcessorCache>,
    ) -> Result<(), Box<dyn Error>> {
        if surface.hdr_tone_map.is_some() {
            return Err(Box::new(D3D11Error(
                "HDR surface routed to the SDR video-processor path",
            )));
        }

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

                    let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;

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
            let (disp_w, disp_h) = surface.display_size();
            let (display_width, display_height) = if rotation_quarter_turns % 2 == 1 {
                (disp_h, disp_w)
            } else {
                (disp_w, disp_h)
            };
            let base_rect =
                aspect_fit_rect(display_width, display_height, output_width, output_height);
            let (source_rect, dest_rect) = compute_zoomed_rects(
                &base_rect,
                view,
                surface.width,
                surface.height,
                output_width,
                output_height,
                rotation_quarter_turns,
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

            // Create a fresh input view for this frame's texture. It is moved
            // into the stream below and dropped right after the Blt, so the
            // kernel-mode view is allocated and freed within this call.
            let mut new_view = None;
            self.video_device.CreateVideoProcessorInputView(
                &surface.texture,
                &cache.enumerator,
                &input_desc,
                Some(&mut new_view),
            )?;
            let input_view =
                new_view.ok_or(D3D11Error("CreateVideoProcessorInputView returned no view"))?;

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

            // The context_lock serialises all ID3D11VideoContext calls
            // with CopySubresourceRegion on worker threads.
            // ID3D11VideoContext methods (Set*, Blt) are NOT covered by
            // SetMultithreadProtected; without the lock the two threads
            // race on the same underlying immediate context, crashing
            // in d3d11.dll.
            let mut streams = [stream];
            let blt_result = {
                let _lock = self.context_lock.lock().unwrap_or_else(|e| e.into_inner());
                // The pixel-verified SDR configuration. Without an explicit
                // color space the driver guesses the YCbCr matrix and nominal
                // range; guessing wrong (e.g. treating limited-range 16–235
                // video as full range) washes out blacks and dulls color.
                // D3D11_VIDEO_PROCESSOR_COLOR_SPACE is a bitfield, LSB
                // first: Usage:1 RGB_Range:1 YCbCr_Matrix:1 YCbCr_xvYCC:1
                // Nominal_Range:2. YCbCr_Matrix 1 = BT.709, 0 = BT.601;
                // Nominal_Range 1 = 16–235, 2 = 0–255.
                let input_color_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: ((surface.color.bt709 as u32) << 2)
                        | (if surface.color.full_range { 2 } else { 1 } << 4),
                };
                // Output is the BGRA backbuffer: playback usage, full-range
                // RGB. Full-range output is coupled to the 8-bit
                // B8G8R8A8_UNORM swapchain format (see dxgi.rs); a future
                // 10-bit/HDR backbuffer must update this color space in
                // lockstep.
                let output_color_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                self.video_context.VideoProcessorSetStreamColorSpace(
                    &cache.processor,
                    0,
                    &input_color_space,
                );
                self.video_context
                    .VideoProcessorSetOutputColorSpace(&cache.processor, &output_color_space);
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
                self.video_context.VideoProcessorBlt(
                    &cache.processor,
                    &cache.output_view,
                    0,
                    &streams,
                )
            };
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            // Surface a blit failure through the error path rather than letting
            // the caller present a stale/blank backbuffer: render_surface in
            // dxgi.rs propagates this Err *before* calling Present, and the
            // session turns it into a device-recovery attempt. Log at the
            // failure site so the cause is attributable in the trace.
            if let Err(error) = &blt_result {
                flog!("VideoProcessorBlt failed: {error}");
            }
            blt_result?;
        }

        Ok(())
    }

    /// Open-time gate for the HDR→SDR tone-map path: can a pixel shader
    /// actually sample the frames this decoder will produce?
    ///
    /// D3D11 exposes NV12/P010 to shaders only through per-plane views, and
    /// that is the one capability the shader path needs beyond a working
    /// device. Probed here on throwaway textures, at open, so an incapable
    /// device declines the file cleanly — a failure raised later, at the first
    /// draw, is misread by device recovery as device-lost and crash-loops.
    ///
    /// Both decode formats are required rather than only the one this file
    /// will use: the stream's bit depth is not reliably known at open
    /// (`bits_per_raw_sample` may be 0, and FFmpeg's pixel-format descriptors
    /// are not bound), and a hardware path can fall back to software mid-file,
    /// switching P010 to NV12 under us. Requiring both keeps the gate honest
    /// without guessing, and costs nothing real: NV12 plane views are
    /// universal, and any GPU that can decode 10-bit HEVC exposes P010 ones.
    pub(crate) fn supports_hdr_shader_tone_map(&self) -> bool {
        [DXGI_FORMAT_P010, DXGI_FORMAT_NV12]
            .into_iter()
            .all(|format| self.supports_plane_shader_views(format))
    }

    fn supports_plane_shader_views(&self, format: DXGI_FORMAT) -> bool {
        let Ok((luma_format, chroma_format)) = plane_srv_formats(format) else {
            return false;
        };

        // Even dimensions: chroma is half-resolution in both axes.
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 64,
            Height: 64,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_DECODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        // SAFETY: descriptor is fully initialized; the texture and any views
        // are local and dropped on return.
        let mut texture = None;
        unsafe {
            if self
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .is_err()
            {
                return false;
            }
        }
        let Some(texture) = texture else {
            return false;
        };

        [luma_format, chroma_format]
            .into_iter()
            .all(|format| create_plane_srv(&self.device, &texture, format).is_ok())
    }

    pub(crate) fn create_hdr_tone_map_renderer(
        &self,
    ) -> Result<HdrToneMapRenderer, Box<dyn Error>> {
        let vertex_blob = compile_shader(HDR_TONE_MAP_VERTEX_SHADER, b"main\0", b"vs_4_0\0")?;
        let pixel_blob = compile_shader(HDR_TONE_MAP_PIXEL_SHADER, b"main\0", b"ps_4_0\0")?;

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

        // Bilinear: this is what upsamples the half-resolution chroma plane.
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

        let vertex_buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: (size_of::<SubtitleVertex>() * 6) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let constant_buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of::<ToneMapParams>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };

        let vertex_bytecode = shader_blob_bytes(&vertex_blob);
        let pixel_bytecode = shader_blob_bytes(&pixel_blob);

        let mut vertex_shader = None;
        let mut pixel_shader = None;
        let mut input_layout = None;
        let mut sampler = None;
        let mut vertex_buffer = None;
        let mut constant_buffer = None;

        // SAFETY: all descriptors are fully initialized, the bytecode slices
        // outlive the calls that consume them, and every out-pointer targets a
        // local that lives for the duration of the call.
        unsafe {
            self.device
                .CreateVertexShader(vertex_bytecode, None, Some(&mut vertex_shader))?;
            self.device
                .CreatePixelShader(pixel_bytecode, None, Some(&mut pixel_shader))?;
            self.device.CreateInputLayout(
                &input_elements,
                vertex_bytecode,
                Some(&mut input_layout),
            )?;
            self.device
                .CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            self.device
                .CreateBuffer(&vertex_buffer_desc, None, Some(&mut vertex_buffer))?;
            self.device
                .CreateBuffer(&constant_buffer_desc, None, Some(&mut constant_buffer))?;
        }

        Ok(HdrToneMapRenderer {
            vertex_shader: vertex_shader
                .ok_or(D3D11Error("CreateVertexShader returned no tone-map shader"))?,
            pixel_shader: pixel_shader
                .ok_or(D3D11Error("CreatePixelShader returned no tone-map shader"))?,
            input_layout: input_layout
                .ok_or(D3D11Error("CreateInputLayout returned no tone-map layout"))?,
            sampler: sampler.ok_or(D3D11Error(
                "CreateSamplerState returned no tone-map sampler",
            ))?,
            vertex_buffer: vertex_buffer.ok_or(D3D11Error(
                "CreateBuffer returned no tone-map vertex buffer",
            ))?,
            constant_buffer: constant_buffer.ok_or(D3D11Error(
                "CreateBuffer returned no tone-map constant buffer",
            ))?,
        })
    }

    /// Present one HDR frame by tone-mapping it to SDR in a pixel shader.
    ///
    /// The SDR counterpart of this is [`Self::render_video_surface`], which
    /// blts through the video processor. Both compute their geometry the same
    /// way (aspect fit → zoom/pan → rotation), so an HDR and an SDR file of
    /// the same dimensions land on exactly the same pixels.
    pub(crate) fn render_video_surface_tone_mapped(
        &self,
        surface: &VideoSurface,
        renderer: &HdrToneMapRenderer,
        render_target: &RenderTargetView,
        output_width: u32,
        output_height: u32,
        view: &crate::render::ViewTransform,
    ) -> Result<(), Box<dyn Error>> {
        let color_space = surface.hdr_tone_map.ok_or(D3D11Error(
            "tone-map render path called with an SDR surface",
        ))?;
        let signal = crate::render::hdr::tone_map_signal(color_space)?;

        // SAFETY: `surface.texture` is a live texture owned by the surface.
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { surface.texture.GetDesc(&mut texture_desc) };

        let (luma_format, chroma_format) = plane_srv_formats(texture_desc.Format)?;
        let luma_view = create_plane_srv(&self.device, &surface.texture, luma_format)?;
        let chroma_view = create_plane_srv(&self.device, &surface.texture, chroma_format)?;
        let params = ToneMapParams::new(signal, texture_desc.Format == DXGI_FORMAT_P010);

        // Geometry, identical to the video-processor path.
        let rotation_quarter_turns = view.rotation_quarter_turns % 4;
        let (disp_w, disp_h) = surface.display_size();
        let (display_width, display_height) = if rotation_quarter_turns % 2 == 1 {
            (disp_h, disp_w)
        } else {
            (disp_w, disp_h)
        };
        let base_rect = aspect_fit_rect(display_width, display_height, output_width, output_height);
        let (source_rect, dest_rect) = compute_zoomed_rects(
            &base_rect,
            view,
            surface.width,
            surface.height,
            output_width,
            output_height,
            rotation_quarter_turns,
        );
        let vertices = tone_map_quad_vertices(
            &source_rect,
            &dest_rect,
            surface.width,
            surface.height,
            output_width,
            output_height,
            rotation_quarter_turns,
        );

        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: output_width.max(1) as f32,
            Height: output_height.max(1) as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let strides = [size_of::<SubtitleVertex>() as u32];
        let offsets = [0u32];

        // The context_lock serialises use of the immediate context with the
        // decode worker's CopySubresourceRegion — same contract as the video
        // processor path.
        let _lock = self.lock_context();

        // SAFETY:
        // - both plane views reference `surface.texture`, alive for this call
        // - the mapped writes copy exactly the buffers' declared byte widths
        // - every bound object belongs to this device
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(
                &renderer.vertex_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                vertices.as_ptr(),
                mapped.pData.cast::<SubtitleVertex>(),
                vertices.len(),
            );
            self.context.Unmap(&renderer.vertex_buffer, 0);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(
                &renderer.constant_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(&params, mapped.pData.cast::<ToneMapParams>(), 1);
            self.context.Unmap(&renderer.constant_buffer, 0);

            // The quad covers only the dest rect, so the letterbox bars are
            // this clear. (The video processor filled them itself, from the
            // output target rect.)
            self.context
                .OMSetRenderTargets(Some(&[Some(render_target.view.clone())]), None);
            self.context
                .ClearRenderTargetView(&render_target.view, &[0.0, 0.0, 0.0, 1.0]);

            // Zoom/pan can push the picture entirely off-screen; the cleared
            // backbuffer is then the whole (correct) frame.
            if dest_rect.right <= dest_rect.left || dest_rect.bottom <= dest_rect.top {
                return Ok(());
            }

            self.context.RSSetViewports(Some(&[viewport]));
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(Some(&renderer.input_layout));
            self.context.IASetVertexBuffers(
                0,
                1,
                Some([Some(renderer.vertex_buffer.clone())].as_ptr()),
                Some(strides.as_ptr()),
                Some(offsets.as_ptr()),
            );
            self.context
                .VSSetShader(Some(&renderer.vertex_shader), None);
            self.context.PSSetShader(Some(&renderer.pixel_shader), None);
            self.context
                .PSSetSamplers(0, Some(&[Some(renderer.sampler.clone())]));
            self.context
                .PSSetShaderResources(0, Some(&[Some(luma_view), Some(chroma_view)]));
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(renderer.constant_buffer.clone())]));
            self.context
                .OMSetBlendState(None, Some(&[0.0, 0.0, 0.0, 0.0]), u32::MAX);
            self.context.Draw(6, 0);

            // Unbind the planes: the next frame copies into a texture the
            // driver must not still see bound as a shader resource.
            self.context.PSSetShaderResources(0, Some(&[None, None]));
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
            self.device
                .CreateVertexShader(vertex_bytecode, None, Some(&mut vertex_shader))?;
            self.device
                .CreatePixelShader(pixel_bytecode, None, Some(&mut pixel_shader))?;
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
            vertex_shader: vertex_shader
                .ok_or(D3D11Error("CreateVertexShader returned no shader"))?,
            pixel_shader: pixel_shader.ok_or(D3D11Error("CreatePixelShader returned no shader"))?,
            input_layout: input_layout.ok_or(D3D11Error("CreateInputLayout returned no layout"))?,
            sampler: sampler.ok_or(D3D11Error("CreateSamplerState returned no sampler"))?,
            blend_state: blend_state
                .ok_or(D3D11Error("CreateBlendState returned no blend state"))?,
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
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
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
        let vertices =
            subtitle_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
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
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no subtitle texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no subtitle view",
            ))?,
            vertex_buffer: vertex_buffer.ok_or(D3D11Error(
                "CreateBuffer returned no subtitle vertex buffer",
            ))?,
            width: bitmap.width,
            height: bitmap.height,
        }))
    }

    pub(crate) fn create_timeline_overlay(
        &self,
        model: &crate::render::timeline::TimelineOverlayModel,
        existing: Option<SubtitleOverlay>,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_timeline_bitmap(model)? else {
            return Ok(None);
        };

        let vertices = timeline_quad_vertices(bitmap.width, bitmap.height, model.viewport_height);

        // Reuse existing GPU resources if dimensions haven't changed.
        if let Some(overlay) = existing {
            if overlay.width == bitmap.width && overlay.height == bitmap.height {
                unsafe {
                    self.context.UpdateSubresource(
                        &overlay.texture,
                        0,
                        None,
                        bitmap.pixels.as_ptr().cast(),
                        bitmap.width.saturating_mul(4),
                        0,
                    );
                    self.context.UpdateSubresource(
                        &overlay.vertex_buffer,
                        0,
                        None,
                        vertices.as_ptr().cast(),
                        0,
                        0,
                    );
                }
                return Ok(Some(overlay));
            }
        }

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
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
        let vertex_buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: (size_of::<SubtitleVertex>() * vertices.len()) as u32,
            Usage: D3D11_USAGE_DEFAULT,
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

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no timeline texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no timeline view",
            ))?,
            vertex_buffer: vertex_buffer.ok_or(D3D11Error(
                "CreateBuffer returned no timeline vertex buffer",
            ))?,
            width: bitmap.width,
            height: bitmap.height,
        }))
    }

    pub(crate) fn create_volume_overlay(
        &self,
        text: &str,
        viewport_width: u32,
        viewport_height: u32,
        existing: Option<SubtitleOverlay>,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_volume_bitmap(text)? else {
            return Ok(None);
        };

        let vertices =
            volume_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);

        // Reuse existing GPU resources if dimensions haven't changed.
        if let Some(overlay) = existing {
            if overlay.width == bitmap.width && overlay.height == bitmap.height {
                unsafe {
                    self.context.UpdateSubresource(
                        &overlay.texture,
                        0,
                        None,
                        bitmap.pixels.as_ptr().cast(),
                        bitmap.width.saturating_mul(4),
                        0,
                    );
                    self.context.UpdateSubresource(
                        &overlay.vertex_buffer,
                        0,
                        None,
                        vertices.as_ptr().cast(),
                        0,
                        0,
                    );
                }
                return Ok(Some(overlay));
            }
        }

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
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
        let vertex_buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: (size_of::<SubtitleVertex>() * vertices.len()) as u32,
            Usage: D3D11_USAGE_DEFAULT,
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
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no volume texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no volume view",
            ))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no volume vertex buffer"))?,
            width: bitmap.width,
            height: bitmap.height,
        }))
    }

    pub(crate) fn create_idle_overlay(
        &self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_idle_bitmap()? else {
            return Ok(None);
        };

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
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
        let vertices =
            idle_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
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
        let mut texture = None;
        let mut shader_resource_view = None;
        let mut vertex_buffer = None;

        unsafe {
            self.device
                .CreateTexture2D(&texture_desc, Some(&initial_data), Some(&mut texture))?;
            self.device.CreateShaderResourceView(
                texture
                    .as_ref()
                    .ok_or(D3D11Error("CreateTexture2D returned no idle texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no idle texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view
                .ok_or(D3D11Error("CreateShaderResourceView returned no idle view"))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no idle vertex buffer"))?,
            width: bitmap.width,
            height: bitmap.height,
        }))
    }

    pub(crate) fn create_help_overlay(
        &self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_help_bitmap()? else {
            return Ok(None);
        };

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: bitmap.pixels.as_ptr().cast(),
            SysMemPitch: bitmap.width.saturating_mul(4),
            SysMemSlicePitch: 0,
        };
        let vertices =
            idle_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
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
        let mut texture = None;
        let mut shader_resource_view = None;
        let mut vertex_buffer = None;

        unsafe {
            self.device
                .CreateTexture2D(&texture_desc, Some(&initial_data), Some(&mut texture))?;
            self.device.CreateShaderResourceView(
                texture
                    .as_ref()
                    .ok_or(D3D11Error("CreateTexture2D returned no help texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no help texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view
                .ok_or(D3D11Error("CreateShaderResourceView returned no help view"))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no help vertex buffer"))?,
            width: bitmap.width,
            height: bitmap.height,
        }))
    }

    /// Build the Recent-files overlay: a list of `rows` (filename, position)
    /// with `selected` highlighted. Mirrors `create_help_overlay`.
    pub(crate) fn create_recent_overlay(
        &self,
        rows: &[(String, String)],
        selected: usize,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<Option<SubtitleOverlay>, Box<dyn Error>> {
        let Some(bitmap) = render_recent_bitmap(rows, selected)? else {
            return Ok(None);
        };

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: bitmap.width,
            Height: bitmap.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: bitmap.pixels.as_ptr().cast(),
            SysMemPitch: bitmap.width.saturating_mul(4),
            SysMemSlicePitch: 0,
        };
        let vertices =
            idle_quad_vertices(bitmap.width, bitmap.height, viewport_width, viewport_height);
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
        let mut texture = None;
        let mut shader_resource_view = None;
        let mut vertex_buffer = None;

        unsafe {
            self.device
                .CreateTexture2D(&texture_desc, Some(&initial_data), Some(&mut texture))?;
            self.device.CreateShaderResourceView(
                texture
                    .as_ref()
                    .ok_or(D3D11Error("CreateTexture2D returned no recent texture"))?,
                None,
                Some(&mut shader_resource_view),
            )?;
            self.device.CreateBuffer(
                &vertex_buffer_desc,
                Some(&vertex_buffer_data),
                Some(&mut vertex_buffer),
            )?;
        }

        let texture = texture.ok_or(D3D11Error("CreateTexture2D returned no recent texture"))?;
        Ok(Some(SubtitleOverlay {
            texture,
            shader_resource_view: shader_resource_view.ok_or(D3D11Error(
                "CreateShaderResourceView returned no recent view",
            ))?,
            vertex_buffer: vertex_buffer
                .ok_or(D3D11Error("CreateBuffer returned no recent vertex buffer"))?,
            width: bitmap.width,
            height: bitmap.height,
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
            self.context.IASetInputLayout(Some(&renderer.input_layout));
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
            self.context.PSSetShader(Some(&renderer.pixel_shader), None);
            self.context
                .PSSetSamplers(0, Some(&[Some(renderer.sampler.clone())]));
            self.context
                .PSSetShaderResources(0, Some(&[Some(overlay.shader_resource_view.clone())]));
            self.context.OMSetBlendState(
                Some(&renderer.blend_state),
                Some(&[0.0, 0.0, 0.0, 0.0]),
                u32::MAX,
            );
            self.context.Draw(6, 0);
            self.context.PSSetShaderResources(0, Some(&[None]));
            self.context
                .OMSetBlendState(None, Some(&[0.0, 0.0, 0.0, 0.0]), u32::MAX);
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

    pub(crate) fn upload_nv12_surface_contiguous(
        &self,
        width: u32,
        height: u32,
        data: &[u8],
        stride: usize,
        sar_num: u32,
        sar_den: u32,
        color: SurfaceColor,
        hdr_tone_map: Option<DXGI_COLOR_SPACE_TYPE>,
    ) -> Result<VideoSurface, Box<dyn Error>> {
        if width == 0 || height == 0 {
            return Err(Box::new(D3D11Error(
                "software upload requires non-zero dimensions",
            )));
        }
        // Guard: bail out if the device was removed (GPU TDR) before issuing
        // any GPU commands.  The worker thread calls this from background
        // threads and would otherwise crash inside d3d11.dll.
        if self.is_device_removed() {
            return Err(Box::new(D3D11Error(
                "D3D11 device removed (TDR) during software upload",
            )));
        }

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
            pSysMem: data.as_ptr().cast(),
            SysMemPitch: stride as u32,
            SysMemSlicePitch: data.len() as u32,
        };
        let mut texture = None;

        // SAFETY:
        // - `data` is a contiguous NV12 buffer (Y plane then interleaved UV plane)
        // - it stays alive for the duration of CreateTexture2D
        // - the created texture remains owned by the returned VideoSurface
        //
        // CreateTexture2D with pInitialData internally uses the immediate
        // context to upload the data.  The runtime's CritSec (from
        // SetMultithreadProtected) covers ID3D11DeviceContext methods, but
        // NOT ID3D11VideoContext methods.  Hold context_lock to prevent
        // racing with VideoProcessorBlt on the UI thread.
        let _lock = self.lock_context();
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&initial_data), Some(&mut texture))?;
        }

        Ok(VideoSurface {
            texture: texture.ok_or(D3D11Error("CreateTexture2D returned no software texture"))?,
            subresource_index: 0,
            width,
            height,
            sar_num,
            sar_den,
            color,
            hdr_tone_map,
        })
    }
}

/// Vertex shader for the tone-map quad. Positions arrive already in NDC and
/// texcoords already in texture space — both are computed on the CPU from the
/// same rects the video-processor path uses — so there is nothing to transform.
const HDR_TONE_MAP_VERTEX_SHADER: &[u8] = b"
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

/// The HDR→SDR tone-map pixel shader: YCbCr → R'G'B' → linear light → tone
/// curve → BT.709 → sRGB encode, sampling the decoded frame's luma and chroma
/// planes directly.
///
/// Luminance is normalized so that 1.0 is diffuse ("graphics") white, which
/// BT.2408 puts at 203 cd/m². That is what makes the two transfers
/// comparable: a PQ signal is absolute (1.0 = 10 000 cd/m²) while an HLG
/// signal is scene-referred and only becomes display light after the OOTF.
/// Once both are in units of diffuse white, one tone curve serves both.
const HDR_TONE_MAP_PIXEL_SHADER: &[u8] = b"
Texture2D<float>  luma_tex   : register(t0);
Texture2D<float2> chroma_tex : register(t1);
SamplerState      samp       : register(s0);

cbuffer ToneMapParams : register(b0) {
    float4 range;   // y_offset, y_scale, c_offset, c_scale
    float4 params;  // sample_scale, transfer (0 = PQ, 1 = HLG), knee, unused
};

// BT.2020 non-constant-luminance luma coefficients.
static const float3 LUMA_BT2020 = float3(0.2627f, 0.6780f, 0.0593f);

static const float PQ_PEAK_NITS = 10000.0f;
static const float HLG_PEAK_NITS = 1000.0f;
static const float DIFFUSE_WHITE_NITS = 203.0f;

// SMPTE ST 2084 EOTF. Returns [0,1], where 1.0 is PQ_PEAK_NITS.
float3 pq_eotf(float3 signal) {
    const float m1 = 0.1593017578125f;
    const float m2 = 78.84375f;
    const float c1 = 0.8359375f;
    const float c2 = 18.8515625f;
    const float c3 = 18.6875f;
    float3 encoded = pow(max(signal, 0.0f), 1.0f / m2);
    float3 numerator = max(encoded - c1, 0.0f);
    float3 denominator = max(c2 - c3 * encoded, 1e-6f);
    return pow(numerator / denominator, 1.0f / m1);
}

// BT.2100 HLG inverse OETF: signal -> scene light in [0,1].
float3 hlg_inverse_oetf(float3 signal) {
    const float a = 0.17883277f;
    const float b = 0.28466892f;
    const float c = 0.55991073f;
    float3 lower = (signal * signal) / 3.0f;
    float3 upper = (exp((signal - c) / a) + b) / 12.0f;
    return lerp(lower, upper, step(0.5f, signal));
}

// Highlight roll-off. Identity below the knee -- so midtones, skin, and sky
// pass through untouched -- then an exponential shoulder that is
// C1-continuous at the knee and asymptotic to 1.0, so however bright the
// input, it compresses rather than clipping to a flat white.
float tone_curve(float value, float knee) {
    if (value <= knee) {
        return value;
    }
    float headroom = 1.0f - knee;
    return knee + headroom * (1.0f - exp(-(value - knee) / headroom));
}

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float luma_sample = luma_tex.Sample(samp, uv).x * params.x;
    float2 chroma_sample = chroma_tex.Sample(samp, uv).xy * params.x;

    float luma = (luma_sample - range.x) * range.y;
    float cb = (chroma_sample.x - range.z) * range.w;
    float cr = (chroma_sample.y - range.z) * range.w;

    // BT.2020 NCL YCbCr -> nonlinear R'G'B'.
    float3 signal;
    signal.r = luma + 1.47460f * cr;
    signal.g = luma - 0.16455f * cb - 0.57135f * cr;
    signal.b = luma + 1.88140f * cb;
    signal = saturate(signal);

    // Transfer -> linear light, in units of diffuse white.
    float3 linear_rgb;
    if (params.y < 0.5f) {
        linear_rgb = pq_eotf(signal) * (PQ_PEAK_NITS / DIFFUSE_WHITE_NITS);
    } else {
        float3 scene = hlg_inverse_oetf(signal);
        // HLG OOTF: scene light -> display light, system gamma 1.2 at the
        // 1000-nit nominal peak. Driven by scene luminance so it scales the
        // channels together instead of shifting hue.
        float scene_luma = max(dot(scene, LUMA_BT2020), 1e-6f);
        linear_rgb = scene * pow(scene_luma, 0.2f) * (HLG_PEAK_NITS / DIFFUSE_WHITE_NITS);
    }

    // Per-channel roll-off. Highlights desaturate toward white, the way film
    // does, and no channel can leave [0,1]. Scaling by a luminance ratio
    // instead would preserve hue but leave individual channels above 1.0, and
    // clipping those shifts hue anyway -- worse, and unevenly.
    float3 mapped;
    mapped.r = tone_curve(linear_rgb.r, params.z);
    mapped.g = tone_curve(linear_rgb.g, params.z);
    mapped.b = tone_curve(linear_rgb.b, params.z);

    // BT.2020 -> BT.709 primaries, in linear light. Colors outside the BT.709
    // gamut go negative here and are clipped by the saturate.
    float3 rgb709;
    rgb709.r =  1.66049f * mapped.r - 0.58764f * mapped.g - 0.07285f * mapped.b;
    rgb709.g = -0.12455f * mapped.r + 1.13290f * mapped.g - 0.00835f * mapped.b;
    rgb709.b = -0.01824f * mapped.r - 0.10057f * mapped.g + 1.11881f * mapped.b;
    rgb709 = saturate(rgb709);

    // sRGB encode: the transfer the 8-bit SDR backbuffer is scanned out with
    // (DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709).
    float3 encoded = (rgb709 <= 0.0031308f)
        ? rgb709 * 12.92f
        : 1.055f * pow(rgb709, 1.0f / 2.4f) - 0.055f;

    return float4(encoded, 1.0f);
}
\0";

/// The SRV formats that address a planar video texture's two planes. D3D11
/// does not let a shader sample NV12/P010 directly: it exposes each plane as
/// its own view, the luma plane at full resolution and the chroma plane at
/// half in both axes. Normalized texture coordinates address both, so the
/// chroma view's bilinear filter *is* the chroma upsampler.
fn plane_srv_formats(format: DXGI_FORMAT) -> Result<(DXGI_FORMAT, DXGI_FORMAT), Box<dyn Error>> {
    if format == DXGI_FORMAT_P010 {
        Ok((DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM))
    } else if format == DXGI_FORMAT_NV12 {
        Ok((DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM))
    } else {
        Err(Box::new(D3D11Error(
            "HDR tone mapping needs an NV12 or P010 decoded frame",
        )))
    }
}

fn create_plane_srv(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
) -> Result<ID3D11ShaderResourceView, Box<dyn Error>> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };

    // SAFETY: the descriptor is fully initialized and `texture` belongs to
    // `device`; `view` targets a local that outlives the call.
    let mut view = None;
    unsafe {
        device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view))?;
    }
    view.ok_or_else(|| {
        Box::new(D3D11Error(
            "CreateShaderResourceView returned no plane view",
        )) as Box<dyn Error>
    })
}

/// Two triangles covering `dest_rect` of the output, textured from
/// `source_rect` of the decoded frame.
///
/// Rotation is applied by permuting which source corner each destination
/// corner samples, which is the shader-path equivalent of the video
/// processor's `VideoProcessorSetStreamRotation`.
fn tone_map_quad_vertices(
    source_rect: &RECT,
    dest_rect: &RECT,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    rotation_quarter_turns: u8,
) -> [SubtitleVertex; 6] {
    let output_width = output_width.max(1) as f32;
    let output_height = output_height.max(1) as f32;
    let source_width = source_width.max(1) as f32;
    let source_height = source_height.max(1) as f32;

    let left = dest_rect.left as f32 / output_width * 2.0 - 1.0;
    let right = dest_rect.right as f32 / output_width * 2.0 - 1.0;
    let top = 1.0 - dest_rect.top as f32 / output_height * 2.0;
    let bottom = 1.0 - dest_rect.bottom as f32 / output_height * 2.0;

    let u0 = source_rect.left as f32 / source_width;
    let u1 = source_rect.right as f32 / source_width;
    let v0 = source_rect.top as f32 / source_height;
    let v1 = source_rect.bottom as f32 / source_height;

    // Destination corners, and the source corners they sample at zero
    // rotation, both clockwise from the top-left.
    let positions = [
        [left, top, 0.0],
        [right, top, 0.0],
        [right, bottom, 0.0],
        [left, bottom, 0.0],
    ];
    let source_corners = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];

    // Rotating the picture `r` quarter-turns clockwise puts the source's
    // top-left corner `r` places clockwise around the destination, so each
    // destination corner samples the source corner `r` places back.
    let r = (rotation_quarter_turns % 4) as usize;
    let vertex = |corner: usize| SubtitleVertex {
        position: positions[corner],
        texcoord: source_corners[(corner + 4 - r) % 4],
    };

    // Winding matches subtitle_quad_vertices, which the rasterizer's default
    // state already accepts.
    [
        vertex(0),
        vertex(1),
        vertex(3),
        vertex(3),
        vertex(1),
        vertex(2),
    ]
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
        SubtitleVertex {
            position: [left, top, 0.0],
            texcoord: [0.0, 0.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [right, bottom, 0.0],
            texcoord: [1.0, 1.0],
        },
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
        SubtitleVertex {
            position: [-1.0, top, 0.0],
            texcoord: [0.0, 0.0],
        },
        SubtitleVertex {
            position: [1.0, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [-1.0, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [-1.0, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [1.0, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [1.0, bottom, 0.0],
            texcoord: [1.0, 1.0],
        },
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
        SubtitleVertex {
            position: [left, top, 0.0],
            texcoord: [0.0, 0.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [right, bottom, 0.0],
            texcoord: [1.0, 1.0],
        },
    ]
}

fn idle_quad_vertices(
    overlay_width: u32,
    overlay_height: u32,
    viewport_width: u32,
    viewport_height: u32,
) -> [SubtitleVertex; 6] {
    // Center the overlay in the viewport.
    let left_px = ((viewport_width as f32 - overlay_width as f32) / 2.0).max(0.0);
    let top_px = ((viewport_height as f32 - overlay_height as f32) / 2.0).max(0.0);
    let right_px = (left_px + overlay_width as f32).min(viewport_width as f32);
    let bottom_px = (top_px + overlay_height as f32).min(viewport_height as f32);

    let left = left_px / viewport_width as f32 * 2.0 - 1.0;
    let right = right_px / viewport_width as f32 * 2.0 - 1.0;
    let top = 1.0 - top_px / viewport_height as f32 * 2.0;
    let bottom = 1.0 - bottom_px / viewport_height as f32 * 2.0;

    [
        SubtitleVertex {
            position: [left, top, 0.0],
            texcoord: [0.0, 0.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [left, bottom, 0.0],
            texcoord: [0.0, 1.0],
        },
        SubtitleVertex {
            position: [right, top, 0.0],
            texcoord: [1.0, 0.0],
        },
        SubtitleVertex {
            position: [right, bottom, 0.0],
            texcoord: [1.0, 1.0],
        },
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

/// Computes clamped source and dest rects for the render paths.
///
/// Both rects must stay within their respective texture bounds. When the view
/// transform would push the dest rect outside the output, we clip it and adjust
/// the source rect proportionally so only the visible portion of the video is
/// sampled.
///
/// `rotation_quarter_turns` is the display rotation applied *after* this crop
/// (by the video processor's stream rotation, or by the tone-map shader's
/// corner permutation). It is needed here because the destination axes are the
/// source axes only at 0°: at 90°/270° a horizontal span of the *presented*
/// picture is a vertical span of the source texture. Cropping on the wrong axis
/// takes a region of the wrong shape, which the rotate-then-fit stretches to
/// fill the dest rect — so zooming a rotated video used to smear it.
fn compute_zoomed_rects(
    base: &RECT,
    view: &crate::render::ViewTransform,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    rotation_quarter_turns: u8,
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
            RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
        );
    }

    // The fractions of the *presented* (already-rotated) picture that survive
    // the clip, along the destination's own axes.
    let u0 = ((cl - vl) / vw).clamp(0.0, 1.0);
    let u1 = ((cr - vl) / vw).clamp(0.0, 1.0);
    let v0 = ((ct - vt) / vh).clamp(0.0, 1.0);
    let v1 = ((cb - vt) / vh).clamp(0.0, 1.0);

    // Undo the rotation to get the same region in the source texture's axes.
    // Rotating the picture r quarter-turns clockwise maps source (su, sv) to
    // destination (u, v); inverting that per r gives the source span.
    let (su0, su1, sv0, sv1) = match rotation_quarter_turns % 4 {
        1 => (v0, v1, 1.0 - u1, 1.0 - u0),
        2 => (1.0 - u1, 1.0 - u0, 1.0 - v1, 1.0 - v0),
        3 => (1.0 - v1, 1.0 - v0, u0, u1),
        _ => (u0, u1, v0, v1),
    };

    let sw = source_width as f32;
    let sh = source_height as f32;
    let source_rect = RECT {
        left: ((su0 * sw).round() as i32).clamp(0, source_width as i32),
        top: ((sv0 * sh).round() as i32).clamp(0, source_height as i32),
        right: ((su1 * sw).round() as i32).clamp(1, source_width as i32),
        bottom: ((sv1 * sh).round() as i32).clamp(1, source_height as i32),
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
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    }
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
            debug_assert!(DeleteDC(dc).as_bool());
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
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for subtitles",
            )));
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

        let source: &[u8] = std::slice::from_raw_parts(
            bits.cast::<u8>(),
            (bitmap_width * bitmap_height * 4) as usize,
        );
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
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());

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
    let track_cy = track_top + (track_bottom - track_top) / 2;
    let track_half_h = ((track_bottom - track_top) as f32) / 2.0;

    // Gradient background: transparent at top, semi-opaque at bottom.
    for y in 0..height {
        let t = y as f32 / height.max(1) as f32;
        let alpha = (t * t * 180.0) as u8;
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            pixels[offset] = 0;
            pixels[offset + 1] = 0;
            pixels[offset + 2] = 0;
            pixels[offset + 3] = alpha;
        }
    }

    // Unplayed track — rounded pill shape, dim.
    fill_rounded_rect(
        &mut pixels,
        width,
        height,
        track_left,
        track_top,
        track_right,
        track_bottom,
        track_half_h,
        [255, 255, 255, 60],
    );

    // In/out range fill — drawn before the played track so the bright played portion
    // sits on top of it; only shown when both markers are set.
    if let (Some(ix), Some(ox)) = (model.in_point_marker_x, model.out_point_marker_x) {
        let range_left = ix.max(0) as u32;
        let range_right = ox.max(0) as u32;
        if range_right > range_left {
            fill_rounded_rect(
                &mut pixels,
                width,
                height,
                range_left,
                track_top,
                range_right,
                track_bottom,
                track_half_h,
                [60, 160, 255, 130],
            );
        }
    }

    // Played track — bright pill starting at the in-point (if set) so the region
    // before I reads as dim/excluded rather than as played content.
    let played_left = model
        .in_point_marker_x
        .map_or(track_left, |ix| (ix.max(0) as u32).max(track_left));
    let played_right = (track_left + model.played_px).min(track_right);
    if played_right > played_left {
        fill_rounded_rect(
            &mut pixels,
            width,
            height,
            played_left,
            track_top,
            played_right,
            track_bottom,
            track_half_h,
            [255, 255, 255, 230],
        );
    }

    // In/out marker ticks — 2px-wide white vertical bars slightly taller than the track.
    let marker_top = track_top.saturating_sub(3);
    let marker_bottom = (track_bottom + 3).min(height);
    if let Some(x) = model.in_point_marker_x {
        let mx = x.clamp(0, width as i32 - 2) as u32;
        fill_rect(
            &mut pixels,
            width,
            height,
            mx,
            marker_top,
            mx + 2,
            marker_bottom,
            [255, 255, 255, 220],
        );
    }
    if let Some(x) = model.out_point_marker_x {
        let mx = (x - 1).clamp(0, width as i32 - 2) as u32;
        fill_rect(
            &mut pixels,
            width,
            height,
            mx,
            marker_top,
            mx + 2,
            marker_bottom,
            [255, 255, 255, 220],
        );
    }

    // Handle — anti-aliased white circle.
    let handle_cx = model
        .handle_center_x
        .clamp(layout.track_left, layout.track_right) as u32;
    fill_circle_aa(
        &mut pixels,
        width,
        height,
        handle_cx as f32,
        track_cy as f32,
        6.0,
        [255, 255, 255, 255],
    );

    let left_label = match model.preview_position_secs {
        Some(preview_secs) if preview_secs != model.current_position_secs => format!(
            "{}  \u{2192}  {}",
            crate::render::timeline::format_timestamp(model.current_position_secs),
            crate::render::timeline::format_timestamp(preview_secs)
        ),
        _ => crate::render::timeline::format_timestamp(model.current_position_secs),
    };
    let right_label = crate::render::timeline::format_timestamp(model.duration_secs);

    draw_timeline_label(&mut pixels, width, height, &left_label, true)?;
    if model.loop_enabled {
        let right_label = format!("\u{27F3}  {right_label}");
        draw_timeline_label(&mut pixels, width, height, &right_label, false)?;
    } else {
        draw_timeline_label(&mut pixels, width, height, &right_label, false)?;
    }

    Ok(Some(SubtitleBitmap {
        width,
        height,
        pixels,
    }))
}

fn render_idle_bitmap() -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    let text = "Drop a file to play";
    let mut text_wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Box::new(D3D11Error("CreateCompatibleDC returned null")));
        }

        let font = CreateFontW(
            -16,
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
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error("CreateFontW returned null")));
        }

        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let mut text_rect = RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 0,
        };
        let _ = DrawTextW(
            dc,
            &mut text_wide,
            &mut text_rect,
            DT_CALCRECT | DT_CENTER | DT_SINGLELINE | DT_NOPREFIX,
        );

        let padding_x = 24i32;
        let padding_y = 16i32;
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
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for idle overlay",
            )));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (bitmap_width * bitmap_height * 4) as usize);

        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
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
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        let source: &[u8] = std::slice::from_raw_parts(
            bits.cast::<u8>(),
            (bitmap_width * bitmap_height * 4) as usize,
        );
        let mut pixels = vec![0u8; source.len()];
        pixels.copy_from_slice(source);
        for px in pixels.chunks_exact_mut(4) {
            let intensity = px[0].max(px[1]).max(px[2]);
            if intensity > 0 && px[3] == 0 {
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
                px[3] = (intensity / 2).max(40);
            }
        }

        let _ = SelectObject(dc, old_bitmap);
        let _ = SelectObject(dc, old_font);
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());

        Ok(Some(SubtitleBitmap {
            width: bitmap_width,
            height: bitmap_height,
            pixels,
        }))
    }
}

fn render_help_bitmap() -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    const ROWS: &[(&str, &str)] = &[
        ("Space", "Pause / resume"),
        ("\u{2190} / \u{2192}", "Seek 5 s  (hold: 15 s)"),
        ("Ctrl+F / B", "Step frame \u{00B1}1"),
        ("Ctrl+O", "Open media file"),
        ("Ctrl+Shift+O", "Recent files"),
        ("PgUp / PgDn", "Previous / next in queue"),
        ("Ctrl+S", "Save screenshot"),
        ("S", "Toggle subtitles"),
        ("I / O", "Set in / out point"),
        ("Shift+I / O", "Clear in / out point"),
        ("R", "Loop range \u{00B7} auto-replay"),
        ("[ / ]", "Speed \u{2212} / +"),
        ("\\", "Reset speed"),
        ("Backspace", "Cancel scrub"),
        ("Esc", "Exit fullscreen"),
        ("Ctrl+H", "Toggle fullscreen"),
        ("Ctrl+W", "Fill screen height"),
        ("Ctrl+Q", "Half native resolution"),
        ("Ctrl+R / E", "Rotate \u{00B1}90\u{00B0}"),
        ("Ctrl+Scroll", "Zoom at cursor"),
        ("Ctrl+Drag", "Pan  (when zoomed)"),
        ("Ctrl+0", "Reset view"),
        ("`", "Toggle HW/SW decode"),
        ("Mousewheel", "Volume"),
    ];

    const PAD_X: i32 = 20;
    const PAD_Y: i32 = 16;
    const HEADER_H: i32 = 24;
    const SEP: i32 = 8;
    const LINE_H: i32 = 20;
    const COL_DESC_X: i32 = PAD_X + 118; // where description column begins
    const BW: u32 = 390;
    let bh = (PAD_Y + HEADER_H + SEP + ROWS.len() as i32 * LINE_H + PAD_Y) as u32;

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Box::new(D3D11Error("CreateCompatibleDC returned null")));
        }

        let font = CreateFontW(
            -13,
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
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateFontW returned null for help overlay",
            )));
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: BW as i32,
            biHeight: -(bh as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if bitmap.0.is_null() || bits.is_null() {
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for help overlay",
            )));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        std::ptr::write_bytes(bits, 0, (BW * bh * 4) as usize);

        let _ = SetBkMode(dc, TRANSPARENT);
        // GDI draws in BGR; alpha is left as 0 — we fix it in post-process.
        let _ = SetTextColor(dc, COLORREF(0x00E8E8E8)); // light text

        // Header "Controls"
        let mut header_wide: Vec<u16> = "Controls".encode_utf16().chain(Some(0)).collect();
        let mut header_rect = RECT {
            left: PAD_X,
            top: PAD_Y,
            right: BW as i32 - PAD_X,
            bottom: PAD_Y + HEADER_H,
        };
        let _ = DrawTextW(
            dc,
            &mut header_wide,
            &mut header_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        // Key-binding rows
        for (i, (key, desc)) in ROWS.iter().enumerate() {
            let y = PAD_Y + HEADER_H + SEP + i as i32 * LINE_H;
            let row_bottom = y + LINE_H;

            let mut key_wide: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
            let mut key_rect = RECT {
                left: PAD_X,
                top: y,
                right: COL_DESC_X - 8,
                bottom: row_bottom,
            };
            let _ = DrawTextW(
                dc,
                &mut key_wide,
                &mut key_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );

            let mut desc_wide: Vec<u16> = desc.encode_utf16().chain(Some(0)).collect();
            let mut desc_rect = RECT {
                left: COL_DESC_X,
                top: y,
                right: BW as i32 - PAD_X,
                bottom: row_bottom,
            };
            let _ = DrawTextW(
                dc,
                &mut desc_wide,
                &mut desc_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        }

        let source: &[u8] = std::slice::from_raw_parts(bits.cast::<u8>(), (BW * bh * 4) as usize);
        let mut pixels = vec![0u8; source.len()];
        pixels.copy_from_slice(source);

        // Post-process: GDI wrote RGB but left alpha=0.
        // Text pixels (non-zero channel) → bright white with alpha.
        // All other pixels → dark semi-transparent background.
        for px in pixels.chunks_exact_mut(4) {
            let intensity = px[0].max(px[1]).max(px[2]);
            if intensity > 4 {
                // Text — make bright white, alpha proportional to intensity.
                px[0] = 235;
                px[1] = 235;
                px[2] = 240;
                px[3] = intensity.min(230);
            } else {
                // Background — dark, slightly blue-tinted.
                px[0] = 22;
                px[1] = 20;
                px[2] = 18;
                px[3] = 218;
            }
        }

        // Separator line between header and rows (1px, semi-opaque white).
        let sep_y = (PAD_Y + HEADER_H + SEP / 2) as u32;
        if sep_y < bh {
            fill_rect(
                &mut pixels,
                BW,
                bh,
                PAD_X as u32,
                sep_y,
                BW - PAD_X as u32,
                sep_y + 1,
                [255, 255, 255, 60],
            );
        }

        let _ = SelectObject(dc, old_bitmap);
        let _ = SelectObject(dc, old_font);
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());

        Ok(Some(SubtitleBitmap {
            width: BW,
            height: bh,
            pixels,
        }))
    }
}

fn render_recent_bitmap(
    rows: &[(String, String)],
    selected: usize,
) -> Result<Option<SubtitleBitmap>, Box<dyn Error>> {
    const PAD_X: i32 = 22;
    const PAD_Y: i32 = 16;
    const HEADER_H: i32 = 24;
    const SEP: i32 = 8;
    const LINE_H: i32 = 22;
    const FOOTER_H: i32 = 22;
    const BW: u32 = 560;
    const POS_W: i32 = 78; // right-hand position column width
    const MAX_NAME_CHARS: usize = 56;

    let row_count = rows.len().max(1) as i32; // at least one line ("No recent files")
    let rows_top = PAD_Y + HEADER_H + SEP;
    let bh = (rows_top + row_count * LINE_H + SEP + FOOTER_H + PAD_Y) as u32;

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
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateFontW returned null for recent overlay",
            )));
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: BW as i32,
            biHeight: -(bh as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if bitmap.0.is_null() || bits.is_null() {
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for recent overlay",
            )));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        std::ptr::write_bytes(bits, 0, (BW * bh * 4) as usize);

        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00E8E8E8));

        // Header.
        let mut header_wide: Vec<u16> = "Recent Files".encode_utf16().chain(Some(0)).collect();
        let mut header_rect = RECT {
            left: PAD_X,
            top: PAD_Y,
            right: BW as i32 - PAD_X,
            bottom: PAD_Y + HEADER_H,
        };
        let _ = DrawTextW(
            dc,
            &mut header_wide,
            &mut header_rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        if rows.is_empty() {
            let mut empty_wide: Vec<u16> =
                "No recent files".encode_utf16().chain(Some(0)).collect();
            let mut empty_rect = RECT {
                left: PAD_X,
                top: rows_top,
                right: BW as i32 - PAD_X,
                bottom: rows_top + LINE_H,
            };
            let _ = DrawTextW(
                dc,
                &mut empty_wide,
                &mut empty_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        } else {
            for (i, (name, pos)) in rows.iter().enumerate() {
                let y = rows_top + i as i32 * LINE_H;
                let row_bottom = y + LINE_H;
                let marker = if i == selected { "\u{203A} " } else { "  " };
                let truncated = truncate_chars(name, MAX_NAME_CHARS);
                let mut name_wide: Vec<u16> = format!("{marker}{truncated}")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let mut name_rect = RECT {
                    left: PAD_X,
                    top: y,
                    right: BW as i32 - PAD_X - POS_W,
                    bottom: row_bottom,
                };
                let _ = DrawTextW(
                    dc,
                    &mut name_wide,
                    &mut name_rect,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );

                let mut pos_wide: Vec<u16> = pos.encode_utf16().chain(Some(0)).collect();
                let mut pos_rect = RECT {
                    left: BW as i32 - PAD_X - POS_W,
                    top: y,
                    right: BW as i32 - PAD_X,
                    bottom: row_bottom,
                };
                let _ = DrawTextW(
                    dc,
                    &mut pos_wide,
                    &mut pos_rect,
                    DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
            }
        }

        // Footer hint.
        let footer_y = rows_top + row_count * LINE_H + SEP;
        let mut footer_wide: Vec<u16> =
            "Enter Open   \u{2191}\u{2193} Select   Del Remove   Esc Close"
                .encode_utf16()
                .chain(Some(0))
                .collect();
        let mut footer_rect = RECT {
            left: PAD_X,
            top: footer_y,
            right: BW as i32 - PAD_X,
            bottom: footer_y + FOOTER_H,
        };
        let _ = DrawTextW(
            dc,
            &mut footer_wide,
            &mut footer_rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        let source: &[u8] = std::slice::from_raw_parts(bits.cast::<u8>(), (BW * bh * 4) as usize);
        let mut pixels = vec![0u8; source.len()];
        pixels.copy_from_slice(source);

        for px in pixels.chunks_exact_mut(4) {
            let intensity = px[0].max(px[1]).max(px[2]);
            if intensity > 4 {
                px[0] = 235;
                px[1] = 235;
                px[2] = 240;
                px[3] = intensity.min(230);
            } else {
                px[0] = 22;
                px[1] = 20;
                px[2] = 18;
                px[3] = 218;
            }
        }

        // Highlight the selected row: brighten only its background pixels, so
        // the (white) text on that row is left untouched.
        if !rows.is_empty() && selected < rows.len() {
            let y0 = (rows_top + selected as i32 * LINE_H) as u32;
            let y1 = (y0 as i32 + LINE_H) as u32;
            for y in y0..y1.min(bh) {
                for x in 0..BW {
                    let idx = ((y * BW + x) * 4) as usize;
                    // Only recolor background pixels (dark), not text (bright).
                    if pixels[idx] < 40 && pixels[idx + 1] < 40 {
                        pixels[idx] = 46;
                        pixels[idx + 1] = 52;
                        pixels[idx + 2] = 70;
                        pixels[idx + 3] = 230;
                    }
                }
            }
        }

        let sep_y = (PAD_Y + HEADER_H + SEP / 2) as u32;
        if sep_y < bh {
            fill_rect(
                &mut pixels,
                BW,
                bh,
                PAD_X as u32,
                sep_y,
                BW - PAD_X as u32,
                sep_y + 1,
                [255, 255, 255, 60],
            );
        }

        let _ = SelectObject(dc, old_bitmap);
        let _ = SelectObject(dc, old_font);
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());

        Ok(Some(SubtitleBitmap {
            width: BW,
            height: bh,
            pixels,
        }))
    }
}

/// Truncate `name` to at most `max` characters, appending an ellipsis when cut.
fn truncate_chars(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max {
        return name.to_string();
    }
    let kept: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{kept}\u{2026}")
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
            debug_assert!(DeleteDC(dc).as_bool());
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
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for volume overlay",
            )));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (bitmap_width * bitmap_height * 4) as usize);
        let source = std::slice::from_raw_parts_mut(
            bits.cast::<u8>(),
            (bitmap_width * bitmap_height * 4) as usize,
        );
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

        let source: &[u8] = std::slice::from_raw_parts(
            bits.cast::<u8>(),
            (bitmap_width * bitmap_height * 4) as usize,
        );
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
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());

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
        left: 16,
        top: 4,
        right: width as i32 - 16,
        bottom: 24,
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
            debug_assert!(DeleteDC(dc).as_bool());
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
            debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
            debug_assert!(DeleteDC(dc).as_bool());
            return Err(Box::new(D3D11Error(
                "CreateDIBSection failed for timeline label",
            )));
        }

        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        std::ptr::write_bytes(bits, 0, (width * height * 4) as usize);
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let _ = DrawTextW(dc, &mut text_wide, &mut draw_rect, draw_flags);

        let source = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        for (source_px, dest_px) in source
            .chunks_exact(4)
            .zip(destination_pixels.chunks_exact_mut(4))
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
        debug_assert!(DeleteObject(HGDIOBJ(bitmap.0)).as_bool());
        debug_assert!(DeleteObject(HGDIOBJ(font.0)).as_bool());
        debug_assert!(DeleteDC(dc).as_bool());
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

fn fill_circle_aa(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    color: [u8; 4],
) {
    let r_outer = radius + 0.5;
    let min_x = (cx - r_outer).floor().max(0.0) as u32;
    let max_x = ((cx + r_outer).ceil() as u32).min(width.saturating_sub(1));
    let min_y = (cy - r_outer).floor().max(0.0) as u32;
    let max_y = ((cy + r_outer).ceil() as u32).min(height.saturating_sub(1));

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > r_outer {
                continue;
            }
            // Smooth edge: 1px anti-alias fringe.
            let coverage = (radius - dist + 0.5).clamp(0.0, 1.0);
            let alpha = (color[3] as f32 * coverage) as u8;
            let offset = ((y * width + x) * 4) as usize;
            blend_pixel(
                &mut pixels[offset..offset + 4],
                [color[0], color[1], color[2], alpha],
            );
        }
    }
}

fn fill_rounded_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    radius: f32,
    color: [u8; 4],
) {
    let left = left.min(width);
    let right = right.min(width);
    let top = top.min(height);
    let bottom = bottom.min(height);
    let rect_h = bottom.saturating_sub(top) as f32;
    let rect_w = right.saturating_sub(left) as f32;
    let r = radius.min(rect_h / 2.0).min(rect_w / 2.0);

    let min_x = left.saturating_sub(1);
    let max_x = (right + 1).min(width);
    let min_y = top.saturating_sub(1);
    let max_y = (bottom + 1).min(height);

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let inner_x = px >= left as f32 + r && px <= right as f32 - r;
            let inner_y = py >= top as f32 + r && py <= bottom as f32 - r;

            let coverage = if inner_x && inner_y {
                // Fully inside.
                1.0
            } else if inner_x {
                // Top or bottom edge.
                let cy = if py < top as f32 + r {
                    top as f32 + r
                } else {
                    bottom as f32 - r
                };
                let dist = (py - cy).abs();
                (r - dist + 0.5).clamp(0.0, 1.0)
            } else if inner_y {
                // Left or right edge.
                let cx = if px < left as f32 + r {
                    left as f32 + r
                } else {
                    right as f32 - r
                };
                let dist = (px - cx).abs();
                (r - dist + 0.5).clamp(0.0, 1.0)
            } else {
                // Corner — distance from corner circle center.
                let cx = if px < left as f32 + r {
                    left as f32 + r
                } else {
                    right as f32 - r
                };
                let cy = if py < top as f32 + r {
                    top as f32 + r
                } else {
                    bottom as f32 - r
                };
                let dist = ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt();
                (r - dist + 0.5).clamp(0.0, 1.0)
            };

            if coverage <= 0.0 {
                continue;
            }

            let alpha = (color[3] as f32 * coverage) as u8;
            let offset = ((y * width + x) * 4) as usize;
            blend_pixel(
                &mut pixels[offset..offset + 4],
                [color[0], color[1], color[2], alpha],
            );
        }
    }
}

fn blend_pixel(dest: &mut [u8], src: [u8; 4]) {
    let sa = src[3] as u32;
    if sa == 0 {
        return;
    }
    if sa == 255 || dest[3] == 0 {
        dest.copy_from_slice(&src);
        return;
    }
    let da = dest[3] as u32;
    let out_a = sa + da - (sa * da / 255);
    if out_a == 0 {
        return;
    }
    for i in 0..3 {
        dest[i] = ((src[i] as u32 * sa + dest[i] as u32 * da * (255 - sa) / 255) / out_a) as u8;
    }
    dest[3] = out_a as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::hdr::{HdrToneMapSignal, HdrTransfer};

    /// Apply the constant buffer's level math exactly as the shader's first
    /// three lines do, so these tests pin the numbers the GPU actually uses.
    fn decode_levels(params: &ToneMapParams, luma_code: f32, chroma_code: f32) -> (f32, f32) {
        let [y_offset, y_scale, c_offset, c_scale] = params.range;
        let scale = params.params[0];
        let luma = (luma_code * scale - y_offset) * y_scale;
        let chroma = (chroma_code * scale - c_offset) * c_scale;
        (luma, chroma)
    }

    fn signal(transfer: HdrTransfer, full_range: bool) -> HdrToneMapSignal {
        HdrToneMapSignal {
            transfer,
            full_range,
        }
    }

    #[test]
    fn studio_10_bit_levels_map_video_black_and_white_to_0_and_1() {
        let params = ToneMapParams::new(signal(HdrTransfer::Hlg, false), true);

        // P010 puts the 10-bit code in the high bits of a 16-bit word, so a
        // UNORM fetch of code C returns C * 64 / 65535 — that is what the
        // shader samples, and sample_scale must undo it.
        let fetch = |code_10bit: f32| code_10bit * 64.0 / 65535.0;

        // Studio range: 64 is video black, 940 nominal white, 512 neutral.
        let (black, neutral) = decode_levels(&params, fetch(64.0), fetch(512.0));
        assert!(
            black.abs() < 1e-4,
            "video black must decode to 0.0, got {black}"
        );
        assert!(
            neutral.abs() < 1e-4,
            "neutral chroma must decode to 0.0, got {neutral}"
        );

        let (white, _) = decode_levels(&params, fetch(940.0), fetch(512.0));
        assert!(
            (white - 1.0).abs() < 1e-4,
            "nominal white must decode to 1.0, got {white}"
        );

        // Chroma extremes land on ±0.5 (the Cb/Cr range the matrix expects).
        let (_, high) = decode_levels(&params, fetch(64.0), fetch(960.0));
        assert!(
            (high - 0.5).abs() < 1e-3,
            "chroma 960 must be +0.5, got {high}"
        );
    }

    #[test]
    fn studio_8_bit_levels_map_video_black_and_white_to_0_and_1() {
        // NV12: the UNORM fetch already normalizes against 255, so no rescale.
        let params = ToneMapParams::new(signal(HdrTransfer::Pq, false), false);
        assert_eq!(params.params[0], 1.0, "8-bit needs no sample rescale");

        let (black, _) = decode_levels(&params, 16.0 / 255.0, 128.0 / 255.0);
        let (white, _) = decode_levels(&params, 235.0 / 255.0, 128.0 / 255.0);
        assert!(
            black.abs() < 1e-4,
            "video black must decode to 0.0, got {black}"
        );
        assert!(
            (white - 1.0).abs() < 1e-4,
            "nominal white must decode to 1.0, got {white}"
        );
    }

    #[test]
    fn full_range_levels_pass_luma_through_and_center_chroma() {
        let params = ToneMapParams::new(signal(HdrTransfer::Hlg, true), true);
        let fetch = |code_10bit: f32| code_10bit * 64.0 / 65535.0;

        // Full range: code 0 is black and 1023 is white, with no footroom.
        let (black, neutral) = decode_levels(&params, fetch(0.0), fetch(512.0));
        let (white, _) = decode_levels(&params, fetch(1023.0), fetch(512.0));
        assert!(
            black.abs() < 1e-4,
            "full-range black must be 0.0, got {black}"
        );
        assert!(
            (white - 1.0).abs() < 1e-4,
            "full-range white must be 1.0, got {white}"
        );
        assert!(
            neutral.abs() < 1e-4,
            "full-range neutral chroma must be 0.0, got {neutral}"
        );
    }

    #[test]
    fn transfer_selector_distinguishes_pq_from_hlg() {
        // The shader branches on this float; swapping the two applies the
        // wrong EOTF, which is a gross error rather than a shade of grading.
        let pq = ToneMapParams::new(signal(HdrTransfer::Pq, false), true);
        let hlg = ToneMapParams::new(signal(HdrTransfer::Hlg, false), true);
        assert_eq!(pq.params[1], 0.0);
        assert_eq!(hlg.params[1], 1.0);
    }

    #[test]
    fn rotation_permutes_which_source_corner_each_dest_corner_samples() {
        let source = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 50,
        };
        let dest = RECT {
            left: 0,
            top: 0,
            right: 200,
            bottom: 100,
        };
        // Vertex 0 is the destination's top-left corner (see the winding).
        let top_left_uv = |turns: u8| {
            tone_map_quad_vertices(&source, &dest, 100, 50, 200, 100, turns)[0].texcoord
        };

        // Unrotated, the destination's top-left samples the source's top-left.
        assert_eq!(top_left_uv(0), [0.0, 0.0]);
        // Rotating the picture 180° puts the source's bottom-right there.
        assert_eq!(top_left_uv(2), [1.0, 1.0]);
        // 90° clockwise puts the source's bottom-left there, 270° its top-right.
        assert_eq!(top_left_uv(1), [0.0, 1.0]);
        assert_eq!(top_left_uv(3), [1.0, 0.0]);
    }

    /// Aspect of the picture as presented: the source crop, rotated.
    fn presented_aspect(source: &RECT, rotation_quarter_turns: u8) -> f32 {
        let w = (source.right - source.left) as f32;
        let h = (source.bottom - source.top) as f32;
        if rotation_quarter_turns % 2 == 1 {
            h / w
        } else {
            w / h
        }
    }

    fn aspect(rect: &RECT) -> f32 {
        (rect.right - rect.left) as f32 / (rect.bottom - rect.top) as f32
    }

    #[test]
    fn zooming_a_rotated_video_crops_on_the_rotated_axes() {
        // A 16:9 source turned a quarter-turn and zoomed into a landscape
        // window. The destination's horizontal axis is the source's *vertical*
        // one here, so cropping on the source's own axes takes a region of the
        // wrong shape — and the rotate-then-fit stretches it to fill the dest.
        // That was the bug: zoom after rotate smeared the picture.
        let view = crate::render::ViewTransform {
            zoom: 2.0,
            pan_x: 0.0,
            pan_y: 0.0,
            rotation_quarter_turns: 1,
        };
        // Rotated display is 900x1600, aspect-fit into 1920x1080 => pillarboxed.
        let base = aspect_fit_rect(900, 1600, 1920, 1080);
        let (source, dest) = compute_zoomed_rects(&base, &view, 1600, 900, 1920, 1080, 1);

        // The clip is full-width but only the middle half of the height of the
        // presented picture, so the source keeps its full height and half its
        // width — 800x900, not 1600x450.
        assert_eq!(source.right - source.left, 800, "source crop width");
        assert_eq!(source.bottom - source.top, 900, "source crop height");

        // The real invariant: once rotated, the crop must match the shape of
        // the rect it is fitted into, or it is being stretched.
        let error = (presented_aspect(&source, 1) - aspect(&dest)).abs();
        assert!(
            error < 0.01,
            "presented aspect {} must match dest aspect {}",
            presented_aspect(&source, 1),
            aspect(&dest)
        );
    }

    #[test]
    fn zooming_preserves_aspect_at_every_rotation() {
        // Same invariant across all four rotations and both window shapes: the
        // rotated crop must always match the destination it is fitted into.
        for turns in 0..4u8 {
            for (out_w, out_h) in [(1920u32, 1080u32), (1080, 1920)] {
                let view = crate::render::ViewTransform {
                    zoom: 1.8,
                    pan_x: 40.0,
                    pan_y: -25.0,
                    rotation_quarter_turns: turns,
                };
                let (dw, dh) = if turns % 2 == 1 {
                    (900, 1600)
                } else {
                    (1600, 900)
                };
                let base = aspect_fit_rect(dw, dh, out_w, out_h);
                let (source, dest) =
                    compute_zoomed_rects(&base, &view, 1600, 900, out_w, out_h, turns);

                let error = (presented_aspect(&source, turns) - aspect(&dest)).abs();
                assert!(
                    error < 0.02,
                    "turns={turns} out={out_w}x{out_h}: presented {} vs dest {}",
                    presented_aspect(&source, turns),
                    aspect(&dest)
                );
            }
        }
    }

    #[test]
    fn unrotated_zoom_is_unchanged() {
        // The 0-turn path must keep its pre-existing behavior exactly: this is
        // the pixel-verified case that shipped.
        let view = crate::render::ViewTransform {
            zoom: 2.0,
            pan_x: 0.0,
            pan_y: 0.0,
            rotation_quarter_turns: 0,
        };
        let base = aspect_fit_rect(1600, 900, 1920, 1080);
        let (source, _) = compute_zoomed_rects(&base, &view, 1600, 900, 1920, 1080, 0);
        // Zoom 2x centered keeps the middle half of each axis.
        assert_eq!(source.left, 400);
        assert_eq!(source.right, 1200);
        assert_eq!(source.top, 225);
        assert_eq!(source.bottom, 675);
    }

    #[test]
    fn quad_maps_the_dest_rect_onto_normalized_device_coordinates() {
        // A letterboxed dest rect must land where the video processor would
        // have put it: the two paths must not disagree by a pixel.
        let source = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        let dest = RECT {
            left: 50,
            top: 0,
            right: 150,
            bottom: 100,
        };
        let quad = tone_map_quad_vertices(&source, &dest, 100, 100, 200, 100, 0);

        // x = 50 of 200 -> -0.5 in NDC; y = 0 -> +1.0 (NDC y is up).
        assert_eq!(quad[0].position, [-0.5, 1.0, 0.0]);
        // The opposite corner: x = 150 -> +0.5, y = 100 -> -1.0.
        assert_eq!(quad[5].position, [0.5, -1.0, 0.0]);
    }
}
