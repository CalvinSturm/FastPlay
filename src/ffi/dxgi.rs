use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::{c_void, OsStr},
    fmt,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    ptr::null_mut,
    rc::Rc,
};

use windows::{
    core::{w, Interface, PCWSTR},
    Win32::{
        Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Direct3D11::ID3D11Texture2D,
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
                DXGI_CREATE_FACTORY_FLAGS, DXGI_ERROR_DEVICE_REMOVED,
                DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT, DXGI_SCALING_STRETCH,
                DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
                DXGI_USAGE_RENDER_TARGET_OUTPUT,
            },
            Gdi::{
                GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO,
                MONITOR_DEFAULTTONEAREST,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, VK_LBUTTON},
            Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP},
            WindowsAndMessaging::{
                AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                DispatchMessageW, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
                GetWindowPlacement, LoadCursorW, LoadImageW, PeekMessageW, PostQuitMessage,
                RegisterClassExW, SetWindowLongPtrW,
                SetWindowPlacement, SetWindowPos, SetWindowTextW, ShowWindow,
                TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
                GWLP_USERDATA, GWL_STYLE, HICON, HMENU, HWND_TOP, IDC_ARROW, IMAGE_ICON,
                MSG, PM_REMOVE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
                SWP_NOZORDER,
                SW_SHOW, WINDOWPLACEMENT, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE,
                WM_DESTROY, WM_DROPFILES, WM_ENTERMENULOOP, WM_ENTERSIZEMOVE, WM_EXITMENULOOP,
                WM_EXITSIZEMOVE, WM_KEYDOWN, WM_LBUTTONDBLCLK,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVING, WM_NCCREATE,
                WM_NCLBUTTONDOWN, WM_NCRBUTTONUP, WM_SIZE, WM_SYSCOMMAND, WM_TIMER,
                WNDCLASSEXW, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

use crate::{
    ffi::d3d11::{D3D11Device, RenderTargetView, SubtitleOverlay, SubtitleRenderer, VideoProcessorCache, VideoSurface},
    platform::input::InputEvent,
};

// SetCapture / ReleaseCapture / GetSystemMetrics are not exposed by the
// `windows` 0.58 crate for our feature set, so we link them directly.
extern "system" {
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> BOOL;
    fn GetSystemMetrics(index: i32) -> i32;
}
const SM_CXDRAG: i32 = 68;
const SM_CYDRAG: i32 = 69;

const MAX_MESSAGES_PER_PUMP: usize = 64;
const MODAL_TICK_TIMER_ID: usize = 1;
const MODAL_TICK_INTERVAL_MS: u32 = 8;

#[derive(Debug)]
pub struct DxgiError(String);

impl fmt::Display for DxgiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for DxgiError {}

#[derive(Clone, Copy, Debug)]
pub struct ResizeRequest {
    pub width: u32,
    pub height: u32,
}

struct WindowState {
    is_open: Cell<bool>,
    resize_request: Cell<Option<ResizeRequest>>,
    input_events: RefCell<Vec<InputEvent>>,
    modal_tick_fn: Cell<Option<unsafe fn(*mut c_void)>>,
    modal_tick_ctx: Cell<*mut c_void>,
    in_modal_loop: Cell<bool>,
    caption_tracking: Cell<bool>,
    caption_drag_origin: Cell<POINT>,
}

pub struct NativeWindowInner {
    hwnd: HWND,
    state: Rc<WindowState>,
    is_borderless: Cell<bool>,
    saved_placement: Cell<WINDOWPLACEMENT>,
    saved_style: Cell<u32>,
}

impl NativeWindowInner {
    pub fn create(title: &str, width: u32, height: u32) -> Result<Self, Box<dyn Error>> {
        let instance = module_handle()?;
        let class_name = w!("FastPlayWindowClass");
        register_window_class(instance, class_name)?;

        let title_wide = to_wide(title);
        let state = Rc::new(WindowState {
            is_open: Cell::new(true),
            resize_request: Cell::new(None),
            input_events: RefCell::new(Vec::new()),
            modal_tick_fn: Cell::new(None),
            modal_tick_ctx: Cell::new(null_mut()),
            in_modal_loop: Cell::new(false),
            caption_tracking: Cell::new(false),
            caption_drag_origin: Cell::new(POINT::default()),
        });
        let state_ptr = Rc::into_raw(state.clone()) as *mut WindowState;

        let (window_width, window_height) = adjust_window_size(width, height)?;

        // SAFETY:
        // - class name and title pointers stay alive for the duration of the call
        // - the create parameter carries an `Rc<WindowState>` raw pointer reclaimed in WM_DESTROY
        // - dimensions and styles are standard overlapped-window values
        let hwnd = match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                PCWSTR(title_wide.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                window_width,
                window_height,
                HWND(null_mut()),
                HMENU(null_mut()),
                instance,
                Some(state_ptr.cast()),
            )
        } {
            Ok(hwnd) => hwnd,
            Err(error) => {
                // SAFETY: balances the earlier `Rc::into_raw` on window-creation failure.
                unsafe {
                    drop(Rc::from_raw(state_ptr));
                }
                return Err(Box::new(error));
            }
        };

        // SAFETY: `hwnd` is a live top-level window created above.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            DragAcceptFiles(hwnd, true);
        }

        let mut placement = WINDOWPLACEMENT::default();
        placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;

        Ok(Self {
            hwnd,
            state,
            is_borderless: Cell::new(false),
            saved_placement: Cell::new(placement),
            saved_style: Cell::new(WS_OVERLAPPEDWINDOW.0),
        })
    }

    pub fn set_title(&self, title: &str) {
        let wide = to_wide(title);
        // SAFETY: hwnd is a live window owned by this struct.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    pub fn pump_messages(&self) -> Result<(), Box<dyn Error>> {
        let mut message = MSG::default();
        let mut processed = 0usize;

        while processed < MAX_MESSAGES_PER_PUMP {
            // SAFETY:
            // - `message` points to valid writable storage
            // - null HWND means messages for the current thread
            let has_message =
                unsafe { PeekMessageW(&mut message, HWND(null_mut()), 0, 0, PM_REMOVE) };
            if !has_message.as_bool() {
                break;
            }

            if message.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                self.state.is_open.set(false);
                break;
            }

            // SAFETY: `message` was produced by PeekMessageW for this thread.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            processed = processed.saturating_add(1);
        }

        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.state.is_open.get()
    }

    pub fn take_resize_request(&self) -> Option<ResizeRequest> {
        let request = self.state.resize_request.get();
        self.state.resize_request.set(None);
        request
    }

    /// # Safety
    ///
    /// `ctx` must remain valid for as long as the callback is installed.
    /// The callback will be invoked on the UI thread during the Win32
    /// modal move/resize loop via `WM_TIMER`.
    pub unsafe fn install_modal_tick(&self, ctx: *mut c_void, tick_fn: unsafe fn(*mut c_void)) {
        self.state.modal_tick_fn.set(Some(tick_fn));
        self.state.modal_tick_ctx.set(ctx);
    }

    pub fn clear_modal_tick(&self) {
        self.state.modal_tick_fn.set(None);
        self.state.modal_tick_ctx.set(null_mut());
    }

    pub fn take_input_events(&self, out: &mut Vec<InputEvent>) {
        std::mem::swap(out, &mut *self.state.input_events.borrow_mut());
    }

    pub fn is_borderless(&self) -> bool {
        self.is_borderless.get()
    }

    pub fn is_in_modal_loop(&self) -> bool {
        self.state.in_modal_loop.get()
    }

    pub fn client_size(&self) -> Result<(u32, u32), Box<dyn Error>> {
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(self.hwnd, &mut rect)?;
        }
        Ok((
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
    }

    pub fn cursor_client_position(&self) -> Result<Option<(i32, i32)>, Box<dyn Error>> {
        let mut cursor = POINT::default();
        unsafe {
            if GetCursorPos(&mut cursor).is_err() {
                return Ok(None);
            }
            if !ScreenToClient(self.hwnd, &mut cursor).as_bool() {
                return Ok(None);
            }
        }

        let (width, height) = self.client_size()?;
        if cursor.x < 0 || cursor.y < 0 || cursor.x >= width as i32 || cursor.y >= height as i32 {
            return Ok(None);
        }

        Ok(Some((cursor.x, cursor.y)))
    }

    pub fn is_left_button_down(&self) -> bool {
        unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
    }

    pub fn resize_for_content(&self, content_width: u32, content_height: u32, center: bool) {
        if self.is_borderless.get() || content_width == 0 || content_height == 0 {
            return;
        }

        unsafe {
            // Get the work area of the current monitor to clamp the window size.
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let work = info.rcWork;
            let work_w = (work.right - work.left).max(0) as u32;
            let work_h = (work.bottom - work.top).max(0) as u32;

            // Scale down to fit the work area if needed, preserving aspect ratio.
            let mut w = content_width;
            let mut h = content_height;
            if w > work_w || h > work_h {
                let scale_x = work_w as f64 / w as f64;
                let scale_y = work_h as f64 / h as f64;
                let scale = scale_x.min(scale_y);
                w = (w as f64 * scale) as u32;
                h = (h as f64 * scale) as u32;
            }

            // Convert client size to window size (accounts for title bar / borders).
            let Ok((win_w, win_h)) = adjust_window_size(w, h) else {
                return;
            };

            // Clamp window size to work area after adding chrome.
            let win_w = win_w.min(work_w as i32);
            let win_h = win_h.min(work_h as i32);

            if center {
                let x = work.left + (work_w as i32 - win_w) / 2;
                let y = work.top + (work_h as i32 - win_h) / 2;
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    x,
                    y,
                    win_w,
                    win_h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            } else {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    0,
                    0,
                    win_w,
                    win_h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE | SWP_FRAMECHANGED,
                );
            }
        }
    }

    /// Resize the window to fill the work area top-to-bottom with no black
    /// padding, keeping the window's horizontal center position.
    pub fn fit_window_to_content(&self, content_width: u32, content_height: u32) {
        if self.is_borderless.get() || content_width == 0 || content_height == 0 {
            return;
        }

        unsafe {
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let work = info.rcWork;

            let Ok((chrome_w, chrome_h)) = adjust_window_size(0, 0) else {
                return;
            };
            let max_client_w = ((work.right - work.left) - chrome_w).max(1) as u32;
            let max_client_h = ((work.bottom - work.top) - chrome_h).max(1) as u32;

            // Fill the full work-area height; derive width from aspect ratio.
            let scale = max_client_h as f64 / content_height as f64;
            let w = ((content_width as f64 * scale) as u32).max(1).min(max_client_w);
            let h = if w < (content_width as f64 * scale) as u32 {
                // Width was clamped — recalculate height from width constraint.
                ((content_height as f64 * (w as f64 / content_width as f64)) as u32)
                    .max(1)
                    .min(max_client_h)
            } else {
                max_client_h
            };

            let Ok((win_w, win_h)) = adjust_window_size(w, h) else {
                return;
            };

            // Keep horizontal center, snap to work-area top.
            let mut rect = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rect);
            let old_center_x = (rect.left + rect.right) / 2;
            let x = (old_center_x - win_w / 2)
                .max(work.left)
                .min(work.right - win_w);
            let y = work.top;

            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOP,
                x,
                y,
                win_w,
                win_h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }

    /// Resize the window so the client area is exactly `content_width × content_height`,
    /// keeping the window's current horizontal center and top edge, clamped to the work area.
    pub fn set_window_client_size(&self, content_width: u32, content_height: u32) {
        if self.is_borderless.get() || content_width == 0 || content_height == 0 {
            return;
        }

        unsafe {
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let work = info.rcWork;

            let Ok((win_w, win_h)) = adjust_window_size(content_width, content_height) else {
                return;
            };

            let mut rect = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rect);
            let old_center_x = (rect.left + rect.right) / 2;
            let x = (old_center_x - win_w / 2)
                .max(work.left)
                .min(work.right - win_w);
            let y = rect.top.max(work.top).min(work.bottom - win_h);

            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOP,
                x,
                y,
                win_w,
                win_h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }

    pub fn toggle_borderless_fullscreen(&self) {
        // SAFETY:
        // - hwnd is a live top-level window owned by this wrapper
        // - GetWindowLongPtrW / SetWindowLongPtrW / SetWindowPos are safe with valid HWND
        // - MonitorFromWindow / GetMonitorInfoW use system-provided handles
        unsafe {
            if self.is_borderless.get() {
                // Restore windowed mode.
                let style = self.saved_style.get();
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, style as isize);
                let _ = SetWindowPlacement(self.hwnd, &self.saved_placement.get());
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE
                        | SWP_NOZORDER
                        | SWP_FRAMECHANGED
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
                );
                self.is_borderless.set(false);
            } else {
                // Save current placement and style.
                let mut placement = WINDOWPLACEMENT::default();
                placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                let _ = GetWindowPlacement(self.hwnd, &mut placement);
                self.saved_placement.set(placement);

                let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32;
                self.saved_style.set(style);

                // Remove window chrome, apply popup style.
                let borderless_style = (style & !WS_OVERLAPPEDWINDOW.0) | WS_POPUP.0;
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, borderless_style as isize);

                // Fill the current monitor.
                let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    let rc = info.rcMonitor;
                    let _ = SetWindowPos(
                        self.hwnd,
                        HWND_TOP,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        SWP_NOACTIVATE | SWP_FRAMECHANGED,
                    );
                }
                self.is_borderless.set(true);
            }
        }
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for NativeWindowInner {
    fn drop(&mut self) {
        if self.state.is_open.get() && self.hwnd.0 != null_mut() {
            // SAFETY: `hwnd` belongs to this wrapper and is no longer used after drop.
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Outcome of a swap chain Present call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentResult {
    /// Frame was presented normally.
    Ok,
    /// The window is fully occluded (minimized, covered) — no frame was shown.
    Occluded,
    /// The D3D11 device was lost or reset and must be rebuilt.
    DeviceLost,
}

pub struct DxgiSwapChain {
    swap_chain: IDXGISwapChain1,
    backbuffer: Option<ID3D11Texture2D>,
    render_target: Option<RenderTargetView>,
    width: u32,
    height: u32,
    subtitle_renderer: Option<SubtitleRenderer>,
    vp_cache: Option<VideoProcessorCache>,
}

impl DxgiSwapChain {
    /// Release all resources derived from the swap chain's backbuffer so
    /// that the swap chain's COM refcount can reach zero.  Must be called
    /// before dropping the struct when another swap chain will be created
    /// on the same HWND.
    pub fn release_resources(&mut self) {
        self.vp_cache = None;
        self.subtitle_renderer = None;
        self.render_target = None;
        self.backbuffer = None;
    }

    pub fn create(
        window: &NativeWindowInner,
        device: &D3D11Device,
    ) -> Result<Self, Box<dyn Error>> {
        let factory: IDXGIFactory2 = create_factory()?;
        let dxgi_device: IDXGIDevice = device.raw_device().cast()?;

        // HDR swap chain support is deferred until the full pipeline
        // (video processor color space, tone mapping) is in place.
        // For now, always use the standard SDR format.
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 0,
            Height: 0,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: BOOL(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };

        // SAFETY:
        // - `dxgi_device` and `window.hwnd()` are valid for the lifetime of the created swap chain
        // - descriptor requests a standard windowed flip-model chain
        // - full-screen descriptor and output restriction are intentionally omitted for v1
        let swap_chain = unsafe {
            factory.CreateSwapChainForHwnd(&dxgi_device, window.hwnd(), &desc, None, None)?
        };

        let (backbuffer, render_target) = create_backbuffer_state(device, &swap_chain)?;

        Ok(Self {
            swap_chain,
            backbuffer: Some(backbuffer),
            render_target: Some(render_target),
            width: 0,
            height: 0,
            subtitle_renderer: None,
            vp_cache: None,
        })
    }

    pub fn render(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
    ) -> Result<PresentResult, Box<dyn Error>> {
        let render_target = self
            .render_target
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer is not bound".into()))?;

        device.clear_render_target(render_target, clear_color);
        if let Some(overlay) = subtitle_overlay {
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                self.width,
                self.height,
            )?;
        }
        if let Some(overlay) = timeline_overlay {
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                self.width,
                self.height,
            )?;
        }
        if let Some(overlay) = volume_overlay {
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                self.width,
                self.height,
            )?;
        }

        self.present()
    }

    pub fn render_surface(
        &mut self,
        device: &D3D11Device,
        surface: &VideoSurface,
        subtitle_overlay: Option<&SubtitleOverlay>,
        timeline_overlay: Option<&SubtitleOverlay>,
        volume_overlay: Option<&SubtitleOverlay>,
        view: &crate::render::ViewTransform,
    ) -> Result<PresentResult, Box<dyn Error>> {
        let backbuffer = self
            .backbuffer
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer texture is not bound".into()))?;

        let (output_width, output_height) = if self.width == 0 || self.height == 0 {
            current_backbuffer_size(backbuffer)?
        } else {
            (self.width, self.height)
        };

        device.render_video_surface(surface, backbuffer, output_width, output_height, view, &mut self.vp_cache)?;
        if let Some(overlay) = subtitle_overlay {
            let render_target = self
                .render_target
                .as_ref()
                .ok_or_else(|| DxgiError("swap-chain render target is not bound".into()))?;
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                output_width,
                output_height,
            )?;
        }
        if let Some(overlay) = timeline_overlay {
            let render_target = self
                .render_target
                .as_ref()
                .ok_or_else(|| DxgiError("swap-chain render target is not bound".into()))?;
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                output_width,
                output_height,
            )?;
        }
        if let Some(overlay) = volume_overlay {
            let render_target = self
                .render_target
                .as_ref()
                .ok_or_else(|| DxgiError("swap-chain render target is not bound".into()))?;
            let renderer = self
                .subtitle_renderer
                .get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(
                renderer,
                overlay,
                render_target,
                output_width,
                output_height,
            )?;
        }

        self.present()
    }

    pub fn resize(
        &mut self,
        device: &D3D11Device,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn Error>> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // Drop swap-chain-dependent views before ResizeBuffers.
        self.vp_cache = None;
        self.backbuffer = None;
        self.render_target = None;

        // SAFETY:
        // - buffer count and format are preserved with zero placeholders
        // - width/height come from the current client area
        // - flags remain zero for the normal windowed resize path
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                Default::default(),
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }

        let (backbuffer, render_target) = create_backbuffer_state(device, &self.swap_chain)?;
        self.backbuffer = Some(backbuffer);
        self.render_target = Some(render_target);
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn present(&self) -> Result<PresentResult, Box<dyn Error>> {
        // SAFETY:
        // - swap chain is live and bound to the current window
        // - sync interval 1 avoids tearing
        // - flags remain zero; DXGI_PRESENT_RESTART is intentionally not used
        let hr = unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)) };

        // DXGI_STATUS_OCCLUDED (0x087A0001): the window is fully covered or
        // minimized — the frame wasn't shown but no action is needed.
        const DXGI_STATUS_OCCLUDED_RAW: i32 = 0x087A0001u32 as i32;
        match hr.0 {
            s if s >= 0 && s != DXGI_STATUS_OCCLUDED_RAW => Ok(PresentResult::Ok),
            DXGI_STATUS_OCCLUDED_RAW => Ok(PresentResult::Occluded),
            _ if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET => {
                Ok(PresentResult::DeviceLost)
            }
            _ => {
                hr.ok()?;
                // unreachable — ok() would have returned Err above
                Ok(PresentResult::Ok)
            }
        }
    }

    pub fn viewport_size(&self) -> Result<(u32, u32), Box<dyn Error>> {
        if self.width != 0 && self.height != 0 {
            return Ok((self.width, self.height));
        }

        let backbuffer = self
            .backbuffer
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer texture is not bound".into()))?;
        current_backbuffer_size(backbuffer)
    }
}

