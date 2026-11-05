//! The settings window. `form` converts between `Settings` and the flat `SettingsData` the UI
//! edits (pure, unit tested); `SettingsCtl` owns the window and its background checks.

use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::format;
use crate::audio_devices::{AudioDevice, Level};
use crate::settings::{Settings, Theme as ThemeSetting};
use crate::state::ShortcutStatus;
use crate::{AudioMeter, SettingsData, SettingsWindow};

pub const DELAYS: [u32; 5] = [0, 2, 3, 5, 10];

/// Lists the form needs to map indices to values.
#[derive(Debug, Default, Clone)]
pub struct Lists {
    /// Device ids per option; `None` is "Windows default".
    pub system_ids: Vec<Option<String>>,
    pub system_names: Vec<String>,
    pub microphone_ids: Vec<Option<String>>,
    pub microphone_names: Vec<String>,
}

pub mod form {
    use super::*;

    fn device_options(
        devices: &[AudioDevice],
        kind: &str,
        saved: &Option<String>,
    ) -> (Vec<Option<String>>, Vec<String>) {
        let mut ids = vec![None];
        let mut names = vec!["Windows default".to_string()];
        let of_kind: Vec<_> = devices.iter().filter(|d| d.kind == kind).collect();
        if let Some(id) = saved {
            if !of_kind.iter().any(|d| &d.id == id) {
                ids.push(Some(id.clone()));
                names.push("Unavailable saved device".into());
            }
        }
        for d in of_kind {
            ids.push(Some(d.id.clone()));
            names.push(if d.is_default {
                format!("{} (default)", d.name)
            } else {
                d.name.clone()
            });
        }
        (ids, names)
    }

    /// Options for the current settings and devices.
    pub fn lists(s: &Settings, devices: &[AudioDevice]) -> Lists {
        let (system_ids, system_names) = device_options(devices, "output", &s.system_audio_device);
        let (microphone_ids, microphone_names) =
            device_options(devices, "input", &s.microphone_device);
        Lists {
            system_ids,
            system_names,
            microphone_ids,
            microphone_names,
        }
    }

    fn index_of<T: PartialEq>(list: &[T], value: &T) -> i32 {
        list.iter().position(|v| v == value).map_or(0, |i| i as i32)
    }

    fn nearest(list: &[u32], value: u32) -> i32 {
        list.iter()
            .enumerate()
            .min_by_key(|(_, v)| v.abs_diff(value))
            .map_or(0, |(i, _)| i as i32)
    }

    pub fn to_data(s: &Settings, l: &Lists) -> SettingsData {
        SettingsData {
            capture_shortcut: s.capture_shortcut.clone().into(),
            record_shortcut: s.record_shortcut.clone().into(),
            save_screenshots: s.save_screenshots,
            delay_index: nearest(&DELAYS, s.capture_delay_seconds),
            capture_cursor: s.capture_cursor,
            show_magnifier: s.show_magnifier,
            record_audio: s.record_audio,
            record_microphone: s.record_microphone,
            system_device_index: index_of(&l.system_ids, &s.system_audio_device),
            microphone_device_index: index_of(&l.microphone_ids, &s.microphone_device),
            ffmpeg_path: s.encoding.ffmpeg_path.clone().into(),
            save_directory: s.save_directory.clone().into(),
            video_directory: s.video_directory.clone().into(),
            theme: match s.theme {
                ThemeSetting::Sistema => 0,
                ThemeSetting::Oscuro => 1,
                ThemeSetting::Claro => 2,
            },
            start_with_windows: s.start_with_windows,
            play_sound: s.play_sound,
        }
    }

    fn pick<T: Clone>(list: &[T], i: i32, fallback: T) -> T {
        usize::try_from(i)
            .ok()
            .and_then(|i| list.get(i))
            .cloned()
            .unwrap_or(fallback)
    }

