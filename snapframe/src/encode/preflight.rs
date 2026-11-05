//! Cheap recording checks, shared by recording, previews and both test buttons.
//! Performance limits are conservative policy, not universal hardware predictions.
use crate::error::{AppError, Result};
use crate::settings::EncodingSettings;

pub fn extra_args(options: &EncodingSettings) -> Result<Vec<String>> {
    let args: Vec<String> = serde_json::from_str(&options.extra_args)
        .map_err(|_| AppError::Msg("FFmpeg arguments must be a JSON array of strings.".into()))?;
    let mut index = 0;
    while index < args.len() {
        let option = &args[index];
        let base = option.split(':').next().unwrap_or(option);
        if !option.starts_with('-') || option == "-" || option.contains('=') {
            return Err(AppError::Msg(format!("Expected a video option, found {option:?}. Output files and shell commands do not belong in extra arguments.")));
        }
        if matches!(
            base,
            "-i" | "-f"
                | "-map"
                | "-map_metadata"
                | "-map_chapters"
                | "-c"
                | "-codec"
                | "-r"
                | "-framerate"
                | "-fps_mode"
                | "-vsync"
                | "-enc_time_base"
                | "-time_base"
                | "-s"
                | "-video_size"
                | "-pix_fmt"
                | "-vf"
                | "-af"
                | "-filter"
                | "-filter_complex"
                | "-filter_script"
                | "-filter_complex_script"
                | "-an"
                | "-vn"
                | "-sn"
                | "-dn"
                | "-ar"
                | "-ac"
                | "-t"
                | "-to"
                | "-ss"
                | "-sseof"
                | "-fs"
                | "-shortest"
                | "-movflags"
                | "-frag_duration"
                | "-frag_size"
                | "-min_frag_duration"
                | "-flush_packets"
                | "-max_interleave_delta"
                | "-progress"
                | "-stats_period"
                | "-stdin"
                | "-nostdin"
                | "-y"
                | "-n"
                | "-report"
                | "-attach"
                | "-dump_attachment"
                | "-pass"
                | "-passlogfile"
        ) || option.ends_with(":a")
            || option.contains(":a:")
        {
            return Err(AppError::Msg(format!("{option} is managed by Snapframe. Choose the encoder, frame rate, selection and audio in the app; extra arguments are for video encoding options.")));
        }
        let value = args
            .get(index + 1)
            .filter(|v| !v.starts_with('-') || v.parse::<f64>().is_ok())
            .ok_or_else(|| AppError::Msg(format!("{option} needs a value in the JSON array.")))?;
        match base {
            "-preset"
                if !super::ffmpeg::encoder_presets(&options.encoder).contains(&value.as_str()) =>
            {
                return Err(AppError::Msg(format!(
                    "Preset {value:?} is not valid for {}.",
                    options.encoder
                )))
            }
            "-crf" | "-cq" | "-qp" | "-global_quality"
                if value
                    .parse::<f64>()
                    .map_or(true, |q| !q.is_finite() || !(0.0..=51.0).contains(&q)) =>
            {
                return Err(AppError::Msg(format!("{option} must be between 0 and 51.")))
            }
            "-bf" | "-threads" | "-g" => {
                let max = if base == "-bf" {
                    16
                } else if base == "-threads" {
                    256
                } else {
                    1_000_000
                };
                if value
                    .parse::<u32>()
                    .map_or(true, |n| n > max || (base == "-g" && n == 0))
                {
                    return Err(AppError::Msg(format!(
                        "{option} needs an integer in {}..={max}.",
                        u32::from(base == "-g")
                    )));
                }
            }
            _ => {}
        }
        index += 2;
    }
    Ok(args)
}

const P1080_30: u64 = 1920 * 1080 * 30;
pub(crate) const P1440_60: u64 = 2560 * 1440 * 60;
const P2160_60: u64 = 3840 * 2160 * 60;

/// Pixel-rate tier: 0 up to 1080p30, 1 below 1440p60, 2 below 4K60, 3 from 4K60.
pub(crate) fn rate_tier(rate: u64) -> usize {
    match rate {
        r if r >= P2160_60 => 3,
        r if r >= P1440_60 => 2,
        r if r > P1080_30 => 1,
        _ => 0,
    }
}

/// x264/x265 speed presets, fastest first; FFmpeg's default for both is medium.
pub(crate) const X26X: [&str; 9] = [
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
];

/// Slowest x264 preset (index into [`X26X`]) accepted per [`rate_tier`]:
/// veryslow, slow, medium, veryfast. 4K60 x264 needs a many-core CPU even then.
pub(crate) const X264_SLOWEST: [usize; 4] = [8, 6, 5, 2];

