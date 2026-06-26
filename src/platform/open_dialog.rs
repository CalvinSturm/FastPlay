use std::path::PathBuf;
use std::ptr::null_mut;

use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
};

/// Show the native Windows file open dialog. Returns `Some(path)` if the user
/// selected a file, or `None` if the dialog was cancelled.
///
/// # Safety
///
/// `hwnd` must be a valid window handle or null (for no owner).
pub fn show_open_file_dialog(hwnd: HWND) -> Option<PathBuf> {
    // COM should already be initialized (OleInitialize in window creation),
    // but ensure it is for safety — this is a no-op if already initialized.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()? };

    // ── File type filters ────────────────────────────────────────────────
    // Common video and audio formats, plus a catch-all.
    let media_filter_name: Vec<u16> = "Media Files\0".encode_utf16().collect();
    // The supported-extension list lives in `app::media_ext` so the dialog filter
    // and the play queue cannot drift apart.
    let media_filter_spec: Vec<u16> =
        format!("{}\0", crate::app::media_ext::media_dialog_filter_spec())
            .encode_utf16()
            .collect();
    let all_filter_name: Vec<u16> = "All Files\0".encode_utf16().collect();
    let all_filter_spec: Vec<u16> = "*.*\0".encode_utf16().collect();

    let filters = [
        COMDLG_FILTERSPEC {
            pszName: windows::core::PCWSTR(media_filter_name.as_ptr()),
            pszSpec: windows::core::PCWSTR(media_filter_spec.as_ptr()),
        },
        COMDLG_FILTERSPEC {
            pszName: windows::core::PCWSTR(all_filter_name.as_ptr()),
            pszSpec: windows::core::PCWSTR(all_filter_spec.as_ptr()),
        },
    ];

    unsafe {
        let file_dialog: IFileDialog = dialog.cast().ok()?;
        let _ = file_dialog.SetFileTypes(&filters);
        let _ = file_dialog.SetFileTypeIndex(1); // 1-based: "Media Files" first
        let _ = file_dialog.SetTitle(windows::core::w!("Open Media File"));

        // Force filesystem paths, require existing paths.
        if let Ok(options) = file_dialog.GetOptions() {
            let _ = file_dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
        }

        // Show the dialog. S_OK = user picked a file, any other = cancelled.
        let owner = if hwnd.0 != null_mut() {
            hwnd
        } else {
            HWND(null_mut())
        };
        if dialog.Show(owner).is_err() {
            // User cancelled or error — either way, no file to open.
            return None;
        }

        let result = file_dialog.GetResult().ok()?;
        let path_pwstr = result.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path_str = path_pwstr.to_string().ok()?;
        windows::Win32::System::Com::CoTaskMemFree(Some(path_pwstr.0 as *const _));
        let path = PathBuf::from(path_str);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}