fn create_backbuffer_state(
    device: &D3D11Device,
    swap_chain: &IDXGISwapChain1,
) -> Result<(ID3D11Texture2D, RenderTargetView), Box<dyn Error>> {
    // SAFETY:
    // - buffer index 0 exists for the swap chain's backbuffer
    // - requested interface matches the underlying texture type
    let backbuffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0)? };
    let render_target = device.create_render_target_view(&backbuffer)?;
    Ok((backbuffer, render_target))
}

fn current_backbuffer_size(backbuffer: &ID3D11Texture2D) -> Result<(u32, u32), Box<dyn Error>> {
    let mut desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();

    unsafe {
        backbuffer.GetDesc(&mut desc);
    }

    Ok((desc.Width, desc.Height))
}

fn create_factory() -> Result<IDXGIFactory2, Box<dyn Error>> {
    // SAFETY:
    // - zero flags request a standard DXGI factory
    // - returned interface is owned by the windows crate smart pointer
    let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory2>(DXGI_CREATE_FACTORY_FLAGS(0))? };
    Ok(factory)
}

fn module_handle() -> Result<HINSTANCE, Box<dyn Error>> {
    // SAFETY: null module name requests the current process image handle.
    let module = unsafe { GetModuleHandleW(None)? };
    Ok(module.into())
}

