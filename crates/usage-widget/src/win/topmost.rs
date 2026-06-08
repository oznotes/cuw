//! Force the borderless widget above other windows.
//!
//! gpui does NOT implement always-on-top on Windows (`WindowKind::PopUp` only
//! sets `WS_EX_TOOLWINDOW`, never `HWND_TOPMOST`). So we grab the raw `HWND`
//! (via `raw-window-handle` from the gpui window) and set topmost ourselves.
//!
//! M0 (highest project risk): confirm this holds above other windows and the
//! exact `windows` 0.58 `SetWindowPos` signature (the second arg may be `HWND`
//! rather than `Option<HWND>` depending on the crate version).

/// Pin the window identified by `hwnd` (a raw Win32 handle as `isize`) topmost.
#[cfg(target_os = "windows")]
pub fn apply_topmost(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: hwnd comes from the live gpui window's raw-window-handle.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub fn apply_topmost(_hwnd: isize) {}
