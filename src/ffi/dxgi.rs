use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::OsStr,
    fmt,
    os::windows::ffi::OsStrExt,
    ptr::null_mut,
    rc::Rc,
};

use windows::{
    core::{w, Interface, PCWSTR},
    Win32::{
        Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Direct3D11::ID3D11Texture2D,
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
                DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
                DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
                DXGI_USAGE_RENDER_TARGET_OUTPUT,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GetClientRect, LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassW,
            SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
            CW_USEDEFAULT, GWLP_USERDATA, HMENU, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW,
            WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_NCCREATE, WM_SIZE, WNDCLASSW,
            WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
    },
};

use crate::{
    ffi::d3d11::{D3D11Device, RenderTargetView, VideoSurface},
    platform::input::InputEvent,
};

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
}

pub struct NativeWindowInner {
    hwnd: HWND,
    state: Rc<WindowState>,
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

        Ok(Self { hwnd, state })
    }

    pub fn pump_messages(&mut self) -> Result<(), Box<dyn Error>> {
        let mut message = MSG::default();

        loop {
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

    pub fn take_input_events(&self) -> Vec<InputEvent> {
        std::mem::take(&mut *self.state.input_events.borrow_mut())
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
        })
    }

    pub fn render(
        &mut self,
        device: &D3D11Device,
        clear_color: [f32; 4],
    ) -> Result<(), Box<dyn Error>> {
        let render_target = self
            .render_target
            .as_ref()
            .ok_or_else(|| DxgiError("swap-chain backbuffer is not bound".into()))?;

        device.clear_render_target(render_target, clear_color);

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

        device.render_video_surface(surface, backbuffer, output_width, output_height)?;

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
                if wparam.0 as u32 == windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE.0 as u32 {
                    state.input_events.borrow_mut().push(InputEvent::TogglePause);
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
