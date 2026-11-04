use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Instant;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor as WcMonitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::capture::Rect;
use crate::error::{AppError, Result};

/// windows-capture identifies monitors by HMONITOR, so ask Win32 for it.
pub(crate) fn monitor_at(x: i32, y: i32) -> Result<WcMonitor> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};

    let handle = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if handle.is_invalid() {
        return Err(AppError::Msg("No display at this position".into()));
    }
    Ok(WcMonitor::from_raw_hmonitor(handle.0))
}

/// A captured frame, cropped to the region, in BGRA.
pub struct CapturedFrame {
    pub bgra: Vec<u8>,
}

#[derive(Clone)]
pub struct CaptureFlags {
    pub sender: SyncSender<CapturedFrame>,
    /// Crop inside the monitor: x1, y1, x2, y2.
    pub crop: (u32, u32, u32, u32),
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub min_interval: std::time::Duration,
    /// Used buffers returned for the next frame. See `Reciclados`.
    pub reciclados: Reciclados,
}

/// Frame buffers that are reused instead of allocating ~8 MB per frame. Copying a 1080p frame
/// into a fresh `Vec` took 3.4 ms versus 0.5 ms into a reused one, because Windows has to zero
/// new pages.
///
/// The consumer returns buffers with `devolver`; capture takes one with `coger`. If none is
/// available a new one is allocated, so this never blocks.
#[derive(Clone, Default)]
pub struct Reciclados(Arc<parking_lot::Mutex<Vec<Vec<u8>>>>);

impl Reciclados {
    /// Maximum number of buffers kept; extras are dropped.
    const TOPE: usize = 4;

    /// An empty buffer, reusing previous memory when available.
    pub fn coger(&self) -> Vec<u8> {
        self.0.lock().pop().unwrap_or_default()
    }

    /// Return a buffer for the next frame.
    pub fn devolver(&self, mut bufer: Vec<u8>) {
        bufer.clear();
        let mut guardados = self.0.lock();
        if guardados.len() < Self::TOPE {
            guardados.push(bufer);
        }
    }
}

fn frame_due(next: &mut Option<Instant>, now: Instant, interval: std::time::Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let deadline = next.unwrap_or(now);
    let tolerance = (interval / 16).min(std::time::Duration::from_micros(500));
    if now + tolerance < deadline {
        return false;
    }
    let periods = now.saturating_duration_since(deadline).as_nanos() / interval.as_nanos() + 1;
    *next = Some(deadline + interval.mul_f64(periods as f64));
    true
}

pub struct RegionCapture {
    flags: CaptureFlags,
    next_emit: Option<Instant>,
    /// Scratch buffer the crate needs when the texture has row padding. Kept between frames to
    /// avoid reallocating.
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for RegionCapture {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            flags: ctx.flags,
            next_emit: None,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        if self.flags.stop.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }
        if self.flags.pause.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Keep a target cadence without rounding 120 fps to whole milliseconds or
        // halving delivery when the monitor runs at a different refresh rate.
        let now = Instant::now();
        if !frame_due(&mut self.next_emit, now, self.flags.min_interval) {
            return Ok(());
        }

        let (x1, y1, x2, y2) = self.flags.crop;
        // When recording a window its size can change, but the encoder needs a constant frame
        // size. Copy what fits and leave the rest black; skip the frame if the window is gone.
        let (ancho_real, alto_real) = (frame.width(), frame.height());
        let (hasta_x, hasta_y) = (x2.min(ancho_real), y2.min(alto_real));
        if x1 >= hasta_x || y1 >= hasta_y {
            return Ok(());
        }
        let buffer = frame.buffer_crop(x1, y1, hasta_x, hasta_y)?;
        // Scratch buffer for row-padded textures: reuse a returned buffer if there is one.
        let mut data = self.flags.reciclados.coger();
        let pedido = ((x2 - x1) as usize, (y2 - y1) as usize);
        let hay = ((hasta_x - x1) as usize, (hasta_y - y1) as usize);
        let sin_relleno = buffer.as_nopadding_buffer(&mut self.scratch);
        if hay == pedido {
            data.extend_from_slice(sin_relleno);
        } else {
            data.resize(pedido.0 * pedido.1 * 4, 0);
            for fila in 0..hay.1 {
                let desde = fila * hay.0 * 4;
                let hasta = desde + hay.0 * 4;
                let destino = fila * pedido.0 * 4;
                data[destino..destino + hay.0 * 4].copy_from_slice(&sin_relleno[desde..hasta]);
            }
        }

