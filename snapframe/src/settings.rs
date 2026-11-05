use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    /// An FFmpeg encoder name, or `auto` (see `encode/auto.rs`).
    pub encoder: String,
    pub bitrate_mbps: f64,
    /// Peak bitrate for constant-quality encoding (0 = none), so busy video cannot balloon
    /// the file while static desktop content stays small.
    pub max_bitrate_mbps: f64,
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
            bitrate_mbps: 0.0,
            max_bitrate_mbps: 0.0,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Normally automatic (see `encode/auto.rs`); a fixed encoder can be set by editing
    /// settings.json.
    pub encoding: EncodingSettings,
    pub capture_shortcut: String,
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
            // Automatic encoding, Medium quality (see encode/auto.rs).
            encoding: crate::encode::auto::settings(1, &EncodingSettings::default().ffmpeg_path),
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
            fps: 60,
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

/// Stable settings folder, shared across app versions.
pub fn config_dir() -> PathBuf {
    // Tests must never touch the user's real settings.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("snapframe-test-config-{}", std::process::id()));
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("com.bonejay.winshotx")
}

fn config_path() -> Result<PathBuf> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

pub fn load() -> Settings {
    let Ok(path) = config_path() else {
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

pub fn save(settings: &Settings) -> Result<()> {
    let path = config_path()?;
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod english_edition_tests {
    use super::*;
    #[test]
    fn new_installs_record_automatically() {
        let settings = Settings::default();
        assert!(crate::encode::auto::is_auto(&settings.encoding));
        assert_eq!(settings.encoding.preset, "medium");
        crate::encode::ffmpeg::validate(&settings.encoding).unwrap();
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
