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

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderCheck {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

/// Encode a short generated picture and tone. Never accesses capture devices.
pub fn test_encoding(
    options: &crate::settings::EncodingSettings,
    fps: u32,
) -> Result<EncoderCheck> {
    use crate::record::stream_encoder::{AudioFormat, Encoder, OutputPaths};
    use std::time::{Duration, Instant};
    validate(options)?;
    if !(5..=120).contains(&fps) {
        return Err(AppError::Msg(
            "Frame rate must be between 5 and 120 fps.".into(),
        ));
    }
    let dir = std::env::temp_dir().join(format!("winshotx-encoder-test-{}", uuid::Uuid::new_v4()));
    let started = Instant::now();
    let result = (|| {
        let paths = OutputPaths::reserve(&dir)?;
        let mut encoder = Encoder::start(
            paths,
            options,
            1280,
            720,
            fps,
            Some(AudioFormat {
                channels: 2,
                sample_rate: 48000,
            }),
        )?;
        let mut bgra = vec![0u8; 1280 * 720 * 4];
        for (i, pixel) in bgra.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[(i % 256) as u8, (i / 1280 % 256) as u8, 100, 255]);
        }
        let tone = |start: u32, end: u32| -> Vec<u8> {
            let mut data = Vec::with_capacity((end - start) as usize * 4);
            for t in start..end {
                let value =
                    ((t as f64 * 440.0 * std::f64::consts::TAU / 48000.0).sin() * 5000.0) as i16;
                data.extend_from_slice(&value.to_le_bytes());
                data.extend_from_slice(&value.to_le_bytes());
            }
            data
        };
        let count = fps.div_ceil(2);
        encoder.audio(tone(0, count * 48000 / fps))?;
        encoder.end_audio();
        for i in 0..count {
            if started.elapsed() >= Duration::from_secs(20) {
                return Err(AppError::Msg(
                    "The encoder test exceeded 20 seconds. Try a faster preset or encoder.".into(),
                ));
            }
            bgra[0] = i as u8;
            encoder.video(&bgra)?;
        }
        let path = encoder.finish_by(started + Duration::from_secs(20))?;
        Ok(EncoderCheck {
            width: 1280,
            height: 720,
            fps,
            bytes: std::fs::metadata(path)?.len(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result.map_err(|e: AppError| AppError::Msg(format!("Encoder test failed: {e}")))
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
    let _: Vec<String> = serde_json::from_str(&options.extra_args)
        .map_err(|_| AppError::Msg("FFmpeg arguments must be a JSON array of strings.".into()))?;
    Ok(())
}

pub(crate) fn codec_args(options: &crate::settings::EncodingSettings) -> Result<Vec<String>> {
    validate(options)?;
    let crf = options.crf;
    let mut args = vec![
        "-c:v".into(),
        options.encoder.clone(),
        "-pix_fmt".into(),
        "yuv420p".into(),
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
        let report = test_encoding(&options, 120).unwrap();
        assert_eq!(report.fps, 120);
        assert!(report.bytes > 1000);
        options.extra_args = r#"["-preset", "not-a-preset"]"#.into();
        assert!(test_encoding(&options, 60)
            .unwrap_err()
            .to_string()
            .contains("Encoder test failed"));
    }
    fn args(options: &EncodingSettings) -> Vec<String> {
        crate::record::stream_encoder::arguments(
            options,
            1920,
            1080,
            30,
            Some(crate::record::stream_encoder::AudioFormat { channels: 2, sample_rate: 48000 }),
            "tcp://127.0.0.1:1",
            Some("tcp://127.0.0.1:2"),
            std::path::Path::new("C:/output with spaces.mp4"),
        )
        .unwrap()
    }
    #[test]
    fn software_quality_and_audio_are_explicit() {
        let a = args(&EncodingSettings::default());
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(a.windows(2).any(|w| w == ["-crf", "20"]));
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