fn register_window_class(instance: HINSTANCE, class_name: PCWSTR) -> Result<(), Box<dyn Error>> {
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW)? };
    let icon = load_fastplay_icon(32, 32);
    let small_icon = load_fastplay_icon(16, 16);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: cursor,
        hIcon: icon,
        hIconSm: small_icon,
        lpszClassName: class_name,
        ..Default::default()
    };

    // SAFETY:
    // - class structure references static data and a valid window procedure
    // - M0 registers a single process-local class for one native shell window
    let atom = unsafe { RegisterClassExW(&class) };
    if atom == 0 {
        return Err(Box::new(DxgiError("RegisterClassExW failed".into())));
    }

    Ok(())
}

fn load_fastplay_icon(width: i32, height: i32) -> HICON {
    let module = unsafe { GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0)).unwrap_or_default() };
    let handle = unsafe {
        LoadImageW(
            module,
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            width,
            height,
            Default::default(),
        )
    };

    match handle {
        Ok(handle) => HICON(handle.0),
        Err(error) => {
            eprintln!("icon load error width={} height={} error={}", width, height, error);
            HICON::default()
        }
    }
}

fn adjust_window_size(width: u32, height: u32) -> Result<(i32, i32), Box<dyn Error>> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };

    // SAFETY:
    // - `rect` points to writable storage
    // - style matches the one used in CreateWindowExW
    unsafe {
        AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0))?;
    }

    Ok((rect.right - rect.left, rect.bottom - rect.top))
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            // SAFETY:
            // - `lparam` contains the CREATESTRUCTW pointer for WM_NCCREATE
            // - create params carry the `Rc<WindowState>` raw pointer from CreateWindowExW
            let createstruct = lparam.0 as *const CREATESTRUCTW;
            let state_ptr = (*createstruct).lpCreateParams as *const WindowState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            LRESULT(1)
        }
        WM_SIZE => {
            if let Some(state) = window_state(hwnd) {
                let mut rect = RECT::default();
                // SAFETY: `hwnd` is the live window being resized and `rect` is writable.
                if GetClientRect(hwnd, &mut rect).is_ok() {
                    let width = (rect.right - rect.left).max(0) as u32;
                    let height = (rect.bottom - rect.top).max(0) as u32;
                    state
                        .resize_request
                        .set(Some(ResizeRequest { width, height }));
                }
            }

            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CLOSE => {
            if let Some(state) = window_state(hwnd) {
                state.is_open.set(false);
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(state) = window_state(hwnd) {
                let ctrl_held =
                    (GetKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32)
                        as u16
                        & 0x8000)
                        != 0;

                match wparam.0 as u32 {
                    // Ctrl+H → toggle borderless fullscreen
                    0x48 if ctrl_held => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::ToggleBorderlessFullscreen);
                    }
                    0x52 if ctrl_held => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::RotateClockwise);
                    }
                    // R (no modifier) → toggle auto-replay
                    0x52 => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::ToggleAutoReplay);
                    }
                    0x45 if ctrl_held => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::RotateCounterClockwise);
                    }
                    // Ctrl+W → fit window to video (no black padding)
                    0x57 if ctrl_held => {
                        state.input_events.borrow_mut().push(InputEvent::FitWindow);
                    }
                    // Ctrl+Q → half the video's native resolution
                    0x51 if ctrl_held => {
                        state.input_events.borrow_mut().push(InputEvent::HalfSizeWindow);
                    }
                    // Ctrl+0 → reset view
                    0x30 if ctrl_held => {
                        state.input_events.borrow_mut().push(InputEvent::ResetView);
                    }
                    0x53 => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::ToggleSubtitles);
                    }
                    key if key == windows::Win32::UI::Input::KeyboardAndMouse::VK_LEFT.0 as u32 => {
                        // Bit 30 of lparam: previous key state (1 = was down).
                        // Accelerate seek on held key repeats.
                        let held = (lparam.0 as u32 >> 30) & 1 != 0;
                        let step = if held { 15 } else { 5 };
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::SeekRelativeSeconds(-step));
                    }
                    key if key
                        == windows::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT.0 as u32 =>
                    {
                        let held = (lparam.0 as u32 >> 30) & 1 != 0;
                        let step = if held { 15 } else { 5 };
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::SeekRelativeSeconds(step));
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = window_state(hwnd) {
                let fw_keys = (wparam.0 & 0xFFFF) as u16;
                let ctrl_held = (fw_keys & 0x0008) != 0;
                if ctrl_held {
                    let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                    // WM_MOUSEWHEEL lparam is in screen coordinates; convert to client.
                    let screen_x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let screen_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let mut pt = POINT {
                        x: screen_x,
                        y: screen_y,
                    };
                    let _ = ScreenToClient(hwnd, &mut pt);
                    state
                        .input_events
                        .borrow_mut()
                        .push(InputEvent::ZoomAtCursor {
                            delta,
                            cursor_x: pt.x,
                            cursor_y: pt.y,
                        });
                } else {
                    let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                    let steps = if delta > 0 {
                        1
                    } else if delta < 0 {
                        -1
                    } else {
                        0
                    };
                    if steps != 0 {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::AdjustVolumeSteps(steps));
                    }
                }
            }
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(state) = window_state(hwnd) {
                if wparam.0 as u32 == ' ' as u32 {
                    state
                        .input_events
                        .borrow_mut()
                        .push(InputEvent::TogglePause);
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_NCLBUTTONDOWN => {
            // DefWindowProcW enters a blocking DragDetect loop on caption
            // clicks that only processes mouse messages — WM_TIMER never
            // fires, freezing playback.  Instead, start our own non-blocking
            // drag tracking via SetCapture.  The normal message pump stays
            // alive so tick() keeps running from the main loop.
            const HTCAPTION: usize = 2;
            if wparam.0 == HTCAPTION {
                if let Some(state) = window_state(hwnd) {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    state.caption_tracking.set(true);
                    state.caption_drag_origin.set(pt);
                    SetCapture(hwnd);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            if let Some(state) = window_state(hwnd) {
                if state.caption_tracking.get() {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let origin = state.caption_drag_origin.get();
                    let dx = (pt.x - origin.x).abs();
                    let dy = (pt.y - origin.y).abs();
                    let cx = GetSystemMetrics(SM_CXDRAG);
                    let cy = GetSystemMetrics(SM_CYDRAG);
                    if dx > cx || dy > cy {
                        // Mouse crossed the drag threshold — end tracking
                        // and enter the real modal move loop via SC_MOVE.
                        state.caption_tracking.set(false);
                        let _ = ReleaseCapture();
                        // SC_MOVE | HTCAPTION tells Windows the move is
                        // mouse-initiated from the caption, preserving
                        // Aero Snap behaviour.
                        return DefWindowProcW(
                            hwnd,
                            WM_SYSCOMMAND,
                            WPARAM(0xF012), // SC_MOVE | HTCAPTION
                            LPARAM(0),
                        );
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            if let Some(state) = window_state(hwnd) {
                // If a double-click arrives while we hold capture from a
                // first click on the caption, clean up the tracking state.
                if state.caption_tracking.get() {
                    state.caption_tracking.set(false);
                    let _ = ReleaseCapture();
                }
                state
                    .input_events
                    .borrow_mut()
                    .push(InputEvent::ToggleBorderlessFullscreen);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = window_state(hwnd) {
                if state.caption_tracking.get() {
                    state.caption_tracking.set(false);
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(state) = window_state(hwnd) {
                state.caption_tracking.set(false);
            }
            LRESULT(0)
        }
        WM_MOVING => {
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_NCRBUTTONUP => {
            // Right-clicking the title bar shows the system context menu.
            // DefWindowProcW internally calls TrackPopupMenu which blocks.
            // Start the tick timer before so ticking begins immediately.
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    hwnd,
                    MODAL_TICK_TIMER_ID,
                    MODAL_TICK_INTERVAL_MS,
                    None,
                );
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_SYSCOMMAND => {
            // Several SC_* codes enter modal loops inside DefWindowProcW.
            // Start the tick timer before the call so playback keeps running.
            const SC_SIZE: usize = 0xF000;
            const SC_MOVE: usize = 0xF010;
            const SC_MOUSEMENU: usize = 0xF090;
            const SC_KEYMENU: usize = 0xF100;
            let cmd = wparam.0 & 0xFFF0;
            if cmd == SC_MOVE || cmd == SC_SIZE || cmd == SC_MOUSEMENU || cmd == SC_KEYMENU {
                if let Some(state) = window_state(hwnd) {
                    state.in_modal_loop.set(true);
                }
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SetTimer(
                        hwnd,
                        MODAL_TICK_TIMER_ID,
                        MODAL_TICK_INTERVAL_MS,
                        None,
                    );
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_ENTERSIZEMOVE => {
            if let Some(state) = window_state(hwnd) {
                state.in_modal_loop.set(true);
            }
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    hwnd,
                    MODAL_TICK_TIMER_ID,
                    MODAL_TICK_INTERVAL_MS,
                    None,
                );
            }
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            if let Some(state) = window_state(hwnd) {
                state.in_modal_loop.set(false);
            }
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, MODAL_TICK_TIMER_ID);
            }
            LRESULT(0)
        }
        WM_ENTERMENULOOP => {
            // Menu modal loop (system menu, etc.).  Keep playback alive
            // with the timer — no sync tick to avoid calling Present during
            // DispatchMessageW.
            if let Some(state) = window_state(hwnd) {
                state.in_modal_loop.set(true);
            }
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    hwnd,
                    MODAL_TICK_TIMER_ID,
                    MODAL_TICK_INTERVAL_MS,
                    None,
                );
            }
            LRESULT(0)
        }
        WM_EXITMENULOOP => {
            if let Some(state) = window_state(hwnd) {
                state.in_modal_loop.set(false);
            }
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, MODAL_TICK_TIMER_ID);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == MODAL_TICK_TIMER_ID {
                if let Some(state) = window_state(hwnd) {
                    if let Some(tick_fn) = state.modal_tick_fn.get() {
                        let ctx = state.modal_tick_ctx.get();
                        tick_fn(ctx);
                    }
                }
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            let hdrop = HDROP(wparam.0 as *mut c_void);
            // SAFETY: hdrop is valid for the duration of this message.
            let file_count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
            if file_count > 0 {
                // Query the length of the first file path (excluding null).
                let len = DragQueryFileW(hdrop, 0, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, 0, Some(&mut buf));
                // Trim trailing null.
                if buf.last() == Some(&0) {
                    buf.pop();
                }
                let path = PathBuf::from(std::ffi::OsString::from_wide(&buf));
                if let Some(state) = window_state(hwnd) {
                    state
                        .input_events
                        .borrow_mut()
                        .push(InputEvent::FileDropped(path));
                }
            }
            DragFinish(hdrop);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state_ptr) = take_window_state(hwnd) {
                let state = Rc::from_raw(state_ptr);
                state.is_open.set(false);
            }
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn window_state(hwnd: HWND) -> Option<&'static WindowState> {
    let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
        as *const WindowState;
    ptr.as_ref()
}

unsafe fn take_window_state(hwnd: HWND) -> Option<*const WindowState> {
    let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
        as *const WindowState;
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}
