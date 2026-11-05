//! Process-level plumbing: single instance, global shortcuts, tray icon, shortcut recording,
//! and native handles of Slint windows.

pub mod hotkeys;
pub mod instance;
pub mod keycapture;
pub mod tray;

/// The Win32 HWND behind a Slint window, once it exists.
pub fn hwnd(window: &slint::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle();
    match handle.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
