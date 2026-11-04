use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use parking_lot::Mutex;

use crate::capture::{self, MonitorInfo, Rect};
use crate::error::Result;
use crate::state::{AppState, CandadoCaptura};

/// Overlay windows waiting to be shown until each reports that its image is painted
/// (`overlay_listo`). Windows stay parked off-screen while loading, so nothing appears half-
/// drawn.
///
/// The generation tells captures apart: a late reply from a cancelled capture cannot show a
/// window.
struct Pendientes {
    generacion: u64,
    ventanas: Vec<(u32, String, MonitorInfo)>,
    cursor: Option<(i32, i32)>,
    con_foco: bool,
}

static PENDIENTES: Mutex<Pendientes> = Mutex::new(Pendientes {
    generacion: 0,
    ventanas: Vec::new(),
    cursor: None,
    con_foco: false,
});

/// How long to wait for an overlay to report it is painted before showing it anyway, so a
/// frontend error is still visible and can be dismissed with Escape.
const ESPERA_MAXIMA: std::time::Duration = std::time::Duration::from_millis(900);

/// The current capture number: sent in the payload and echoed back in `overlay_listo`.
pub fn generacion_actual() -> u64 {
    PENDIENTES.lock().generacion
}

pub const OVERLAY_PREFIX: &str = "overlay-";

/// WebView2 browser arguments for ALL windows. The first three are wry's defaults; the other
/// three stop Chromium from throttling hidden windows, since overlays wait parked off-screen
/// and would otherwise react ~300 ms late.
///
/// They must be identical for every window, including `main` in `tauri.conf.json`: WebView2
/// shares one browser process configured by the first window, and a window asking for different
/// arguments stays blank.
pub const NAVEGADOR_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-renderer-backgrounding --disable-backgrounding-occluded-windows --disable-background-timer-throttling";

static OVERLAY_SEQUENCE: AtomicU32 = AtomicU32::new(0);
pub const RECORDER_PREFIX: &str = "recorder-";
/// The capture-delay countdown window. There is one, on the screen with the cursor, hidden
/// instead of closed like the overlays.
pub const COUNTDOWN_LABEL: &str = "countdown";

/// Countdown window size in logical pixels, so it looks the same at any display scale.
const CUENTA_LADO: f64 = 132.0;

/// Time for the desktop to repaint where the countdown was before freezing, so it does not
/// appear in the capture.
const MARGEN_REPINTADO: std::time::Duration = std::time::Duration::from_millis(120);

/// Why the overlay was opened, decided by the shortcut that was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlayIntent {
    Capture,
    Record,
}

impl OverlayIntent {
    /// Whether the result is a screenshot: decides whether the delay applies and which mode the
    /// mode bar starts in.
    pub fn es_foto(self) -> bool {
        self != Self::Record
    }
}

/// Entry point of the capture shortcut: prepare the screen and open the selection.
///
/// Two things must happen before the screen is frozen, so they cannot be overlay keys: the
/// capture delay and hiding the desktop icons.
pub fn open_overlays(app: &AppHandle, intent: OverlayIntent) -> Result<()> {
    let state = app.state::<AppState>();
    if state.is_recording() {
        return Ok(());
    }
    // A second press while a capture is in progress is ignored instead of starting another
    // capture on top. With a delay, the lock also covers the countdown.
    let Some(candado) = state.intentar_capturar() else {
        return Ok(());
    };
    *state.intent.write() = intent;

    // The delay only applies to screenshots. For recordings the frozen screen is just a
    // background for choosing the region.
    let segundos = state.settings.read().capture_delay_seconds;
    if segundos == 0 || !intent.es_foto() {
        return congelar_y_abrir(app, candado);
    }

    // The delay must not sleep here: this runs inside the global-shortcut handler on the main
    // event loop, and sleeping would freeze the whole app, including the countdown.
    mostrar_cuenta_atras(app, segundos);
    let espera = app.clone();
    std::thread::spawn(move || {
        // Hide the countdown slightly before the end so the desktop can repaint underneath it.
        // The margin comes out of the countdown, so the total delay stays as configured.
        let total = std::time::Duration::from_secs(u64::from(segundos));
        std::thread::sleep(total.saturating_sub(MARGEN_REPINTADO));

        // Window handling must happen on the main thread.
        let para_esconder = espera.clone();
        let _ = espera.run_on_main_thread(move || esconder_cuenta_atras(&para_esconder));

        std::thread::sleep(MARGEN_REPINTADO);
        let seguir = espera.clone();
        let _ = espera.run_on_main_thread(move || {
            if let Err(error) = congelar_y_abrir(&seguir, candado) {
                eprintln!("could not open the overlay: {error}");
            }
        });
    });
    Ok(())
}

