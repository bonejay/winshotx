#[cfg(windows)]
mod wgc;
use std::sync::Arc;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Rectangle in physical pixels of the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn center(&self) -> (i32, i32) {
        (
            self.x + (self.width / 2) as i32,
            self.y + (self.height / 2) as i32,
        )
    }

    /// Video codecs require even dimensions.
    pub fn to_even(mut self) -> Self {
        self.width = (self.width / 2) * 2;
        self.height = (self.height / 2) * 2;
        self.width = self.width.max(2);
        self.height = self.height.max(2);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub id: u32,
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub is_primary: bool,
}

impl MonitorInfo {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x + self.width as i32
            && y < self.y + self.height as i32
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowRect {
    pub title: String,
    pub rect: Rect,
    /// The window's HWND, so the window itself can be recorded. Zero if unavailable.
    #[serde(default)]
    pub hwnd: isize,
}

/// A frozen monitor image: the background the user selects on, kept as raw pixels for the
/// overlay, cropping and stitching.
#[derive(Debug, Clone)]
pub struct Freeze {
    pub monitor: MonitorInfo,
    pub image: Arc<RgbaImage>,
}

/// Monitor position, size, and name.
fn describir(index: usize, monitor: &xcap::Monitor) -> Result<MonitorInfo> {
    Ok(MonitorInfo {
        id: index as u32,
        label: monitor
            .name()
            .unwrap_or_else(|_| format!("Monitor {}", index + 1)),
        x: monitor.x().map_err(|e| AppError::Msg(e.to_string()))?,
        y: monitor.y().map_err(|e| AppError::Msg(e.to_string()))?,
        width: monitor.width().map_err(|e| AppError::Msg(e.to_string()))?,
        height: monitor.height().map_err(|e| AppError::Msg(e.to_string()))?,
        scale: monitor.scale_factor().unwrap_or(1.0),
        is_primary: monitor.is_primary().unwrap_or(false),
    })
}

/// The monitors, without capturing them.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    xcap::Monitor::all()
        .map_err(|e| AppError::Msg(e.to_string()))?
        .iter()
        .enumerate()
        .map(|(index, monitor)| describir(index, monitor))
        .collect()
}

/// Screenshot of one monitor through Windows.Graphics.Capture, the same API as recordings. GDI
/// is the fallback when it is unavailable or fails, and that is logged.
fn fotografiar(monitor: &xcap::Monitor, info: &MonitorInfo) -> Result<RgbaImage> {
    #[cfg(windows)]
    if wgc::disponible() {
        match wgc::fotografiar(info) {
            Ok(imagen) => return Ok(imagen),
            Err(error) => eprintln!(
                "[capture] {}: Windows.Graphics.Capture failed ({error}), falling back to GDI",
                info.label
            ),
        }
    }
    monitor
        .capture_image()
        .map_err(|e| AppError::Msg(e.to_string()))
}

/// Release the screenshot GPU device; see `wgc::liberar`.
pub fn liberar() {
    #[cfg(windows)]
    wgc::liberar();
}

/// Freeze only the given monitors (by system enumeration index), in parallel, so the screen
/// under the cursor can be shown before the others are ready.
pub fn freeze_monitors(indices: &[usize]) -> Result<Vec<Freeze>> {
    if indices.is_empty() {
        return Ok(Vec::new());
    }

    // Monitors are captured in parallel, one thread each. Each thread looks up its own monitor
    // because `xcap::Monitor` holds a raw pointer and cannot be sent between threads; re-
    // enumerating costs microseconds.
    let capturas = std::thread::scope(|scope| -> Result<Vec<_>> {
        let handles: Vec<_> = indices
            .iter()
            .map(|&index| {
                scope.spawn(move || -> Result<(usize, MonitorInfo, image::RgbaImage)> {
                    let suyos = xcap::Monitor::all().map_err(|e| AppError::Msg(e.to_string()))?;
                    let monitor = suyos
                        .get(index)
                        .ok_or_else(|| AppError::Msg("A display was disconnected".into()))?;
                    let info = describir(index, monitor)?;
                    let image = fotografiar(monitor, &info)?;
                    Ok((index, info, image))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| AppError::Msg("Could not freeze a display".into()))?
            })
            .collect()
    })?;

    if capturas.is_empty() {
        return Err(AppError::Msg("No display was detected".into()));
    }

    Ok(capturas
        .into_iter()
        .map(|(_, info, image)| Freeze {
            monitor: info,
            image: Arc::new(image),
        })
        .collect())
}

/// Whether the window is click-through (`WS_EX_TRANSPARENT`), like NVIDIA, Discord, or Game Bar
/// overlays. They cover the screen invisibly and would otherwise win window snapping.
#[cfg(windows)]
fn deja_pasar_el_raton(hwnd: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };
    let estilo = unsafe { GetWindowLongPtrW(HWND(hwnd as isize as *mut _), GWL_EXSTYLE) } as u32;
    estilo & WS_EX_TRANSPARENT.0 != 0
}

