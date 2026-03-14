use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::{c_void, OsStr},
    fmt,
    os::windows::ffi::OsStrExt,
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
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
                DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
                DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
                DXGI_USAGE_RENDER_TARGET_OUTPUT,
            },
            Gdi::{GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::GetKeyState,
            WindowsAndMessaging::{
                AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                DispatchMessageW, GetClientRect, GetWindowLongPtrW, GetWindowPlacement,
                LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassW,
                SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, ShowWindow,
                TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
                GWL_STYLE, GWLP_USERDATA, HMENU, HWND_TOP, IDC_ARROW, MSG,
                PM_REMOVE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW,
                WINDOW_EX_STYLE, WINDOWPLACEMENT, WM_CHAR, WM_CLOSE, WM_DESTROY,
                WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_KEYDOWN, WM_MOUSEWHEEL, WM_NCCREATE,
                WM_SIZE, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_POPUP,
                WS_VISIBLE,
            },
        },
    },
};

use crate::{
    ffi::d3d11::{D3D11Device, RenderTargetView, SubtitleOverlay, SubtitleRenderer, VideoSurface},
    platform::input::InputEvent,
};

const MAX_MESSAGES_PER_PUMP: usize = 64;
const MODAL_TICK_TIMER_ID: usize = 1;
const MODAL_TICK_INTERVAL_MS: u32 = 16;

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
    pub unsafe fn install_modal_tick(
        &self,
        ctx: *mut c_void,
        tick_fn: unsafe fn(*mut c_void),
    ) {
        self.state.modal_tick_fn.set(Some(tick_fn));
        self.state.modal_tick_ctx.set(ctx);
    }

    pub fn clear_modal_tick(&self) {
        self.state.modal_tick_fn.set(None);
        self.state.modal_tick_ctx.set(null_mut());
    }

    pub fn take_input_events(&self) -> Vec<InputEvent> {
        std::mem::take(&mut *self.state.input_events.borrow_mut())
    }

    pub fn is_borderless(&self) -> bool {
        self.is_borderless.get()
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
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED
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

pub struct DxgiSwapChain {
    swap_chain: IDXGISwapChain1,
    backbuffer: Option<ID3D11Texture2D>,
    render_target: Option<RenderTargetView>,
    width: u32,
    height: u32,
    subtitle_renderer: Option<SubtitleRenderer>,
}

impl DxgiSwapChain {
    pub fn create(
        window: &NativeWindowInner,
        device: &D3D11Device,
    ) -> Result<Self, Box<dyn Error>> {
        let factory: IDXGIFactory2 = create_factory()?;
        let dxgi_device: IDXGIDevice = device.raw_device().cast()?;
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
        })
    }

    pub fn render(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
        subtitle_overlay: Option<&SubtitleOverlay>,
    ) -> Result<(), Box<dyn Error>> {
        let render_target = self
            .render_target
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer is not bound".into()))?;

        device.clear_render_target(render_target, clear_color);
        if let Some(overlay) = subtitle_overlay {
            let renderer = self.subtitle_renderer.get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(renderer, overlay, render_target)?;
        }

        // SAFETY:
        // - swap chain is live and bound to the current window
        // - sync interval 1 avoids tearing for this simple M0 shell
        // - flags remain zero; DXGI_PRESENT_RESTART is intentionally not used
        unsafe {
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }

        Ok(())
    }

    pub fn render_surface(
        &mut self,
        device: &D3D11Device,
        surface: &VideoSurface,
        subtitle_overlay: Option<&SubtitleOverlay>,
        view: &crate::render::ViewTransform,
    ) -> Result<(), Box<dyn Error>> {
        let backbuffer = self
            .backbuffer
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer texture is not bound".into()))?;

        let (output_width, output_height) = if self.width == 0 || self.height == 0 {
            current_backbuffer_size(backbuffer)?
        } else {
            (self.width, self.height)
        };

        device.render_video_surface(surface, backbuffer, output_width, output_height, view)?;
        if let Some(overlay) = subtitle_overlay {
            let render_target = self
                .render_target
                .as_ref()
                .ok_or_else(|| DxgiError("swap-chain render target is not bound".into()))?;
            let renderer = self.subtitle_renderer.get_or_insert(device.create_subtitle_renderer()?);
            device.render_subtitle_overlay(renderer, overlay, render_target)?;
        }

        unsafe {
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }

        Ok(())
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
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: class_name,
        ..Default::default()
    };

    // SAFETY:
    // - class structure references static data and a valid window procedure
    // - M0 registers a single process-local class for one native shell window
    let atom = unsafe { RegisterClassW(&class) };
    if atom == 0 {
        return Err(Box::new(DxgiError("RegisterClassW failed".into())));
    }

    Ok(())
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
                let ctrl_held = (GetKeyState(
                    windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32,
                ) as u16
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
                    // Ctrl+0 → reset view
                    0x30 if ctrl_held => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::ResetView);
                    }
                    0x53 => {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::ToggleSubtitles);
                    }
                    key if key
                        == windows::Win32::UI::Input::KeyboardAndMouse::VK_LEFT.0 as u32 =>
                    {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::SeekRelativeSeconds(-5));
                    }
                    key if key
                        == windows::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT.0 as u32 =>
                    {
                        state
                            .input_events
                            .borrow_mut()
                            .push(InputEvent::SeekRelativeSeconds(5));
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
                    state.input_events.borrow_mut().push(InputEvent::ZoomAtCursor {
                        delta,
                        cursor_x: pt.x,
                        cursor_y: pt.y,
                    });
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
        WM_ENTERSIZEMOVE => {
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
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                    hwnd,
                    MODAL_TICK_TIMER_ID,
                );
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