    /// Apply the form to the previous settings. Frame rate and encoding come from the
    /// recording toolbar and are kept.
    pub fn from_data(prev: &Settings, d: &SettingsData, l: &Lists) -> Settings {
        let mut s = prev.clone();
        s.capture_shortcut = d.capture_shortcut.to_string();
        s.record_shortcut = d.record_shortcut.to_string();
        s.save_screenshots = d.save_screenshots;
        s.capture_delay_seconds = pick(&DELAYS, d.delay_index, 0);
        s.capture_cursor = d.capture_cursor;
        s.show_magnifier = d.show_magnifier;
        s.record_audio = d.record_audio;
        s.record_microphone = d.record_microphone;
        s.system_audio_device = pick(&l.system_ids, d.system_device_index, None);
        s.microphone_device = pick(&l.microphone_ids, d.microphone_device_index, None);
        let path = d.ffmpeg_path.trim();
        s.encoding.ffmpeg_path = if path.is_empty() {
            "ffmpeg".into()
        } else {
            path.into()
        };
        s.save_directory = d.save_directory.to_string();
        s.video_directory = d.video_directory.to_string();
        s.theme = match d.theme {
            1 => ThemeSetting::Oscuro,
            2 => ThemeSetting::Claro,
            _ => ThemeSetting::Sistema,
        };
        s.start_with_windows = d.start_with_windows;
        s.play_sound = d.play_sound;
        s
    }

    /// The level meter for one source, as the settings page shows it.
    pub fn meter(level: Option<&Level>, checking: bool, enabled: bool) -> AudioMeter {
        let mut m = AudioMeter {
            db: -60.0,
            ..AudioMeter::default()
        };
        if !enabled {
            m.caption = "Not enabled".into();
        } else if let Some(error) = level.and_then(|l| l.error.clone()) {
            m.caption = error.into();
            m.error = true;
        } else if !checking {
            m.caption = "Use “Check levels” before recording".into();
        } else if level.is_none_or(|l| l.state == "connecting") {
            m.caption = "Connecting…".into();
        } else if let Some(l) = level {
            m.show_bar = true;
            m.db = l.db.clamp(-60.0, 0.0);
            m.clipping = l.peak >= 0.98;
            m.value = if l.peak > 0.0 {
                format!("{:.1} dBFS", l.db)
            } else {
                "−∞ dBFS".into()
            }
            .into();
            m.caption = if l.peak >= 0.98 {
                "Near clipping"
            } else if l.peak > 0.001 {
                "Signal detected"
            } else if l.peak > 0.0 {
                "Very quiet"
            } else {
                "No signal"
            }
            .into();
        }
        m
    }
}

/// Whether Windows apps use the dark theme ("Follow Windows").
pub fn windows_prefers_dark() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let mut value = 1u32;
        let mut size = 4u32;
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
        .is_ok();
        return ok && value == 0;
    }
    #[allow(unreachable_code)]
    true
}

fn strings(list: &[String]) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = list.iter().map(|s| s.into()).collect();
    Rc::new(VecModel::from(v)).into()
}

pub struct SettingsCtl {
    pub window: SettingsWindow,
    pub lists: Lists,
    pub devices: Vec<AudioDevice>,
    /// Encoders the configured FFmpeg build has (empty while unknown).
    pub available: Vec<String>,
    pub ffmpeg_ok: bool,
    /// Identifies the settings an encoder test ran with, so stale results are not shown.
    pub test_key: String,
    pub audio_check: Option<String>,
    pub audio_timer: slint::Timer,
    sized: bool,
}

impl SettingsCtl {
    pub fn new() -> Option<Self> {
        Some(Self {
            window: SettingsWindow::new().ok()?,
            lists: Lists::default(),
            devices: Vec::new(),
            available: Vec::new(),
            ffmpeg_ok: true,
            test_key: String::new(),
            audio_check: None,
            audio_timer: slint::Timer::default(),
            sized: false,
        })
    }

