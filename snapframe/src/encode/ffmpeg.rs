use std::process::Command;

use crate::error::{AppError, Result};

#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW: no flashing console windows

pub(crate) fn command(executable: &str) -> Command {
    let mut cmd = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(NO_WINDOW);
    }
    cmd
}

pub fn available_at(executable: &str) -> bool {
    command(executable)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(serde::Serialize)]
pub struct FfmpegInfo {
    pub available: bool,
    pub version: String,
    pub encoders: Vec<String>,
    pub error: Option<String>,
}
pub fn info(executable: &str) -> FfmpegInfo {
    let version = match command(executable).arg("-version").output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("FFmpeg")
            .to_string(),
        result => {
            return FfmpegInfo {
                available: false,
                version: String::new(),
                encoders: Vec::new(),
                error: Some(match result {
                    Err(error) => format!("Could not start FFmpeg: {error}"),
                    Ok(_) => {
                        "The selected executable did not report a valid FFmpeg version.".into()
                    }
                }),
            }
        }
    };
    let encoders = command(executable)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let flags = fields.next()?;
                    let encoder = fields.next()?;
                    (flags.starts_with('V') && encoder != "=").then(|| encoder.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    FfmpegInfo {
        available: true,
        version,
        encoders,
        error: None,
    }
}

pub fn test_encoding(
    options: &crate::settings::EncodingSettings,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<super::check::EncoderCheck> {
    super::check::run(options, width, height, fps)
}

pub fn encoder_presets(encoder: &str) -> &'static [&'static str] {
    if encoder == super::auto::AUTO {
        &super::auto::LEVELS
    } else if encoder.contains("nvenc") {
        &["p1", "p2", "p3", "p4", "p5", "p6", "p7"]
    } else if encoder.contains("qsv") {
        &[
            "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow",
        ]
    } else if encoder == "libsvtav1" {
        &["12", "10", "8", "6", "4"]
    } else if encoder == "libx264" || encoder == "libx265" {
        &[
            "ultrafast",
            "superfast",
            "veryfast",
            "faster",
            "fast",
            "medium",
            "slow",
            "slower",
            "veryslow",
        ]
    } else {
        &[]
    }
}

pub fn validate(options: &crate::settings::EncodingSettings) -> Result<()> {
    if !["crf", "bitrate"].contains(&options.rate_control.as_str()) || options.crf > 51 {
        return Err(AppError::Msg(
            "Choose constant quality (0-51) or target bitrate.".into(),
        ));
    }
    if options.ffmpeg_path.trim().is_empty() {
        return Err(AppError::Msg(
            "Enter an FFmpeg executable path or ffmpeg for PATH lookup.".into(),
        ));
    }
    if options.rate_control == "bitrate" && options.bitrate().is_none() {
        return Err(AppError::Msg(
            "Target bitrate must be greater than zero.".into(),
        ));
    }
    if super::auto::is_auto(options) {
        // Resolved per recording; the result is validated again then.
        return if super::auto::LEVELS.contains(&options.preset.as_str()) {
            Ok(())
        } else {
            Err(AppError::Msg(
                "Choose a quality: low, medium or high.".into(),
            ))
        };
    }
    if !["native", "ffmpeg"].contains(&options.engine.as_str()) {
        return Err(AppError::Msg("Unknown encoding engine.".into()));
    }
    if ![
        "libx264",
        "libx265",
        "libsvtav1",
        "libaom-av1",
        "h264_nvenc",
        "hevc_nvenc",
        "av1_nvenc",
        "h264_qsv",
        "hevc_qsv",
        "h264_amf",
        "hevc_amf",
    ]
    .contains(&options.encoder.as_str())
    {
        return Err(AppError::Msg("Unsupported video encoder.".into()));
    }
    if !options.max_bitrate_mbps.is_finite() || !(0.0..=200.0).contains(&options.max_bitrate_mbps) {
        return Err(AppError::Msg(
            "Peak bitrate must be between 0 and 200 Mbps.".into(),
        ));
    }
    if !options.bitrate_mbps.is_finite() || !(0.0..=200.0).contains(&options.bitrate_mbps) {
        return Err(AppError::Msg(
            "Bitrate must be between 0 and 200 Mbps.".into(),
        ));
    }
    if !options.gpu.is_empty() && options.gpu.parse::<u32>().is_err() {
        return Err(AppError::Msg(
            "GPU device index must be a non-negative integer.".into(),
        ));
    }
    if !options.preset.is_empty()
        && !encoder_presets(&options.encoder).contains(&options.preset.as_str())
    {
        return Err(AppError::Msg(format!(
            "Preset {:?} is not valid for {}.",
            options.preset, options.encoder
        )));
    }
    super::preflight::extra_args(options)?;
    Ok(())
}

