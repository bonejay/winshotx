//! Automatic encoding, chosen on the toolbar as a target frame rate plus a quality level
//! (stored in `preset`: low, medium or high, shown as Small, Balanced and Sharp; encoder
//! `auto`). Levels are tuned for mostly desktop content where file size matters more than
//! detail: constant quality keeps static screens tiny, and a peak bitrate scaled to the pixel
//! rate keeps occasional video from ballooning the file. The encoder, speed preset
//! and quality value are picked when the recording size is known:
//!
//! - NVIDIA NVENC H.264 whenever this machine has it. It is a separate chip on the GPU, so
//!   games or 3D load barely slow it down.
//! - Otherwise x264, at a speed preset from the pixel rate (size × fps), one step slower on
//!   CPUs with 16+ threads, one step faster with 4 or fewer or when the CPU is already busy,
//!   and never slower than preflight accepts.
use std::sync::atomic::{AtomicU8, Ordering};

use super::preflight::{rate_tier, P1440_60, X264_SLOWEST, X26X};
use crate::settings::EncodingSettings;

pub const AUTO: &str = "auto";
pub const LEVELS: [&str; 3] = ["low", "medium", "high"];
/// Toolbar names of [`LEVELS`].
pub const LEVEL_LABELS: [&str; 3] = ["Small", "Balanced", "Sharp"];

/// Peak bitrate per level, in bits per pixel per frame, with a floor for small selections.
/// 4K60 → 10 / 17 / 30 Mbps; 1080p60 → 2.5 / 4.4 / 7.5 Mbps.
const PEAK_BITS_PER_PIXEL: [f64; 3] = [0.02, 0.035, 0.06];
const PEAK_FLOOR_MBPS: [f64; 3] = [1.0, 1.5, 2.5];

/// The peak bitrate (Mbps) for a level at this size and frame rate.
pub fn peak_mbps(level: usize, width: u32, height: u32, fps: u32) -> f64 {
    let rate = f64::from(width) * f64::from(height) * f64::from(fps);
    let level = level.min(2);
    let mbps = (rate * PEAK_BITS_PER_PIXEL[level] / 1_000_000.0).max(PEAK_FLOOR_MBPS[level]);
    ((mbps * 10.0).round() / 10.0).min(200.0)
}

pub fn is_auto(e: &EncodingSettings) -> bool {
    e.encoder == AUTO
}

/// Index of a quality level; unknown values count as medium.
pub fn level_index(level: &str) -> usize {
    LEVELS.iter().position(|l| *l == level).unwrap_or(1)
}

/// Automatic settings with this quality level, keeping the FFmpeg location.
pub fn settings(level: usize, ffmpeg_path: &str) -> EncodingSettings {
    EncodingSettings {
        encoder: AUTO.into(),
        preset: LEVELS[level.min(2)].into(),
        ffmpeg_path: ffmpeg_path.into(),
        ..Default::default()
    }
}

/// What the automatic choice depends on besides size and frame rate.
#[derive(Debug, Clone, Copy)]
pub struct Machine {
    pub gpu: bool,
    /// Logical processors.
    pub threads: usize,
    /// CPU use in percent over the last couple of seconds.
    pub cpu_busy: u8,
}

impl Machine {
    pub fn here() -> Self {
        Self {
            gpu: gpu_available(),
            threads: std::thread::available_parallelism().map_or(8, |n| n.get()),
            cpu_busy: CPU_BUSY.load(Ordering::Relaxed),
        }
    }
}

/// Concrete settings for this size and frame rate; non-automatic settings are returned as is.
pub fn resolve(
    e: &EncodingSettings,
    width: u32,
    height: u32,
    fps: u32,
    machine: Machine,
) -> EncodingSettings {
    if !is_auto(e) {
        return e.clone();
    }
    let level = level_index(&e.preset);
    let (encoder, preset, crf) = if machine.gpu {
        ("h264_nvenc", ["p4", "p5", "p6"][level], [34, 30, 25][level])
    } else {
        // Indexes into X26X. Rows: up to 1080p30, below 1440p60, below 4K60, 4K60 and above.
        const X264: [[isize; 3]; 4] = [[3, 5, 6], [2, 4, 5], [1, 2, 3], [0, 1, 2]];
        let tier = rate_tier(u64::from(width) * u64::from(height) * u64::from(fps));
        let mut speed = X264[tier][level];
        if machine.threads >= 16 {
            speed += 1;
        } else if machine.threads <= 4 {
            speed -= 1;
        }
        if machine.cpu_busy >= 60 {
            speed -= 1;
        }
        let speed = speed.clamp(0, X264_SLOWEST[tier] as isize) as usize;
        ("libx264", X26X[speed], [32, 28, 23][level])
    };
    EncodingSettings {
        engine: "ffmpeg".into(),
        ffmpeg_path: e.ffmpeg_path.clone(),
        rate_control: "crf".into(),
        crf,
        encoder: encoder.into(),
        bitrate_mbps: 0.0,
        max_bitrate_mbps: peak_mbps(level, width, height, fps),
        preset: preset.into(),
        gpu: String::new(),
        extra_args: "[]".into(),
    }
}

