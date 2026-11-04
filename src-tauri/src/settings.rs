use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::error::Result;

/// Application color theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum Theme {
    /// Follow the Windows setting, live.
    #[default]
    Sistema,
    Claro,
    Oscuro,
}

/// Capture preview quality and final video encoding preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EncodingSettings {
    pub engine: String,
    pub ffmpeg_path: String,
    pub rate_control: String,
    pub crf: u8,
    pub encoder: String,
    pub quality: u8,
    pub bitrate_mbps: f64,
    pub preset: String,
    pub gpu: String,
    pub extra_args: String,
}
fn default_ffmpeg_path() -> String {
    if let Some(program_files) = std::env::var_os("ProgramFiles(x86)") {
        let svp = PathBuf::from(program_files)
            .join("SVP 4")
            .join("utils")
            .join("ffmpeg.exe");
        if svp.is_file() {
            return svp.to_string_lossy().into_owned();
        }
    }
    "ffmpeg".into()
}

impl Default for EncodingSettings {
    fn default() -> Self {
        Self {
            engine: "ffmpeg".into(),
            ffmpeg_path: default_ffmpeg_path(),
            rate_control: "crf".into(),
            crf: 20,
            encoder: "libx264".into(),
            quality: 85,
            bitrate_mbps: 0.0,
            preset: "veryfast".into(),
            gpu: String::new(),
            extra_args: "[]".into(),
        }
    }
}
impl EncodingSettings {
    pub fn bitrate(&self) -> Option<u32> {
        (self.bitrate_mbps.is_finite() && self.bitrate_mbps > 0.0)
            .then(|| (self.bitrate_mbps.clamp(0.1, 200.0) * 1_000_000.0) as u32)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureTarget {
    #[default]
    Rectangle,
    Window,
    Screen,
}

/// A named recording setup: frame rate plus encoding. Selecting one copies it into
/// `Settings::fps` and `Settings::encoding`, which are what recordings actually use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingPreset {
    pub name: String,
    pub fps: u32,
    pub encoding: EncodingSettings,
}

impl Default for RecordingPreset {
    fn default() -> Self {
        Self {
            name: "Preset".into(),
            fps: 60,
            encoding: EncodingSettings::default(),
        }
    }
}

fn preset(name: &str, fps: u32, encoder: &str, preset: &str, crf: u8) -> RecordingPreset {
    RecordingPreset {
        name: name.into(),
        fps,
        encoding: EncodingSettings {
            encoder: encoder.into(),
            preset: preset.into(),
            crf,
            ..EncodingSettings::default()
        },
    }
}

/// Built-in starting points; the user can change, add, or delete presets.
pub fn default_presets() -> Vec<RecordingPreset> {
    vec![
        preset("Smooth 60 fps", 60, "libx264", "veryfast", 20),
        preset("Light 30 fps", 30, "libx264", "veryfast", 24),
        preset("NVIDIA 60 fps", 60, "h264_nvenc", "p4", 21),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Saved recording presets, shown in the settings and in the capture bar.
    pub presets: Vec<RecordingPreset>,
    /// Name of the preset `fps` and `encoding` were last taken from.
    pub active_preset: String,
    pub encoding: EncodingSettings,
    pub capture_shortcut: String,
    pub capture_target: CaptureTarget,
    pub record_target: CaptureTarget,
    pub record_shortcut: String,
    /// Where screenshots are saved (when saved instead of only copied).
    pub save_directory: String,
    /// Where recordings are saved.
    pub video_directory: String,
    /// Light, dark, or follow Windows.
    pub theme: Theme,
    /// Also save screenshots to `save_directory` (they are always copied to the clipboard).
    pub save_screenshots: bool,
    pub capture_cursor: bool,
    pub record_audio: bool,
    pub system_audio_device: Option<String>,
    pub microphone_device: Option<String>,
    /// Also record the microphone, mixed with system audio when both are on.
    #[serde(default)]
    pub record_microphone: bool,
    pub fps: u32,
    pub play_sound: bool,
    pub show_magnifier: bool,
    pub start_with_windows: bool,
    /// Seconds to wait before freezing the screen (0 = none), so open menus can be captured.
    pub capture_delay_seconds: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            encoding: EncodingSettings::default(),
            presets: default_presets(),
            active_preset: String::new(),
            capture_target: CaptureTarget::Rectangle,
            record_target: CaptureTarget::Rectangle,
            capture_shortcut: "PrintScreen".into(),
            record_shortcut: "Shift+PrintScreen".into(),
            save_directory: DEFAULT_SCREENSHOT_DIR.into(),
            video_directory: DEFAULT_VIDEO_DIR.into(),
            theme: Theme::Sistema,
            save_screenshots: false,
            capture_cursor: true,
            record_audio: false,
            system_audio_device: None,
            microphone_device: None,
            record_microphone: false,
            fps: 30,
            play_sound: false,
            show_magnifier: true,
            start_with_windows: false,
            capture_delay_seconds: 0,
        }
    }
}