/// For a CPU encoder too slow at this pixel rate, what to choose instead.
/// Conservative policy for a typical desktop CPU: settings that pass can still fail
/// the encoder test, but rejected ones are not realistic live on any common machine.
fn cpu_limit(encoder: &str, preset: &str, rate: u64) -> Option<String> {
    const SVT: [&str; 5] = ["12", "10", "8", "6", "4"];
    let (ladder, default, slowest) = match encoder {
        // Slowest accepted per tier (up to 1080p30, below 1440p60, below 4K60, 4K60+).
        "libx264" => (&X26X[..], "medium", X264_SLOWEST),
        // x265 needs roughly two presets faster than x264 for the same throughput.
        "libx265" => (&X26X[..], "medium", [5, 4, 3, 0]),
        "libsvtav1" => (&SVT[..], "10", [4, 3, 2, 0]),
        // libaom is not a live encoder above small sizes; there is no preset ladder.
        "libaom-av1" => return (rate >= P1080_30).then(|| "Use libsvtav1".into()),
        _ => return None,
    };
    let allowed = slowest[rate_tier(rate)];
    let preset = if preset.is_empty() { default } else { preset };
    let rank = ladder.iter().position(|p| *p == preset)?;
    (rank > allowed).then(|| format!("Use preset {} or faster", ladder[allowed]))
}

pub fn recording(options: &EncodingSettings, width: u32, height: u32, fps: u32) -> Result<()> {
    super::ffmpeg::validate(options)?;
    if width < 2
        || height < 2
        || width > 16_384
        || height > 16_384
        || width % 2 != 0
        || height % 2 != 0
    {
        return Err(AppError::Msg(
            "Choose an even recording size between 2 and 16384 pixels per side.".into(),
        ));
    }
    if !(5..=120).contains(&fps) {
        return Err(AppError::Msg(
            "Frame rate must be between 5 and 120 fps.".into(),
        ));
    }
    let args = extra_args(options)?;
    let mut speed = options.preset.as_str();
    for pair in args.chunks_exact(2) {
        if pair[0].split(':').next() == Some("-preset") {
            speed = &pair[1];
        }
    }
    let rate = u64::from(width) * u64::from(height) * u64::from(fps);
    if let Some(instead) = cpu_limit(&options.encoder, speed, rate) {
        let speed = if speed.is_empty() { "default" } else { speed };
        return Err(AppError::Msg(format!(
            "{width}×{height} at {fps} fps is too much for CPU {} preset {speed} to encode live. \
             {instead}, an NVIDIA (NVENC) profile, a smaller selection or a lower frame rate, \
             then run Test encoder at that size.",
            options.encoder
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heavy_cpu_settings_are_rejected_including_preset_overrides() {
        let mut e = EncodingSettings {
            preset: "medium".into(),
            ..Default::default()
        };
        assert!(recording(&e, 3840, 2160, 60).is_err());
        assert!(recording(&e, 2160, 3840, 120).is_err());
        assert!(recording(&e, 1920, 1080, 60).is_ok());
        e.preset = "fast".into();
        assert!(recording(&e, 3840, 2160, 60).is_err());
        e.preset = "faster".into();
        assert!(recording(&e, 3840, 2160, 60).is_err());
        e.preset = "slow".into();
        assert!(recording(&e, 2560, 1440, 60).is_err());
        assert!(recording(&e, 1920, 1080, 60).is_ok());
        e.preset = "veryfast".into();
        assert!(recording(&e, 3840, 2160, 60).is_ok());
        e.extra_args = r#"["-preset:v","slow"]"#.into();
        assert!(recording(&e, 3840, 2160, 60).is_err());
        e.extra_args = "[]".into();
        e.encoder = "libx265".into();
        e.preset = String::new();
        assert!(recording(&e, 1920, 1080, 60).is_err());
        assert!(recording(&e, 1920, 1080, 30).is_ok());
        e.encoder = "libaom-av1".into();
        assert!(recording(&e, 1920, 1080, 60).is_err());
        e.encoder = "h264_nvenc".into();
        e.preset = "p5".into();
        assert!(recording(&e, 3840, 2160, 120).is_ok());
    }
    #[test]
    fn managed_inputs_output_timing_and_audio_cannot_be_overridden() {
        for args in [
            r#"["-i","other.mp4"]"#,
            r#"["-vf","fps=120"]"#,
            r#"["-r:v","120"]"#,
            r#"["-movflags","faststart"]"#,
            r#"["-c:v","libx264"]"#,
            r#"["-b:a","64k"]"#,
            r#"["-t","5"]"#,
            r#"["output.mp4"]"#,
            r#"["-crf"]"#,
            r#"["-crf","99"]"#,
        ] {
            assert!(
                extra_args(&EncodingSettings {
                    extra_args: args.into(),
                    ..Default::default()
                })
                .is_err(),
                "{args}"
            );
        }
        let e = EncodingSettings {
            extra_args: r#"["-crf","18","-tune","animation","-bf","2","-threads","0"]"#.into(),
            ..Default::default()
        };
        assert!(recording(&e, 1920, 1080, 60).is_ok());
    }
}