/// [`resolve`] for this machine as it is right now.
pub fn resolve_here(e: &EncodingSettings, width: u32, height: u32, fps: u32) -> EncodingSettings {
    if !is_auto(e) {
        return e.clone();
    }
    resolve(e, width, height, fps, Machine::here())
}

pub fn gpu_available() -> bool {
    #[cfg(windows)]
    return crate::record::nvenc::available();
    #[cfg(not(windows))]
    false
}

static CPU_BUSY: AtomicU8 = AtomicU8::new(0);

/// Measure overall CPU use every two seconds for the lifetime of the app (one cheap system
/// call each time).
pub fn start_cpu_sampler() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        use windows::Win32::{Foundation::FILETIME, System::Threading::GetSystemTimes};
        let read = || {
            let (mut idle, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
            let v = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
            // Kernel time includes idle time.
            Some((v(idle), v(kernel) + v(user)))
        };
        let mut last = read();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let now = read();
            if let (Some((i0, t0)), Some((i1, t1))) = (last, now) {
                let total = t1.saturating_sub(t0);
                if total > 0 {
                    let busy = 100 - (i1.saturating_sub(i0) * 100 / total).min(100);
                    CPU_BUSY.store(busy as u8, Ordering::Relaxed);
                }
            }
            last = now;
        }
    });
}

/// A caution for settings that pass preflight but are heavy for a typical CPU.
pub fn caution(
    resolved: &EncodingSettings,
    width: u32,
    height: u32,
    fps: u32,
) -> Option<&'static str> {
    let cpu = resolved.encoder.starts_with("lib");
    let rate = u64::from(width) * u64::from(height) * u64::from(fps);
    (cpu && rate >= P1440_60).then_some("heavy for a CPU, may drop frames")
}

/// One short line: "GPU · NVENC H.264 P5 · ≤17 Mbps" or "CPU · x264 fast · ≤4.4 Mbps".
pub fn describe(resolved: &EncodingSettings) -> String {
    let (hardware, encoder) = describe_parts(resolved);
    format!("{hardware} · {encoder}")
}