/// Freeze the screens and open one overlay per monitor. Freezing first makes the selection
/// stable and pixel-exact.
///
/// Overlay windows are reused between captures (`close_overlays` hides them) because creating a
/// WebView2 costs 200-800 ms per screen. A reused window gets no React remount, so it is
/// notified with `EVENT_OVERLAY_SHOW`.
///
/// The capture lock is passed in and released at the end; with a delay it was taken earlier on
/// another thread.
fn congelar_y_abrir(app: &AppHandle, candado: CandadoCaptura) -> Result<()> {
    let state = app.state::<AppState>();

    // The monitors, without capturing them yet: needed to know which one has the cursor.
    let monitors = capture::monitors()?;
    if monitors.is_empty() {
        return Err(crate::error::AppError::Msg(
            "No display was detected".into(),
        ));
    }
    let cursor = cursor_position();

    // A new capture: anything pending from the previous one no longer counts.
    let generacion = {
        let mut pendientes = PENDIENTES.lock();
        pendientes.generacion += 1;
        pendientes.ventanas.clear();
        pendientes.cursor = cursor;
        pendientes.con_foco = false;
        pendientes.generacion
    };

    // Windows of monitors that no longer exist stay hidden in the pool.
    let etiquetas_actuales: std::collections::HashSet<String> = monitors
        .iter()
        .map(|m| format!("{OVERLAY_PREFIX}{}", m.id))
        .collect();
    for (label, window) in app.webview_windows() {
        if label.starts_with(OVERLAY_PREFIX) && !etiquetas_actuales.contains(&label) {
            let _ = window.hide();
        }
    }

    // Freeze the screen under the cursor first, on its own, and send its image; the others
    // follow right after.
    let primera = cursor
        .and_then(|(x, y)| monitors.iter().position(|m| m.contains(x, y)))
        .or_else(|| monitors.iter().position(|m| m.is_primary))
        .unwrap_or(0);
    let resto: Vec<usize> = (0..monitors.len()).filter(|&i| i != primera).collect();
    state.freezes.write().clear();

    // All of this runs on a separate thread. Messages to windows go through the main thread's
    // loop, so blocking it while the other screens freeze would delay the first screen too.
    let app = app.clone();
    std::thread::spawn(move || {
        // The capture lock lives until both batches are frozen.
        let candado = candado;
        let primera_id = monitors[primera].id;
        for (numero, tanda) in [vec![primera], resto].into_iter().enumerate() {
            if tanda.is_empty() {
                continue;
            }
            let freezes = match capture::freeze_monitors(&tanda) {
                Ok(freezes) => freezes,
                Err(error) => {
                    eprintln!("could not freeze the screen: {error}");
                    continue;
                }
            };
            let monitores: Vec<MonitorInfo> = freezes.iter().map(|f| f.monitor.clone()).collect();
            app.state::<AppState>().freezes.write().extend(freezes);
            if numero == 1 {
                // The other screens are only sent once the first one is visible: serving their
                // PNGs occupies the main thread, which is also the one showing the first
                // screen. They are already captured in parallel; only the notification waits.
                esperar_a_que_se_vea(primera_id, generacion);
            }
            let dentro = app.clone();
            let _ = app.run_on_main_thread(move || {
                let state = dentro.state::<AppState>();
                for monitor in &monitores {
                    if let Err(error) = preparar_overlay(&dentro, &state, monitor) {
                        eprintln!("could not prepare the overlay: {error}");
                    }
                }
            });
        }
        drop(candado);

        // If a screen does not report in time it is shown anyway: the overlay's error screen
        // with Escape is better than nothing.
        std::thread::sleep(ESPERA_MAXIMA);
        let dentro = app.clone();
        let _ = app.run_on_main_thread(move || mostrar_las_que_falten(&dentro, generacion));
    });

    Ok(())
}

