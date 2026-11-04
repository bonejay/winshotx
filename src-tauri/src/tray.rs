use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::error::Result;
use crate::state::AppState;
use crate::windows_mgr::{self, OverlayIntent};

/// Use the native Windows menu: it does not depend on a second WebView loading.
pub fn build(app: &AppHandle) -> Result<()> {
    let menu = tauri::menu::MenuBuilder::new(app)
        .text("capture", "Take a screenshot")
        .text("record", "Start / stop recording")
        .separator()
        .text("folder", "Open screenshot folder")
        .text("video-folder", "Open video folder")
        .text("settings", "Settings")
        .separator()
        .text("quit", "Quit WinShotX")
        .build()?;
    let mut builder = TrayIconBuilder::with_id("winshotx")
        .tooltip("WinShotX - Screenshots and video")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| ejecutar(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = windows_mgr::show_settings(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Runs a tray menu action. Shared by the native tray menu and the `tray_menu_action` command
/// used by the settings window.
pub fn ejecutar(app: &AppHandle, id: &str) {
    match id {
        "capture" => {
            let _ = windows_mgr::open_overlays(app, OverlayIntent::Capture);
        }
        "record" => {
            if app.state::<AppState>().is_recording() {
                let _ = crate::recorder::stop(app);
            } else {
                let _ = windows_mgr::open_overlays(app, OverlayIntent::Record);
            }
        }
        // Open the screenshot or video folder.
        "folder" | "video-folder" => {
            let carpeta = {
                let state = app.state::<AppState>();
                let settings = state.settings.read();
                if id == "folder" {
                    settings.save_directory.clone()
                } else {
                    settings.video_directory.clone()
                }
            };
            let _ = std::fs::create_dir_all(&carpeta);
            let _ = crate::platform::open_folder(&std::path::PathBuf::from(carpeta));
        }
        "settings" => {
            let _ = windows_mgr::show_settings(app);
        }
        "quit" => app.exit(0),
        _ => {}
    }
}
