mod archivos;
mod audio_devices;
mod capture;
pub(crate) mod commands;
mod encode;
mod error;
mod hotkeys;
mod platform;
mod record;
mod recorder;
mod settings;
mod state;
mod tray;
mod windows_mgr;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::state::AppState;

/// Sent when the settings window becomes visible again, so it can refresh stale state.
pub const EVENT_SETTINGS_SHOWN: &str = "winshotx://settings-shown";

/// Overlay windows are reused between captures; this tells an already-mounted overlay to load a
/// new capture.
pub const EVENT_OVERLAY_SHOW: &str = "winshotx://overlay-show";

/// Seconds left on the capture delay. The countdown window counts down by itself; Rust sends
/// the start value and 0 when done.
pub const EVENT_COUNTDOWN: &str = "winshotx://countdown";

/// The tray menu was opened: refresh its state.
pub const EVENT_TRAY_MENU_OPENED: &str = "winshotx://tray-menu-opened";

pub fn run() {
    // Without WebView2 the windows would only show a browser error page, so check before
    // creating any window. See `platform::webview`.
    if !platform::webview::hay_con_que_pintar() {
        return;
    }

    tauri::Builder::default()
        // Must be the first plugin: a second instance exits and hands over instead of stealing
        // the global shortcuts.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A second instance with `--capture` starts a capture, `--cancel` closes it, and no
            // arguments shows the settings.
            if args.iter().any(|arg| arg == "--capture") {
                if let Err(error) =
                    windows_mgr::open_overlays(app, windows_mgr::OverlayIntent::Capture)
                {
                    eprintln!("could not open the overlay: {error}");
                }
                return;
            }
            if args.iter().any(|arg| arg == "--cancel") {
                windows_mgr::close_overlays(app);
                return;
            }
            let _ = windows_mgr::show_settings(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            let mut config = settings::load(&handle);
            // Configure a supplied executable once, without changing the Windows PATH.
            let mut launch_args = std::env::args();
            while let Some(argument) = launch_args.next() {
                if argument == "--ffmpeg-path" {
                    if let Some(path) = launch_args.next() {
                        if !crate::encode::ffmpeg::available_at(&path) {
                            return Err(
                                "The supplied FFmpeg executable could not be started.".into()
                            );
                        }
                        config.encoding.ffmpeg_path = path;
                        config.encoding.engine = "ffmpeg".into();
                        config.encoding.rate_control = "crf".into();
                        config.encoding.crf = 20;
                        settings::save(&handle, &config)?;
                    }
                }
            }

            let temp_root = std::env::temp_dir().join("winshotx-english");
            // Older builds kept frame caches and replay rings here.
            let _ = std::fs::remove_dir_all(temp_root.join("sessions"));
            let _ = std::fs::remove_dir_all(temp_root.join("replay"));
            let _ = std::fs::remove_dir_all(temp_root.join("pins"));

            app.manage(AppState::new(config.clone()));

            hotkeys::register(&handle, &config);
            tray::build(&handle)?;

            // Precreate capture windows hidden; only an explicit capture may show them.
            windows_mgr::precrear_overlays(&handle);
            // Create the GPU capture device up front (~100 ms) so the first shortcut does not
            // pay for it.
            std::thread::spawn(capture::preparar);

            if std::env::args().any(|arg| arg == "--settings") {
                windows_mgr::show_settings(&handle)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::CloseRequested { .. }) {
                audio_devices::stop(None);
            }
            // Closing the settings window keeps the app running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::overlay_bootstrap,
            commands::freeze_bytes,
            commands::freeze_png,
            commands::overlay_listo,
            commands::capture_still,
            commands::capture_all_screens,
            commands::cancel_capture,
            commands::copy_color,
            commands::start_recording,
            commands::stop_recording,
            commands::pause_recording,
            commands::cancel_recording,
            commands::ffmpeg_info,
            commands::test_encoder,
            commands::list_audio_devices,
            commands::start_audio_preview,
            commands::audio_preview_levels,
            commands::stop_audio_preview,
            commands::get_settings,
            commands::set_settings,
            commands::apply_preset,
            commands::ffmpeg_command,
            commands::open_settings,
            commands::pick_directory,
            commands::shortcut_status,
            commands::open_folder,
            commands::tray_menu_action,
        ])
        .build(tauri::generate_context!())
        .expect("could not build the application")
        .run(|_app, event| {
            if let RunEvent::ExitRequested { api, code, .. } = event {
                // With no windows open the app stays in the tray unless an exit was requested.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
