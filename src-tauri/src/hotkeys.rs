use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

use crate::settings::Settings;
use crate::state::AppState;
use crate::windows_mgr::{self, OverlayIntent};

/// Which shortcuts were actually registered. Registration fails when another app already owns
/// the combination, and the settings page reports that.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutStatus {
    pub capture: bool,
    pub record: bool,
}

/// Open the capture overlay.
fn on_capture(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state() != ShortcutState::Pressed {
        return;
    }
    if let Err(error) = windows_mgr::open_overlays(app, OverlayIntent::Capture) {
        eprintln!("could not open the overlay: {error}");
    }
}

/// Register the global shortcuts and report which ones took effect.
///
/// Shortcuts from the previous call are unregistered one by one instead of with
/// `unregister_all`, which clears the plugin's table before talking to Windows: after a single
/// failure the plugin and Windows disagree and every later registration reports "already
/// registered".
///
/// Calls are serialized because settings changes can arrive from several async command threads
/// at once.
pub fn register(app: &AppHandle, settings: &Settings) -> ShortcutStatus {
    static DE_UNO_EN_UNO: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _turno = DE_UNO_EN_UNO.lock();

    let manager = app.global_shortcut();
    let state = app.state::<AppState>();

    let previos = std::mem::take(&mut *state.registered.write());
    for shortcut in previos {
        if let Err(error) = manager.unregister(shortcut) {
            eprintln!("could not unregister a shortcut: {error}");
        }
    }

    // Print Screen only reaches us once the Snipping Tool no longer owns it.
    if [&settings.capture_shortcut, &settings.record_shortcut]
        .iter()
        .any(|s| s.contains("PrintScreen"))
    {
        crate::platform::printscreen::release_from_snipping_tool();
    }

    let mut status = ShortcutStatus::default();
    let mut puestos: Vec<Shortcut> = Vec::new();

    match settings.capture_shortcut.parse::<Shortcut>() {
        Err(error) => eprintln!("invalid capture shortcut: {error}"),
        Ok(shortcut) => {
            if let Err(error) = manager.on_shortcut(shortcut, on_capture) {
                eprintln!(
                    "could not register the capture shortcut {}: {error}",
                    settings.capture_shortcut
                );
            } else {
                status.capture = true;
                puestos.push(shortcut);
            }
        }
    }

    match settings.record_shortcut.parse::<Shortcut>() {
        Err(error) => eprintln!("invalid record shortcut: {error}"),
        Ok(shortcut) => {
            let resultado = manager.on_shortcut(shortcut, |app, _shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                // While recording, the record shortcut stops the recording.
                if app.state::<AppState>().is_recording() {
                    if let Err(error) = crate::recorder::stop(app) {
                        eprintln!("could not stop the recording: {error}");
                    }
                } else {
                    let _ = windows_mgr::open_overlays(app, OverlayIntent::Record);
                }
            });
            if let Err(error) = resultado {
                eprintln!(
                    "could not register the record shortcut {}: {error}",
                    settings.record_shortcut
                );
            } else {
                status.record = true;
                puestos.push(shortcut);
            }
        }
    }

    // The plugin may report success before Windows answers, so this is the best available
    // status.
    *state.registered.write() = puestos;
    *state.shortcuts.write() = status;
    status
}
