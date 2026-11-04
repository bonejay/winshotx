use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::capture::{self, MonitorInfo, Rect, WindowRect};
use crate::encode::{ffmpeg, png};
use crate::error::{AppError, Result};
use crate::recorder::{self, RecordOptions, SessionInfo};
use crate::settings::Settings;
use crate::state::AppState;
use crate::windows_mgr::{self, OverlayIntent};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayPayload {
    monitor: MonitorInfo,
    /// Which shortcut press this payload belongs to. The overlay echoes it in `overlay_listo`,
    /// so a late reply from a cancelled capture cannot show a window nobody asked for.
    generation: u64,
    windows: Vec<WindowRect>,
    settings: Settings,
    /// Whether the overlay was opened for a screenshot or a recording.
    intent: OverlayIntent,
    /// This screen's number (from 1) and the screen count.
    screen_number: usize,
    screen_count: usize,
    /// The last captured region in virtual-desktop coordinates, or none yet.
    last_region: Option<Rect>,
}

/// Build one screen's overlay payload. Used by `overlay_bootstrap` (first mount) and by
/// `windows_mgr::open_overlays` (sent with the event when a window is reused).
pub fn build_overlay_payload(state: &AppState, monitor_id: u32) -> Result<OverlayPayload> {
    let freezes = state.freezes.read();
    let freeze = freezes
        .iter()
        .find(|f| f.monitor.id == monitor_id)
        .ok_or_else(|| AppError::Msg(format!("Monitor {monitor_id} has no frozen capture")))?;

    Ok(OverlayPayload {
        monitor: freeze.monitor.clone(),
        generation: windows_mgr::generacion_actual(),
        windows: capture::window_rects(),
        settings: state.settings.read().clone(),
        intent: *state.intent.read(),
        // The id is the monitor's system enumeration index, so screen numbers stay stable even
        // while `freezes` is still being filled.
        screen_number: freeze.monitor.id as usize + 1,
        screen_count: capture::monitors()
            .map(|m| m.len())
            .unwrap_or(freezes.len())
            .max(1),
        last_region: *state.last_region.read(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StillResult {
    path: Option<String>,
    copied: bool,
    width: u32,
    height: u32,
}

#[tauri::command]
pub async fn overlay_bootstrap(
    state: State<'_, AppState>,
    monitor_id: u32,
) -> Result<OverlayPayload> {
    build_overlay_payload(&state, monitor_id)
}

/// A monitor's frozen screen as PNG for the overlay. Compressed once in `capture::freeze_all`;
/// much faster to transfer than raw pixels.
#[tauri::command]
pub async fn freeze_png(
    state: State<'_, AppState>,
    monitor_id: u32,
) -> Result<tauri::ipc::Response> {
    let png = {
        let freezes = state.freezes.read();
        freezes
            .iter()
            .find(|f| f.monitor.id == monitor_id)
            .map(|f| f.png.clone())
            .ok_or_else(|| AppError::Msg(format!("Monitor {monitor_id} has no frozen capture")))?
    };
    Ok(tauri::ipc::Response::new((*png).clone()))
}

/// The overlay for this monitor has painted its image: show it. Windows stay parked off-screen
/// until then, so nothing appears half-drawn.
#[tauri::command]
pub async fn overlay_listo(app: AppHandle, monitor_id: u32, generation: u64) {
    windows_mgr::overlay_listo(&app, monitor_id, generation);
}

/// Overlay fallback: the frozen screen as an uncompressed BMP if the PNG fails, so a full-
/// screen opaque overlay never stays black.
#[tauri::command]
pub async fn freeze_bytes(
    state: State<'_, AppState>,
    monitor_id: u32,
) -> Result<tauri::ipc::Response> {
    let image = {
        let freezes = state.freezes.read();
        freezes
            .iter()
            .find(|f| f.monitor.id == monitor_id)
            .map(|f| f.image.clone())
            .ok_or_else(|| AppError::Msg(format!("Monitor {monitor_id} has no frozen capture")))?
    };
    Ok(tauri::ipc::Response::new(capture::bmp_bytes(&image)))
}

#[tauri::command]
pub async fn capture_still(app: AppHandle, region: Rect, action: String) -> Result<StillResult> {
    let image = {
        let state = app.state::<AppState>();
        let freezes = state.freezes.read();
        capture::crop_from_freeze(&freezes, region)?
    };
    // Remember the region only after a successful crop, and only for region captures (not full-
    // desktop captures).
    *app.state::<AppState>().last_region.write() = Some(region);
    entregar(&app, image, &action)
}

#[tauri::command]
pub async fn capture_all_screens(app: AppHandle, action: String) -> Result<StillResult> {
    let (image, _) = {
        let state = app.state::<AppState>();
        let freezes = state.freezes.read();
        capture::stitch_all(&freezes)?
    };
    entregar(&app, image, &action)
}

fn entregar(app: &AppHandle, image: image::RgbaImage, action: &str) -> Result<StillResult> {
    let state = app.state::<AppState>();
    let (width, height) = (image.width(), image.height());
    let (save, con_sonido) = {
        let ajustes = state.settings.read();
        (ajustes.save_screenshots, ajustes.play_sound)
    };
    // Play the shutter sound for every capture, before writing anything so it is not delayed.
    if con_sonido {
        crate::platform::sonido::obturador();
    }
    if action != "copy" {
        return Err(AppError::Msg(format!("Unknown action: {action}")));
    }

    let mut result = StillResult {
        path: None,
        copied: false,
        width,
        height,
    };
    let bytes = png::to_bytes(&image)?;
    crate::platform::clipboard::copy_image(&image, &bytes)?;
    result.copied = true;
    if save {
        let directory = PathBuf::from(state.settings.read().save_directory.clone());
        // `archivos` picks a name that does not exist yet, so two captures in the same second do
        // not overwrite each other.
        let path = crate::archivos::destino(&directory, "png")?;
        png::save(&image, &path, width, height)?;
        result.path = Some(path.to_string_lossy().to_string());
    }
    windows_mgr::close_overlays(app);
    Ok(result)
}

/// Copy a color to the clipboard as `#rrggbb`, from the magnifier. The format is validated here
/// too so the command cannot write arbitrary text.
#[tauri::command]
pub async fn copy_color(color: String) -> Result<()> {
    if !es_un_color(&color) {
        return Err(AppError::Msg("Invalid color".into()));
    }
    crate::platform::clipboard::copy_text(&color)?;
    Ok(())
}

/// Exactly `#` followed by six hex digits.
fn es_un_color(texto: &str) -> bool {
    texto.len() == 7 && texto.starts_with('#') && texto[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

#[tauri::command]
pub async fn cancel_capture(app: AppHandle) {
    windows_mgr::close_overlays(&app);
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    region: Rect,
    options: RecordOptions,
) -> Result<SessionInfo> {
    if options.format != "video" {
        return Err(AppError::Msg(
            "Only video recording is supported in this edition.".into(),
        ));
    }
    recorder::start(&app, region, options)
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> Result<()> {
    recorder::stop(&app)
}

#[tauri::command]
pub async fn pause_recording(app: AppHandle, paused: bool) -> Result<()> {
    recorder::set_paused(&app, paused)
}

#[tauri::command]
pub async fn cancel_recording(app: AppHandle) -> Result<()> {
    recorder::cancel(&app)
}

#[tauri::command]
pub async fn shortcut_status(state: State<'_, AppState>) -> Result<crate::hotkeys::ShortcutStatus> {
    Ok(*state.shortcuts.read())
}


#[tauri::command]
pub async fn open_folder(path: String) -> Result<()> {
    crate::platform::open_folder(&PathBuf::from(path))
}

/// Apply a saved preset: copies its frame rate and encoding into the active settings. Does
/// not re-register shortcuts, so it is fast enough to call from the capture bar.
#[tauri::command]
pub async fn apply_preset(app: AppHandle, name: String) -> Result<Settings> {
    let state = app.state::<AppState>();
    let snapshot = {
        let mut guard = state.settings.write();
        let preset = guard
            .presets
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| AppError::Msg(format!("No preset named {name}")))?;
        guard.fps = preset.fps.clamp(5, 120);
        guard.encoding = crate::record::stream_encoder::streaming_options(&preset.encoding);
        guard.active_preset = preset.name;
        guard.clone()
    };
    crate::settings::save(&app, &snapshot)?;
    Ok(snapshot)
}

/// The FFmpeg command these options produce, as shown in the settings. It is built by the same
/// function recordings use, with placeholder inputs and a 1920x1080 region.
#[tauri::command]
pub async fn ffmpeg_command(
    app: AppHandle,
    options: crate::settings::EncodingSettings,
    fps: u32,
) -> Result<String> {
    let options = crate::record::stream_encoder::streaming_options(&options);
    let folder = app.state::<AppState>().settings.read().video_directory.clone();
    let destination = std::path::Path::new(&folder).join("winshotx-<time>.recording.mp4");
    let args = crate::record::stream_encoder::arguments(
        &options,
        1920,
        1080,
        fps.clamp(5, 120),
        Some(crate::record::stream_encoder::AudioFormat {
            channels: 2,
            sample_rate: 48000,
        }),
        "tcp://127.0.0.1:<video>",
        Some("tcp://127.0.0.1:<audio>"),
        &destination,
    )?;
    let quote = |a: &str| {
        if a.is_empty() || a.contains([' ', '"', '<', '>', '|', '&']) {
            format!("\"{}\"", a.replace('"', "\\\""))
        } else {
            a.to_string()
        }
    };
    Ok(std::iter::once(quote(&options.ffmpeg_path))
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" "))
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings> {
    Ok(state.settings.read().clone())
}

#[tauri::command]
pub async fn set_settings(app: AppHandle, mut settings: Settings) -> Result<Settings> {
    settings.encoding = crate::record::stream_encoder::streaming_options(&settings.encoding);
    crate::settings::fix_settings_directories(&mut settings);
    crate::encode::ffmpeg::validate(&settings.encoding)?;
    let state = app.state::<AppState>();
    let previous = state.settings.read().clone();
    *state.settings.write() = settings.clone();
    crate::settings::save(&app, &settings)?;

    // Always re-register, even if the shortcuts did not change: that is the only way to retry
    // after another app released a combination. `register` unregisters first, so repeating it
    // is harmless.
    crate::hotkeys::register(&app, &settings);
    if previous.start_with_windows != settings.start_with_windows {
        crate::platform::autostart::set(settings.start_with_windows)?;
    }
    Ok(settings)
}

/// Run a tray menu action from the frontend via `tray::ejecutar`.
#[tauri::command]
pub async fn tray_menu_action(app: AppHandle, action: String) -> Result<()> {
    crate::tray::ejecutar(&app, &action);
    Ok(())
}

#[tauri::command]
pub async fn pick_directory(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .blocking_pick_folder()
        .map(|folder| folder.to_string())
}

/// Open the settings from the overlay toolbar. The capture closes first so the settings window
/// is not hidden behind the overlays.
#[tauri::command]
pub async fn open_settings(app: AppHandle) -> Result<()> {
    windows_mgr::close_overlays(&app);
    windows_mgr::show_settings(&app)
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<crate::audio_devices::AudioDevice>> {
    crate::audio_devices::list()
}
#[tauri::command]
pub async fn start_audio_preview(
    app: AppHandle,
    system: bool,
    microphone: bool,
    system_device: Option<String>,
    microphone_device: Option<String>,
) -> Result<String> {
    let state = app.state::<AppState>();
    if state.is_recording() {
        return Err(AppError::Msg(
            "Stop recording before checking audio levels.".into(),
        ));
    }
    let id = crate::audio_devices::start(system, microphone, system_device, microphone_device)?;
    if state.is_recording() {
        crate::audio_devices::stop(Some(&id));
        return Err(AppError::Msg(
            "Recording started; audio level check has ended.".into(),
        ));
    }
    Ok(id)
}
#[tauri::command]
pub async fn audio_preview_levels(id: String) -> Result<crate::audio_devices::Levels> {
    crate::audio_devices::levels(&id)
}
#[tauri::command]
pub async fn stop_audio_preview(id: String) {
    crate::audio_devices::stop(Some(&id));
}

#[tauri::command]
pub async fn ffmpeg_info(
    app: AppHandle,
    path: Option<String>,
) -> crate::encode::ffmpeg::FfmpegInfo {
    let executable = path.unwrap_or_else(|| {
        app.state::<AppState>()
            .settings
            .read()
            .encoding
            .ffmpeg_path
            .clone()
    });
    ffmpeg::info(&executable)
}

#[tauri::command]
pub async fn test_encoder(
    options: crate::settings::EncodingSettings,
    fps: u32,
) -> Result<ffmpeg::EncoderCheck> {
    tauri::async_runtime::spawn_blocking(move || ffmpeg::test_encoding(&options, fps))
        .await
        .map_err(|e| AppError::Msg(e.to_string()))?
}
