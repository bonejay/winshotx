//! Small pure helpers shared by the windows: text formatting, shortcut labels, window
//! placement and the recording bar's rolling frame rate.

use crate::capture::Rect;
use crate::platform::window_style::MonitorPlace;

/// `m:ss`, as shown by the recording timer.
pub fn duration(ms: u64) -> String {
    let total = (ms + 500) / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.0} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / (K * K))
    } else {
        format!("{:.1} GB", f / (K * K * K))
    }
}

/// Readable label for one key of a stored shortcut. Shortcuts are stored with physical key
/// codes (`CmdOrCtrl+Shift+KeyS`), which do not change with the keyboard layout.
pub fn key_label(code: &str) -> String {
    if let Some(d) = code.strip_prefix("Digit") {
        return d.into();
    }
    if let Some(k) = code.strip_prefix("Key") {
        return k.into();
    }
    if let Some(n) = code.strip_prefix("Numpad") {
        return format!("Num {n}");
    }
    match code {
        "CmdOrCtrl" | "CommandOrControl" | "Control" | "Ctrl" => "Ctrl",
        "Super" | "Meta" => "Win",
        "Escape" => "Esc",
        "PrintScreen" => "Print Screen",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "Backslash" => "\\",
        "Slash" => "/",
        "Comma" => ",",
        "Period" => ".",
        "Minus" => "-",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        other => other,
    }
    .into()
}

pub fn shortcut_parts(shortcut: &str) -> Vec<String> {
    shortcut
        .split('+')
        .filter(|p| !p.is_empty())
        .map(key_label)
        .collect()
}

/// Recording bar size in logical pixels.
pub const BAR_SIZE: (f64, f64) = (540.0, 56.0);

/// Where the recording bar goes: centred under the region on its monitor, above it if the
/// region reaches the bottom, always inside the monitor. Physical pixels.
pub fn bar_position(region: Rect, monitor: Option<MonitorPlace>) -> (i32, i32, u32, u32) {
    let scale = monitor.map(|m| m.scale).unwrap_or(1.0);
    let margin = (8.0 * scale) as i32;
    let mut w = (BAR_SIZE.0 * scale) as i32;
    let mut h = (BAR_SIZE.1 * scale) as i32;
    if let Some(m) = monitor {
        w = w.min((m.width - margin * 2).max(1));
        h = h.min((m.height - margin * 2).max(1));
    }
    let gap = (12.0 * scale) as i32;
    let mut x = region.x + (region.width as i32 - w) / 2;
    let mut y = region.y + region.height as i32 + gap;
    if let Some(m) = monitor {
        let left = m.x + margin;
        let right = (m.x + m.width - w - margin).max(left);
        x = x.clamp(left, right);
        let bottom = m.y + m.height - h - margin;
        if y > bottom {
            // Above the region, or inside its bottom edge when it fills the screen.
            y = (region.y - h - gap).max(m.y + margin);
            if region.y - h - gap < m.y + margin {
                y = bottom;
            }
        }
    }
    (x, y, w as u32, h as u32)
}

/// Bottom-right corner of a monitor's work area, for popups. Physical pixels.
pub fn corner_position(m: MonitorPlace, size: (f64, f64)) -> (i32, i32, u32, u32) {
    let (w, h) = ((size.0 * m.scale) as i32, (size.1 * m.scale) as i32);
    let margin = (16.0 * m.scale) as i32;
    let (wx, wy, ww, wh) = m.work;
    (
        wx + ww - w - margin,
        wy + wh - h - margin,
        w as u32,
        h as u32,
    )
}

/// Centred on a monitor. Physical pixels.
pub fn centred(m: MonitorPlace, size: (f64, f64)) -> (i32, i32, u32, u32) {
    let (w, h) = ((size.0 * m.scale) as i32, (size.1 * m.scale) as i32);
    (
        m.x + (m.width - w) / 2,
        m.y + (m.height - h) / 2,
        w as u32,
        h as u32,
    )
}

/// Captured frames per second over the last ~2 s of recorded time. Paused time does not
/// advance `elapsed_ms`, so the window simply waits while paused.
#[derive(Debug, Default)]
pub struct RollingFps {
    samples: std::collections::VecDeque<(u64, u64)>,
}

impl RollingFps {
    const WINDOW_MS: u64 = 2_000;