#[cfg(not(windows))]
fn deja_pasar_el_raton(_hwnd: u32) -> bool {
    false
}

/// OBS's rule for capturable windows (`check_window_valid` in
/// `libobs/util/windows/window-helpers.c`): visible, not cloaked, not a tool window, not a
/// child. This drops invisible full-screen overlays such as NVIDIA's, which otherwise sit
/// under every desktop click and record as black.
#[cfg(windows)]
fn capturable(hwnd: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, WS_CHILD, WS_EX_TOOLWINDOW,
    };
    let hwnd = HWND(hwnd as isize as *mut _);
    let mut cloaked = 0u32;
    let cloaked = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0;
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    unsafe { IsWindowVisible(hwnd) }.as_bool()
        && !cloaked
        && ex_style & WS_EX_TOOLWINDOW.0 == 0
        && style & WS_CHILD.0 == 0
}

#[cfg(not(windows))]
fn capturable(_hwnd: u32) -> bool {
    true
}

/// Visible window rectangles for overlay window snapping. Order matters: it is the Z order from
/// `EnumWindows` (front first), and the overlay picks the first one under the cursor. Do not
/// reorder.
pub fn window_rects() -> Vec<WindowRect> {
    let Ok(windows) = xcap::Window::all() else {
        return Vec::new();
    };
    // Skip our own overlay windows, which cover the whole screen.
    let own_pid = std::process::id();
    windows
        .iter()
        .filter(|w| w.pid().map(|pid| pid != own_pid).unwrap_or(true))
        .filter(|w| !w.is_minimized().unwrap_or(true))
        .filter(|w| !w.id().map(deja_pasar_el_raton).unwrap_or(false))
        .filter(|w| w.id().map(capturable).unwrap_or(false))
        .filter_map(|w| {
            let title = w.title().unwrap_or_default();
            let width = w.width().ok()?;
            let height = w.height().ok()?;
            if width < 40 || height < 40 {
                return None;
            }
            Some(WindowRect {
                title,
                hwnd: w.id().map(|id| id as isize).unwrap_or(0),
                rect: Rect {
                    x: w.x().ok()?,
                    y: w.y().ok()?,
                    width,
                    height,
                },
            })
        })
        .collect()
}

/// Crop the requested region from the frozen image of the monitor that contains it.
pub fn crop_from_freeze(freezes: &[Freeze], region: Rect) -> Result<image::RgbaImage> {
    let (cx, cy) = region.center();
    let freeze = freezes
        .iter()
        .find(|f| f.monitor.contains(cx, cy))
        .or_else(|| freezes.first())
        .ok_or_else(|| AppError::Msg("No frozen display is available".into()))?;

    let image = &freeze.image;
    let local_x = (region.x - freeze.monitor.x).max(0) as u32;
    let local_y = (region.y - freeze.monitor.y).max(0) as u32;
    let width = region.width.min(image.width().saturating_sub(local_x));
    let height = region.height.min(image.height().saturating_sub(local_y));
    if width == 0 || height == 0 {
        return Err(AppError::Msg("The selection is outside the display".into()));
    }
    Ok(image::imageops::crop_imm(image.as_ref(), local_x, local_y, width, height).to_image())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    #[ignore = "Reads this machine's real window list"]
    fn live_invisible_overlays_are_not_offered_as_windows() {
        let offered = super::window_rects();
        let titles: Vec<_> = offered.iter().map(|w| w.title.as_str()).collect();
        println!("offered: {titles:?}");
        assert!(!titles.contains(&"NVIDIA GeForce Overlay"));
    }
}