        // Bound queued raw frames so high-rate capture cannot grow memory without
        // limit when encoding or storage is slower. Timing retains the real gap.
        match self
            .flags
            .sender
            .try_send(CapturedFrame { bgra: data })
        {
            Ok(()) => {}
            Err(TrySendError::Full(frame)) => self.flags.reciclados.devolver(frame.bgra),
            Err(TrySendError::Disconnected(frame)) => {
                self.flags.reciclados.devolver(frame.bgra);
                self.flags.stop.store(true, Ordering::Relaxed);
                control.stop();
            }
        }
        Ok(())
    }

    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        self.flags.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
}

pub type Control = windows_capture::capture::CaptureControl<
    RegionCapture,
    Box<dyn std::error::Error + Send + Sync>,
>;

/// Start capturing the region on the monitor that contains it. `monitor_origin` comes from
/// xcap's monitor enumeration.
pub fn start(
    region: Rect,
    monitor_origin: (i32, i32),
    capture_cursor: bool,
    fps: u32,
    flags_base: CaptureFlags,
) -> Result<Control> {
    let (cx, cy) = region.center();
    let monitor = monitor_at(cx, cy)?;

    let x1 = (region.x - monitor_origin.0).max(0) as u32;
    let y1 = (region.y - monitor_origin.1).max(0) as u32;
    let flags = CaptureFlags {
        crop: (x1, y1, x1 + region.width, y1 + region.height),
        min_interval: std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
        ..flags_base
    };

    let settings = Settings::new(
        monitor,
        if capture_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        },
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    RegionCapture::start_free_threaded(settings).map_err(|e| AppError::Msg(e.to_string()))
}

/// Capture a single window, even when other windows cover it, instead of a screen region.
///
/// The size is fixed when recording starts: if the window grows the original area is recorded;
/// if it shrinks the rest is black.
pub fn start_window(
    hwnd: isize,
    capture_cursor: bool,
    fps: u32,
    flags_base: CaptureFlags,
) -> Result<Control> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};

    let ventana = HWND(hwnd as *mut core::ffi::c_void);
    // Use the real composed frame bounds, not `GetWindowRect`, which includes the invisible
    // shadow border.
    let mut marco = RECT::default();
    let medida = unsafe {
        DwmGetWindowAttribute(
            ventana,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut marco).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if medida.is_err() {
        return Err(AppError::Msg("The selected window no longer exists".into()));
    }
    let ancho = (marco.right - marco.left).max(1) as u32;
    let alto = (marco.bottom - marco.top).max(1) as u32;

    let flags = CaptureFlags {
        crop: (0, 0, ancho, alto),
        min_interval: std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
        ..flags_base
    };

    let settings = Settings::new(
        windows_capture::window::Window::from_raw_hwnd(ventana.0),
        if capture_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        },
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    RegionCapture::start_free_threaded(settings).map_err(|e| AppError::Msg(e.to_string()))
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[test]
    fn a_144_hz_source_can_deliver_a_120_fps_target() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..1440)
            .filter(|i| {
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 144.0),
                    interval,
                )
            })
            .count();
        assert!(
            (1199..=1201).contains(&accepted),
            "{accepted} frames in ten seconds"
        );
    }
    #[test]
    fn a_240_hz_source_is_limited_to_120_fps_without_phase_drift() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..2400)
            .filter(|i| {
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 240.0),
                    interval,
                )
            })
            .count();
        assert!(
            (1199..=1201).contains(&accepted),
            "{accepted} frames in ten seconds"
        );
    }
    #[test]
    fn small_delivery_jitter_does_not_drop_every_other_frame() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..1200)
            .filter(|i| {
                let jitter = if i % 2 == 0 { 0.0001 } else { 0.0 };
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 120.0 + jitter),
                    interval,
                )
            })
            .count();
        assert_eq!(accepted, 1200);
    }
}
