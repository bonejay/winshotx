use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Instant;

use windows_capture::monitor::Monitor as WcMonitor;

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

/// Packed pixels or one timestamped compressed transport packet, selected by PixelFormat.
pub struct CapturedFrame {
    pub pixels: Vec<u8>,
}

/// Absorb a short writer scheduling stall without discarding capture frames.
pub const FRAME_QUEUE: usize = 4;

#[derive(Clone)]
pub struct CaptureFlags {
    pub sender: SyncSender<CapturedFrame>,
    pub format: super::stream_encoder::PixelFormat,
    /// Native NVENC settings, only for compressed GPU output.
    pub encoding: Option<crate::settings::EncodingSettings>,
    /// Crop inside the monitor: x1, y1, x2, y2.
    pub crop: (u32, u32, u32, u32),
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub min_interval: std::time::Duration,
    /// Used buffers returned for the next frame. See `Reciclados`.
    pub reciclados: Reciclados,
    /// Diagnostics counters (see `record::stats`).
    pub stats: Arc<super::stats::Stats>,
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

pub(super) fn frame_due(
    next: &mut Option<Instant>,
    now: Instant,
    interval: std::time::Duration,
) -> bool {
    if interval.is_zero() {
        return true;
    }
    let deadline = next.unwrap_or(now);
    // Windows delivers frames with 1-2 ms of jitter. A tight tolerance dropped every frame
    // that came slightly early, which on a 60 Hz screen at 60 fps meant random repeated frames.
    // A quarter period absorbs jitter but still rejects the in-between frames of a faster
    // source (e.g. every other frame of 60 Hz at 30 fps).
    let tolerance = interval / 4;
    if now + tolerance < deadline {
        return false;
    }
    let periods = now.saturating_duration_since(deadline).as_nanos() / interval.as_nanos() + 1;
    *next = Some(deadline + interval.mul_f64(periods as f64));
    true
}

pub use super::capture_session::Control;

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
    super::capture_session::start(
        super::capture_session::Target::Monitor(monitor.as_raw_hmonitor() as isize),
        capture_cursor,
        flags,
    )
}

/// A fixed even output size; resized windows are cropped or padded by the worker.
pub fn start_window(
    hwnd: isize,
    capture_cursor: bool,
    fps: u32,
    flags_base: CaptureFlags,
) -> Result<Control> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut bounds = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            HWND(hwnd as *mut _),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut bounds).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .map_err(|_| AppError::Msg("The selected window no longer exists".into()))?;
    let crop = if flags_base.crop.2 > 0 && flags_base.crop.3 > 0 {
        flags_base.crop
    } else {
        (
            0,
            0,
            ((bounds.right - bounds.left).max(2) as u32) & !1,
            ((bounds.bottom - bounds.top).max(2) as u32) & !1,
        )
    };
    let flags = CaptureFlags {
        crop,
        min_interval: std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
        ..flags_base
    };
    super::capture_session::start(
        super::capture_session::Target::Window(hwnd),
        capture_cursor,
        flags,
    )
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[test]
    #[ignore = "Opens an animated 1080p window and measures real Windows capture for 30 seconds"]
    fn sustained_60_fps_capture() {
        let animation = super::super::validation::Animation::new(1920, 1080);
        let stats = Arc::new(super::super::stats::Stats::new());
        let recycled = Reciclados::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let flags = CaptureFlags {
            sender: tx,
            format: super::super::stream_encoder::PixelFormat::Bgra,
            encoding: None,
            crop: (0, 0, 0, 0),
            stop: stop.clone(),
            pause: Arc::new(AtomicBool::new(false)),
            min_interval: std::time::Duration::ZERO,
            reciclados: recycled.clone(),
            stats: stats.clone(),
        };
        let control = start_window(animation.hwnd, false, 60, flags).unwrap();
        let started = Instant::now();
        let mut log =
            super::super::stats::Log::start("Sustained 1080p60 animated capture validation");
        let mut received = 0;
        while started.elapsed() < std::time::Duration::from_secs(30) {
            if let Ok(frame) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                assert_eq!(frame.pixels.len(), 1920 * 1080 * 4);
                recycled.devolver(frame.pixels);
                received += 1;
            }
            log.tick(&stats, 60);
        }
        stop.store(true, Ordering::Release);
        control.stop().unwrap();
        println!("Captured {received} fresh frames in {:.3}s ({:.2} fps); arrived {}, throttled {}, queue full {}",
            started.elapsed().as_secs_f64(), received as f64 / started.elapsed().as_secs_f64(),
            stats.arrived.load(Ordering::Relaxed), stats.throttled.load(Ordering::Relaxed), stats.queue_full.load(Ordering::Relaxed));
        assert!(
            received >= 1785,
            "Capture supplied fewer than 59.5 fresh fps: {received}"
        );
        assert_eq!(stats.capture_full.load(Ordering::Relaxed), 0);
        assert_eq!(stats.queue_full.load(Ordering::Relaxed), 0);
    }
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