/// Send a monitor's window its capture and mark it pending. Creates the window if needed.
fn preparar_overlay(app: &AppHandle, state: &AppState, monitor: &MonitorInfo) -> Result<()> {
    let label = format!("{OVERLAY_PREFIX}{}", monitor.id);

    let window = if let Some(existente) = app.get_webview_window(&label) {
        // The payload travels in the event itself, saving an `overlay_bootstrap` round trip.
        // Use `emit_to` with this window's label: plain `emit` goes to every window and the
        // screens would overwrite each other's payloads.
        if let Ok(payload) = crate::commands::build_overlay_payload(state, monitor.id) {
            let _ = existente.emit_to(label.as_str(), crate::EVENT_OVERLAY_SHOW, payload);
        }
        existente
    } else {
        // A newly created window boots by itself and requests its payload with
        // `overlay_bootstrap`.
        build_overlay_window(app, &label, monitor.id)?
    };

    // Set the size now, while parked, so the webview can lay out while the image loads.
    // Positioning happens when it is shown (`overlay_listo` or the fallback timeout).
    window.set_size(PhysicalSize::new(monitor.width, monitor.height))?;
    PENDIENTES
        .lock()
        .ventanas
        .push((monitor.id, label, monitor.clone()));
    Ok(())
}

/// Wait until that monitor's overlay has left the pending list (i.e. was shown), with a
/// timeout: the wait orders things, it must not block.
fn esperar_a_que_se_vea(monitor_id: u32, generacion: u64) {
    let tope = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < tope {
        {
            let pendientes = PENDIENTES.lock();
            if pendientes.generacion != generacion
                || !pendientes
                    .ventanas
                    .iter()
                    .any(|(id, _, _)| *id == monitor_id)
            {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

/// That monitor's overlay is painted: show it and focus it if it is on the cursor's screen.
/// Called from a command thread; windows are handled on the main thread.
pub fn overlay_listo(app: &AppHandle, monitor_id: u32, generacion: u64) {
    let dentro = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some((label, monitor, con_foco)) = sacar_pendiente(monitor_id, generacion) else {
            // From an earlier capture, or already shown by the fallback: nothing to do.
            return;
        };
        ensennar_overlay(&dentro, &label, &monitor, con_foco);
    });
}

/// Remove that monitor's window from the pending list if it belongs to this capture, and decide
/// whether it gets focus: the cursor's screen, or the last remaining one if the cursor is on
/// none.
fn sacar_pendiente(monitor_id: u32, generacion: u64) -> Option<(String, MonitorInfo, bool)> {
    let mut pendientes = PENDIENTES.lock();
    if pendientes.generacion != generacion {
        return None;
    }
    let posicion = pendientes
        .ventanas
        .iter()
        .position(|(id, _, _)| *id == monitor_id)?;
    let (_, label, monitor) = pendientes.ventanas.remove(posicion);
    let cursor = pendientes.cursor;
    let es_la_del_raton = cursor.is_some_and(|(x, y)| monitor.contains(x, y));
    let otra_lo_tendra = pendientes
        .ventanas
        .iter()
        .any(|(_, _, m)| cursor.is_some_and(|(x, y)| m.contains(x, y)));
    let con_foco = !pendientes.con_foco && (es_la_del_raton || !otra_lo_tendra);
    if con_foco {
        pendientes.con_foco = true;
    }
    Some((label, monitor, con_foco))
}

/// Position the window on its screen and show it. Always on the main thread.
fn ensennar_overlay(app: &AppHandle, label: &str, monitor: &MonitorInfo, con_foco: bool) {
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    // Position in physical pixels, unaffected by DPI scaling. Position and show happen in one
    // `SetWindowPos` call; the following `show()` only syncs Tauri's state.
    #[cfg(windows)]
    let colocada = crate::platform::window_style::colocar_y_ensennar(&window, monitor.x, monitor.y);
    #[cfg(not(windows))]
    let colocada = false;
    if !colocada {
        let _ = window.set_position(PhysicalPosition::new(monitor.x, monitor.y));
    }
    let _ = window.show();
    // If Windows rescaled the window for a different DPI when moving it, fix the size here.
    // Normally this is just a read.
    if window
        .inner_size()
        .is_ok_and(|s| s.width != monitor.width || s.height != monitor.height)
    {
        let _ = window.set_size(PhysicalSize::new(monitor.width, monitor.height));
    }
    if con_foco {
        // Without this, Windows uses the first click to activate the window and the selection
        // never starts.
        let _ = window.set_focus();
        #[cfg(windows)]
        force_foreground(&window);
    }
}

/// Fallback: after the timeout, show whatever has not reported.
fn mostrar_las_que_falten(app: &AppHandle, generacion: u64) {
    let que_faltan: Vec<(u32, String, MonitorInfo, bool)> = {
        let mut pendientes = PENDIENTES.lock();
        if pendientes.generacion != generacion {
            return;
        }
        let cursor = pendientes.cursor;
        let mut lista = Vec::new();
        for (id, label, monitor) in std::mem::take(&mut pendientes.ventanas) {
            let con_foco =
                !pendientes.con_foco && cursor.is_some_and(|(x, y)| monitor.contains(x, y));
            if con_foco {
                pendientes.con_foco = true;
            }
            lista.push((id, label, monitor, con_foco));
        }
        // If the cursor is on no screen, focus the first remaining window.
        if !pendientes.con_foco {
            if let Some(primera) = lista.first_mut() {
                primera.3 = true;
                pendientes.con_foco = true;
            }
        }
        lista
    };
    for (_, label, monitor, con_foco) in que_faltan {
        ensennar_overlay(app, &label, &monitor, con_foco);
    }
}

/// Show the seconds left, centered on the screen with the cursor. The window is created on
/// first use and then kept hidden, like the overlays; it is not precreated because few people
/// use the delay.
fn mostrar_cuenta_atras(app: &AppHandle, segundos: u32) {
    let window = match app.get_webview_window(COUNTDOWN_LABEL) {
        Some(existente) => {
            // Target the event at this window, as everywhere else: an untargeted listener
            // receives events for all windows.
            let _ = existente.emit_to(COUNTDOWN_LABEL, crate::EVENT_COUNTDOWN, segundos);
            existente
        }
        None => {
            // The first value goes in the URL because the page is not listening yet when the
            // event would be sent.
            let url = WebviewUrl::App(format!("cuenta.html?segundos={segundos}").into());
            let construida = WebviewWindowBuilder::new(app, COUNTDOWN_LABEL, url)
                .additional_browser_args(NAVEGADOR_ARGS)
                .title("winshotx")
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
                .shadow(true)
                .focused(false)
                .visible(false)
                .inner_size(CUENTA_LADO, CUENTA_LADO)
                .build();
            let Ok(nueva) = construida else {
                eprintln!("could not create the countdown window");
                return;
            };
            crate::platform::window_style::rounded_corners(&nueva);
            // Without this the countdown would take focus and close the menu being captured.
            crate::platform::window_style::never_focus(&nueva);
            nueva
        }
    };

    colocar_cuenta_atras(&window);
    let _ = window.show();
}

/// Center it on the monitor with the cursor, recomputed each time because the cursor and the
/// display scale can differ from last time. It is hidden before the screen is frozen.
fn colocar_cuenta_atras(window: &tauri::WebviewWindow) {
    colocar_ventanita(window, None)
}

/// The same placement on a given screen instead of the cursor's.
fn colocar_ventanita(window: &tauri::WebviewWindow, en: Option<u32>) {
    let Ok(monitores) = xcap::Monitor::all() else {
        return;
    };
    // Use the requested screen if given, otherwise the cursor's.
    if let Some(indice) = en {
        if let Some(monitor) = monitores.get(indice as usize) {
            poner_en(window, monitor);
            return;
        }
    }
    let cursor = cursor_position();
    let dentro = |m: &xcap::Monitor, x: i32, y: i32| -> bool {
        let (Ok(mx), Ok(my), Ok(mw), Ok(mh)) = (m.x(), m.y(), m.width(), m.height()) else {
            return false;
        };
        x >= mx && x < mx + mw as i32 && y >= my && y < my + mh as i32
    };
    let elegido = cursor
        .and_then(|(x, y)| monitores.iter().find(|m| dentro(m, x, y)))
        .or_else(|| monitores.iter().find(|m| m.is_primary().unwrap_or(false)))
        .or_else(|| monitores.first());

    let Some(monitor) = elegido else { return };
    poner_en(window, monitor);
}

/// Center it on that monitor, sized for its display scale.
fn poner_en(window: &tauri::WebviewWindow, monitor: &xcap::Monitor) {
    let (Ok(x), Ok(y), Ok(width), Ok(height)) =
        (monitor.x(), monitor.y(), monitor.width(), monitor.height())
    else {
        return;
    };

    // Use the target screen's scale, not the window's current screen, which may differ in
    // mixed-DPI setups.
    let escala = f64::from(monitor.scale_factor().unwrap_or(1.0));
    let lado = (CUENTA_LADO * escala) as u32;

    let _ = window.set_size(PhysicalSize::new(lado, lado));
    let _ = window.set_position(PhysicalPosition::new(
        x + (width as i32 - lado as i32) / 2,
        y + (height as i32 - lado as i32) / 2,
    ));
}

/// Hide it and reset it to 0 (drawn as the camera icon), so a stale frame next time shows the
/// icon instead of an old number.
fn esconder_cuenta_atras(app: &AppHandle) {
    let Some(window) = app.get_webview_window(COUNTDOWN_LABEL) else {
        return;
    };
    let _ = window.hide();
    let _ = window.emit_to(COUNTDOWN_LABEL, crate::EVENT_COUNTDOWN, 0u32);
}

fn build_overlay_window(
    app: &AppHandle,
    label: &str,
    monitor_id: u32,
) -> Result<tauri::WebviewWindow> {
    let url = WebviewUrl::App(format!("overlay.html?monitor={monitor_id}").into());
    Ok(WebviewWindowBuilder::new(app, label, url)
        .additional_browser_args(NAVEGADOR_ARGS)
        .title("winshotx")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .shadow(false)
        .visible(false)
        .build()?)
}

/// Prepare capture windows without ever showing them at startup.
/// Off-screen coordinates are not a visibility guarantee on mixed-DPI desktops.
pub fn precrear_overlays(app: &AppHandle) {
    let Ok(monitors) = capture::monitors() else {
        return;
    };
    for monitor in &monitors {
        let label = format!("{OVERLAY_PREFIX}{}", monitor.id);
        if app.get_webview_window(&label).is_some() {
            continue;
        }
        let Ok(window) = build_overlay_window(app, &label, monitor.id) else {
            continue;
        };
        let _ = window.set_position(aparcadero_de(monitor, &monitors));
        let _ = window.set_size(PhysicalSize::new(monitor.width, monitor.height));
        let _ = window.hide();
    }
}

#[cfg(windows)]
fn cursor_position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((point.x, point.y))
}

#[cfg(not(windows))]
fn cursor_position() -> Option<(i32, i32)> {
    None
}

/// Hide the overlay instead of closing it so the window can be reused; see `open_overlays`.
pub fn close_overlays(app: &AppHandle) {
    // The capture is closed: windows waiting to be shown are no longer shown.
    {
        let mut pendientes = PENDIENTES.lock();
        pendientes.generacion += 1;
        pendientes.ventanas.clear();
    }
    for (label, window) in app.webview_windows() {
        if label.starts_with(OVERLAY_PREFIX) {
            let _ = window.hide();
        }
    }
}

/// Where to park a monitor's overlay: outside all screens, but positioned so the nearest
/// monitor is its own (or one with the same scale). Windows assigns a window the DPI of the
/// nearest monitor; parking next to a monitor with another scale made Windows rescale the
/// overlay when it was moved onto its screen.
///
/// Tries above, left, below, and right of the whole desktop, aligned with the monitor, and
/// picks the first whose nearest monitor is its own, otherwise one with the same scale.
fn aparcadero_de(monitor: &MonitorInfo, todos: &[MonitorInfo]) -> PhysicalPosition<i32> {
    const MARGEN: i32 = 100;
    let izquierda = todos.iter().map(|m| m.x).min().unwrap_or(monitor.x);
    let arriba = todos.iter().map(|m| m.y).min().unwrap_or(monitor.y);
    let derecha = todos
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(monitor.x);
    let abajo = todos
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(monitor.y);
    let (ancho, alto) = (monitor.width as i32, monitor.height as i32);
    let candidatos = [
        (monitor.x, arriba - alto - MARGEN),
        (izquierda - ancho - MARGEN, monitor.y),
        (monitor.x, abajo + MARGEN),
        (derecha + MARGEN, monitor.y),
    ];
    let mas_cercano = |x: i32, y: i32| {
        todos.iter().min_by_key(|m| {
            distancia_entre(
                (x, y, x + ancho, y + alto),
                (m.x, m.y, m.x + m.width as i32, m.y + m.height as i32),
            )
        })
    };
    let elegido = candidatos
        .iter()
        .find(|(x, y)| mas_cercano(*x, *y).is_some_and(|m| m.id == monitor.id))
        .or_else(|| {
            candidatos.iter().find(|(x, y)| {
                mas_cercano(*x, *y).is_some_and(|m| (m.scale - monitor.scale).abs() < 0.01)
            })
        })
        .copied()
        .unwrap_or(candidatos[0]);
    PhysicalPosition::new(elegido.0, elegido.1)
}

/// Squared distance between two rectangles (left, top, right, bottom); zero if they touch or
/// overlap.
fn distancia_entre(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> i64 {
    let dx = i64::from((b.0 - a.2).max(a.0 - b.2).max(0));
    let dy = i64::from((b.1 - a.3).max(a.1 - b.3).max(0));
    dx * dx + dy * dy
}

/// The floating recording toolbar below the region. Windows are created on the main thread;
/// creating them from a command thread blocks waiting for the event loop.
///
/// The bar is excluded from capture because it can end up inside the recorded region (for
/// example when recording the full screen).
///
/// Returns the window label so the caller closes this bar and not a newer recording's bar.
pub fn open_recorder(app: &AppHandle, region: Rect) -> Result<String> {
    close_recorder(app);
    let label = format!(
        "{RECORDER_PREFIX}{}",
        OVERLAY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("recorder.html".into()))
        .additional_browser_args(NAVEGADOR_ARGS)
        .title("winshotx")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .visible(false)
        .inner_size(BARRA_ANCHO, BARRA_ALTO)
        .build()?;
    crate::platform::window_style::rounded_corners(&window);
    if !crate::platform::window_style::fuera_de_la_captura(&window, true) {
        // If Windows does not support capture exclusion, the bar may appear in the video when
        // it overlaps; recording still works.
        eprintln!("[record] Windows refused to exclude the recording toolbar from capture");
    }

    colocar_barra(&window, region);
    window.show()?;
    Ok(label)
}

/// Close a specific recording toolbar.
pub fn close_recorder_label(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.close();
    }
}

/// Place the bar below the region, on the screen that contains it. Desktop coordinates can be
/// negative for monitors left of or above the primary, so they must not be clamped to zero; the
/// scale comes from the target monitor, not the newly created window.
fn colocar_barra(window: &tauri::WebviewWindow, region: Rect) {
    let centro = (
        region.x + region.width as i32 / 2,
        region.y + region.height as i32 / 2,
    );
    let monitor = monitor_de(centro.0, centro.1);
    let (x, y) = sitio_de_la_barra(region, monitor);
    let _ = window.set_position(PhysicalPosition::new(x, y));
    let _ = window.set_size(medida_de_la_barra(monitor));
}

/// The bar position calculation, separate so it can be tested without screens. If the monitor
/// is unknown, it goes right below the region.
fn medida_de_la_barra(monitor: Option<MonitorSitio>) -> PhysicalSize<u32> {
    let scale = monitor.map(|m| m.escala).unwrap_or(1.0);
    let width = (BARRA_ANCHO * scale) as i32;
    let height = (BARRA_ALTO * scale) as i32;
    let margin = (8.0 * scale) as i32;
    PhysicalSize::new(
        monitor
            .map(|m| width.min((m.ancho - margin * 2).max(1)))
            .unwrap_or(width) as u32,
        monitor
            .map(|m| height.min((m.alto - margin * 2).max(1)))
            .unwrap_or(height) as u32,
    )
}

fn sitio_de_la_barra(region: Rect, monitor: Option<MonitorSitio>) -> (i32, i32) {
    let escala = monitor.map(|m| m.escala).unwrap_or(1.0);
    let size = medida_de_la_barra(monitor);
    let ancho_px = size.width as i32;
    let alto_px = size.height as i32;
    let hueco = (12.0 * escala) as i32;

    let mut x = region.x + (region.width as i32 - ancho_px) / 2;
    let mut y = region.y + region.height as i32 + hueco;

    if let Some(m) = monitor {
        let margen = (8.0 * escala) as i32;
        let izquierda = m.x + margen;
        let derecha = (m.x + m.ancho - ancho_px - margen).max(izquierda);
        x = x.clamp(izquierda, derecha);
        // If the region reaches the bottom edge, the bar goes above it instead.
        let tope = m.y + m.alto - alto_px - margen;
        if y > tope {
            y = (region.y - alto_px - hueco).max(m.y + margen);
        }
    }
    (x, y)
}

/// Recording toolbar size in logical pixels. Must match the window builder or the bar is no
/// longer centered.
const BARRA_ANCHO: f64 = 540.0;
const BARRA_ALTO: f64 = 56.0;

/// The monitor data needed to place a window on it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MonitorSitio {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) ancho: i32,
    pub(crate) alto: i32,
    pub(crate) escala: f64,
}

