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

use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020,
};

use crate::{
    ffi::{
        d3d11::{D3D11Device, SurfaceColor},
        dxgi::DxgiSwapChain,
        ffmpeg::{
            AVColorPrimaries_AVCOL_PRI_BT2020, AVColorRange_AVCOL_RANGE_MPEG,
            AVColorSpace_AVCOL_SPC_BT2020_NCL,
            AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        },
    },
    platform::window::NativeWindow,
    render::hdr::{ContentColorInfo, ContentColorMode, HdrPresentationCapabilities},
};

/// Which HDR pipeline the validation frame is rendered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateMode {
    /// The dedicated video-processor blt with `ColorSpace1` configuration
    /// (`hdr10_validation_blt`) — the original PQ skeleton oracle.
    Vp,
    /// The production tone-map shader in PQ-output mode, PQ input tag.
    ShaderPq,
    /// The production tone-map shader in PQ-output mode, HLG input tag.
    ShaderHlg,
}

pub struct HdrValidateConfig {
    nv12_path: PathBuf,
    width: u32,
    height: u32,
    out_path: PathBuf,
    mode: ValidateMode,
    /// Negative control: feed the pipeline a deliberately wrong input
    /// color space so the harness can prove the pixel oracle fails on a
    /// wrong constant. For the VP blt that is SDR BT.709; for the shader
    /// modes it is the *other* HDR transfer (PQ↔HLG), the wrong constant
    /// that pipeline could actually be handed.
    wrong_matrix: bool,
}

/// `None` unless the validation env vars are all present — the ordinary
/// player never enters this module.
pub fn config_from_env() -> Option<HdrValidateConfig> {
    let nv12_path = std::env::var_os("FASTPLAY_HDR_VALIDATE_NV12")?;
    let out_path = std::env::var_os("FASTPLAY_HDR_VALIDATE_OUT")?;
    let size = std::env::var("FASTPLAY_HDR_VALIDATE_SIZE").ok()?;
    let (width, height) = size.split_once('x')?;
    let mode = match std::env::var("FASTPLAY_HDR_VALIDATE_MODE").as_deref() {
        Err(_) | Ok("vp") => ValidateMode::Vp,
        Ok("shader-pq") => ValidateMode::ShaderPq,
        Ok("shader-hlg") => ValidateMode::ShaderHlg,
        // An unknown mode must not fall back to normal playback with the
        // other validation vars set — the harness would hang on a player.
        Ok(other) => {
            eprintln!("[hdr-validate] unknown FASTPLAY_HDR_VALIDATE_MODE '{other}'");
            std::process::exit(2);
        }
    };
    Some(HdrValidateConfig {
        nv12_path: nv12_path.into(),
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        out_path: out_path.into(),
        mode,
        wrong_matrix: std::env::var_os("FASTPLAY_HDR_VALIDATE_WRONG_MATRIX").is_some(),
    })
}

pub fn run(config: HdrValidateConfig) -> Result<(), Box<dyn Error>> {
    // The standard HDR signal the harness generates: PQ (or HLG for the
    // shader-hlg mode), BT.2020 NCL matrix, limited range.
    let (mode, transfer) = match config.mode {
        ValidateMode::Vp | ValidateMode::ShaderPq => (
            ContentColorMode::Hdr10Pq,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        ),
        ValidateMode::ShaderHlg => (
            ContentColorMode::Hlg,
            AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
        ),
    };
    let content = ContentColorInfo {
        mode,
        color_primaries: AVColorPrimaries_AVCOL_PRI_BT2020,
        color_transfer: transfer,
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

    // The shader modes read the input transfer from the surface's tone-map
    // tag, exactly as production does; the wrong-matrix control swaps in
    // the other HDR transfer. The VP mode attaches no tag (its blt reads
    // the verified_* helpers) and its control stays the SDR BT.709 space.
    let tone_map_tag = match (config.mode, config.wrong_matrix) {
        (ValidateMode::Vp, _) => None,
        (ValidateMode::ShaderPq, false) | (ValidateMode::ShaderHlg, true) => {
            Some(DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020)
        }
        (ValidateMode::ShaderPq, true) | (ValidateMode::ShaderHlg, false) => {
            Some(DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020)
        }
    };
    if config.wrong_matrix {
        println!(
            "[hdr-validate] NEGATIVE CONTROL: forcing wrong input color space \
             ({})",
            match config.mode {
                ValidateMode::Vp => "YCBCR_STUDIO_G22_LEFT_P709",
                ValidateMode::ShaderPq => "HLG transfer on PQ content",
                ValidateMode::ShaderHlg => "PQ transfer on HLG content",
            }
        );
    }

    // The SurfaceColor tag drives only the SDR blt path; both HDR
    // pipelines ignore it.
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
        tone_map_tag,
    )?;

    let capture = match config.mode {
        ValidateMode::Vp => {
            let stream_color_space_override = config
                .wrong_matrix
                .then_some(DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            // Blt with ColorSpace1 configuration + format-conversion check
            // (structural oracle #2), then raw backbuffer readback — no
            // Present.
            swap_chain.hdr10_validation_pass(
                &device,
                &surface,
                &content,
                stream_color_space_override,
            )?
        }
        ValidateMode::ShaderPq | ValidateMode::ShaderHlg => {
            println!("[hdr-validate] rendering through the production tone-map shader (PQ output)");
            swap_chain.hdr_shader_validation_pass(&device, &surface)?
        }
    };

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