    pub fn apply_theme(&self, theme: ThemeSetting) {
        let dark = match theme {
            ThemeSetting::Oscuro => true,
            ThemeSetting::Claro => false,
            ThemeSetting::Sistema => windows_prefers_dark(),
        };
        self.window.global::<crate::Theme>().set_dark(dark);
        self.window
            .global::<crate::Palette>()
            .set_color_scheme(if dark {
                slint::language::ColorScheme::Dark
            } else {
                slint::language::ColorScheme::Light
            });
    }

    /// Write settings, lists and derived texts into the window.
    pub fn load(&mut self, s: &Settings, status: ShortcutStatus) {
        self.lists = form::lists(s, &self.devices);
        let w = &self.window;
        let l = &self.lists;
        w.set_data(form::to_data(s, l));
        w.set_capture_parts(strings(&format::shortcut_parts(&s.capture_shortcut)));
        w.set_record_parts(strings(&format::shortcut_parts(&s.record_shortcut)));
        w.set_capture_active(status.capture);
        w.set_record_active(status.record);
        w.set_system_devices(strings(&l.system_names));
        w.set_microphone_devices(strings(&l.microphone_names));
        let default_of = |kind: &str| {
            self.devices
                .iter()
                .find(|d| d.kind == kind && d.is_default)
                .map_or("No default device is available.".to_string(), |d| {
                    format!("Currently: {}", d.name)
                })
        };
        w.set_system_hint(
            if s.system_audio_device.is_none() {
                default_of("output")
            } else {
                String::new()
            }
            .into(),
        );
        w.set_microphone_hint(
            if s.microphone_device.is_none() {
                default_of("input")
            } else {
                String::new()
            }
            .into(),
        );
        let device_name = |id: &Option<String>, kind: &str| {
            id.as_ref()
                .and_then(|id| {
                    self.devices
                        .iter()
                        .find(|d| &d.id == id)
                        .map(|d| d.name.clone())
                })
                .or_else(|| {
                    self.devices
                        .iter()
                        .find(|d| d.kind == kind && d.is_default)
                        .map(|d| format!("{} (default)", d.name))
                })
                .unwrap_or_else(|| "Windows default".into())
        };
        let mut audio = Vec::new();
        if s.record_audio {
            audio.push(format!(
                "System sound: {}",
                device_name(&s.system_audio_device, "output")
            ));
        }
        if s.record_microphone {
            audio.push(format!(
                "Microphone: {}",
                device_name(&s.microphone_device, "input")
            ));
        }
        w.set_audio_summary(
            if audio.is_empty() {
                "No audio".to_string()
            } else {
                audio.join(" · ")
            }
            .into(),
        );
        let key = test_key(s, w.get_test_size_index());
        if key != self.test_key {
            w.set_test_result("".into());
            w.set_test_ok(false);
        }
        self.apply_theme(s.theme);
    }

    /// Size the window to fit comfortably on its screen the first time it is shown.
    pub fn show(&mut self) {
        let w = &self.window;
        let _ = w.show();
        if !self.sized {
            self.sized = true;
            let (cx, cy) = crate::platform::window_style::cursor_position().unwrap_or_default();
            if let Some(m) = crate::platform::window_style::monitor_at(cx, cy) {
                let (_, _, ww, wh) = m.work;
                let width = (960.0 * m.scale)
                    .min(f64::from(ww) * 0.8)
                    .max(640.0 * m.scale) as u32;
                let height = (840.0 * m.scale)
                    .min(f64::from(wh) * 0.85)
                    .max(480.0 * m.scale) as u32;
                w.window().set_size(slint::PhysicalSize::new(width, height));
                w.window().set_position(slint::PhysicalPosition::new(
                    m.work.0 + (ww - width as i32) / 2,
                    m.work.1 + (wh - height as i32) / 2,
                ));
            }
        }
        if let Some(hwnd) = crate::shell::hwnd(w.window()) {
            crate::platform::window_style::force_foreground(hwnd);
        }
    }

