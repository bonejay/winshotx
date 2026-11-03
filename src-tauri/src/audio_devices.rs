//! Endpoint selection and an explicit, temporary audio level check. No samples are saved.
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub is_default: bool,
}

#[cfg(windows)]
pub fn list() -> Result<Vec<AudioDevice>> {
    use windows::Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            eCapture, eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
            DEVICE_STATE_ACTIVE,
        },
        System::Com::{
            CoCreateInstance, CoTaskMemFree,
            StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
            CLSCTX_ALL, STGM_READ,
        },
    };
    let _com = crate::record::audio::Com::inicializar()?;
    unsafe fn id(device: &IMMDevice) -> Result<String> {
        let ptr = device.GetId()?;
        let value = ptr.to_string();
        CoTaskMemFree(Some(ptr.0.cast()));
        value.map_err(|error| AppError::Msg(format!("Invalid audio device ID: {error}")))
    }
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut devices = Vec::new();
        for (flow, kind) in [(eRender, "output"), (eCapture, "input")] {
            let default_id = enumerator
                .GetDefaultAudioEndpoint(flow, eConsole)
                .ok()
                .and_then(|d| id(&d).ok());
            let collection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            for index in 0..collection.GetCount()? {
                let device = collection.Item(index)?;
                let device_id = id(&device)?;
                let name = (|| -> Result<String> {
                    let store = device.OpenPropertyStore(STGM_READ)?;
                    let mut value = store.GetValue(&PKEY_Device_FriendlyName)?;
                    let string = PropVariantToStringAlloc(&value);
                    let _ = PropVariantClear(&mut value);
                    let ptr = string?;
                    let result = ptr.to_string();
                    CoTaskMemFree(Some(ptr.0.cast()));
                    result.map_err(|error| {
                        AppError::Msg(format!("Invalid audio device name: {error}"))
                    })
                })()
                .unwrap_or_else(|_| format!("Audio {kind} {}", index + 1));
                devices.push(AudioDevice {
                    is_default: default_id.as_ref() == Some(&device_id),
                    id: device_id,
                    name,
                    kind: kind.into(),
                });
            }
        }
        Ok(devices)
    }
}
#[cfg(not(windows))]
pub fn list() -> Result<Vec<AudioDevice>> {
    Err(AppError::Unsupported)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub peak: f32,
    pub db: f32,
    pub state: String,
    pub error: Option<String>,
}
impl Level {
    fn new(enabled: bool) -> Self {
        Self {
            peak: 0.0,
            db: -100.0,
            state: if enabled { "connecting" } else { "disabled" }.into(),
            error: None,
        }
    }
    fn set_peak(&mut self, peak: f32) {
        self.peak = if peak.is_finite() {
            peak.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.db = if self.peak > 0.00001 {
            20.0 * self.peak.log10()
        } else {
            -100.0
        };
    }
    fn fail(&mut self, error: String) {
        self.state = "error".into();
        self.error = Some(error);
        self.set_peak(0.0);
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Levels {
    pub id: String,
    pub running: bool,
    pub system: Level,
    pub microphone: Level,
}
struct Shared {
    stop: AtomicBool,
    running: AtomicBool,
    last_poll: Mutex<Instant>,
    system: Mutex<Level>,
    microphone: Mutex<Level>,
}
struct Preview {
    id: String,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
static ACTIVE: Mutex<Option<Preview>> = Mutex::new(None);

#[cfg(windows)]
pub fn start(
    system: bool,
    microphone: bool,
    system_device: Option<String>,
    microphone_device: Option<String>,
) -> Result<String> {
    if !system && !microphone {
        return Err(AppError::Msg(
            "Enable system audio or microphone to check levels.".into(),
        ));
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        running: AtomicBool::new(true),
        last_poll: Mutex::new(Instant::now()),
        system: Mutex::new(Level::new(system)),
        microphone: Mutex::new(Level::new(microphone)),
    });
    let worker = shared.clone();
    let thread = std::thread::spawn(move || {
        use crate::record::audio::{empezar_con_dispositivos, Captura, Fuentes};
        let pause = Arc::new(AtomicBool::new(false));
        let open = |enabled: bool, is_system: bool, level: &Mutex<Level>| -> Option<Captura> {
            if !enabled {
                return None;
            }
            match empezar_con_dispositivos(
                Fuentes {
                    sistema: is_system,
                    microfono: !is_system,
                },
                pause.clone(),
                system_device.clone(),
                microphone_device.clone(),
            ) {
                Ok(capture) => {
                    level.lock().state = "ready".into();
                    Some(capture)
                }
                Err(error) => {
                    level.lock().fail(error.to_string());
                    None
                }
            }
        };
        let output = open(system, true, &worker.system);
        let input = open(microphone, false, &worker.microphone);
        while !worker.stop.load(Ordering::Relaxed)
            && worker.last_poll.lock().elapsed() < Duration::from_secs(3)
        {
            for (capture, level) in [(&output, &worker.system), (&input, &worker.microphone)] {
                let Some(capture) = capture else {
                    continue;
                };
                let mut peak = 0.0f32;
                loop {
                    match capture.trozos.try_recv() {
                        Ok(chunk) => {
                            peak = peak.max(sample_peak(&chunk.datos));
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            level.lock().fail("The audio device stopped responding. Refresh devices and try again.".into());
                            break;
                        }
                    }
                }
                let mut value = level.lock();
                if value.state == "ready" {
                    let smoothed = peak.max(value.peak * 0.65);
                    value.set_peak(smoothed);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        drop(output);
        drop(input);
        worker.running.store(false, Ordering::Relaxed);
    });
    let old = ACTIVE.lock().replace(Preview {
        id: id.clone(),
        shared,
        thread: Some(thread),
    });
    drop(old);
    Ok(id)
}
#[cfg(not(windows))]
pub fn start(
    _system: bool,
    _microphone: bool,
    _system_device: Option<String>,
    _microphone_device: Option<String>,
) -> Result<String> {
    Err(AppError::Unsupported)
}

fn sample_peak(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .filter(|v| v.is_finite())
        .fold(0.0f32, |peak, value| peak.max(value.abs()))
        .clamp(0.0, 1.0)
}
pub fn levels(id: &str) -> Result<Levels> {
    let active = ACTIVE.lock();
    let preview = active
        .as_ref()
        .filter(|p| p.id == id)
        .ok_or_else(|| AppError::Msg("Audio level check has ended.".into()))?;
    *preview.shared.last_poll.lock() = Instant::now();
    let system = preview.shared.system.lock().clone();
    let microphone = preview.shared.microphone.lock().clone();
    Ok(Levels {
        id: id.into(),
        running: preview.shared.running.load(Ordering::Relaxed),
        system,
        microphone,
    })
}
pub fn stop(id: Option<&str>) {
    let old = {
        let mut active = ACTIVE.lock();
        if active
            .as_ref()
            .is_some_and(|p| id.is_none_or(|id| p.id == id))
        {
            active.take()
        } else {
            None
        }
    };
    drop(old);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meter_handles_silence_clipping_and_invalid_samples() {
        let bytes: Vec<u8> = [0.0f32, -0.5, f32::NAN]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(sample_peak(&bytes), 0.5);
        let mut level = Level::new(true);
        level.set_peak(sample_peak(&bytes));
        assert!((level.db + 6.0206).abs() < 0.001);
        level.set_peak(0.0);
        assert_eq!(level.db, -100.0);
        level.set_peak(2.0);
        assert_eq!(level.db, 0.0);
        level.set_peak(f32::NAN);
        assert!(level.db.is_finite());
    }
    #[test]
    fn stale_cleanup_cannot_stop_another_monitor_session() {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            running: AtomicBool::new(true),
            last_poll: Mutex::new(Instant::now()),
            system: Mutex::new(Level::new(false)),
            microphone: Mutex::new(Level::new(false)),
        });
        *ACTIVE.lock() = Some(Preview {
            id: "new".into(),
            shared: shared.clone(),
            thread: None,
        });
        stop(Some("old"));
        assert!(!shared.stop.load(Ordering::Relaxed));
        stop(Some("new"));
        assert!(shared.stop.load(Ordering::Relaxed));
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "Requires local Windows audio endpoints"]
    fn live_device_selection_and_preview_watchdog() {
        let devices = list().unwrap();
        let output = devices
            .iter()
            .find(|d| d.kind == "output" && d.is_default)
            .or_else(|| devices.iter().find(|d| d.kind == "output"));
        let input = devices
            .iter()
            .find(|d| d.kind == "input" && d.is_default)
            .or_else(|| devices.iter().find(|d| d.kind == "input"));
        assert!(
            output.is_some() || input.is_some(),
            "No audio endpoints are available"
        );
        let id = start(
            output.is_some(),
            input.is_some(),
            output.map(|d| d.id.clone()),
            input.map(|d| d.id.clone()),
        )
        .unwrap();
        let timeout = Instant::now() + Duration::from_secs(5);
        let snapshot = loop {
            let snapshot = levels(&id).unwrap();
            if snapshot.system.state != "connecting" && snapshot.microphone.state != "connecting" {
                break snapshot;
            }
            assert!(Instant::now() < timeout, "Audio check did not initialize");
            std::thread::sleep(Duration::from_millis(100));
        };
        if output.is_some() {
            assert_eq!(
                snapshot.system.state, "ready",
                "{:?}",
                snapshot.system.error
            );
        }
        if input.is_some() {
            assert_eq!(
                snapshot.microphone.state, "ready",
                "{:?}",
                snapshot.microphone.error
            );
        }
        println!(
            "{} endpoints enumerated; playback={}, microphone={}",
            devices.len(),
            snapshot.system.state,
            snapshot.microphone.state
        );
        std::thread::sleep(Duration::from_millis(3400));
        assert!(
            !levels(&id).unwrap().running,
            "The level check continued without polling"
        );
        stop(Some(&id));
        use crate::record::audio::{empezar_con_dispositivos, Fuentes};
        assert!(
            empezar_con_dispositivos(
                Fuentes {
                    sistema: true,
                    microfono: false
                },
                Arc::new(AtomicBool::new(false)),
                Some("missing-endpoint-id".into()),
                None
            )
            .is_err(),
            "A missing device must not fall back to another source"
        );
    }
}