pub(crate) fn codec_args(options: &crate::settings::EncodingSettings) -> Result<Vec<String>> {
    validate(options)?;
    let crf = options.crf;
    let mut args = vec![
        "-c:v".into(),
        options.encoder.clone(),
        "-pix_fmt".into(),
        if options.encoder == "libx264"
            || options.encoder.ends_with("_nvenc")
            || options.encoder.ends_with("_qsv")
            || options.encoder.ends_with("_amf")
        {
            "nv12"
        } else {
            "yuv420p"
        }
        .into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
    ];
    if !options.preset.is_empty() {
        args.extend(["-preset".into(), options.preset.clone()]);
    }
    if options.encoder.ends_with("_nvenc") && !options.gpu.is_empty() {
        args.extend(["-gpu".into(), options.gpu.clone()]);
    }
    if let Some(bitrate) = (options.rate_control == "bitrate")
        .then(|| options.bitrate())
        .flatten()
    {
        args.extend(["-b:v".into(), bitrate.to_string()]);
    } else if options.encoder.ends_with("_nvenc") {
        args.extend([
            "-rc".into(),
            "vbr".into(),
            "-cq".into(),
            crf.to_string(),
            "-b:v".into(),
            "0".into(),
        ]);
    } else if options.encoder.ends_with("_qsv") {
        args.extend(["-global_quality".into(), crf.to_string()]);
    } else if options.encoder.ends_with("_amf") {
        args.extend([
            "-rc".into(),
            "cqp".into(),
            "-qp_i".into(),
            crf.to_string(),
            "-qp_p".into(),
            crf.to_string(),
        ]);
    } else {
        args.extend(["-crf".into(), crf.to_string()]);
    }
    // Capped constant quality: a peak rate with a two-second buffer.
    if options.rate_control != "bitrate"
        && options.max_bitrate_mbps > 0.0
        && (options.encoder.starts_with("lib") || options.encoder.ends_with("_nvenc"))
    {
        let peak = (options.max_bitrate_mbps * 1_000_000.0) as u64;
        args.extend([
            "-maxrate".into(),
            peak.to_string(),
            "-bufsize".into(),
            (peak * 2).to_string(),
        ]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::EncodingSettings;
    #[test]
    #[ignore = "Requires an installed FFmpeg executable"]
    fn live_manual_encoder_check_success_and_failure() {
        let mut options = EncodingSettings::default();
        let report = test_encoding(&options, 640, 360, 120).unwrap();
        assert_eq!(report.fps, 120);
        assert!(report.bytes > 1000);
        options.extra_args = r#"["-preset", "not-a-preset"]"#.into();
        assert!(test_encoding(&options, 640, 360, 60)
            .unwrap_err()
            .to_string()
            .contains("not valid"));
    }
    fn args(options: &EncodingSettings) -> Vec<String> {
        crate::record::stream_encoder::arguments(
            options,
            1920,
            1080,
            30,
            Some(crate::record::stream_encoder::AudioFormat {
                channels: 2,
                sample_rate: 48000,
            }),
            "tcp://127.0.0.1:1",
            Some("tcp://127.0.0.1:2"),
            std::path::Path::new("C:/output with spaces.mp4"),
        )
        .unwrap()
    }
    #[test]
    fn constant_quality_can_have_a_peak_bitrate() {
        let mut o = crate::settings::EncodingSettings {
            max_bitrate_mbps: 4.4,
            ..Default::default()
        };
        let args = codec_args(&o).unwrap();
        let at = args.iter().position(|a| a == "-maxrate").unwrap();
        assert_eq!(args[at + 1], "4400000");
        assert_eq!(args[at + 3], "8800000", "two-second buffer");
        o.max_bitrate_mbps = 0.0;
        assert!(!codec_args(&o).unwrap().contains(&"-maxrate".to_string()));
        o.max_bitrate_mbps = 500.0;
        assert!(validate(&o).is_err());
    }
    #[test]
    fn software_quality_and_audio_are_explicit() {
        let a = args(&EncodingSettings::default());
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(a.windows(2).any(|w| w == ["-crf", "20"]));
        assert!(a.windows(2).any(|w| w == ["-pix_fmt", "nv12"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert_eq!(a.last().unwrap(), "C:/output with spaces.mp4");
    }
    #[test]
    fn hardware_bitrate_device_and_custom_args_are_preserved() {
        let o = EncodingSettings {
            encoder: "hevc_nvenc".into(),
            preset: "p4".into(),
            gpu: "1".into(),
            bitrate_mbps: 12.5,
            rate_control: "bitrate".into(),
            extra_args: r#"["-metadata", "title=My recording", "-g", "120"]"#.into(),
            ..EncodingSettings::default()
        };
        let a = args(&o);
        assert!(a.windows(2).any(|w| w == ["-gpu", "1"]));
        assert!(a.windows(2).any(|w| w == ["-pix_fmt", "nv12"]));
        assert!(a.windows(2).any(|w| w == ["-b:v", "12500000"]));
        assert!(a.contains(&"title=My recording".into()));
        assert!(!a.contains(&"-crf".into()));
    }
    #[test]
    fn rejects_invalid_preferences() {
        assert!(validate(&EncodingSettings {
            extra_args: "ffmpeg -i input".into(),
            ..EncodingSettings::default()
        })
        .is_err());
        assert!(validate(&EncodingSettings {
            bitrate_mbps: -1.0,
            ..EncodingSettings::default()
        })
        .is_err());
        assert!(validate(&EncodingSettings {
            gpu: "abc".into(),
            ..EncodingSettings::default()
        })
        .is_err());
    }
}
