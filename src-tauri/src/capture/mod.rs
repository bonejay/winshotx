#[cfg(windows)]
mod wgc;
use std::sync::Arc;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Rectangle in physical pixels of the virtual desktop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

/// A frozen monitor image: the background the user selects on. Kept in memory as raw pixels
/// (for cropping and stitching in Rust) and as a fast PNG sent to the overlay webview, which is
/// much faster than sending ~8 MB of raw pixels per screen.
#[derive(Debug, Clone)]
pub struct Freeze {
    pub monitor: MonitorInfo,
    pub image: Arc<RgbaImage>,
    pub png: Arc<Vec<u8>>,
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

/// Prepare screenshot capture before the first shortcut; see `wgc::preparar`.
pub fn preparar() {
    #[cfg(windows)]
    wgc::preparar();
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

    // Each screen is also encoded to PNG on its own thread (~28 ms per 1080p screen with fast
    // compression).
    std::thread::scope(|scope| -> Result<Vec<Freeze>> {
        let handles: Vec<_> = capturas
            .into_iter()
            .map(|(_, info, image)| {
                scope.spawn(move || -> Result<Freeze> {
                    let png = crate::encode::png::to_bytes(&image)?;
                    Ok(Freeze {
                        monitor: info,
                        image: Arc::new(image),
                        png: Arc::new(png),
                    })
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| AppError::Msg("Image compression failed".into()))?
            })
            .collect()
    })
}

/// A hand-built 32-bit BMP of the image: the overlay's fallback if PNG fails. Uncompressed and
/// simple (54-byte header, bottom-up BGRA rows), ~6 ms per screen.
pub fn bmp_bytes(image: &RgbaImage) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let fila = width as usize * 4;
    let pixeles = fila * height as usize;
    let mut out = Vec::with_capacity(54 + pixeles);

    // BITMAPFILEHEADER (14 bytes).
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + pixeles) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes): 32 bpp, uncompressed, bottom-up rows.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixeles as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // Rows reversed and channels swapped in four-byte chunks so the compiler emits wide copies
    // instead of per-byte bounds checks.
    let raw = image.as_raw();
    let cabecera = out.len();
    out.resize(cabecera + pixeles, 0);
    let cuerpo = &mut out[cabecera..];
    for (y, destino) in cuerpo.chunks_exact_mut(fila).enumerate() {
        let origen = &raw[(height as usize - 1 - y) * fila..(height as usize - y) * fila];
        for (d, s) in destino.chunks_exact_mut(4).zip(origen.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    }
    out
}

/// Whether the window is click-through (`WS_EX_TRANSPARENT`), like NVIDIA, Discord, or Game Bar
/// overlays. They cover the screen invisibly and would otherwise win window snapping.
#[cfg(windows)]
fn deja_pasar_el_raton(hwnd: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT};
    let estilo = unsafe { GetWindowLongPtrW(HWND(hwnd as isize as *mut _), GWL_EXSTYLE) } as u32;
    estilo & WS_EX_TRANSPARENT.0 != 0
}

#[cfg(not(windows))]
fn deja_pasar_el_raton(_hwnd: u32) -> bool {
    false
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

/// Bounding rectangle of all monitors (the whole virtual desktop). Areas outside every monitor
/// belong to no screen; see `stitch_all`.
pub fn virtual_desktop(freezes: &[Freeze]) -> Option<Rect> {
    let primero = freezes.first()?;
    let (mut x0, mut y0) = (primero.monitor.x, primero.monitor.y);
    let (mut x1, mut y1) = (
        primero.monitor.x + primero.monitor.width as i32,
        primero.monitor.y + primero.monitor.height as i32,
    );
    for f in freezes.iter().skip(1) {
        x0 = x0.min(f.monitor.x);
        y0 = y0.min(f.monitor.y);
        x1 = x1.max(f.monitor.x + f.monitor.width as i32);
        y1 = y1.max(f.monitor.y + f.monitor.height as i32);
    }
    Some(Rect {
        x: x0,
        y: y0,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}

/// Combine all frozen monitors into one image at their real positions. Gaps between misaligned
/// monitors stay transparent: saved PNGs keep that, while the clipboard composites over white.
pub fn stitch_all(freezes: &[Freeze]) -> Result<(image::RgbaImage, Rect)> {
    let marco = virtual_desktop(freezes)
        .ok_or_else(|| AppError::Msg("No frozen displays are available".into()))?;

    let mut lienzo = image::RgbaImage::from_pixel(marco.width, marco.height, image::Rgba([0, 0, 0, 0]));

    // Row-by-row memory copies instead of `imageops::overlay`, which blended per pixel (~50 ms
    // for three screens). Frozen screens are opaque and monitors do not overlap, so a plain
    // copy is equivalent.
    let ancho_lienzo = marco.width as usize * 4;
    for f in freezes {
        let dx = (f.monitor.x - marco.x).max(0) as usize;
        let dy = (f.monitor.y - marco.y).max(0) as usize;
        let (fw, fh) = f.image.dimensions();
        let fila = fw as usize * 4;
        let origen = f.image.as_raw();
        let destino = lienzo.as_mut();
        for y in 0..fh as usize {
            let a = (dy + y) * ancho_lienzo + dx * 4;
            if a + fila > destino.len() {
                break;
            }
            destino[a..a + fila].copy_from_slice(&origen[y * fila..(y + 1) * fila]);
        }
    }

    Ok((lienzo, marco))
}
