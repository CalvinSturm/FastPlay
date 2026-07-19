use crate::{
    ffi::{
        d3d11::{BgraFrameCapture, D3D11Device, SubtitleOverlay, VideoSurface},
        dxgi::PresentResult,
    },
    platform::window::NativeWindow,
    render::{
        hdr::{swapchain_kind_for_path, VideoPresentationPath},
        surface_registry::{SurfaceRegistry, VideoSurfaceHandle},
        swapchain::SwapChainPresenter,
        timeline::TimelineOverlayModel,
    },
};

pub struct Presenter {
    // Field order is drop order. The swap chain, surface registry, and overlays
    // all hold COM objects (textures, views, processors) created from `device`.
    // A D3D11 child object's final Release notifies its owning device, so the
    // device must outlive them all — `device` is declared LAST so it drops last.
    // Getting this wrong is a shutdown use-after-free that only surfaces once
    // the decode worker (which holds its own device clone) has exited, leaving
    // this the last device reference — e.g. closing the window after the clip
    // has finished playing.
    swap_chain: Option<SwapChainPresenter>,
    /// The presentation path the live swapchain was built for. Rebuild
    /// paths (resize failure, device loss) reconstruct THIS path's
    /// swapchain kind; the kind only ever changes through
    /// [`Self::ensure_swapchain_for_path`] at file-open.
    swapchain_path: VideoPresentationPath,
    surfaces: SurfaceRegistry,
    current_surface: Option<VideoSurfaceHandle>,
    subtitle_overlay: Option<SubtitleOverlay>,
    timeline_overlay: Option<SubtitleOverlay>,
    timeline_model: Option<TimelineOverlayModel>,
    volume_overlay: Option<SubtitleOverlay>,
    volume_text: Option<String>,
    idle_overlay: Option<SubtitleOverlay>,
    help_overlay: Option<SubtitleOverlay>,
    recent_overlay: Option<SubtitleOverlay>,
    has_ever_shown_content: bool,
    device: D3D11Device,
}

impl Presenter {
    pub fn new(window: &NativeWindow) -> Result<Self, Box<dyn std::error::Error>> {
        let device = D3D11Device::create()?;
        let swap_chain = SwapChainPresenter::new(window, &device)?;

        let idle_overlay = device.create_idle_overlay(1280, 720).ok().flatten();

        Ok(Self {
            device,
            swap_chain: Some(swap_chain),
            swapchain_path: VideoPresentationPath::ExistingSdr,
            surfaces: SurfaceRegistry::default(),
            current_surface: None,
            subtitle_overlay: None,
            timeline_overlay: None,
            timeline_model: None,
            volume_overlay: None,
            volume_text: None,
            idle_overlay,
            help_overlay: None,
            recent_overlay: None,
            has_ever_shown_content: false,
        })
    }

    /// Show the Recent-files overlay (mutually exclusive with the help overlay;
    /// `rows` are (filename, position) pairs, `selected` is the highlighted row).
    pub fn show_recent_overlay(
        &mut self,
        rows: &[(String, String)],
        selected: usize,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.recent_overlay =
            self.device
                .create_recent_overlay(rows, selected, viewport_width, viewport_height)?;
        Ok(())
    }

    pub fn clear_recent_overlay(&mut self) {
        self.recent_overlay = None;
    }

    pub fn render(
        &mut self,
        view: &crate::render::ViewTransform,
    ) -> Result<PresentResult, Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        if let Some(handle) = self.current_surface {
            if let Some(entry) = self.surfaces.get(handle) {
                if !self.has_ever_shown_content {
                    self.has_ever_shown_content = true;
                    self.idle_overlay = None;
                }
                return sc.render_surface(
                    &self.device,
                    &entry.surface,
                    self.subtitle_overlay.as_ref(),
                    self.timeline_overlay.as_ref(),
                    self.volume_overlay.as_ref(),
                    self.recent_overlay.as_ref().or(self.help_overlay.as_ref()),
                    view,
                );
            }
        }

