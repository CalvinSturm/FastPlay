//! HDR presentation-path skeleton.
//!
//! This module is the pure, COM-free half of the SDR/HDR fork: content color
//! classification, display/processor capability modelling, and the single
//! decision function that selects a [`VideoPresentationPath`] before any
//! renderer or swapchain work happens for a newly opened file.
//!
//! Nothing here resolves a DXGI color-space constant, an HDR metadata unit
//! conversion, or an FFmpeg side-data layout. Every such value is fenced
//! behind a `verified_*` helper that returns a typed error until it is
//! verified in its own later commit. Search for `HDR-VERIFY` to find all of
//! them.
//!
//! The verified SDR path never enters this module beyond
//! [`select_video_presentation_path`] returning
//! [`VideoPresentationPath::ExistingSdr`].

use std::{error::Error, fmt};

use windows::Win32::Graphics::Dxgi::{
    Common::{
        DXGI_COLOR_SPACE_TYPE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_R10G10B10A2_UNORM,
    },
    DXGI_HDR_METADATA_HDR10,
};

use crate::ffi::ffmpeg::{
    AVColorPrimaries, AVColorPrimaries_AVCOL_PRI_BT2020, AVColorRange, AVColorSpace,
    AVColorTransferCharacteristic, AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67,
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
    /// Hybrid Log-Gamma transfer.
    Hlg,
    /// HDR-signalled but ambiguous (e.g. BT.2020 primaries with an
    /// unspecified transfer). Never silently treated as SDR or as HDR10.
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
    /// HDR10 content on an SDR display: needs the (unimplemented)
    /// HDR-to-SDR tone mapper.
    HdrToSdrToneMapRequired,
    /// Any HDR-signalled combination we cannot present correctly.
    UnsupportedHdr,
}

/// Complete color classification for an opened stream. Retains the raw
/// FFmpeg tags so later verification commits can map them to DXGI color
/// spaces without re-deriving anything.
#[derive(Debug, Clone)]
// The raw tag fields are read by the HDR-VERIFY color-space mapping
// commits; only `mode` drives the skeleton's decision today.
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
    /// `IDXGISwapChain3::CheckColorSpaceSupport` accepted the HDR10 color
    /// space. HDR-VERIFY: requires the verified swapchain color-space value.
    pub(crate) swapchain_hdr10_color_space_supported: bool,
    /// `ID3D11VideoContext1` is available on the device.
    pub(crate) video_context1_available: bool,
    /// `CheckVideoProcessorFormatConversion` accepted NV12/P010 HDR10 input
    /// to the HDR output format. HDR-VERIFY: requires verified color-space
    /// values and runs against a real processor enumerator.
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
    ToneMappingNotImplemented,
    SwapChain4Unavailable,
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
            Self::ToneMappingNotImplemented => {
                "this video is HDR but the display is in SDR mode; HDR-to-SDR tone mapping is \
                 not implemented yet"
            }
            Self::SwapChain4Unavailable => {
                "HDR metadata requires IDXGISwapChain4, which this system does not expose"
            }
        };
        f.write_str(message)
    }
}

impl Error for HdrError {}

