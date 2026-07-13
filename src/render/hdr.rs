//! HDR presentation-path skeleton.
//!
//! This module is the pure, COM-free half of the SDR/HDR fork: content color
//! classification, display/processor capability modelling, and the single
//! decision function that selects a [`VideoPresentationPath`] before any
//! renderer or swapchain work happens for a newly opened file.
//!
//! Every value that could be guessed is fenced behind a `verified_*`
//! helper: the HDR10 color spaces are resolved against the installed
//! windows 0.58 bindings and validated structurally + by pixel
//! (`bench/verify-colors-pq.ps1`); everything still unresolved (metadata
//! conversion, side-data layouts, display-active policy) remains a typed
//! error. Search for `HDR-VERIFY` to find the open items.
//!
//! PQ and HLG both present today, tone-mapped to SDR in a pixel shader
//! (`HdrToneMapRenderer`) rather than by the GPU video processor — see
//! [`VideoPresentationPath::HdrToSdrToneMapRequired`] for why the processor
//! cannot do it. This module resolves the stream's DXGI color space and
//! [`tone_map_signal`] decodes it into that shader's inputs.
//!
//! The verified SDR path never enters this module beyond
//! [`select_video_presentation_path`] returning
//! [`VideoPresentationPath::ExistingSdr`].

use std::{error::Error, fmt};

use windows::Win32::Graphics::Dxgi::{
    Common::{
        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_TYPE,
        DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_R10G10B10A2_UNORM,
    },
    DXGI_HDR_METADATA_HDR10,
};

use crate::ffi::ffmpeg::{
    AVColorPrimaries, AVColorPrimaries_AVCOL_PRI_BT2020, AVColorRange,
    AVColorRange_AVCOL_RANGE_JPEG, AVColorSpace, AVColorSpace_AVCOL_SPC_BT2020_NCL,
    AVColorSpace_AVCOL_SPC_UNSPECIFIED, AVColorTransferCharacteristic,
    AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
    AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
    AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED,
};

/// Project-owned rational mirroring FFmpeg's `AVRational` (which lacks a
/// `Debug` derive in our bindings). Values stay unconverted source
/// numerator/denominator pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrRational {
    pub(crate) num: i32,
    pub(crate) den: i32,
}

/// What the stream (and later the first frame) says the content is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentColorMode {
    /// SDR transfer, or fully untagged content that today's verified SDR
    /// path already handles (BT.601/BT.709 heuristics in `SurfaceColor`).
    Sdr,
    /// PQ / SMPTE ST 2084 transfer (HDR10 family).
    Hdr10Pq,
    /// Hybrid Log-Gamma transfer with BT.2020 primaries (standard BT.2100
    /// HLG signalling).
    Hlg,
    /// HDR-signalled but ambiguous, contradictory, or incomplete (e.g.
    /// BT.2020 primaries with an unspecified transfer, or PQ/HLG with
    /// non-BT.2020 or unspecified primaries). Never silently treated as
    /// SDR or as HDR10.
    Unknown,
}

/// The one control value for the SDR/HDR fork. Selected once per opened
/// file, before any frame is decoded, from stream-level tags only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoPresentationPath {
    /// The pixel-verified SDR pipeline, byte-for-byte the pre-HDR flow.
    ExistingSdr,
    /// HDR10 content presented on an active HDR display without tone
    /// mapping. Requires every capability bit checked in
    /// [`select_video_presentation_path`].
    Hdr10Passthrough,
    /// HDR content tone-mapped to SDR in our own pixel shader and presented
    /// through the existing SDR swapchain. The stream's colorimetry is
    /// resolved to a DXGI color space ([`tone_map_stream_color_space`]) and
    /// decoded into shader inputs ([`tone_map_signal`]).
    ///
    /// This does *not* use the video processor's HDR-to-SDR conversion.
    /// That was the original design and it is unreachable on real hardware:
    /// NVIDIA's video processor advertises no GHLG input conversion at all,
    /// and accepts PQ only to linear-scRGB or HDR10 outputs — never to the
    /// gamma-2.2 sRGB the 8-bit SDR backbuffer scans out. Driver conversion
    /// tables are not a portable substitute for doing the transfer math.
    ///
    /// Selected for HDR10 on an SDR display (and, until passthrough is
    /// implemented, whenever passthrough is not available) and for HLG.
    HdrToSdrToneMapRequired,
    /// Any HDR-signalled combination we cannot present correctly.
    UnsupportedHdr,
}

/// Complete color classification for an opened stream. Retains the raw
/// FFmpeg tags so later verification commits can map them to DXGI color
/// spaces without re-deriving anything.
#[derive(Debug, Clone)]
// `mode`, `color_space`, and `color_range` drive today's decisions
// (path selection and verified_hdr_stream_color_space); the remaining raw
// fields are read by the HDR-VERIFY metadata-conversion and
// frame-refinement commits.
#[allow(dead_code)]
pub(crate) struct ContentColorInfo {
    pub(crate) mode: ContentColorMode,
    pub(crate) color_primaries: AVColorPrimaries,
    pub(crate) color_transfer: AVColorTransferCharacteristic,
    pub(crate) color_space: AVColorSpace,
    pub(crate) color_range: AVColorRange,
    /// Populated only by first-frame refinement on the HDR path; stream-level
    /// classification never parses side data.
    pub(crate) mastering_display: Option<MasteringDisplayMetadata>,
    pub(crate) content_light: Option<ContentLightMetadata>,
}

/// Mastering display metadata in FFmpeg's own unconverted representation
/// (`AVMasteringDisplayMetadata` mirrors: rationals, plus validity flags).
///
/// HDR-VERIFY: the mapping from these rationals to
/// `DXGI_HDR_METADATA_HDR10` units (0.00002 chromaticity steps, 0.0001-nit
/// luminance steps, rounding, clamping) is done only in
/// [`build_dxgi_hdr10_metadata`] and is unresolved.
#[derive(Debug, Clone)]
// Read by the HDR-VERIFY metadata-conversion commit.
#[allow(dead_code)]
pub(crate) struct MasteringDisplayMetadata {
    /// CIE 1931 xy chromaticity per R/G/B primary, as source rationals.
    pub(crate) display_primaries: [[HdrRational; 2]; 3],
    /// CIE 1931 xy white point, as source rationals.
    pub(crate) white_point: [HdrRational; 2],
    /// Minimum luminance (cd/m²), as a source rational.
    pub(crate) min_luminance: HdrRational,
    /// Maximum luminance (cd/m²), as a source rational.
    pub(crate) max_luminance: HdrRational,
    pub(crate) has_primaries: bool,
    pub(crate) has_luminance: bool,
}

