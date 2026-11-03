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
        .text("folder", "Open capture folder")
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
                let _ = windows_mgr::open_overlays(tray.app_handle(), OverlayIntent::Capture);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Lo que hace cada entrada del menu, en un solo sitio.
///
/// Lo llaman el menu dibujado (`tray_menu.rs`, por el comando `tray_menu_action`) y el
/// menu del sistema, que sigue existiendo de reserva. Estaba escrito dentro del manejador
/// del menu nativo, y dejarlo ahi habria significado dos listas de acciones que se
/// separarian al primer cambio.
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
        // Donde acaban las capturas guardadas. Sin esto habia que abrir los ajustes
        // para llegar a una carpeta que se visita todos los dias.
        "folder" => {
            let carpeta = app
                .state::<AppState>()
                .settings
                .read()
                .save_directory
                .clone();
            let _ = std::fs::create_dir_all(&carpeta);
            let _ = crate::platform::open_folder(&std::path::PathBuf::from(carpeta));
        }
        "settings" => {
            let _ = windows_mgr::show_settings(app);
        }
        // La comprobacion vive en la ventana de ajustes: desde la bandeja se
        // abre y se le dice que mire, para no tener dos caminos distintos.
        "quit" => app.exit(0),
        _ => {}
    }
}
