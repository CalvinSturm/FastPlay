//! Dev-gated HDR10 color-space validation entry (`bench/verify-colors-pq.ps1`).
//!
//! Renders one raw NV12 frame through the real HDR10 pieces — the
//! R10G10B10A2 swapchain from [`create_hdr10_skeleton`], the resolved
//! `verified_*` color spaces, `CheckVideoProcessorFormatConversion`, and
//! `VideoProcessor{SetStream,SetOutput}ColorSpace1` — then dumps the raw
//! backbuffer readback (pre-Present, so DWM never touches it) for the
//! harness to compare against ffmpeg's reference decode.
//!
//! Never active in normal use: [`config_from_env`] returns `None` unless
//! the `FASTPLAY_HDR_VALIDATE_*` environment variables are set, and
//! `main::run` checks it before any playback object is created.
//!
//! [`create_hdr10_skeleton`]: crate::ffi::dxgi::DxgiSwapChain::create_hdr10_skeleton

use std::{error::Error, fs, path::PathBuf};

use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;

use crate::{
    ffi::{
        d3d11::{D3D11Device, SurfaceColor},
        dxgi::DxgiSwapChain,
        ffmpeg::{
            AVColorPrimaries_AVCOL_PRI_BT2020, AVColorRange_AVCOL_RANGE_MPEG,
            AVColorSpace_AVCOL_SPC_BT2020_NCL, AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        },
    },
    platform::window::NativeWindow,
    render::hdr::{ContentColorInfo, ContentColorMode, HdrPresentationCapabilities},
};

pub struct HdrValidateConfig {
    nv12_path: PathBuf,
    width: u32,
    height: u32,
    out_path: PathBuf,
    /// Negative control: feed the processor a deliberately wrong input
    /// color space (SDR BT.709) so the harness can prove the pixel oracle
    /// fails on a wrong constant.
    wrong_matrix: bool,
}

/// `None` unless the validation env vars are all present — the ordinary
/// player never enters this module.
pub fn config_from_env() -> Option<HdrValidateConfig> {
    let nv12_path = std::env::var_os("FASTPLAY_HDR_VALIDATE_NV12")?;
    let out_path = std::env::var_os("FASTPLAY_HDR_VALIDATE_OUT")?;
    let size = std::env::var("FASTPLAY_HDR_VALIDATE_SIZE").ok()?;
    let (width, height) = size.split_once('x')?;
    Some(HdrValidateConfig {
        nv12_path: nv12_path.into(),
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        out_path: out_path.into(),
        wrong_matrix: std::env::var_os("FASTPLAY_HDR_VALIDATE_WRONG_MATRIX").is_some(),
    })
}

pub fn run(config: HdrValidateConfig) -> Result<(), Box<dyn Error>> {
    // The standard HDR10 signal the harness generates: PQ transfer,
    // BT.2020 NCL matrix, limited range.
    let content = ContentColorInfo {
        mode: ContentColorMode::Hdr10Pq,
        color_primaries: AVColorPrimaries_AVCOL_PRI_BT2020,
        color_transfer: AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        color_space: AVColorSpace_AVCOL_SPC_BT2020_NCL,
        color_range: AVColorRange_AVCOL_RANGE_MPEG,
        mastering_display: None,
        content_light: None,
    };

    let nv12 = fs::read(&config.nv12_path)?;
    let expected_len = config.width as usize * config.height as usize * 3 / 2;
    if nv12.len() != expected_len {
        return Err(format!(
            "NV12 input is {} bytes, expected {} for {}x{}",
            nv12.len(),
            expected_len,
            config.width,
            config.height
        )
        .into());
    }

    // Window sized exactly to the video so the full-frame blt maps 1:1.
    let window = NativeWindow::create("FastPlay HDR validation", config.width, config.height)?;
    let device = D3D11Device::create()?;

    // Creates the 10-bit swapchain, runs CheckColorSpaceSupport on this
    // display, and commits SetColorSpace1 — structural oracle #1.
    let capabilities = HdrPresentationCapabilities::default();
    let mut swap_chain = DxgiSwapChain::create_hdr10_skeleton(
        window.raw_window(),
        &device,
        &content,
        &capabilities,
    )?;
    println!(
        "[hdr-validate] R10G10B10A2 swapchain created; CheckColorSpaceSupport accepted \
         RGB_FULL_G2084_NONE_P2020 and SetColorSpace1 committed"
    );

    // The SurfaceColor tag drives only the SDR blt path; the HDR blt reads
    // its color spaces from the verified_* helpers instead.
    let surface = device.upload_nv12_surface_contiguous(
        config.width,
        config.height,
        &nv12,
        config.width as usize,
        1,
        1,
        SurfaceColor {
            bt709: false,
            full_range: false,
        },
        // This harness drives the dedicated hdr10_validation_blt (which reads
        // its color spaces from the verified_* helpers), not the production
        // tone-map blt path, so no tone-map tag is attached here.
        None,
    )?;

    let stream_color_space_override = config
        .wrong_matrix
        .then_some(DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
    if stream_color_space_override.is_some() {
        println!(
            "[hdr-validate] NEGATIVE CONTROL: forcing wrong input color space \
             YCBCR_STUDIO_G22_LEFT_P709"
        );
    }

    // Blt with ColorSpace1 configuration + format-conversion check
    // (structural oracle #2), then raw backbuffer readback — no Present.
    let capture = swap_chain.hdr10_validation_pass(
        &device,
        &surface,
        &content,
        stream_color_space_override,
    )?;

    // Dump: magic, dimensions, then raw R10G10B10A2 dwords for the harness.
    let mut out = Vec::with_capacity(16 + capture.pixels.len());
    out.extend_from_slice(b"R10A2\0");
    out.extend_from_slice(&capture.width.to_le_bytes());
    out.extend_from_slice(&capture.height.to_le_bytes());
    out.extend_from_slice(&capture.pixels);
    fs::write(&config.out_path, out)?;
    println!(
        "[hdr-validate] wrote {}x{} R10G10B10A2 readback to {}",
        capture.width,
        capture.height,
        config.out_path.display()
    );

    // Per the project's shutdown strategy, GPU objects are never torn down
    // in-process (intermittent driver faults → WER stalls). Leak them
    // deliberately and let process exit reclaim everything.
    std::mem::forget(surface);
    std::mem::forget(swap_chain);
    std::mem::forget(device);
    std::mem::forget(window);
    Ok(())
}
