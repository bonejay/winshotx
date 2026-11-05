//! The native overlay window: a borderless, topmost, layered popup per monitor. Its pixels come
//! from a [`Canvas`] via `UpdateLayeredWindowIndirect`, so the content exists before the window
//! is shown and no blank frame can appear. Input messages are forwarded to the app.

use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, LoadCursorW,
    RegisterClassExW, SetCursor, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    UpdateLayeredWindowIndirect, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HCURSOR, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
    IDC_SIZEWE, MA_ACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, ULW_OPAQUE,
    UPDATELAYEREDWINDOWINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::model::LRect;
use super::paint::Canvas;

/// Input from an overlay window, in its client (monitor-local physical) pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    Down(i32, i32),
    Move(i32, i32),
    Up(i32, i32),
    RightDown,
    /// A key: the character for letters and digits, `None` for Escape.
    Key(Option<char>, bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Cross,
    Arrow,
    Hand,
    SizeNwse,
    SizeNesw,
    SizeNs,
    SizeWe,
}

const CLASS: PCWSTR = w!("SnapframeOverlay");

fn register() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    })
}

fn id_of(hwnd: HWND) -> Option<u32> {
    let v = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    (v > 0).then(|| (v - 1) as u32)
}

fn point(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    (
        i32::from((v & 0xffff) as u16 as i16),
        i32::from((v >> 16) as u16 as i16),
    )
}

/// Map a virtual-key code to what the overlay understands.
pub fn key_of(vk: u32) -> Option<Option<char>> {
    match vk {
        0x1B => Some(None),
        0x30..=0x39 | 0x41..=0x5A => Some(char::from_u32(vk).map(|c| c.to_ascii_lowercase())),
        0x60..=0x69 => char::from_digit(vk - 0x60, 10).map(Some),
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(id) = id_of(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let forward = |input: Input| crate::app::with_app(|a| a.overlay_input(id, input));
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            forward(Input::Move(x, y));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let (x, y) = point(lparam);
            forward(Input::Down(x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let (x, y) = point(lparam);
            forward(Input::Up(x, y));
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            forward(Input::RightDown);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if let Some(key) = key_of(wparam.0 as u32) {
                let ctrl = (GetKeyState(i32::from(VK_CONTROL.0)) as u16 & 0x8000) != 0;
                forward(Input::Key(key, ctrl));
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETCURSOR if (lparam.0 as u32 & 0xffff) == HTCLIENT => {
            let mut cursor = Cursor::Cross;
            crate::app::with_app(|a| cursor = a.overlay_cursor(id));
            let which = match cursor {
                Cursor::Cross => IDC_CROSS,
                Cursor::Arrow => IDC_ARROW,
                Cursor::Hand => IDC_HAND,
                Cursor::SizeNwse => IDC_SIZENWSE,
                Cursor::SizeNesw => IDC_SIZENESW,
                Cursor::SizeNs => IDC_SIZENS,
                Cursor::SizeWe => IDC_SIZEWE,
            };
            if let Ok(c) = LoadCursorW(None, which) {
                SetCursor(Some(HCURSOR(c.0)));
            }
            LRESULT(1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub struct OverlayWindow {
    pub hwnd: HWND,
    pub canvas: Canvas,
    x: i32,
    y: i32,
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl OverlayWindow {
    /// Create (hidden) for the monitor at `x, y` with this physical size.
    pub fn new(id: u32, x: i32, y: i32, width: i32, height: i32, scale: f64) -> Option<Self> {
        if !register() {
            eprintln!("[overlay] could not register the window class");
            return None;
        }
        let Some(canvas) = Canvas::new(width, height, scale) else {
            eprintln!("[overlay] could not create the {width}x{height} back buffer");
            return None;
        };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                w!("Snapframe capture"),
                WS_POPUP,
                x,
                y,
                width,
                height,
                None,
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
        }
        .map_err(|e| eprintln!("[overlay] CreateWindowExW failed: {e}"))
        .ok()?;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, id as isize + 1);
        }
        crate::platform::window_style::no_transitions(hwnd.0 as isize);
        Some(Self { hwnd, canvas, x, y })
    }

    /// Send the back buffer (or the changed parts of it) to the window.
    pub fn present(&self, dirty: &[LRect]) {
        let full = [LRect::new(0, 0, self.canvas.width, self.canvas.height)];
        let parts: &[LRect] = if dirty.len() > 8 || dirty.is_empty() {
            &full
        } else {
            dirty
        };
        let dst = POINT {
            x: self.x,
            y: self.y,
        };
        let size = SIZE {
            cx: self.canvas.width,
            cy: self.canvas.height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 0,
        };
        for r in parts {
            let rect = RECT {
                left: r.x,
                top: r.y,
                right: r.right(),
                bottom: r.bottom(),
            };
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: Default::default(),
                pptDst: &dst,
                psize: &size,
                hdcSrc: self.canvas.dc,
                pptSrc: &src,
                crKey: Default::default(),
                pblend: &blend,
                dwFlags: ULW_OPAQUE,
                prcDirty: &rect,
            };
            unsafe {
                let _ = UpdateLayeredWindowIndirect(self.hwnd, &info);
            }
        }
    }

    pub fn show(&self, focus: bool) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.x,
                self.y,
                self.canvas.width,
                self.canvas.height,
                SWP_SHOWWINDOW
                    | if focus {
                        Default::default()
                    } else {
                        SWP_NOACTIVATE
                    },
            );
            if focus {
                let _ = SetForegroundWindow(self.hwnd);
                let _ = SetFocus(Some(self.hwnd));
            }
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_map_to_overlay_keys() {
        assert_eq!(key_of(0x1B), Some(None));
        assert_eq!(key_of(0x46), Some(Some('f')));
        assert_eq!(key_of(0x33), Some(Some('3')));
        assert_eq!(key_of(0x63), Some(Some('3')), "numpad digits");
        assert_eq!(key_of(0x10), None);
    }
}