/// Content light level metadata (`AVContentLightMetadata` mirror).
#[derive(Debug, Clone)]
// Read by the HDR-VERIFY metadata-conversion commit.
#[allow(dead_code)]
pub(crate) struct ContentLightMetadata {
    pub(crate) max_content_light_level: Option<u32>,
    pub(crate) max_frame_average_light_level: Option<u32>,
}

/// Raw display descriptor fields preserved from `DXGI_OUTPUT_DESC1` for
/// future display policy. Interpretation (notably whether the fields mean
/// Windows HDR output is *active*, not merely that the panel is capable) is
/// deliberately not done here.
#[derive(Debug, Clone, Copy)]
// Read by the HDR-VERIFY display-policy commit.
#[allow(dead_code)]
pub(crate) struct HdrDisplayDescriptor {
    pub(crate) color_space: DXGI_COLOR_SPACE_TYPE,
    pub(crate) bits_per_color: u32,
    pub(crate) min_luminance: f32,
    pub(crate) max_luminance: f32,
    pub(crate) max_full_frame_luminance: f32,
}

/// Everything the decision function needs to know about the display,
/// swapchain, and video processor. `Default` is the conservative all-false
/// state: with it, HDR content can never select passthrough.
#[derive(Debug, Clone, Default)]
pub(crate) struct HdrPresentationCapabilities {
    /// `IDXGIOutput6` cast succeeded for the playback window's output.
    pub(crate) output6_available: bool,
    /// The attached display advertises HDR capability.
    /// HDR-VERIFY: interpretation of `DXGI_OUTPUT_DESC1` fields is
    /// unresolved; this stays false until verified.
    pub(crate) display_hdr_capable: bool,
    /// Windows HDR output is currently active on that display. A capable
    /// panel with HDR toggled off must leave this false.
    /// HDR-VERIFY: activity detection policy is unresolved.
    pub(crate) display_hdr_active: bool,
    /// `IDXGISwapChain3::CheckColorSpaceSupport` accepted the verified HDR10
    /// swapchain color space.
    pub(crate) swapchain_hdr10_color_space_supported: bool,
    /// `ID3D11VideoContext1` is available on the device.
    pub(crate) video_context1_available: bool,
    /// `CheckVideoProcessorFormatConversion` accepted NV12 HDR10 input to
    /// the HDR output format with the verified color spaces.
    /// HDR-VERIFY: the open-time probe still needs wiring to a live
    /// processor enumerator (passthrough commit); P010 input is unprobed.
    pub(crate) hdr10_format_conversion_supported: bool,
    /// Same, for HLG input. HDR-VERIFY: unresolved.
    pub(crate) hlg_format_conversion_supported: bool,
    /// Raw descriptor preserved for future display policy; `None` when the
    /// output could not be queried.
    pub(crate) display_descriptor: Option<HdrDisplayDescriptor>,
}

/// Typed HDR errors. These surface to the user through the existing
/// worker-error → `OpenFailed` event flow, so the `Display` text is the
/// user-facing message. The SDR path never constructs any of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HdrError {
    UnsupportedHdrPresentation,
    HdrSwapchainColorSpaceUnsupported,
    VideoContext1Unavailable,
    VideoProcessorEnumerator1Unavailable,
    // Raised by the passthrough commit when check_hdr_format_conversion
    // reports the processor cannot convert the HDR input.
    #[allow(dead_code)]
    HdrFormatConversionUnsupported,
    HdrColorSpaceUnverified,
    HdrMetadataConversionUnverified,
    SwapChain4Unavailable,
    /// The tone-map shader cannot sample the decoder's output format on this
    /// device (no NV12/P010 shader-resource views). Raised at open, never at
    /// the first draw: a per-frame render error is misread by device recovery
    /// as device-lost and crash-loops.
    HdrToneMapUnavailable,
}

impl fmt::Display for HdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedHdrPresentation => {
                "this video is HDR-tagged in a combination FastPlay cannot present yet"
            }
            Self::HdrSwapchainColorSpaceUnsupported => {
                "the display path does not support the HDR10 swapchain color space"
            }
            Self::VideoContext1Unavailable => {
                "HDR playback requires ID3D11VideoContext1, which this device does not expose"
            }
            Self::VideoProcessorEnumerator1Unavailable => {
                "HDR playback requires ID3D11VideoProcessorEnumerator1, which this device does \
                 not expose"
            }
            Self::HdrFormatConversionUnsupported => {
                "the GPU video processor cannot convert this HDR format to the display format"
            }
            Self::HdrColorSpaceUnverified => {
                "HDR playback is not available yet: the HDR color-space mapping is unverified"
            }
            Self::HdrMetadataConversionUnverified => {
                "HDR playback is not available yet: HDR metadata conversion is unverified"
            }
            Self::SwapChain4Unavailable => {
                "HDR metadata requires IDXGISwapChain4, which this system does not expose"
            }
            Self::HdrToneMapUnavailable => {
                "this GPU cannot sample the decoded HDR frame format, so HDR tone mapping is \
                 unavailable"
            }
        };
        f.write_str(message)
    }
}

impl Error for HdrError {}

