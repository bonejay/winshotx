//! Native rounded corners. CSS corners on a transparent window leave a dark halo; Windows 11
//! can clip them itself.

#[cfg(windows)]
pub fn rounded_corners(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    let Ok(handle) = window.hwnd() else { return };
    unsafe {
        let hwnd = HWND(handle.0);
        let preference = DWMWCP_ROUND;
        // The attribute does not exist on Windows 10; the error is ignored.
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const preference).cast(),
            std::mem::size_of_val(&preference) as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn rounded_corners(_window: &tauri::WebviewWindow) {}

/// A window that never takes focus, even when shown again or clicked. Used by the countdown so
/// a delayed capture does not close the menu being captured. `WS_EX_NOACTIVATE` prevents
/// activation and `WS_EX_TOOLWINDOW` keeps it out of Alt+Tab.
#[cfg(windows)]
pub fn never_focus(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    let Ok(handle) = window.hwnd() else { return };
    unsafe {
        let hwnd = HWND(handle.0);
        let actual = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let nuevo = actual | (WS_EX_NOACTIVATE.0 as isize) | (WS_EX_TOOLWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, nuevo);
    }
}

#[cfg(not(windows))]
pub fn never_focus(_window: &tauri::WebviewWindow) {}

/// Move and show the window in one call without activating it, for overlays parked off-screen.
/// Returns `false` on failure so the caller can fall back.
#[cfg(windows)]
pub fn colocar_y_ensennar(window: &tauri::WebviewWindow, x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    let Ok(handle) = window.hwnd() else { return false };
    unsafe {
        SetWindowPos(
            HWND(handle.0),
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
        )
        .is_ok()
    }
}

#[cfg(not(windows))]
pub fn colocar_y_ensennar(_window: &tauri::WebviewWindow, _x: i32, _y: i32) -> bool {
    false
}

/// Exclude the window from screen capture without hiding it (`WDA_EXCLUDEFROMCAPTURE`, Windows
/// 10 2004+). Returns `false` on older systems.
#[cfg(windows)]
pub fn fuera_de_la_captura(window: &tauri::WebviewWindow, fuera: bool) -> bool {
    let Ok(handle) = window.hwnd() else { return false };
    return fuera_de_la_captura_hwnd(handle.0 as isize, fuera);
}

/// The same for a raw HWND, so it can be tested without creating a webview window.
#[cfg(windows)]
pub fn fuera_de_la_captura_hwnd(ventana: isize, fuera: bool) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    let hwnd = HWND(ventana as *mut core::ffi::c_void);
    let afinidad = if fuera { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    return unsafe { SetWindowDisplayAffinity(hwnd, afinidad) }.is_ok();
}

#[cfg(not(windows))]
pub fn fuera_de_la_captura(_window: &tauri::WebviewWindow, _fuera: bool) -> bool {
    false
}
