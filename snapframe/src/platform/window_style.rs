//! Win32 window tweaks Slint does not expose: taskbar visibility, never-activate windows,
//! capture exclusion, native rounded corners, foreground activation. All take a raw HWND.

/// A screen's position, size and work area in physical pixels, plus its display scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorPlace {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The area not covered by the taskbar.
    pub work: (i32, i32, i32, i32),
    pub scale: f64,
}

#[cfg(windows)]
mod imp {
    use super::MonitorPlace;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowLongPtrW, SetForegroundWindow, SetWindowDisplayAffinity,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOW, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
        WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }

    fn change_ex_style(raw: isize, add: u32, remove: u32) {
        unsafe {
            let current = GetWindowLongPtrW(hwnd(raw), GWL_EXSTYLE) as u32;
            SetWindowLongPtrW(hwnd(raw), GWL_EXSTYLE, ((current | add) & !remove) as isize);
        }
    }

    /// Keep the window out of the taskbar and Alt+Tab.
    pub fn tool_window(raw: isize) {
        change_ex_style(raw, WS_EX_TOOLWINDOW.0, WS_EX_APPWINDOW.0);
    }

    /// A window that never takes focus, even when clicked, so showing it does not close a menu
    /// that is about to be captured.
    pub fn never_focus(raw: isize) {
        change_ex_style(
            raw,
            WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0,
            WS_EX_APPWINDOW.0,
        );
    }

    /// Exclude the window from screen capture (`WDA_EXCLUDEFROMCAPTURE`, Windows 10 2004+).
    pub fn exclude_from_capture(raw: isize, exclude: bool) -> bool {
        let affinity = if exclude {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        unsafe { SetWindowDisplayAffinity(hwnd(raw), affinity) }.is_ok()
    }

    /// No show/hide fade. During the fade a GPU window that has not presented a frame yet is
    /// drawn black.
    pub fn no_transitions(raw: isize) {
        use windows::Win32::Graphics::Dwm::DWMWA_TRANSITIONS_FORCEDISABLED;
        let disable = windows::core::BOOL(1);
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd(raw),
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&raw const disable).cast(),
                std::mem::size_of_val(&disable) as u32,
            );
        }
    }

    /// Windows 11 native rounded corners; ignored on Windows 10.
    pub fn rounded_corners(raw: isize) {
        let preference = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd(raw),
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const preference).cast(),
                std::mem::size_of_val(&preference) as u32,
            );
        }
    }

    /// Topmost without activating, for floating bars and popups.
    pub fn topmost_no_activate(raw: isize) {
        unsafe {
            let _ = SetWindowPos(
                hwnd(raw),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// Windows only lets a process take the foreground when it is entitled to; after a global
    /// shortcut it must be requested explicitly, or the first click only activates the window.
    pub fn force_foreground(raw: isize) {
        unsafe {
            let _ = ShowWindow(hwnd(raw), SW_SHOW);
            let _ = SetForegroundWindow(hwnd(raw));
        }
    }

    pub fn cursor_position() -> Option<(i32, i32)> {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point).ok()? };
        Some((point.x, point.y))
    }

    /// The monitor containing that desktop point (or the nearest one).
    pub fn monitor_at(x: i32, y: i32) -> Option<MonitorPlace> {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let r = info.rcMonitor;
            let w = info.rcWork;
            Some(MonitorPlace {
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                work: (w.left, w.top, w.right - w.left, w.bottom - w.top),
                scale: f64::from(dpi_x) / 96.0,
            })
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MonitorPlace;
    pub fn tool_window(_: isize) {}
    pub fn never_focus(_: isize) {}
    pub fn exclude_from_capture(_: isize, _: bool) -> bool {
        false
    }
    pub fn rounded_corners(_: isize) {}
    pub fn no_transitions(_: isize) {}
    pub fn topmost_no_activate(_: isize) {}
    pub fn force_foreground(_: isize) {}
    pub fn cursor_position() -> Option<(i32, i32)> {
        None
    }
    pub fn monitor_at(_: i32, _: i32) -> Option<MonitorPlace> {
        None
    }
}

pub use imp::*;