/// Pure classification of stream-level color tags into a
/// [`ContentColorMode`]. The transfer characteristic controls the
/// dynamic-range classification; primaries alone never select HDR:
///
/// - PQ + BT.2020 primaries → `Hdr10Pq` (standard HDR10 signalling).
/// - PQ + anything else → `Unknown`: known non-BT.2020 primaries
///   contradict HDR10, and unspecified primaries are incomplete
///   signalling with no verified default here — neither may be assumed
///   HDR10 or silently tone-mapped.
/// - HLG + BT.2020 primaries → `Hlg` (standard BT.2100 HLG signalling).
/// - HLG + anything else → `Unknown`, by the same contradictory /
///   incomplete reasoning as PQ; never downgraded to SDR.
/// - Unspecified transfer + BT.2020 primaries → `Unknown` (could be PQ,
///   HLG, or wide-gamut SDR).
/// - Any explicit SDR-class transfer, or fully untagged content → `Sdr`.
pub(crate) fn classify_color_tags(
    color_primaries: AVColorPrimaries,
    color_transfer: AVColorTransferCharacteristic,
) -> ContentColorMode {
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084 {
        // Standard HDR10 requires BT.2020 primaries under the PQ transfer.
        // PQ with known non-BT.2020 primaries (e.g. BT.709) is
        // contradictory and not valid HDR10; PQ with unspecified primaries
        // is incomplete. Both dead-end as Unknown (UnsupportedHdr at path
        // level) rather than being guessed into HDR10 or routed into a
        // tone-mapping path.
        return if color_primaries == AVColorPrimaries_AVCOL_PRI_BT2020 {
            ContentColorMode::Hdr10Pq
        } else {
            ContentColorMode::Unknown
        };
    }
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67 {
        // BT.2100 defines HLG over BT.2020 primaries, exactly like PQ, and
        // the repository holds no compatibility evidence for accepting
        // anything else. Known non-BT.2020 primaries are contradictory and
        // unspecified primaries are incomplete; both dead-end as Unknown
        // (UnsupportedHdr at path level), the same conservative policy as
        // PQ above. Today Hlg and Unknown reach the identical dead end, so
        // this costs nothing; if real HLG content ships with unspecified
        // primaries, the future HLG commit may relax this with evidence.
        return if color_primaries == AVColorPrimaries_AVCOL_PRI_BT2020 {
            ContentColorMode::Hlg
        } else {
            ContentColorMode::Unknown
        };
    }
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED {
        if color_primaries == AVColorPrimaries_AVCOL_PRI_BT2020 {
            // BT.2020 primaries with no transfer tag is HDR-signalled but
            // ambiguous: it could be PQ, HLG, or wide-gamut SDR. It must
            // not silently reach the SDR path, and must not be assumed
            // HDR10.
            return ContentColorMode::Unknown;
        }
        // HDR-VERIFY: fully untagged content (unspecified transfer,
        // non-BT.2020 primaries) classifies as Sdr BY POLICY, and
        // permanently so: first-frame refinement runs only on the HDR path
        // and never upgrades an SDR-selected presentation path. An
        // untagged-but-actually-HDR file (stripped-container PQ, some
        // screen recordings) therefore plays washed out through the SDR
        // path — the same bug class this fork exists to fix. Fixing that
        // requires separate refinement/lifecycle work (upgrade from
        // frame-level tags before presentation), not a classifier change.
        return ContentColorMode::Sdr;
    }
    // Every remaining transfer tag falls through to Sdr. For the standard
    // SDR-class transfers (BT.709, gamma 2.2/2.8, SMPTE 170M/240M, sRGB,
    // BT2020_10/12) that is explicit policy; for the exotic and reserved
    // codepoints (linear, log, SMPTE 428, vendor extensions) it is
    // retained legacy behavior — none of them carry an HDR signal, and
    // they continue through the verified SDR path exactly as before this
    // module existed. That includes BT.2020 primaries with an SDR
    // transfer — wide-gamut SDR: the primaries widen the gamut but do not
    // change the dynamic range, so it must never select PQ output.
    ContentColorMode::Sdr
}

/// The single presentation-path decision. Pure and deterministic: no COM
/// calls, no FFmpeg calls — everything it needs arrives in its arguments.
pub(crate) fn select_video_presentation_path(
    content: &ContentColorInfo,
    capabilities: &HdrPresentationCapabilities,
) -> VideoPresentationPath {
    match content.mode {
        ContentColorMode::Sdr => VideoPresentationPath::ExistingSdr,
        ContentColorMode::Hdr10Pq => {
            if capabilities.display_hdr_active
                && capabilities.swapchain_hdr10_color_space_supported
                && capabilities.video_context1_available
                && capabilities.hdr10_format_conversion_supported
            {
                VideoPresentationPath::Hdr10Passthrough
            } else {
                // SDR display, or an HDR display missing some required
                // passthrough capability: only an explicit HDR-to-SDR
                // conversion may present this. It must never fall through
                // to the existing SDR path. (The tone-map path performs its
                // own structural checks — ID3D11VideoContext1 at open,
                // CheckVideoProcessorFormatConversion at the blt — and
                // dead-ends as a typed error when they fail.)
                VideoPresentationPath::HdrToSdrToneMapRequired
            }
        }
        // HLG is never auto-classified as HDR10 passthrough and never
        // reaches the plain SDR path; it presents through the explicit
        // HDR-to-SDR conversion (GHLG stream color space). A native HLG
        // passthrough path is future work.
        ContentColorMode::Hlg => VideoPresentationPath::HdrToSdrToneMapRequired,
        // HDR-signalled but ambiguous content dead-ends explicitly.
        ContentColorMode::Unknown => VideoPresentationPath::UnsupportedHdr,
    }
}

/// Swapchain backbuffer format per path. Pure so the format pairing is
/// unit-testable; the verified SDR constructor keeps its own literal
/// `DXGI_FORMAT_B8G8R8A8_UNORM` untouched and this function must always
/// agree with it (see `sdr_swapchain_format_is_unchanged`).
pub(crate) fn swapchain_format_for_path(path: VideoPresentationPath) -> DXGI_FORMAT {
    match path {
        VideoPresentationPath::ExistingSdr => DXGI_FORMAT_B8G8R8A8_UNORM,
        VideoPresentationPath::Hdr10Passthrough => DXGI_FORMAT_R10G10B10A2_UNORM,
        // Tone mapping renders SDR output; unsupported never creates one.
        VideoPresentationPath::HdrToSdrToneMapRequired => DXGI_FORMAT_B8G8R8A8_UNORM,
        VideoPresentationPath::UnsupportedHdr => DXGI_FORMAT_B8G8R8A8_UNORM,
    }
}

