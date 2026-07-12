use crate::{
    ffi::{
        d3d11::{BgraFrameCapture, D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::{DxgiSwapChain, PresentResult},
    },
    platform::window::NativeWindow,
    render::hdr::{ContentColorInfo, HdrError, HdrPresentationCapabilities, VideoPresentationPath},
};

/// Signature of the verified SDR renderer constructor.
type SdrRendererCtor =
    fn(&NativeWindow, &D3D11Device) -> Result<SwapChainPresenter, Box<dyn std::error::Error>>;

/// Signature of the HDR-to-SDR tone-mapping renderer boundary.
type HdrToSdrRendererCtor = fn(
    &NativeWindow,
    &D3D11Device,
    &ContentColorInfo,
    &HdrPresentationCapabilities,
) -> Result<SwapChainPresenter, Box<dyn std::error::Error>>;

/// Which renderer constructor a [`VideoPresentationPath`] dispatches to.
/// Data-driven (function pointers) so the regression test can assert that
/// `ExistingSdr` maps to the very same [`SwapChainPresenter::new`] the
/// pixel-verified SDR path has always used.
#[derive(Debug)]
pub(crate) enum RendererConstructor {
    /// The pre-HDR, pixel-verified SDR constructor — unchanged.
    ExistingSdr(SdrRendererCtor),
    /// The HDR10 passthrough swapchain skeleton: creates the R10G10B10A2
    /// swapchain with the verified HDR10 color space, but is not yet wired
    /// into the production render path (passthrough commit).
    Hdr10Skeleton,
    /// The HDR-to-SDR tone-mapping path. Tone-mapping is performed by the
    /// video processor at blt time (see `render_video_surface`), writing SDR
    /// sRGB into the ordinary 8-bit backbuffer, so this dispatches to the
    /// same verified SDR swapchain constructor — no distinct HDR swapchain.
    HdrToSdrToneMap(HdrToSdrRendererCtor),
}

/// Pure dispatch from presentation path to renderer constructor. No COM
/// calls; deterministic and unit-tested.
pub(crate) fn renderer_constructor_for_path(
    path: VideoPresentationPath,
) -> Result<RendererConstructor, HdrError> {
    match path {
        VideoPresentationPath::ExistingSdr => {
            Ok(RendererConstructor::ExistingSdr(SwapChainPresenter::new))
        }
        VideoPresentationPath::Hdr10Passthrough => Ok(RendererConstructor::Hdr10Skeleton),
        VideoPresentationPath::HdrToSdrToneMapRequired => Ok(RendererConstructor::HdrToSdrToneMap(
            create_hdr_to_sdr_renderer,
        )),
        VideoPresentationPath::UnsupportedHdr => Err(HdrError::UnsupportedHdrPresentation),
    }
}

/// HDR-to-SDR tone-mapping renderer. The conversion is done by the GPU
/// video processor during `render_video_surface` (it tags the decoded HDR
/// stream color space and an sRGB output space, and the driver tone-maps),
/// writing SDR sRGB straight into the ordinary 8-bit backbuffer. The
/// swapchain is therefore the verified SDR swapchain — this delegates to the
/// same `SwapChainPresenter::new` the SDR path uses. `content`/`capabilities`
/// are unused here because the per-frame color spaces are resolved from the
/// surface's tone-map tag at blt time, not baked into the swapchain.
pub(crate) fn create_hdr_to_sdr_renderer(
    window: &NativeWindow,
    device: &D3D11Device,
    _content: &ContentColorInfo,
    _capabilities: &HdrPresentationCapabilities,
) -> Result<SwapChainPresenter, Box<dyn std::error::Error>> {
    SwapChainPresenter::new(window, device)
}

pub struct SwapChainPresenter {
    swap_chain: DxgiSwapChain,
}

impl SwapChainPresenter {
    pub fn release_resources(&mut self) {
        self.swap_chain.release_resources();
    }

    pub fn new(
        window: &NativeWindow,
        device: &D3D11Device,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            swap_chain: DxgiSwapChain::create(window.raw_window(), device)?,
        })
    }

    /// Presentation-path renderer fork. `ExistingSdr` delegates to the
    /// unchanged [`Self::new`]. `Hdr10Passthrough` builds the validated
    /// HDR10 swapchain skeleton, but its render path does not yet apply
    /// the HDR processor color spaces (the device's HDR processor
    /// configuration is unwired); tone mapping stays a typed error.
    ///
    /// Not yet called by presenter.rs — startup and device recovery are
    /// always SDR today, and HDR files dead-end at open with a typed error.
    /// The HDR passthrough commit migrates presenter.rs here, keyed on the
    /// selected [`VideoPresentationPath`].
    ///
    /// HDR-VERIFY (passthrough commit): the swapchain is created at app
    /// startup, before any file exists, so wiring this fork means deciding
    /// WHEN the 10-bit swapchain is created. Recreating between playbacks
    /// at file-open (never mid-playback) is the expected shape, but it must
    /// follow the existing one-swapchain-per-HWND discipline —
    /// `release_resources()` before drop, exactly like device recovery in
    /// presenter.rs — and respect the v0.4.1 shutdown constraint that D3D
    /// teardown is avoided in-process.
    #[allow(dead_code)]
    pub fn new_for_path(
        window: &NativeWindow,
        device: &D3D11Device,
        path: VideoPresentationPath,
        content: Option<&ContentColorInfo>,
        capabilities: &HdrPresentationCapabilities,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        match renderer_constructor_for_path(path)? {
            RendererConstructor::ExistingSdr(constructor) => constructor(window, device),
            RendererConstructor::Hdr10Skeleton => {
                let content = content.ok_or(HdrError::UnsupportedHdrPresentation)?;
                Ok(Self {
                    swap_chain: DxgiSwapChain::create_hdr10_skeleton(
                        window.raw_window(),
                        device,
                        content,
                        capabilities,
                    )?,
                })
            }
            RendererConstructor::HdrToSdrToneMap(constructor) => {
                let content = content.ok_or(HdrError::UnsupportedHdrPresentation)?;
                constructor(window, device, content, capabilities)
            }
        }
    }

    pub fn render(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        help_overlay: Option<&SubtitleOverlay>,
    ) -> Result<PresentResult, Box<dyn std::error::Error>> {
        self.swap_chain.render(
            device,
            clear_color,
            subtitle_overlay,
            timeline_overlay,
            volume_overlay,
            help_overlay,
        )
    }

    pub fn render_with_capture(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        help_overlay: Option<&SubtitleOverlay>,
    ) -> Result<(PresentResult, BgraFrameCapture), Box<dyn std::error::Error>> {
        self.swap_chain.render_with_capture(
            device,
            clear_color,
            subtitle_overlay,
            timeline_overlay,
            volume_overlay,
            help_overlay,
        )
    }

    pub fn resize(
        &mut self,
        device: &D3D11Device,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.swap_chain.resize(device, width, height)?;
        Ok(())
    }

    pub fn render_surface(
        &mut self,
        device: &D3D11Device,
        surface: &VideoSurface,
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        help_overlay: Option<&SubtitleOverlay>,
        view: &crate::render::ViewTransform,
    ) -> Result<PresentResult, Box<dyn std::error::Error>> {
        self.swap_chain.render_surface(
            device,
            surface,
            subtitle_overlay,
            timeline_overlay,
            volume_overlay,
            help_overlay,
            view,
        )
    }

    pub fn render_surface_with_capture(
        &mut self,
        device: &D3D11Device,
        surface: &VideoSurface,
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        help_overlay: Option<&SubtitleOverlay>,
        view: &crate::render::ViewTransform,
    ) -> Result<(PresentResult, BgraFrameCapture), Box<dyn std::error::Error>> {
        self.swap_chain.render_surface_with_capture(
            device,
            surface,
            subtitle_overlay,
            timeline_overlay,
            volume_overlay,
            help_overlay,
            view,
        )
    }

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        self.swap_chain.viewport_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression proof for the SDR-preservation invariant: the fork's
    /// `ExistingSdr` arm dispatches to the very same `SwapChainPresenter::new`
    /// the pixel-verified SDR path has always used — not a rewritten
    /// constructor.
    #[test]
    fn existing_sdr_dispatches_to_the_verified_sdr_constructor() {
        match renderer_constructor_for_path(VideoPresentationPath::ExistingSdr) {
            Ok(RendererConstructor::ExistingSdr(constructor)) => {
                let verified: SdrRendererCtor = SwapChainPresenter::new;
                assert_eq!(constructor as usize, verified as usize);
            }
            other => panic!("ExistingSdr must dispatch to the SDR constructor, got {other:?}"),
        }
    }

    #[test]
    fn tone_map_required_dispatches_to_the_tone_map_boundary() {
        match renderer_constructor_for_path(VideoPresentationPath::HdrToSdrToneMapRequired) {
            Ok(RendererConstructor::HdrToSdrToneMap(constructor)) => {
                let boundary: HdrToSdrRendererCtor = create_hdr_to_sdr_renderer;
                assert_eq!(constructor as usize, boundary as usize);
            }
            other => panic!("tone-map path must dispatch to the boundary, got {other:?}"),
        }
    }

    #[test]
    fn hdr10_passthrough_dispatches_to_the_hdr_skeleton_not_sdr() {
        assert!(matches!(
            renderer_constructor_for_path(VideoPresentationPath::Hdr10Passthrough),
            Ok(RendererConstructor::Hdr10Skeleton)
        ));
    }

    #[test]
    fn unsupported_hdr_is_a_typed_error() {
        assert!(matches!(
            renderer_constructor_for_path(VideoPresentationPath::UnsupportedHdr),
            Err(HdrError::UnsupportedHdrPresentation)
        ));
    }
}
