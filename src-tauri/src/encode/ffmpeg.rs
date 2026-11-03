use std::path::Path;
use std::process::Command;

use crate::error::{AppError, Result};

#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW: nada de consolas parpadeando

fn command() -> Command {
    let mut cmd = Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(NO_WINDOW);
    }
    cmd
}

/// FFmpeg nunca se empaqueta: solo se usa si el usuario ya lo tiene en el PATH.
pub fn available() -> bool {
    command()
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run(args: &[String]) -> Result<()> {
    let output = command()
        .args(args)
        .output()
        .map_err(|e| AppError::Msg(format!("Could not run FFmpeg: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
        return Err(AppError::Msg(format!("FFmpeg failed: {tail}")));
    }
    Ok(())
}

/// Ruta de maxima calidad: paleta generada por el propio contenido del clip.
pub fn gif_from_video(
    source: &Path,
    destination: &Path,
    fps: u32,
    width: u32,
    quality: u8,
) -> Result<()> {
    let colors = (32 + (quality.clamp(10, 100) as u32 * 224) / 100).min(256);
    let dither = if quality >= 60 {
        "sierra2_4a"
    } else {
        "bayer:bayer_scale=3"
    };
    let filter = format!(
        "fps={fps},scale={width}:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors={colors}:stats_mode=diff[p];[s1][p]paletteuse=dither={dither}:diff_mode=rectangle"
    );
    run(&[
        "-y".into(),
        "-i".into(),
        source.to_string_lossy().to_string(),
        "-filter_complex".into(),
        filter,
        "-loop".into(),
        "0".into(),
        destination.to_string_lossy().to_string(),
    ])
}

pub fn mp4_from_video(
    source: &Path,
    destination: &Path,
    fps: u32,
    width: u32,
    height: u32,
    quality: u8,
    options: &crate::settings::EncodingSettings,
) -> Result<()> {
    run(&mp4_args(
        source,
        destination,
        fps,
        width,
        height,
        quality,
        options,
    )?)
}

pub fn validate(options: &crate::settings::EncodingSettings) -> Result<()> {
    if !["native", "ffmpeg"].contains(&options.engine.as_str()) {
        return Err(AppError::Msg("Unknown encoding engine.".into()));
    }
    if ![
        "libx264",
        "libx265",
        "libsvtav1",
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

/// Construct an argument vector, never a shell command. Custom output arguments win.
fn mp4_args(
    source: &Path,
    destination: &Path,
    fps: u32,
    width: u32,
    height: u32,
    quality: u8,
    options: &crate::settings::EncodingSettings,
) -> Result<Vec<String>> {
    validate(options)?;
    let crf = 34 - (quality.clamp(10, 100) as u32 * 20) / 100;
    let filter = format!("fps={fps},scale={width}:{height}:flags=lanczos");
    let mut args = vec![
        "-y".into(),
        "-nostdin".into(),
        "-i".into(),
        source.to_string_lossy().to_string(),
        "-vf".into(),
        filter,
        "-c:v".into(),
        options.encoder.clone(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
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
    if let Some(bitrate) = options.bitrate() {
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
    let extra: Vec<String> = serde_json::from_str(&options.extra_args)
        .map_err(|_| AppError::Msg("Invalid FFmpeg arguments.".into()))?;
    args.extend(extra);
    args.push(destination.to_string_lossy().into_owned());
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::EncodingSettings;
    fn args(options: &EncodingSettings) -> Vec<String> {
        mp4_args(
            Path::new("C:/input with spaces.mp4"),
            Path::new("C:/output with spaces.mp4"),
            30,
            1920,
            1080,
            85,
            options,
        )
        .unwrap()
    }
    #[test]
    fn software_quality_and_audio_are_explicit() {
        let a = args(&EncodingSettings::default());
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(a.windows(2).any(|w| w == ["-crf", "17"]));
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