// ---------------------------------------------------------------------------
// Verification boundaries. Each returns a typed error until its value is
// verified in a dedicated later commit. None of them may be replaced by a
// guessed constant or a numeric discriminant.
// ---------------------------------------------------------------------------

/// The HDR10 swapchain color space: RGB, full range, PQ (G2084), BT.2020
/// primaries.
///
/// Verified against the installed windows 0.58 bindings
/// (`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020` = 12), structurally via
/// `CheckColorSpaceSupport` on the live HDR display, and by pixel via
/// `bench/verify-colors-pq.ps1` (backbuffer readback vs ffmpeg's PQ
/// reference decode, with a wrong-matrix negative control).
pub(crate) fn verified_hdr10_swapchain_color_space() -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    Ok(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
}

/// The YCbCr input color space for decoded HDR10 frames.
///
/// Resolved only for the standard HDR10 signal — PQ transfer, BT.2020
/// non-constant-luminance matrix (or unspecified, which BT.2100 defines as
/// BT.2020 NCL for PQ), studio/limited range — to
/// `DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020` (13). Everything else
/// stays a typed error rather than a guess:
/// - Full-range PQ YCbCr has NO variant in the installed bindings'
///   `DXGI_COLOR_SPACE_TYPE` enum, so it is inexpressible on this path.
/// - Constant-luminance BT.2020 and non-BT.2020 matrices are unverified.
/// - HLG resolves in its own commit.
///
/// HDR-VERIFY: chroma siting. `..._LEFT_P2020` matches H.264's default
/// 4:2:0 siting; `..._TOPLEFT_P2020` (16) exists for HEVC UHD content that
/// signals top-left. `ContentColorInfo` does not carry chroma_location yet,
/// and the bar-interior pixel oracle cannot distinguish siting (it only
/// shifts chroma upsampling by half a pixel at edges). Revisit when HEVC
/// HDR content is wired.
pub(crate) fn verified_hdr_stream_color_space(
    content: &ContentColorInfo,
) -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    if content.mode != ContentColorMode::Hdr10Pq {
        return Err(HdrError::HdrColorSpaceUnverified);
    }
    if content.color_range == AVColorRange_AVCOL_RANGE_JPEG {
        return Err(HdrError::UnsupportedHdrPresentation);
    }
    if content.color_space != AVColorSpace_AVCOL_SPC_BT2020_NCL
        && content.color_space != AVColorSpace_AVCOL_SPC_UNSPECIFIED
    {
        return Err(HdrError::HdrColorSpaceUnverified);
    }
    Ok(DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020)
}

/// The video processor output color space for HDR10 passthrough: identical
/// to the swapchain space (the processor writes PQ-encoded RGB directly
/// into the R10G10B10A2 backbuffer; no transfer conversion happens between
/// blt and scanout).
pub(crate) fn verified_hdr10_processor_output_color_space(
) -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    verified_hdr10_swapchain_color_space()
}

/// The YCbCr input color space the tone-map blt tags the decoded HDR
/// stream with, so the GPU video processor knows what it is converting
/// *from*:
///
/// - HDR10 PQ reuses [`verified_hdr_stream_color_space`]
///   (`YCBCR_STUDIO_G2084_LEFT_P2020`, resolved + pixel-validated), with
///   the same typed errors for full-range / constant-luminance /
///   non-BT.2020 signals.
/// - HLG (BT.2100: BT.2020 NCL matrix, or unspecified which BT.2100
///   defines as BT.2020 NCL) maps to the only GHLG variants the DXGI enum
///   offers: `YCBCR_STUDIO_GHLG_TOPLEFT_P2020` for studio/unspecified
///   range and `YCBCR_FULL_GHLG_TOPLEFT_P2020` for full range. Validated
///   structurally per-device via `CheckVideoProcessorFormatConversion` at
///   the blt (there is no chroma-siting choice to get wrong: TOPLEFT is
///   the only siting DXGI defines for GHLG).
/// - Anything else stays a typed error rather than a guess.
pub(crate) fn tone_map_stream_color_space(
    content: &ContentColorInfo,
) -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    match content.mode {
        ContentColorMode::Hdr10Pq => verified_hdr_stream_color_space(content),
        ContentColorMode::Hlg => {
            if content.color_space != AVColorSpace_AVCOL_SPC_BT2020_NCL
                && content.color_space != AVColorSpace_AVCOL_SPC_UNSPECIFIED
            {
                return Err(HdrError::HdrColorSpaceUnverified);
            }
            if content.color_range == AVColorRange_AVCOL_RANGE_JPEG {
                Ok(DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020)
            } else {
                Ok(DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020)
            }
        }
        ContentColorMode::Sdr | ContentColorMode::Unknown => Err(HdrError::HdrColorSpaceUnverified),
    }
}

/// The transfer function carried by a frame on the tone-map path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HdrTransfer {
    /// SMPTE ST 2084 (PQ). Absolute: signal 1.0 means 10 000 cd/m²,
    /// independent of the mastering display.
    Pq,
    /// BT.2100 Hybrid Log-Gamma. Scene-referred: the inverse OETF yields
    /// scene light, and the OOTF (system gamma) completes it to display
    /// light. Not interchangeable with PQ — applying the wrong one is a
    /// gross error, not a shade of grading.
    Hlg,
}

/// Everything the tone-map pixel shader needs to know about the decoded
/// signal, decoded back out of the DXGI color space that
/// [`tone_map_stream_color_space`] already resolved.
///
/// The DXGI color space stays the single validated representation of the
/// stream's colorimetry (it is what rejects full-range PQ, constant-luminance
/// matrices, and non-BT.2020 signalling), even though the tone-map path no
/// longer hands it to the video processor. This function is the one place
/// that turns it back into shader inputs, so the shader can never disagree
/// with what was validated at open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrToneMapSignal {
    pub(crate) transfer: HdrTransfer,
    /// Full-range (0–max) samples when true, studio/limited otherwise.
    pub(crate) full_range: bool,
}