/// The monitor containing that desktop point, or the primary one if the point falls between
/// misaligned monitors.
pub(crate) fn monitor_de(x: i32, y: i32) -> Option<MonitorSitio> {
    let monitores = xcap::Monitor::all().ok()?;
    let leer = |m: &xcap::Monitor| -> Option<MonitorSitio> {
        Some(MonitorSitio {
            x: m.x().ok()?,
            y: m.y().ok()?,
            ancho: m.width().ok()? as i32,
            alto: m.height().ok()? as i32,
            escala: f64::from(m.scale_factor().ok()?),
        })
    };
    let dentro = |s: &MonitorSitio| x >= s.x && x < s.x + s.ancho && y >= s.y && y < s.y + s.alto;

    monitores
        .iter()
        .filter_map(leer)
        .find(dentro)
        .or_else(|| monitores.iter().filter_map(leer).next())
}

pub fn close_recorder(app: &AppHandle) {
    for (label, window) in app.webview_windows() {
        if label.starts_with(RECORDER_PREFIX) {
            let _ = window.close();
        }
    }
}

/// The settings window is hidden, not destroyed, so its UI keeps startup data. Notify it when
/// it is shown again so it can refresh.
pub fn show_settings(app: &AppHandle) -> Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.show()?;
        let _ = window.unminimize();
        let _ = window.set_focus();
        // `show` and `set_focus` are not enough when the request does not come from a click in
        // our window (tray menu, second instance): Windows refuses foreground activation and
        // the window stayed hidden.
        #[cfg(windows)]
        force_foreground(&window);
        let _ = window.emit(crate::EVENT_SETTINGS_SHOWN, ());
    }
    Ok(())
}

/// Windows only allows taking the foreground when entitled to it; we are after a global
/// shortcut, but it must be requested explicitly.
#[cfg(windows)]
fn force_foreground(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_SHOW};

    let Ok(handle) = window.hwnd() else { return };
    unsafe {
        let hwnd = HWND(handle.0);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}
