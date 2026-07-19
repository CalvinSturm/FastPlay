use crate::{
    ffi::{
        d3d11::{BgraFrameCapture, D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::{DxgiSwapChain, PresentResult},
    },
    platform::window::NativeWindow,
    render::hdr::{HdrError, SwapchainKind, VideoPresentationPath},
};

/// Signature of the verified SDR renderer constructor.
type SdrRendererCtor =
    fn(&NativeWindow, &D3D11Device) -> Result<SwapChainPresenter, Box<dyn std::error::Error>>;

/// Signature of the HDR-to-SDR tone-mapping renderer boundary. Same shape
/// as the SDR constructor (the tone-map path presents through the same
/// verified SDR swapchain; per-frame shader inputs come from the surface's
/// tone-map tag, not the swapchain), but kept as a distinct named boundary
/// so the dispatch test pins each path to its intended constructor.
type HdrToSdrRendererCtor =
    fn(&NativeWindow, &D3D11Device) -> Result<SwapChainPresenter, Box<dyn std::error::Error>>;

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
    /// The HDR-to-SDR tone-mapping path. Tone-mapping is performed by our own
    /// pixel shader at draw time (`HdrToneMapRenderer`, see
    /// `render_video_surface_tone_mapped`), writing SDR sRGB into the ordinary
    /// 8-bit backbuffer, so this dispatches to the same verified SDR swapchain
    /// constructor — no distinct HDR swapchain.
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
        VideoPresentationPath::HdrPqOutput => Ok(RendererConstructor::Hdr10Skeleton),
        VideoPresentationPath::HdrToSdrToneMapRequired => Ok(RendererConstructor::HdrToSdrToneMap(
            create_hdr_to_sdr_renderer,
        )),
        VideoPresentationPath::UnsupportedHdr => Err(HdrError::UnsupportedHdrPresentation),
    }
}

/// HDR-to-SDR tone-mapping renderer. The conversion is done by our own pixel
/// shader during `render_video_surface_tone_mapped` (`HdrToneMapRenderer`
/// samples the decoded planes and performs the transfer, tone-curve, and
/// gamut math itself — the GPU video processor cannot do this conversion; see
/// that type's docs), writing SDR sRGB straight into the ordinary 8-bit
/// backbuffer. The swapchain is therefore the verified SDR swapchain — this
/// delegates to the same `SwapChainPresenter::new` the SDR path uses; the
/// per-frame shader inputs are resolved from the surface's tone-map tag at
/// draw time, not baked into the swapchain.
pub(crate) fn create_hdr_to_sdr_renderer(
    window: &NativeWindow,
    device: &D3D11Device,
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

    /// The underlying DXGI swap chain, for read-only capability queries.
    pub fn raw_swap_chain(&self) -> &windows::Win32::Graphics::Dxgi::IDXGISwapChain1 {
        self.swap_chain.raw_swap_chain()
    }

    pub fn new(
        window: &NativeWindow,
        device: &D3D11Device,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            swap_chain: DxgiSwapChain::create(window.raw_window(), device)?,
        })
    }

    /// Which construction the underlying swapchain came from.
    pub fn kind(&self) -> SwapchainKind {
        self.swap_chain.kind()
    }

    /// Presentation-path renderer fork. `ExistingSdr` (and the tone-map
    /// path, which presents through the same verified SDR chain) delegates
    /// to the unchanged [`Self::new`]; `HdrPqOutput` builds the validated
    /// HDR10 swapchain skeleton.
    ///
    /// Called by `Presenter::ensure_swapchain_for_path` between playbacks
    /// at file-open (never mid-playback), and by the presenter's rebuild
    /// paths so resize/device recovery restore the same kind. Callers must
    /// follow the one-swapchain-per-HWND discipline — `release_resources()`
    /// before dropping the previous presenter, exactly like device
    /// recovery — and the shutdown constraint that D3D teardown is avoided
    /// in-process.
    pub fn new_for_path(
        window: &NativeWindow,
        device: &D3D11Device,
        path: VideoPresentationPath,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        match renderer_constructor_for_path(path)? {
            RendererConstructor::ExistingSdr(constructor) => constructor(window, device),
            RendererConstructor::Hdr10Skeleton => Ok(Self {
                swap_chain: DxgiSwapChain::create_hdr10_skeleton(window.raw_window(), device)?,
            }),
            RendererConstructor::HdrToSdrToneMap(constructor) => constructor(window, device),
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
            renderer_constructor_for_path(VideoPresentationPath::HdrPqOutput),
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