/// ("GPU" or "CPU", "NVENC H.264 P5").
pub fn describe_parts(resolved: &EncodingSettings) -> (&'static str, String) {
    let e = resolved.encoder.as_str();
    let hardware = if e.contains("nvenc") || e.contains("qsv") || e.contains("amf") {
        "GPU"
    } else {
        "CPU"
    };
    let name = match e {
        "h264_nvenc" => "NVENC H.264",
        "hevc_nvenc" => "NVENC HEVC",
        "av1_nvenc" => "NVENC AV1",
        "libx264" => "x264",
        "libx265" => "x265",
        "libsvtav1" => "SVT-AV1",
        "libaom-av1" => "libaom AV1",
        other => other,
    };
    let preset = if e.contains("nvenc") {
        resolved.preset.to_uppercase()
    } else {
        resolved.preset.clone()
    };
    let peak = resolved.max_bitrate_mbps;
    let peak = if peak <= 0.0 {
        String::new()
    } else if peak < 10.0 {
        format!(" · ≤{peak:.1} Mbps")
    } else {
        format!(" · ≤{peak:.0} Mbps")
    };
    (
        hardware,
        format!("{}{peak}", format!("{name} {preset}").trim_end()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(level: &str) -> EncodingSettings {
        settings(level_index(level), &EncodingSettings::default().ffmpeg_path)
    }

    const GPU: Machine = Machine {
        gpu: true,
        threads: 8,
        cpu_busy: 0,
    };
    const CPU: Machine = Machine {
        gpu: false,
        threads: 8,
        cpu_busy: 0,
    };

    #[test]
    fn gpu_machines_use_nvenc_at_any_size() {
        let r = resolve(&auto("medium"), 3840, 2160, 60, GPU);
        assert_eq!(
            (r.encoder.as_str(), r.preset.as_str(), r.crf),
            ("h264_nvenc", "p5", 30)
        );
        assert_eq!(describe(&r), "GPU · NVENC H.264 P5 · ≤17 Mbps");
    }

    #[test]
    fn cpu_presets_follow_the_selection_size() {
        let p = |w, h, fps, level| resolve(&auto(level), w, h, fps, CPU).preset;
        assert_eq!(p(1920, 1080, 30, "medium"), "medium");
        assert_eq!(p(1920, 1080, 60, "medium"), "fast");
        assert_eq!(p(1280, 720, 30, "high"), "slow");
        assert_eq!(p(2560, 1440, 60, "medium"), "veryfast");
        assert_eq!(p(3840, 2160, 60, "medium"), "superfast");
        assert_eq!(
            describe(&resolve(&auto("low"), 800, 600, 30, CPU)),
            "CPU · x264 faster · ≤1.0 Mbps"
        );
    }

    #[test]
    fn cpu_strength_and_load_shift_the_preset_within_limits() {
        let p = |m: Machine, w, h, fps| resolve(&auto("medium"), w, h, fps, m).preset;
        let big = Machine { threads: 24, ..CPU };
        let small = Machine { threads: 4, ..CPU };
        let busy = Machine {
            cpu_busy: 80,
            ..CPU
        };
        assert_eq!(p(big, 1920, 1080, 60), "medium");
        assert_eq!(p(small, 1920, 1080, 60), "faster");
        assert_eq!(p(busy, 1920, 1080, 60), "faster");
        assert_eq!(
            p(
                Machine {
                    cpu_busy: 80,
                    ..small
                },
                3840,
                2160,
                60
            ),
            "ultrafast"
        );
        // A strong CPU never goes past the preflight limit (veryfast at 4K60).
        assert_eq!(
            resolve(&auto("high"), 3840, 2160, 60, big).preset,
            "veryfast"
        );
    }

    #[test]
    fn peaks_scale_with_pixel_rate_and_keep_a_floor() {
        assert_eq!(peak_mbps(1, 3840, 2160, 60), 17.4);
        assert_eq!(peak_mbps(1, 1920, 1080, 60), 4.4);
        assert_eq!(peak_mbps(0, 3840, 2160, 60), 10.0);
        assert_eq!(peak_mbps(2, 3840, 2160, 60), 29.9);
        assert_eq!(peak_mbps(0, 640, 360, 30), 1.0, "floor");
    }

    #[test]
    fn every_automatic_choice_passes_preflight() {
        for level in LEVELS {
            for (w, h) in [
                (640, 360),
                (1920, 1080),
                (2560, 1440),
                (3840, 2160),
                (7680, 4320),
            ] {
                for fps in [24, 30, 60, 120] {
                    for threads in [2, 8, 32] {
                        for gpu in [false, true] {
                            let m = Machine {
                                gpu,
                                threads,
                                cpu_busy: if threads == 2 { 90 } else { 0 },
                            };
                            let r = resolve(&auto(level), w, h, fps, m);
                            super::super::preflight::recording(&r, w, h, fps)
                                .unwrap_or_else(|e| panic!("{level} {w}x{h}@{fps} {m:?}: {e}"));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "Probes NVENC and encodes with FFmpeg on this machine"]
    fn live_automatic_settings_pass_the_encoder_check() {
        for (width, height) in [(1920, 1080), (3840, 2160)] {
            let report = super::super::check::run(&auto("medium"), width, height, 60).unwrap();
            // The check encodes three seconds of busy generated motion.
            let mbps = report.bytes as f64 * 8.0 / 3.0 / 1_000_000.0;
            let peak = peak_mbps(1, width, height, 60);
            println!(
                "NVENC available: {}; {width}x{height}: {} at {:.0} fps, {mbps:.1} Mbps (peak {peak})",
                gpu_available(),
                report.plan,
                report.measured_fps
            );
            assert_eq!(report.plan.starts_with("GPU"), gpu_available());
            assert!(
                mbps <= peak * 1.25,
                "{mbps:.1} Mbps exceeds the {peak} Mbps peak"
            );
        }
        // The ceiling must bind: near-lossless quality limited to 1 Mbps.
        let mut capped = resolve(&auto("high"), 1920, 1080, 60, Machine::here());
        capped.crf = 18;
        capped.max_bitrate_mbps = 0.0;
        let free = super::super::check::run(&capped, 1920, 1080, 60).unwrap();
        capped.max_bitrate_mbps = 1.0;
        let limited = super::super::check::run(&capped, 1920, 1080, 60).unwrap();
        let (free, limited) = (
            free.bytes as f64 * 8.0 / 3e6,
            limited.bytes as f64 * 8.0 / 3e6,
        );
        println!("quality 18: {free:.1} Mbps uncapped, {limited:.1} Mbps with a 1 Mbps peak");
        assert!(free > 2.0 && limited <= 1.25, "{free:.1} / {limited:.1} Mbps");
    }

    #[test]
    fn fixed_settings_are_untouched_and_heavy_cpu_work_is_flagged() {
        let fixed = EncodingSettings::default();
        assert_eq!(resolve(&fixed, 3840, 2160, 60, GPU).encoder, fixed.encoder);
        let r = resolve(&auto("medium"), 3840, 2160, 60, CPU);
        assert!(caution(&r, 3840, 2160, 60).is_some());
        assert!(caution(&r, 1920, 1080, 60).is_none());
    }
}