    pub fn stop_audio_check(&mut self) {
        self.audio_timer.stop();
        if let Some(id) = self.audio_check.take() {
            crate::audio_devices::stop(Some(&id));
        }
        self.window.set_checking(false);
    }

    pub fn set_meters(&self, s: &Settings, levels: Option<&crate::audio_devices::Levels>) {
        let checking = self.audio_check.is_some();
        self.window.set_system_meter(form::meter(
            levels.map(|l| &l.system),
            checking,
            s.record_audio,
        ));
        self.window.set_microphone_meter(form::meter(
            levels.map(|l| &l.microphone),
            checking,
            s.record_microphone,
        ));
    }

    pub fn set_ffmpeg(&mut self, info: &crate::encode::ffmpeg::FfmpegInfo) {
        self.ffmpeg_ok = info.available;
        self.available = info.encoders.clone();
        self.window.set_ffmpeg_ok(info.available);
        self.window.set_ffmpeg_status(
            if info.available {
                info.version.clone()
            } else {
                info.error
                    .clone()
                    .unwrap_or_else(|| "FFmpeg was not found.".into())
            }
            .into(),
        );
    }
}

/// Identifies what an encoder test checked.
pub fn test_key(s: &Settings, size: i32) -> String {
    serde_json::to_string(&(&s.encoding, s.fps, crate::encode::check::dimensions(size)))
        .unwrap_or_default()
}

/// How often the level check refreshes.
pub const LEVEL_POLL: Duration = Duration::from_millis(120);

#[cfg(test)]
mod tests {
    use super::*;

    fn devices() -> Vec<AudioDevice> {
        vec![
            AudioDevice {
                id: "spk".into(),
                name: "Speakers".into(),
                kind: "output".into(),
                is_default: true,
            },
            AudioDevice {
                id: "hdmi".into(),
                name: "HDMI".into(),
                kind: "output".into(),
                is_default: false,
            },
            AudioDevice {
                id: "mic".into(),
                name: "USB Mic".into(),
                kind: "input".into(),
                is_default: true,
            },
        ]
    }

    #[test]
    fn round_trip_keeps_every_setting() {
        let mut s = Settings::default();
        s.capture_shortcut = "CmdOrCtrl+Shift+KeyS".into();
        s.save_screenshots = true;
        s.capture_delay_seconds = 5;
        s.fps = 60;
        s.record_audio = true;
        s.system_audio_device = Some("hdmi".into());
        s.encoding.encoder = "hevc_nvenc".into();
        s.encoding.preset = "p5".into();
        s.encoding.gpu = "1".into();
        s.theme = ThemeSetting::Claro;
        let l = form::lists(&s, &devices());
        let back = form::from_data(&s, &form::to_data(&s, &l), &l);
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&s).unwrap()
        );
    }

    #[test]
    fn devices_list_default_and_missing_saved_devices() {
        let mut s = Settings::default();
        s.microphone_device = Some("gone".into());
        let l = form::lists(&s, &devices());
        assert_eq!(
            l.system_names,
            ["Windows default", "Speakers (default)", "HDMI"]
        );
        assert_eq!(
            l.microphone_names,
            [
                "Windows default",
                "Unavailable saved device",
                "USB Mic (default)"
            ]
        );
        let d = form::to_data(&s, &l);
        assert_eq!(d.microphone_device_index, 1);
        assert_eq!(d.system_device_index, 0);
    }

    #[test]
    fn meters_explain_their_state() {
        let level = |peak: f32, db: f32| Level {
            peak,
            db,
            state: "running".into(),
            error: None,
        };
        assert_eq!(form::meter(None, false, false).caption, "Not enabled");
        assert_eq!(form::meter(None, true, true).caption, "Connecting…");
        let m = form::meter(Some(&level(0.5, -6.0)), true, true);
        assert!(m.show_bar && !m.clipping);
        assert_eq!(m.value, "-6.0 dBFS");
        assert!(form::meter(Some(&level(0.99, -0.1)), true, true).clipping);
    }
}