/// Decode a validated tone-map stream color space into shader inputs.
///
/// Only the spaces [`tone_map_stream_color_space`] can actually produce are
/// accepted; anything else is a typed error rather than a guessed transfer.
pub(crate) fn tone_map_signal(
    color_space: DXGI_COLOR_SPACE_TYPE,
) -> Result<HdrToneMapSignal, HdrError> {
    // Both PQ sitings map to the same shader inputs: chroma siting shifts
    // upsampling by half a chroma sample and does not change the transfer,
    // matrix, or range the shader applies.
    if color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_TOPLEFT_P2020
    {
        return Ok(HdrToneMapSignal {
            transfer: HdrTransfer::Pq,
            full_range: false,
        });
    }
    if color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020 {
        return Ok(HdrToneMapSignal {
            transfer: HdrTransfer::Hlg,
            full_range: false,
        });
    }
    if color_space == DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020 {
        return Ok(HdrToneMapSignal {
            transfer: HdrTransfer::Hlg,
            full_range: true,
        });
    }
    Err(HdrError::HdrColorSpaceUnverified)
}

/// Conversion boundary between FFmpeg-sourced metadata and DXGI.
///
/// HDR-VERIFY: `DXGI_HDR_METADATA_HDR10` units, scaling, rounding, clamping,
/// FFmpeg rational conversion, and the missing-value fallback policy are all
/// unresolved. Mastering metadata must not become mandatory for HDR
/// playback when this is implemented.
// Wired to apply_hdr10_metadata by the passthrough commit.
#[allow(dead_code)]
pub(crate) fn build_dxgi_hdr10_metadata(
    _mastering: Option<&MasteringDisplayMetadata>,
    _content_light: Option<&ContentLightMetadata>,
) -> Result<DXGI_HDR_METADATA_HDR10, HdrError> {
    Err(HdrError::HdrMetadataConversionUnverified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::ffmpeg::{
        AVColorPrimaries_AVCOL_PRI_BT709, AVColorPrimaries_AVCOL_PRI_SMPTE170M,
        AVColorPrimaries_AVCOL_PRI_UNSPECIFIED, AVColorRange_AVCOL_RANGE_UNSPECIFIED,
        AVColorSpace_AVCOL_SPC_UNSPECIFIED, AVColorTransferCharacteristic_AVCOL_TRC_BT1361_ECG,
        AVColorTransferCharacteristic_AVCOL_TRC_BT2020_10,
        AVColorTransferCharacteristic_AVCOL_TRC_BT2020_12,
        AVColorTransferCharacteristic_AVCOL_TRC_BT709,
        AVColorTransferCharacteristic_AVCOL_TRC_GAMMA22,
        AVColorTransferCharacteristic_AVCOL_TRC_GAMMA28,
        AVColorTransferCharacteristic_AVCOL_TRC_IEC61966_2_1,
        AVColorTransferCharacteristic_AVCOL_TRC_IEC61966_2_4,
        AVColorTransferCharacteristic_AVCOL_TRC_LINEAR,
        AVColorTransferCharacteristic_AVCOL_TRC_LOG,
        AVColorTransferCharacteristic_AVCOL_TRC_LOG_SQRT,
        AVColorTransferCharacteristic_AVCOL_TRC_RESERVED,
        AVColorTransferCharacteristic_AVCOL_TRC_RESERVED0,
        AVColorTransferCharacteristic_AVCOL_TRC_SMPTE170M,
        AVColorTransferCharacteristic_AVCOL_TRC_SMPTE240M,
        AVColorTransferCharacteristic_AVCOL_TRC_SMPTE428,
        AVColorTransferCharacteristic_AVCOL_TRC_V_LOG,
    };

    fn info(mode: ContentColorMode) -> ContentColorInfo {
        ContentColorInfo {
            mode,
            color_primaries: AVColorPrimaries_AVCOL_PRI_UNSPECIFIED,
            color_transfer: AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED,
            color_space: AVColorSpace_AVCOL_SPC_UNSPECIFIED,
            color_range: AVColorRange_AVCOL_RANGE_UNSPECIFIED,
            mastering_display: None,
            content_light: None,
        }
    }

    fn full_capabilities() -> HdrPresentationCapabilities {
        HdrPresentationCapabilities {
            output6_available: true,
            display_hdr_capable: true,
            display_hdr_active: true,
            swapchain_hdr10_color_space_supported: true,
            video_context1_available: true,
            hdr10_format_conversion_supported: true,
            hlg_format_conversion_supported: true,
            display_descriptor: None,
        }
    }

    #[test]
    fn sdr_always_selects_existing_sdr() {
        let sdr = info(ContentColorMode::Sdr);
        assert_eq!(
            select_video_presentation_path(&sdr, &HdrPresentationCapabilities::default()),
            VideoPresentationPath::ExistingSdr
        );
        assert_eq!(
            select_video_presentation_path(&sdr, &full_capabilities()),
            VideoPresentationPath::ExistingSdr
        );
    }

    #[test]
    fn pq_with_full_capabilities_selects_passthrough() {
        assert_eq!(
            select_video_presentation_path(&info(ContentColorMode::Hdr10Pq), &full_capabilities()),
            VideoPresentationPath::Hdr10Passthrough
        );
    }

    #[test]
    fn pq_on_sdr_display_requires_tone_mapping() {
        let caps = HdrPresentationCapabilities {
            display_hdr_active: false,
            ..full_capabilities()
        };
        assert_eq!(
            select_video_presentation_path(&info(ContentColorMode::Hdr10Pq), &caps),
            VideoPresentationPath::HdrToSdrToneMapRequired
        );
    }

    #[test]
    fn pq_without_video_context1_does_not_select_passthrough() {
        let caps = HdrPresentationCapabilities {
            video_context1_available: false,
            ..full_capabilities()
        };
        let path = select_video_presentation_path(&info(ContentColorMode::Hdr10Pq), &caps);
        assert_ne!(path, VideoPresentationPath::Hdr10Passthrough);
        assert_ne!(path, VideoPresentationPath::ExistingSdr);
    }

    #[test]
    fn pq_without_format_conversion_does_not_select_passthrough() {
        let caps = HdrPresentationCapabilities {
            hdr10_format_conversion_supported: false,
            ..full_capabilities()
        };
        let path = select_video_presentation_path(&info(ContentColorMode::Hdr10Pq), &caps);
        assert_ne!(path, VideoPresentationPath::Hdr10Passthrough);
        assert_ne!(path, VideoPresentationPath::ExistingSdr);
    }

    #[test]
    fn pq_without_passthrough_capabilities_selects_tone_map_never_sdr() {
        // Missing passthrough capability (SDR display, no context1, no
        // format conversion) routes PQ to the explicit tone-map path —
        // never the plain SDR path, never a dead end.
        for caps in [
            HdrPresentationCapabilities::default(),
            HdrPresentationCapabilities {
                display_hdr_active: false,
                ..full_capabilities()
            },
            HdrPresentationCapabilities {
                video_context1_available: false,
                ..full_capabilities()
            },
            HdrPresentationCapabilities {
                hdr10_format_conversion_supported: false,
                ..full_capabilities()
            },
        ] {
            assert_eq!(
                select_video_presentation_path(&info(ContentColorMode::Hdr10Pq), &caps),
                VideoPresentationPath::HdrToSdrToneMapRequired
            );
        }
    }

    #[test]
    fn hlg_selects_tone_map_never_passthrough_or_existing_sdr() {
        for caps in [HdrPresentationCapabilities::default(), full_capabilities()] {
            let path = select_video_presentation_path(&info(ContentColorMode::Hlg), &caps);
            assert_eq!(path, VideoPresentationPath::HdrToSdrToneMapRequired);
        }
    }

    #[test]
    fn unknown_never_selects_passthrough_or_existing_sdr() {
        for caps in [HdrPresentationCapabilities::default(), full_capabilities()] {
            let path = select_video_presentation_path(&info(ContentColorMode::Unknown), &caps);
            assert_eq!(path, VideoPresentationPath::UnsupportedHdr);
        }
    }

    #[test]
    fn unknown_with_hdr_transfer_is_unsupported_not_sdr() {
        // An HDR-signalled Unknown (BT.2020 primaries, unspecified transfer)
        // must dead-end even with every capability present.
        let mut content = info(ContentColorMode::Unknown);
        content.color_primaries = AVColorPrimaries_AVCOL_PRI_BT2020;
        assert_eq!(
            select_video_presentation_path(&content, &full_capabilities()),
            VideoPresentationPath::UnsupportedHdr
        );
        // And one carrying the PQ transfer tag itself.
        content.color_transfer = AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084;
        assert_eq!(
            select_video_presentation_path(&content, &full_capabilities()),
            VideoPresentationPath::UnsupportedHdr
        );
    }

    #[test]
    fn classify_pq_is_hdr10() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT2020,
                AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
            ),
            ContentColorMode::Hdr10Pq
        );
    }

    #[test]
    fn classify_hlg_is_hlg_not_sdr() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT2020,
                AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
            ),
            ContentColorMode::Hlg
        );
    }

    #[test]
    fn classify_bt2020_untagged_transfer_is_unknown() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT2020,
                AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED,
            ),
            ContentColorMode::Unknown
        );
    }

    #[test]
    fn classify_untagged_and_bt709_stay_sdr() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_UNSPECIFIED,
                AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED,
            ),
            ContentColorMode::Sdr
        );
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT709,
                AVColorTransferCharacteristic_AVCOL_TRC_BT709,
            ),
            ContentColorMode::Sdr
        );
    }

    #[test]
    fn classify_pq_with_bt709_primaries_is_unknown() {
        // PQ with known non-BT.2020 primaries contradicts standard HDR10.
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT709,
                AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
            ),
            ContentColorMode::Unknown
        );
    }

    #[test]
    fn classify_pq_with_smpte170m_primaries_is_unknown() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_SMPTE170M,
                AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
            ),
            ContentColorMode::Unknown
        );
    }

    #[test]
    fn classify_pq_with_unspecified_primaries_is_unknown() {
        // Incomplete PQ signalling: no verified default exists here, so it
        // must not be assumed to be valid HDR10.
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_UNSPECIFIED,
                AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
            ),
            ContentColorMode::Unknown
        );
    }

    #[test]
    fn classify_contradictory_or_incomplete_hlg_is_unknown() {
        // Same conservative policy as PQ: BT.2100 HLG requires BT.2020
        // primaries. Known non-BT.2020 primaries are contradictory and
        // unspecified primaries are incomplete; neither may be assumed
        // valid HLG, and neither may be downgraded to SDR.
        for primaries in [
            AVColorPrimaries_AVCOL_PRI_BT709,
            AVColorPrimaries_AVCOL_PRI_SMPTE170M,
            AVColorPrimaries_AVCOL_PRI_UNSPECIFIED,
        ] {
            assert_eq!(
                classify_color_tags(
                    primaries,
                    AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
                ),
                ContentColorMode::Unknown
            );
        }
    }

    #[test]
    fn classified_hlg_routes_to_tone_map_never_sdr_or_passthrough() {
        // End to end: standard BT.2100 HLG classifies as Hlg and routes to
        // the explicit HDR-to-SDR conversion under any capabilities — never
        // HDR10 passthrough, never the ordinary SDR path.
        let mode = classify_color_tags(
            AVColorPrimaries_AVCOL_PRI_BT2020,
            AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
        );
        assert_eq!(mode, ContentColorMode::Hlg);
        let mut content = info(mode);
        content.color_primaries = AVColorPrimaries_AVCOL_PRI_BT2020;
        content.color_transfer = AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67;
        for caps in [HdrPresentationCapabilities::default(), full_capabilities()] {
            assert_eq!(
                select_video_presentation_path(&content, &caps),
                VideoPresentationPath::HdrToSdrToneMapRequired
            );
        }
    }

    #[test]
    fn every_explicit_sdr_transfer_stays_sdr_with_bt2020_primaries() {
        // Table of every bound transfer value that reaches the SDR
        // fallthrough arm — everything except PQ, HLG, and unspecified
        // (aliases and the *_NB / EXT_BASE sentinels excluded; V_LOG
        // stands in for the vendor-extension range). Each is paired with
        // BT.2020 primaries, which also pins the wide-gamut-SDR policy:
        // primaries alone never select HDR.
        let sdr_transfers = [
            AVColorTransferCharacteristic_AVCOL_TRC_RESERVED0,
            AVColorTransferCharacteristic_AVCOL_TRC_BT709,
            AVColorTransferCharacteristic_AVCOL_TRC_RESERVED,
            AVColorTransferCharacteristic_AVCOL_TRC_GAMMA22,
            AVColorTransferCharacteristic_AVCOL_TRC_GAMMA28,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE170M,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE240M,
            AVColorTransferCharacteristic_AVCOL_TRC_LINEAR,
            AVColorTransferCharacteristic_AVCOL_TRC_LOG,
            AVColorTransferCharacteristic_AVCOL_TRC_LOG_SQRT,
            AVColorTransferCharacteristic_AVCOL_TRC_IEC61966_2_4,
            AVColorTransferCharacteristic_AVCOL_TRC_BT1361_ECG,
            AVColorTransferCharacteristic_AVCOL_TRC_IEC61966_2_1,
            AVColorTransferCharacteristic_AVCOL_TRC_BT2020_10,
            AVColorTransferCharacteristic_AVCOL_TRC_BT2020_12,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE428,
            AVColorTransferCharacteristic_AVCOL_TRC_V_LOG,
        ];
        for transfer in sdr_transfers {
            assert_eq!(
                classify_color_tags(AVColorPrimaries_AVCOL_PRI_BT2020, transfer),
                ContentColorMode::Sdr,
                "transfer value {transfer} must classify as Sdr",
            );
        }
    }

    #[test]
    fn classify_unspecified_transfer_with_known_non_bt2020_primaries_is_sdr() {
        assert_eq!(
            classify_color_tags(
                AVColorPrimaries_AVCOL_PRI_BT709,
                AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED,
            ),
            ContentColorMode::Sdr
        );
    }

    #[test]
    fn contradictory_pq_dead_ends_as_unsupported_never_tone_mapped() {
        // End to end: classification, then path selection. Contradictory PQ
        // must produce UnsupportedHdr with or without capabilities — never
        // the SDR path and never the tone-mapping path.
        let mode = classify_color_tags(
            AVColorPrimaries_AVCOL_PRI_BT709,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        );
        assert_eq!(mode, ContentColorMode::Unknown);
        let mut content = info(mode);
        content.color_primaries = AVColorPrimaries_AVCOL_PRI_BT709;
        content.color_transfer = AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084;
        for caps in [HdrPresentationCapabilities::default(), full_capabilities()] {
            assert_eq!(
                select_video_presentation_path(&content, &caps),
                VideoPresentationPath::UnsupportedHdr
            );
        }
    }

    #[test]
    fn classified_standard_pq_still_selects_passthrough() {
        // End to end: the valid HDR10 signal still classifies as Hdr10Pq and
        // still selects the passthrough/skeleton path under full
        // capabilities.
        let mode = classify_color_tags(
            AVColorPrimaries_AVCOL_PRI_BT2020,
            AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
        );
        assert_eq!(mode, ContentColorMode::Hdr10Pq);
        let mut content = info(mode);
        content.color_primaries = AVColorPrimaries_AVCOL_PRI_BT2020;
        content.color_transfer = AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084;
        assert_eq!(
            select_video_presentation_path(&content, &full_capabilities()),
            VideoPresentationPath::Hdr10Passthrough
        );
    }

    #[test]
    fn sdr_swapchain_format_is_unchanged() {
        assert_eq!(
            swapchain_format_for_path(VideoPresentationPath::ExistingSdr),
            DXGI_FORMAT_B8G8R8A8_UNORM
        );
    }

    #[test]
    fn hdr10_swapchain_format_is_r10g10b10a2() {
        assert_eq!(
            swapchain_format_for_path(VideoPresentationPath::Hdr10Passthrough),
            DXGI_FORMAT_R10G10B10A2_UNORM
        );
    }

    #[test]
    fn hdr10_swapchain_and_processor_output_are_rgb_full_pq_bt2020() {
        assert_eq!(
            verified_hdr10_swapchain_color_space(),
            Ok(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
        );
        // Passthrough writes PQ RGB straight into the backbuffer, so the
        // processor output space must equal the swapchain space.
        assert_eq!(
            verified_hdr10_processor_output_color_space(),
            verified_hdr10_swapchain_color_space()
        );
    }

    fn hdr10_info(color_space: AVColorSpace, color_range: AVColorRange) -> ContentColorInfo {
        ContentColorInfo {
            mode: ContentColorMode::Hdr10Pq,
            color_primaries: AVColorPrimaries_AVCOL_PRI_BT2020,
            color_transfer: AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084,
            color_space,
            color_range,
            mastering_display: None,
            content_light: None,
        }
    }

    #[test]
    fn standard_hdr10_stream_maps_to_studio_pq_left_bt2020() {
        use crate::ffi::ffmpeg::AVColorRange_AVCOL_RANGE_MPEG;
        for color_space in [
            AVColorSpace_AVCOL_SPC_BT2020_NCL,
            // BT.2100: unspecified matrix with PQ means BT.2020 NCL.
            AVColorSpace_AVCOL_SPC_UNSPECIFIED,
        ] {
            for color_range in [
                AVColorRange_AVCOL_RANGE_MPEG,
                crate::ffi::ffmpeg::AVColorRange_AVCOL_RANGE_UNSPECIFIED,
            ] {
                assert_eq!(
                    verified_hdr_stream_color_space(&hdr10_info(color_space, color_range)),
                    Ok(DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020)
                );
            }
        }
    }

    #[test]
    fn non_standard_hdr10_signals_stay_typed_errors() {
        use crate::ffi::ffmpeg::{
            AVColorRange_AVCOL_RANGE_MPEG, AVColorSpace_AVCOL_SPC_BT2020_CL,
            AVColorSpace_AVCOL_SPC_BT709,
        };
        // Full-range PQ YCbCr has no DXGI_COLOR_SPACE_TYPE variant.
        assert_eq!(
            verified_hdr_stream_color_space(&hdr10_info(
                AVColorSpace_AVCOL_SPC_BT2020_NCL,
                AVColorRange_AVCOL_RANGE_JPEG,
            )),
            Err(HdrError::UnsupportedHdrPresentation)
        );
        // Constant-luminance and non-BT.2020 matrices are unverified.
        for color_space in [
            AVColorSpace_AVCOL_SPC_BT2020_CL,
            AVColorSpace_AVCOL_SPC_BT709,
        ] {
            assert_eq!(
                verified_hdr_stream_color_space(&hdr10_info(
                    color_space,
                    AVColorRange_AVCOL_RANGE_MPEG,
                )),
                Err(HdrError::HdrColorSpaceUnverified)
            );
        }
        // Non-PQ modes (HLG, Unknown, Sdr) do not resolve here.
        for mode in [
            ContentColorMode::Hlg,
            ContentColorMode::Unknown,
            ContentColorMode::Sdr,
        ] {
            assert_eq!(
                verified_hdr_stream_color_space(&info(mode)),
                Err(HdrError::HdrColorSpaceUnverified)
            );
        }
    }

    #[test]
    fn tone_map_signal_round_trips_every_resolvable_stream_space() {
        // Whatever tone_map_stream_color_space resolves, tone_map_signal must
        // decode — otherwise a file passes the open gate and then dead-ends in
        // the renderer. These two functions are the only link between the
        // validated colorimetry and the shader, so they must stay total with
        // respect to each other.
        use crate::ffi::ffmpeg::{AVColorRange_AVCOL_RANGE_JPEG, AVColorRange_AVCOL_RANGE_MPEG};

        let cases = [
            (
                hdr10_info(
                    AVColorSpace_AVCOL_SPC_BT2020_NCL,
                    AVColorRange_AVCOL_RANGE_MPEG,
                ),
                HdrTransfer::Pq,
                false,
            ),
            (
                hlg_info(
                    AVColorSpace_AVCOL_SPC_BT2020_NCL,
                    AVColorRange_AVCOL_RANGE_MPEG,
                ),
                HdrTransfer::Hlg,
                false,
            ),
            (
                hlg_info(
                    AVColorSpace_AVCOL_SPC_BT2020_NCL,
                    AVColorRange_AVCOL_RANGE_JPEG,
                ),
                HdrTransfer::Hlg,
                true,
            ),
        ];
        for (content, transfer, full_range) in cases {
            let space = tone_map_stream_color_space(&content)
                .expect("standard HDR signalling must resolve a stream color space");
            assert_eq!(
                tone_map_signal(space),
                Ok(HdrToneMapSignal {
                    transfer,
                    full_range
                })
            );
        }
    }

    #[test]
    fn tone_map_signal_rejects_a_non_hdr_color_space() {
        // An SDR YCbCr space must never be decoded into a transfer function:
        // the shader would apply a PQ or HLG EOTF to gamma-encoded samples.
        assert_eq!(
            tone_map_signal(
                windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
            ),
            Err(HdrError::HdrColorSpaceUnverified)
        );
    }

    #[test]
    fn tone_map_pq_stream_space_matches_verified_hdr10() {
        // PQ reuses the resolved, pixel-validated HDR10 stream color space.
        let content = hdr10_info(
            AVColorSpace_AVCOL_SPC_BT2020_NCL,
            crate::ffi::ffmpeg::AVColorRange_AVCOL_RANGE_MPEG,
        );
        assert_eq!(
            tone_map_stream_color_space(&content),
            Ok(DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020)
        );
    }

    fn hlg_info(color_space: AVColorSpace, color_range: AVColorRange) -> ContentColorInfo {
        ContentColorInfo {
            mode: ContentColorMode::Hlg,
            color_primaries: AVColorPrimaries_AVCOL_PRI_BT2020,
            color_transfer: AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
            color_space,
            color_range,
            mastering_display: None,
            content_light: None,
        }
    }

    #[test]
    fn tone_map_hlg_stream_space_is_ghlg_topleft() {
        use crate::ffi::ffmpeg::{
            AVColorRange_AVCOL_RANGE_MPEG, AVColorRange_AVCOL_RANGE_UNSPECIFIED,
        };
        // Studio / unspecified range HLG (BT.2020 NCL, or unspecified which
        // BT.2100 defines as BT.2020 NCL) maps to studio GHLG.
        for color_space in [
            AVColorSpace_AVCOL_SPC_BT2020_NCL,
            AVColorSpace_AVCOL_SPC_UNSPECIFIED,
        ] {
            for color_range in [
                AVColorRange_AVCOL_RANGE_MPEG,
                AVColorRange_AVCOL_RANGE_UNSPECIFIED,
            ] {
                assert_eq!(
                    tone_map_stream_color_space(&hlg_info(color_space, color_range)),
                    Ok(DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020)
                );
            }
        }
        // Full-range HLG maps to the full GHLG variant.
        assert_eq!(
            tone_map_stream_color_space(&hlg_info(
                AVColorSpace_AVCOL_SPC_BT2020_NCL,
                AVColorRange_AVCOL_RANGE_JPEG,
            )),
            Ok(DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020)
        );
    }

    #[test]
    fn tone_map_non_standard_hlg_matrix_is_typed_error() {
        use crate::ffi::ffmpeg::{
            AVColorRange_AVCOL_RANGE_MPEG, AVColorSpace_AVCOL_SPC_BT2020_CL,
            AVColorSpace_AVCOL_SPC_BT709,
        };
        for color_space in [
            AVColorSpace_AVCOL_SPC_BT2020_CL,
            AVColorSpace_AVCOL_SPC_BT709,
        ] {
            assert_eq!(
                tone_map_stream_color_space(&hlg_info(color_space, AVColorRange_AVCOL_RANGE_MPEG)),
                Err(HdrError::HdrColorSpaceUnverified)
            );
        }
    }

    #[test]
    fn tone_map_sdr_and_unknown_are_typed_errors() {
        for mode in [ContentColorMode::Sdr, ContentColorMode::Unknown] {
            assert_eq!(
                tone_map_stream_color_space(&info(mode)),
                Err(HdrError::HdrColorSpaceUnverified)
            );
        }
    }

    #[test]
    fn metadata_conversion_remains_a_typed_error() {
        assert!(matches!(
            build_dxgi_hdr10_metadata(None, None),
            Err(HdrError::HdrMetadataConversionUnverified)
        ));
    }
}