    pub fn push(&mut self, elapsed_ms: u64, frames: u64) -> Option<u32> {
        if self.samples.back().is_none_or(|&(ms, _)| elapsed_ms > ms) {
            self.samples.push_back((elapsed_ms, frames));
        }
        while self.samples.len() > 2 && elapsed_ms - self.samples[1].0 >= Self::WINDOW_MS {
            self.samples.pop_front();
        }
        let &(ms0, f0) = self.samples.front()?;
        let span = elapsed_ms.saturating_sub(ms0);
        (span >= 500).then(|| ((frames - f0) as f64 * 1000.0 / span as f64).round() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerates assets/icon.png and assets/icon.ico from assets/logo.svg.
    #[test]
    #[ignore = "Rewrites the icon files"]
    fn generate_icons() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
        let svg = std::fs::read_to_string(format!("{dir}/logo.svg")).unwrap();
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
        let render = |size: u32| {
            let mut pm = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
            let s = size as f32 / 256.0;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(s, s),
                &mut pm.as_mut(),
            );
            // Un-premultiply for PNG.
            let mut rgba = pm.data().to_vec();
            for p in rgba.chunks_exact_mut(4) {
                let a = u32::from(p[3]);
                if a > 0 {
                    for c in &mut p[..3] {
                        *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
            image::RgbaImage::from_raw(size, size, rgba).unwrap()
        };
        render(256).save(format!("{dir}/icon.png")).unwrap();
        let frames: Vec<IcoFrame> = [16, 20, 24, 32, 40, 48, 64, 128, 256]
            .iter()
            .map(|&n| {
                IcoFrame::as_png(render(n).as_raw(), n, n, image::ExtendedColorType::Rgba8).unwrap()
            })
            .collect();
        let file = std::fs::File::create(format!("{dir}/icon.ico")).unwrap();
        IcoEncoder::new(file).encode_images(&frames).unwrap();
    }

    fn monitor(x: i32, y: i32, w: i32, h: i32, scale: f64) -> MonitorPlace {
        MonitorPlace {
            x,
            y,
            width: w,
            height: h,
            work: (x, y, w, h - 48),
            scale,
        }
    }

    #[test]
    fn durations_and_sizes() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(61_400), "1:01");
        assert_eq!(duration(3_600_000), "60:00");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2 KB");
        assert_eq!(bytes(5 * 1024 * 1024 + 300_000), "5.3 MB");
    }

    #[test]
    fn shortcut_labels_are_readable() {
        assert_eq!(
            shortcut_parts("CmdOrCtrl+Shift+KeyS"),
            ["Ctrl", "Shift", "S"]
        );
        assert_eq!(
            shortcut_parts("Shift+PrintScreen"),
            ["Shift", "Print Screen"]
        );
        assert_eq!(shortcut_parts("Super+Digit2"), ["Win", "2"]);
        assert_eq!(shortcut_parts("Alt+Numpad5"), ["Alt", "Num 5"]);
    }

    #[test]
    fn bar_goes_under_the_region_inside_the_monitor() {
        let m = monitor(0, 0, 1920, 1080, 1.0);
        let r = Rect {
            x: 100,
            y: 100,
            width: 800,
            height: 400,
        };
        assert_eq!(bar_position(r, Some(m)), (230, 512, 540, 56));
        // Near the left edge: clamped inside.
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        assert_eq!(bar_position(r, Some(m)).0, 8);
        // Full screen: inside the bottom edge.
        let r = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(bar_position(r, Some(m)).1, 1080 - 56 - 8);
    }

    #[test]
    fn bar_respects_negative_monitors_and_scale() {
        let m = monitor(-2560, -300, 2560, 1440, 1.5);
        let r = Rect {
            x: -2000,
            y: 1000,
            width: 400,
            height: 100,
        };
        let (x, y, w, h) = bar_position(r, Some(m));
        assert_eq!((w, h), (810, 84));
        assert!(x >= -2560 + 12 && x + w as i32 <= -12);
        assert!(y + h as i32 <= 1140 - 12, "fits above the bottom");
        assert!(y < 1000, "region near the bottom puts the bar above it");
    }

    #[test]
    fn popup_corner_uses_the_work_area() {
        let m = monitor(0, 0, 1920, 1080, 1.0);
        assert_eq!(
            corner_position(m, (380.0, 84.0)),
            (1920 - 380 - 16, 1032 - 84 - 16, 380, 84)
        );
        assert_eq!(centred(m, (132.0, 132.0)), (894, 474, 132, 132));
    }

    #[test]
    fn rolling_fps_needs_half_a_second_and_forgets_old_samples() {
        let mut fps = RollingFps::default();
        assert_eq!(fps.push(0, 0), None);
        assert_eq!(fps.push(200, 12), None);
        assert_eq!(fps.push(1000, 60), Some(60));
        for i in 2..10u64 {
            fps.push(i * 1000, 60 + (i - 1) * 30);
        }
        assert_eq!(fps.push(10_000, 330), Some(30));
    }
}
