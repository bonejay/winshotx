use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::capture::{Freeze, Rect};
use crate::error::Result;

use crate::settings::Settings;
use crate::windows_mgr::OverlayIntent;
use tauri_plugin_global_shortcut::Shortcut;

/// Recording details shown in the recording bar that do not change during a recording. Sent
/// with every tick.
#[derive(Debug, Clone)]
pub struct Cabecera {
    pub format: String,
    /// Whether system audio was actually opened.
    pub audio: bool,
    /// Whether the microphone was actually opened.
    pub microphone: bool,
}

/// State shared with the capture and encoder threads while recording.
pub struct RecordingState {
    pub output: PathBuf,
    pub play_sound: bool,
    pub started: Instant,
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub paused_ms: Arc<AtomicU64>,
    pub pause_started: Mutex<Option<Instant>>,
    /// Active (unpaused) duration at the moment of stopping, written before the writer is told
    /// to stop. 0 while recording.
    pub fin_ms: Arc<AtomicU64>,
    pub frames: Arc<AtomicU64>,
    pub bytes: Arc<AtomicU64>,
    pub cabecera: Cabecera,
    /// Set when the recording is discarded instead of saved.
    pub descartar: Arc<AtomicBool>,
    /// Label of this recording's toolbar window, so the right one is closed.
    pub barra: Option<String>,
    #[cfg(windows)]
    pub control: Option<crate::record::win::Control>,
    /// The border drawn around the recorded region. Closes on drop.
    #[cfg(windows)]
    pub marco: Option<crate::platform::marco::Marco>,
    pub writer: Option<JoinHandle<Result<Option<PathBuf>>>>,
}

impl RecordingState {
    /// Recorded time excluding all pauses, including one in progress.
    pub fn elapsed_ms(&self) -> u64 {
        tiempo_grabado(
            self.started.elapsed(),
            self.paused_ms.load(Ordering::Relaxed),
            self.pause_started.lock().map(|desde| desde.elapsed()),
        )
    }
}

/// Elapsed time minus paused time. The current pause is subtracted separately because
/// `paused_ms` is only updated on resume.
pub fn tiempo_grabado(
    desde_el_inicio: Duration,
    pausado_ms: u64,
    pausa_en_curso: Option<Duration>,
) -> u64 {
    (desde_el_inicio.as_millis() as u64)
        .saturating_sub(pausado_ms)
        .saturating_sub(pausa_en_curso.map(|p| p.as_millis() as u64).unwrap_or(0))
}

pub struct AppState {
    pub settings: RwLock<Settings>,
    /// Frozen screens of the current capture, one per monitor.
    pub freezes: RwLock<Vec<Freeze>>,
    pub shortcuts: RwLock<crate::hotkeys::ShortcutStatus>,
    /// Shortcuts we registered, so they can be unregistered one by one (see hotkeys.rs).
    pub registered: RwLock<Vec<Shortcut>>,
    /// Whether the open overlay is for a screenshot or a recording.
    pub intent: RwLock<OverlayIntent>,
    pub recording: Mutex<Option<RecordingState>>,
    pub saving: Arc<AtomicU64>,
    /// The last captured region in virtual-desktop coordinates. Kept after the overlay closes
    /// so the next capture can reuse it.
    pub last_region: RwLock<Option<Rect>>,
    /// Set while a screenshot is being frozen or saved, so pressing the shortcut twice quickly
    /// does not start two concurrent captures. In an `Arc` because with a delay the lock is
    /// held across threads.
    capturando: Arc<AtomicBool>,
}

/// Releases the `capturando` lock on drop, so an early `?` cannot leave it set.
pub struct CandadoCaptura(Arc<AtomicBool>);

impl Drop for CandadoCaptura {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl AppState {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings: RwLock::new(settings),
            freezes: RwLock::new(Vec::new()),
            shortcuts: RwLock::new(crate::hotkeys::ShortcutStatus::default()),
            registered: RwLock::new(Vec::new()),
            intent: RwLock::new(OverlayIntent::Capture),
            recording: Mutex::new(None),
            saving: Arc::new(AtomicU64::new(0)),
            last_region: RwLock::new(None),
            capturando: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.lock().is_some()
    }

    /// `None` if a capture is already in progress.
    pub fn intentar_capturar(&self) -> Option<CandadoCaptura> {
        self.capturando
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| CandadoCaptura(Arc::clone(&self.capturando)))
    }

}