const DEFAULT_SCREENSHOT_DIR: &str = r"F:\Pictures\Screenshots";
const DEFAULT_VIDEO_DIR: &str = r"F:\Videos\Captures";

/// Replace empty or relative folders (older builds could save `.`) with the defaults.
fn fix_directories(settings: &mut Settings) {
    let usable = |dir: &str| !dir.trim().is_empty() && std::path::Path::new(dir).is_absolute();
    if !usable(&settings.save_directory) {
        settings.save_directory = DEFAULT_SCREENSHOT_DIR.into();
    }
    if !usable(&settings.video_directory) {
        settings.video_directory = DEFAULT_VIDEO_DIR.into();
    }
}

fn config_path(app: &AppHandle) -> Result<PathBuf> {
    let dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    let Ok(path) = config_path(app) else {
        return Settings::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    // Notepad and PowerShell write UTF-8 with a BOM, which serde rejects.
    let raw = raw.trim_start_matches('\u{feff}');

    let Ok(mut settings) = serde_json::from_str::<Settings>(raw) else {
        // Keep unreadable settings aside instead of overwriting them with defaults.
        let _ = std::fs::rename(&path, path.with_extension("json.roto"));
        return Settings::default();
    };

    settings.encoding = crate::record::stream_encoder::streaming_options(&settings.encoding);
    fix_directories(&mut settings);
    settings
}


pub fn fix_settings_directories(settings: &mut Settings) {
    fix_directories(settings);
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<()> {
    let path = config_path(app)?;
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod english_edition_tests {
    use super::*;
    #[test]
    fn default_presets_are_valid_and_load_into_older_settings() {
        let settings: Settings = serde_json::from_value(serde_json::json!({ "fps": 30 })).unwrap();
        assert_eq!(settings.presets.len(), default_presets().len());
        for preset in &settings.presets {
            crate::encode::ffmpeg::validate(&preset.encoding).unwrap();
        }
    }
    #[test]
    fn settings_from_older_builds_still_load() {
        let raw = serde_json::json!({ "language": "es", "replayEnabled": true, "onboarded": true, "fps": 60, "saveDirectory": "." });
        let mut settings: Settings = serde_json::from_value(raw).unwrap();
        fix_directories(&mut settings);
        assert_eq!(settings.fps, 60);
        assert_eq!(settings.save_directory, DEFAULT_SCREENSHOT_DIR);
        assert_eq!(settings.video_directory, DEFAULT_VIDEO_DIR);
    }
    #[test]
    fn encoding_preferences_round_trip() {
        let mut settings = Settings::default();
        settings.encoding.encoder = "hevc_nvenc".into();
        settings.encoding.bitrate_mbps = 12.5;
        settings.encoding.gpu = "1".into();
        settings.encoding.extra_args = r#"["-g", "120"]"#.into();
        settings.encoding.ffmpeg_path = r"C:\Program Files (x86)\SVP 4\utils\ffmpeg.exe".into();
        settings.encoding.crf = 18;
        settings.system_audio_device = Some("speaker-endpoint".into());
        settings.microphone_device = Some("microphone-endpoint".into());
        let roundtrip: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(roundtrip.encoding.encoder, "hevc_nvenc");
        assert_eq!(roundtrip.encoding.bitrate(), Some(12_500_000));
        assert_eq!(roundtrip.encoding.gpu, "1");
        assert_eq!(roundtrip.encoding.extra_args, r#"["-g", "120"]"#);
        assert_eq!(
            roundtrip.encoding.ffmpeg_path,
            settings.encoding.ffmpeg_path
        );
        assert_eq!(roundtrip.encoding.crf, 18);
        assert_eq!(roundtrip.system_audio_device, settings.system_audio_device);
        assert_eq!(roundtrip.microphone_device, settings.microphone_device);
    }
}
