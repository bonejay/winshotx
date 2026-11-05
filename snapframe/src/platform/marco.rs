//! The border shown around the region being recorded, so the user can see what is inside the
//! recording.
//!
//! It is four plain Win32 windows, two pixels wide, placed just outside the
//! region so they never appear in the video. They are click-through (`WS_EX_TRANSPARENT`),
//! never take focus (`WS_EX_NOACTIVATE`), stay out of Alt+Tab (`WS_EX_TOOLWINDOW`), and are
//! always on top (`WS_EX_TOPMOST`).
//!
//! They live on their own thread with a message loop and are controlled with
//! `PostThreadMessage` (change color, close).

#![cfg(windows)]

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, HBRUSH,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PostQuitMessage, PostThreadMessageW, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowLongPtrW, ShowWindow, TranslateMessage, GWLP_USERDATA,
    HTTRANSPARENT, LWA_ALPHA, MSG, SW_SHOWNOACTIVATE, WM_APP, WM_ERASEBKGND, WM_NCHITTEST,
    WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::capture::Rect;
use crate::error::{AppError, Result};

/// Red while recording.
const GRABANDO: u32 = rgb(0xEF, 0x44, 0x44);
/// Amber while paused, matching the toolbar dot.
const EN_PAUSA: u32 = rgb(0xF5, 0x9E, 0x0B);

/// `COLORREF` is BGR: blue in the high byte.
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// Each line stores its color in its own window (`GWLP_USERDATA`), so two borders alive at once
/// cannot interfere.
fn color_de_la_ventana(hwnd: HWND) -> u32 {
    match unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u32 {
        0 => GRABANDO,
        color => color,
    }
}

/// Messages sent to the border thread.
const MSG_REPINTAR: u32 = WM_APP + 1;
const MSG_CERRAR: u32 = WM_APP + 2;

/// The on-screen border. Closes when dropped.
pub struct Marco {
    hilo: Option<JoinHandle<()>>,
    /// The message-loop thread, so messages can be posted to it.
    id_hilo: u32,
}

impl Marco {
    /// Open the border around the region with lines `grosor` pixels wide, outside it. Returns
    /// once all four windows exist, or with an error. Recording continues without a border if
    /// this fails.
    pub fn abrir(region: Rect, grosor: i32) -> Result<Self> {
        let (aviso_tx, aviso_rx) = mpsc::channel::<Aviso>();
        let hilo = std::thread::Builder::new()
            .name("winshotx-marco".into())
            .spawn(move || bucle(region, grosor.max(1), aviso_tx))
            .map_err(|e| AppError::Msg(format!("Could not open the capture border: {e}")))?;
        let id_hilo = aviso_rx
            .recv()
            .map_err(|_| AppError::Msg("Could not start the capture border".into()))?
            .map_err(AppError::Msg)?;
        Ok(Self {
            hilo: Some(hilo),
            id_hilo,
        })
    }

    /// Change the color: amber when paused, red when recording.
    pub fn pausado(&self, pausado: bool) {
        let color = if pausado { EN_PAUSA } else { GRABANDO };
        let _ = unsafe {
            PostThreadMessageW(
                self.id_hilo,
                MSG_REPINTAR,
                WPARAM(color as usize),
                LPARAM(0),
            )
        };
    }
}

impl Drop for Marco {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.id_hilo, MSG_CERRAR, WPARAM(0), LPARAM(0)) };
        if let Some(hilo) = self.hilo.take() {
            let _ = hilo.join();
        }
    }
}

/// The four line rectangles outside the region: top, bottom, left, right. Separate so it can be
/// tested.
pub fn lineas(region: Rect, grosor: i32) -> [Rect; 4] {
    let g = grosor.max(1);
    let (x, y) = (region.x, region.y);
    let (w, h) = (region.width as i32, region.height as i32);
    let ancho_con_esquinas = (w + 2 * g) as u32;
    [
        // Top and bottom also cover the corners.
        Rect {
            x: x - g,
            y: y - g,
            width: ancho_con_esquinas,
            height: g as u32,
        },
        Rect {
            x: x - g,
            y: y + h,
            width: ancho_con_esquinas,
            height: g as u32,
        },
        Rect {
            x: x - g,
            y,
            width: g as u32,
            height: h as u32,
        },
        Rect {
            x: x + w,
            y,
            width: g as u32,
            height: h as u32,
        },
    ]
}

/// What the thread reports back: its id, or why it failed.
type Aviso = std::result::Result<u32, String>;

/// Thread body: create the windows, report back, and pump messages until told to close.
fn bucle(region: Rect, grosor: i32, aviso: mpsc::Sender<Aviso>) {
    let ventanas = match unsafe { crear_ventanas(region, grosor) } {
        Ok(v) => v,
        Err(e) => {
            let _ = aviso.send(Err(e));
            return;
        }
    };
    // The thread now has a message queue (created with the first window), so posted messages
    // are not lost.
    let _ = aviso.send(Ok(unsafe { GetCurrentThreadId() }));

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                MSG_REPINTAR if msg.hwnd.0.is_null() => {
                    for &hwnd in &ventanas {
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, msg.wParam.0 as isize);
                        let _ = InvalidateRect(Some(hwnd), None, true);
                    }
                }
                MSG_CERRAR if msg.hwnd.0.is_null() => {
                    for &hwnd in &ventanas {
                        let _ = DestroyWindow(hwnd);
                    }
                    PostQuitMessage(0);
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Window class name. Registered once per process; later `RegisterClassW` calls fail with
/// "already exists", which is fine.
const CLASE: &[u16] = &[
    b'w' as u16,
    b'i' as u16,
    b'n' as u16,
    b's' as u16,
    b'h' as u16,
    b'o' as u16,
    b't' as u16,
    b'x' as u16,
    b'-' as u16,
    b'm' as u16,
    b'a' as u16,
    b'r' as u16,
    b'c' as u16,
    b'o' as u16,
    0,
];

unsafe fn crear_ventanas(region: Rect, grosor: i32) -> std::result::Result<Vec<HWND>, String> {
    let instancia = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let clase = WNDCLASSW {
        lpfnWndProc: Some(procedimiento),
        hInstance: instancia.into(),
        lpszClassName: PCWSTR(CLASE.as_ptr()),
        ..Default::default()
    };
    // Zero means failure unless the class already existed.
    let _ = unsafe { RegisterClassW(&clase) };

    let mut ventanas = Vec::with_capacity(4);
    for linea in lineas(region, grosor) {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                PCWSTR(CLASE.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                linea.x,
                linea.y,
                linea.width as i32,
                linea.height as i32,
                None,
                None,
                Some(instancia.into()),
                None,
            )
        }
        .map_err(|e| format!("could not create a recording border line: {e}"))?;
        // Set the color before showing the window so the first paint is already red.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, GRABANDO as isize) };
        // A `WS_EX_LAYERED` window does not paint until its attributes are set. Fully opaque.
        let _ = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) };
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        ventanas.push(hwnd);
    }
    Ok(ventanas)
}

/// Window procedure: paint a solid color and let the mouse through.
unsafe extern "system" fn procedimiento(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            let brocha: HBRUSH = unsafe { CreateSolidBrush(COLORREF(color_de_la_ventana(hwnd))) };
            unsafe {
                FillRect(hdc, &ps.rcPaint, brocha);
                let _ = DeleteObject(brocha.into());
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        // WM_PAINT fills the whole window; reporting the background as erased avoids flicker.
        WM_ERASEBKGND => LRESULT(1),
        // In case `WS_EX_TRANSPARENT` is not enough: never claim the mouse.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