/// Pure classification of stream-level color tags into a
/// [`ContentColorMode`]. Deliberately keeps today's behavior for everything
/// that is not explicitly HDR-signalled: fully untagged content and exotic
/// SDR transfers continue through the verified SDR path exactly as before
/// this module existed.
///
/// HDR-VERIFY: policy for BT.2020-primaries SDR transfers (BT2020_10/12 wide
/// gamut) is unresolved; they classify as `Sdr` today, matching pre-skeleton
/// behavior.
pub(crate) fn classify_color_tags(
    color_primaries: AVColorPrimaries,
    color_transfer: AVColorTransferCharacteristic,
) -> ContentColorMode {
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084 {
        return ContentColorMode::Hdr10Pq;
    }
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_ARIB_STD_B67 {
        return ContentColorMode::Hlg;
    }
    if color_transfer == AVColorTransferCharacteristic_AVCOL_TRC_UNSPECIFIED
        && color_primaries == AVColorPrimaries_AVCOL_PRI_BT2020
    {
        // BT.2020 primaries with no transfer tag is HDR-signalled but
        // ambiguous: it could be PQ, HLG, or wide-gamut SDR. It must not
        // silently reach the SDR path, and must not be assumed HDR10.
        return ContentColorMode::Unknown;
    }
    // HDR-VERIFY: fully untagged content (unspecified transfer, non-BT.2020
    // primaries) classifies PERMANENTLY as Sdr here, because first-frame
    // refinement is stubbed and unreachable. An untagged-but-actually-HDR
    // file (stripped-container PQ, some screen recordings) therefore plays
    // washed out through the SDR path — the same bug class this fork
    // exists to fix. The frame-refinement commit must revisit this branch
    // (upgrade from frame-level tags before presentation), not inherit it.
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
            } else if !capabilities.display_hdr_active {
                // HDR10 content, SDR display: only an explicit HDR-to-SDR
                // conversion may present this. It must never fall through
                // to the existing SDR path.
                VideoPresentationPath::HdrToSdrToneMapRequired
            } else {
                // HDR display, but some required processor/swapchain
                // capability is missing.
                VideoPresentationPath::UnsupportedHdr
            }
        }
        // HLG is never auto-classified as HDR10 passthrough and never
        // reaches the SDR path. HDR-VERIFY: a dedicated HLG path (native or
        // via processor conversion) is future work.
        ContentColorMode::Hlg => VideoPresentationPath::UnsupportedHdr,
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

/// HDR-VERIFY: exact windows-rs `DXGI_COLOR_SPACE_TYPE` variant for the
/// HDR10 swapchain (RGB, full range, PQ / G2084, BT.2020 primaries).
pub(crate) fn verified_hdr10_swapchain_color_space() -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    Err(HdrError::HdrColorSpaceUnverified)
}

/// HDR-VERIFY: exact YCbCr input color-space variant derived from the
/// decoded texture format (NV12/P010), nominal range, BT.2020 matrix
/// variant, and PQ vs HLG transfer. Must not be guessed from `content`.
pub(crate) fn verified_hdr_stream_color_space(
    _content: &ContentColorInfo,
) -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
    Err(HdrError::HdrColorSpaceUnverified)
}

/// HDR-VERIFY: exact RGB PQ BT.2020 output variant for
/// `VideoProcessorSetOutputColorSpace1`.
pub(crate) fn verified_hdr10_processor_output_color_space(
) -> Result<DXGI_COLOR_SPACE_TYPE, HdrError> {
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
        AVColorPrimaries_AVCOL_PRI_BT709, AVColorPrimaries_AVCOL_PRI_UNSPECIFIED,
        AVColorRange_AVCOL_RANGE_UNSPECIFIED, AVColorSpace_AVCOL_SPC_UNSPECIFIED,
        AVColorTransferCharacteristic_AVCOL_TRC_BT709,
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
    fn hlg_never_selects_passthrough_or_existing_sdr() {
        for caps in [HdrPresentationCapabilities::default(), full_capabilities()] {
            let path = select_video_presentation_path(&info(ContentColorMode::Hlg), &caps);
            assert_eq!(path, VideoPresentationPath::UnsupportedHdr);
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
    fn verification_boundaries_are_typed_errors_not_panics() {
        assert_eq!(
            verified_hdr10_swapchain_color_space(),
            Err(HdrError::HdrColorSpaceUnverified)
        );
        assert_eq!(
            verified_hdr_stream_color_space(&info(ContentColorMode::Hdr10Pq)),
            Err(HdrError::HdrColorSpaceUnverified)
        );
        assert_eq!(
            verified_hdr10_processor_output_color_space(),
            Err(HdrError::HdrColorSpaceUnverified)
        );
        assert!(matches!(
            build_dxgi_hdr10_metadata(None, None),
            Err(HdrError::HdrMetadataConversionUnverified)
        ));
    }
}
