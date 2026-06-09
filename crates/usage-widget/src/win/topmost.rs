// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Always-on-top. gpui does NOT implement this on Windows, so we read the raw
//! `HWND` from the gpui window (it impls `raw_window_handle::HasWindowHandle`)
//! and set `HWND_TOPMOST` via `SetWindowPos`.

/// Pin a gpui window above other windows.
#[cfg(target_os = "windows")]
pub fn pin(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // NB: gpui::Window has an *inherent* `window_handle()` returning a gpui
    // `AnyWindowHandle`, which shadows the trait method — so call the
    // raw-window-handle trait via UFCS to get the real OS handle.
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(w) = handle.as_raw() {
            apply_topmost(w.hwnd.get());
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn pin(_window: &gpui::Window) {}

/// Set `HWND_TOPMOST` on a raw Win32 handle (`isize`).
#[cfg(target_os = "windows")]
fn apply_topmost(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: hwnd is the live gpui window's handle.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}
