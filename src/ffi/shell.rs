use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HWND,
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{SHOW_WINDOW_CMD, SW_SHOW},
        },
    },
};

pub fn open_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("URL required".to_string());
    }

    let url_wide: Vec<u16> = OsStr::new(url).encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(url_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SHOW_WINDOW_CMD(SW_SHOW.0),
        )
    };
    if (result.0 as isize) <= 32 {
        Err(format!(
            "ShellExecuteW failed with code {}",
            result.0 as isize
        ))
    } else {
        Ok(())
    }
}