        sc.render(
            &self.device,
            [0.08, 0.10, 0.14, 1.0],
            self.idle_overlay.as_ref(),
            self.timeline_overlay.as_ref(),
            self.volume_overlay.as_ref(),
            self.recent_overlay.as_ref().or(self.help_overlay.as_ref()),
        )
    }

    pub fn render_with_capture(
        &mut self,
        view: &crate::render::ViewTransform,
    ) -> Result<(PresentResult, BgraFrameCapture), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        if let Some(handle) = self.current_surface {
            if let Some(entry) = self.surfaces.get(handle) {
                if !self.has_ever_shown_content {
                    self.has_ever_shown_content = true;
                    self.idle_overlay = None;
                }
                return sc.render_surface_with_capture(
                    &self.device,
                    &entry.surface,
                    self.subtitle_overlay.as_ref(),
                    self.timeline_overlay.as_ref(),
                    self.volume_overlay.as_ref(),
                    self.recent_overlay.as_ref().or(self.help_overlay.as_ref()),
                    view,
                );
            }
        }

        sc.render_with_capture(
            &self.device,
            [0.08, 0.10, 0.14, 1.0],
            self.idle_overlay.as_ref(),
            self.timeline_overlay.as_ref(),
            self.volume_overlay.as_ref(),
            self.recent_overlay.as_ref().or(self.help_overlay.as_ref()),
        )
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_mut() else {
            return Err("swap chain unavailable".into());
        };
        sc.resize(&self.device, width, height)?;
        if !self.has_ever_shown_content {
            self.idle_overlay = self
                .device
                .create_idle_overlay(width, height)
                .ok()
                .flatten();
        }
        Ok(())
    }

    /// Make the live swapchain match `path`'s kind, recreating it between
    /// playbacks at file-open (never mid-playback). A same-kind path is a
    /// strict no-op on the chain object, so SDR↔SDR (and tone-map) opens
    /// can never churn the pixel-verified SDR swapchain.
    ///
    /// On failure to build the HDR chain, the verified SDR chain is
    /// restored so the window keeps presenting, and the original typed
    /// error is returned for the caller to surface — an HDR open must fail
    /// visibly, never silently render into the wrong chain.
    pub fn ensure_swapchain_for_path(
        &mut self,
        window: &NativeWindow,
        path: VideoPresentationPath,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let target = swapchain_kind_for_path(path);
        if self.swap_chain.as_ref().map(|sc| sc.kind()) == Some(target) {
            // Record the path (it may differ within a kind, e.g.
            // ExistingSdr vs HdrToSdrToneMapRequired — both SDR) so
            // rebuilds keep dispatching to the same constructor family.
            self.swapchain_path = path;
            return Ok(());
        }
        flog!(
            "[swapchain] kind change {:?} -> {:?} (path {:?})",
            self.swap_chain.as_ref().map(|sc| sc.kind()),
            target,
            path
        );
        self.drop_swap_chain();
        match SwapChainPresenter::new_for_path(window, &self.device, path) {
            Ok(sc) => {
                self.swap_chain = Some(sc);
                self.swapchain_path = path;
                Ok(())
            }
            Err(error) => {
                flog!(
                    "[swapchain] {:?} chain creation failed ({error}); restoring SDR",
                    target
                );
                self.swapchain_path = VideoPresentationPath::ExistingSdr;
                self.swap_chain = Some(SwapChainPresenter::new(window, &self.device)?);
                Err(error)
            }
        }
    }

    pub fn rebuild_swap_chain(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Release backbuffer / render-target references, flush the device
        // context, then drop the swap chain — DXGI only allows one per HWND.
        self.drop_swap_chain();
        self.build_swap_chain_for_current_path(window)
    }

    pub fn rebuild_device(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Release everything tied to the old device before creating a new
        // one — DXGI only allows one swap chain per HWND.
        self.reset_surfaces();
        self.drop_swap_chain();
        self.device = D3D11Device::create()?;
        self.build_swap_chain_for_current_path(window)
    }

    /// (Re)build the swapchain of the current path's kind, for the rebuild
    /// paths above. If the HDR chain cannot be rebuilt (e.g. the window now
    /// sits on a display that rejects the HDR10 color space), fall back to
    /// the verified SDR chain and report success: recovery must leave the
    /// window presenting. Any still-HDR content then fails visibly at draw
    /// or is re-decided by the next open's path event — it is never
    /// silently rendered into the wrong chain.
    fn build_swap_chain_for_current_path(
        &mut self,
        window: &NativeWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match SwapChainPresenter::new_for_path(window, &self.device, self.swapchain_path) {
            Ok(sc) => {
                self.swap_chain = Some(sc);
                Ok(())
            }
            Err(error) if self.swapchain_path != VideoPresentationPath::ExistingSdr => {
                flog!(
                    "[swapchain] rebuild of {:?} failed ({error}); falling back to SDR",
                    self.swapchain_path
                );
                self.swapchain_path = VideoPresentationPath::ExistingSdr;
                self.swap_chain = Some(SwapChainPresenter::new(window, &self.device)?);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn drop_swap_chain(&mut self) {
        if let Some(sc) = self.swap_chain.as_mut() {
            sc.release_resources();
        }
        self.device.flush();
        self.swap_chain = None;
    }

    pub fn device(&self) -> &D3D11Device {
        &self.device
    }

    /// Apply HDR10 static metadata to the live HDR swapchain. Callers
    /// treat failure as advisory (logged, never fatal): DWM composites the
    /// PQ chain correctly without metadata.
    pub fn apply_hdr10_metadata(
        &self,
        metadata: &windows::Win32::Graphics::Dxgi::DXGI_HDR_METADATA_HDR10,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_ref() else {
            return Err("swap chain unavailable".into());
        };
        sc.apply_hdr10_metadata(metadata)
    }

    /// Snapshot the display/swapchain/device capabilities that gate HDR
    /// presentation. Must run on the main thread, where the window's swap
    /// chain lives: `GetContainingOutput` on that chain identifies the
    /// display the window is actually on (per-monitor correct).
    ///
    /// Non-fatal by design: any failure — no swap chain, headless/RDP
    /// output, drivers without the newer interfaces — returns the all-false
    /// default, which can only make HDR content dead-end in a typed error.
    /// It can never affect SDR selection, so exotic systems cannot regress
    /// SDR open availability.
    pub fn query_hdr_capabilities(&self) -> crate::render::hdr::HdrPresentationCapabilities {
        let Some(sc) = self.swap_chain.as_ref() else {
            return crate::render::hdr::HdrPresentationCapabilities::default();
        };
        crate::ffi::dxgi::query_hdr_presentation_capabilities(
            &self.device,
            Some(sc.raw_swap_chain()),
        )
        .unwrap_or_default()
    }

    pub fn register_surface(
        &mut self,
        open_gen: crate::playback::generations::OpenGeneration,
        seek_gen: crate::playback::generations::SeekGeneration,
        surface: VideoSurface,
    ) -> VideoSurfaceHandle {
        self.surfaces.insert(open_gen, seek_gen, surface)
    }

    /// Validates that `handle` exists and matches the given generations, then
    /// selects it as the current surface in one registry lookup.
    /// Returns `Ok(previous_handle)` on success or `Err(())` on mismatch.
    pub fn validate_and_select_surface(
        &mut self,
        handle: VideoSurfaceHandle,
        open_gen: crate::playback::generations::OpenGeneration,
        seek_gen: crate::playback::generations::SeekGeneration,
    ) -> Result<Option<VideoSurfaceHandle>, ()> {
        if !matches!(
            self.surfaces.get(handle),
            Some(entry) if entry.open_gen == open_gen && entry.seek_gen == seek_gen
        ) {
            return Err(());
        }
        Ok(self.current_surface.replace(handle))
    }

    pub fn has_selected_surface(&self) -> bool {
        self.current_surface.is_some()
    }

    pub fn current_surface_size(&self) -> Option<(u32, u32)> {
        let handle = self.current_surface?;
        let entry = self.surfaces.get(handle)?;
        Some(entry.surface.display_size())
    }

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn std::error::Error>> {
        let Some(sc) = self.swap_chain.as_ref() else {
            return Err("swap chain unavailable".into());
        };
        sc.viewport_size()
    }

    pub fn set_subtitle_overlay(
        &mut self,
        text: Option<&str>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.subtitle_overlay = match text {
            Some(text) => {
                self.device
                    .create_subtitle_overlay(text, viewport_width, viewport_height)?
            }
            None => None,
        };
        Ok(())
    }

    pub fn clear_subtitle_overlay(&mut self) {
        self.subtitle_overlay = None;
    }

    pub fn set_timeline_overlay(
        &mut self,
        model: Option<TimelineOverlayModel>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.timeline_model == model {
            return Ok(false);
        }

        let existing = self.timeline_overlay.take();
        self.timeline_overlay = match model {
            Some(ref m) => self.device.create_timeline_overlay(m, existing)?,
            None => None,
        };
        self.timeline_model = model;
        Ok(true)
    }

    pub fn set_volume_overlay(
        &mut self,
        text: Option<&str>,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let next_text = text.map(str::to_owned);
        if self.volume_text == next_text {
            return Ok(false);
        }

        let existing = self.volume_overlay.take();
        self.volume_overlay = match text {
            Some(text) => self.device.create_volume_overlay(
                text,
                viewport_width,
                viewport_height,
                existing,
            )?,
            None => None,
        };
        self.volume_text = next_text;
        Ok(true)
    }

    pub fn show_help_overlay(
        &mut self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.help_overlay.is_none() {
            self.help_overlay = self
                .device
                .create_help_overlay(viewport_width, viewport_height)?;
        }
        Ok(())
    }

    pub fn clear_help_overlay(&mut self) {
        self.help_overlay = None;
    }

    pub fn release_surface(&mut self, handle: VideoSurfaceHandle) {
        if self.current_surface == Some(handle) {
            self.current_surface = None;
        }
        self.surfaces.remove(handle);
    }

    /// Replace the idle overlay with a custom message (e.g. for error state).
    pub fn set_idle_overlay(
        &mut self,
        viewport_width: u32,
        viewport_height: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.idle_overlay = self
            .device
            .create_idle_overlay(viewport_width, viewport_height)
            .ok()
            .flatten();
        self.has_ever_shown_content = false;
        Ok(())
    }

    /// Returns true if the idle (no-content) overlay is currently showing.
    pub fn is_showing_idle(&self) -> bool {
        !self.has_ever_shown_content
    }

    pub fn reset_surfaces(&mut self) {
        self.current_surface = None;
        self.surfaces.clear_for_new_epoch();
        self.subtitle_overlay = None;
        self.timeline_overlay = None;
        self.timeline_model = None;
        self.volume_overlay = None;
        self.volume_text = None;
    }

    pub fn surfaces_alive(&self) -> usize {
        self.surfaces.count_alive()
    }
}
