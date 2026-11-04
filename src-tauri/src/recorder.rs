use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::capture::Rect;
use crate::error::{AppError, Result};
use crate::record::stream_encoder::{self, AudioFormat, AudioTimeline, Encoder, OutputPaths};
use crate::state::{AppState, Cabecera, RecordingState};
use crate::windows_mgr;
use std::path::PathBuf;

pub const EVENT_TICK: &str = "winshotx://recording-tick";
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOptions {
    pub format: String,
    pub audio: bool,
    #[serde(default)]
    pub microphone: bool,
    #[serde(default)]
    pub window: Option<isize>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingTick {
    pub elapsed_ms: u64,
    pub frames: u64,
    pub bytes: u64,
    pub paused: bool,
    pub saving: bool,
    pub format: String,
    pub audio: bool,
    pub microphone: bool,
    pub output_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub region: Rect,
    pub fps: u32,
    pub frame_count: u32,
    pub duration_ms: u64,
    pub has_audio: bool,
    pub format: String,
    pub mp4_path: Option<String>,
}
fn monitor_origin(app: &AppHandle, region: Rect) -> (i32, i32) {
    let state = app.state::<AppState>();
    let (cx, cy) = region.center();
    if let Some(freeze) = state
        .freezes
        .read()
        .iter()
        .find(|f| f.monitor.contains(cx, cy))
    {
        return (freeze.monitor.x, freeze.monitor.y);
    }
    xcap::Monitor::from_point(cx, cy)
        .ok()
        .and_then(|m| Some((m.x().ok()?, m.y().ok()?)))
        .unwrap_or((0, 0))
}

#[cfg(windows)]
pub fn start(app: &AppHandle, region: Rect, options: RecordOptions) -> Result<SessionInfo> {
    use crate::record::win::{self, CaptureFlags};
    use std::sync::mpsc::sync_channel;
    if options.format != "video" {
        return Err(AppError::Msg(
            "Only MP4 video recording is supported.".into(),
        ));
    }
    let state = app.state::<AppState>();
    let mut slot = state.recording.lock();
    if slot.is_some() {
        return Err(AppError::Msg("A recording is already in progress".into()));
    }
    let settings = state.settings.read().clone();
    let encoding = stream_encoder::streaming_options(&settings.encoding);
    crate::encode::ffmpeg::validate(&encoding)?;
    crate::audio_devices::stop(None);
    let region = region.to_even();
    // The active settings decide the frame rate (presets change it), not the caller.
    let fps = settings.fps.clamp(5, 120);
    let origin = monitor_origin(app, region);
    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let paused_ms = Arc::new(AtomicU64::new(0));
    let fin_ms = Arc::new(AtomicU64::new(0));
    let discard = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let sources = crate::record::audio::Fuentes {
        sistema: options.audio,
        microfono: options.microphone,
    };
    let audio = if sources.ninguna() {
        None
    } else {
        Some(crate::record::audio::empezar_con_dispositivos(
            sources,
            pause.clone(),
            settings.system_audio_device.clone(),
            settings.microphone_device.clone(),
        )?)
    };
    let started = audio
        .as_ref()
        .map(|a| a.started)
        .unwrap_or_else(Instant::now);
    let audio_format = audio.as_ref().map(|a| AudioFormat {
        channels: a.formato.canales,
        sample_rate: a.formato.muestras_por_segundo,
    });
    let paths = OutputPaths::reserve(std::path::Path::new(&settings.video_directory))?;
    let encoder = match Encoder::start(
        paths.clone(),
        &encoding,
        region.width,
        region.height,
        fps,
        audio_format,
    ) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = std::fs::remove_file(&paths.partial);
            return Err(error);
        }
    };
    windows_mgr::close_overlays(app);
    let (sender, receiver) = sync_channel(2);
    let recycled = win::Reciclados::default();
    let flags = CaptureFlags {
        sender,
        crop: (0, 0, 0, 0),
        stop: stop.clone(),
        pause: pause.clone(),
        min_interval: Duration::ZERO,
        reciclados: recycled.clone(),
    };
    let capture = match options.window.filter(|h| *h != 0) {
        Some(hwnd) => win::start_window(hwnd, settings.capture_cursor, fps, flags),
        None => win::start(region, origin, settings.capture_cursor, fps, flags),
    };
    let control = match capture {
        Ok(c) => c,
        Err(e) => {
            encoder.cancel();
            return Err(e);
        }
    };
    let writer_stop = stop.clone();
    let writer = {
        let timing = WriterTiming {
            started,
            fps,
            stop: stop.clone(),
            pause: pause.clone(),
            paused_ms: paused_ms.clone(),
            finish_ms: fin_ms.clone(),
            discard: discard.clone(),
            frames: frames.clone(),
            bytes: bytes.clone(),
        };
        std::thread::spawn(move || {
            let path = encoder.path().to_owned();
            let result = write_recording(encoder, receiver, audio, recycled, timing);
            writer_stop.store(true, Ordering::Release);
            result.map_err(|e| {
                AppError::Msg(format!(
                    "{e}\n\nRecording fragments retained at {}",
                    path.display()
                ))
            })
        })
    };
    let marco = crate::platform::marco::Marco::abrir(region, grosor_del_marco(region)).ok();
    *slot = Some(RecordingState {
        output: paths.partial.clone(),
        play_sound: settings.play_sound,
        started,
        stop: stop.clone(),
        pause,
        paused_ms,
        pause_started: parking_lot::Mutex::new(None),
        fin_ms,
        frames,
        bytes,
        cabecera: Cabecera {
            format: "video".into(),
            audio: sources.sistema,
            microphone: sources.microfono,
        },
        descartar: discard,
        barra: None,
        control: Some(control),
        marco,
        writer: Some(writer),
    });
    drop(slot);
    match windows_mgr::open_recorder(app, region) {
        Ok(label) => {
            if let Some(recording) = state.recording.lock().as_mut() {
                recording.barra = Some(label);
            }
        }
        Err(error) => eprintln!("Could not show the recording toolbar: {error}"),
    }
    spawn_ticker(app.clone(), paths.partial.clone());
    Ok(SessionInfo {
        id: uuid::Uuid::new_v4().to_string(),
        region,
        fps,
        frame_count: 0,
        duration_ms: 0,
        has_audio: audio_format.is_some(),
        format: "video".into(),
        mp4_path: Some(paths.partial.to_string_lossy().into_owned()),
    })
}

#[cfg(windows)]
struct WriterTiming {
    started: Instant,
    fps: u32,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    paused_ms: Arc<AtomicU64>,
    finish_ms: Arc<AtomicU64>,
    discard: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

#[cfg(windows)]
fn write_recording(
    mut encoder: Encoder,
    receiver: std::sync::mpsc::Receiver<crate::record::win::CapturedFrame>,
    audio: Option<crate::record::audio::Captura>,
    recycled: crate::record::win::Reciclados,
    timing: WriterTiming,
) -> Result<Option<PathBuf>> {
    let mut picture: Option<Vec<u8>> = None;
    let mut sent = 0u64;
    let mut active = Duration::ZERO;
    let mut timeline = audio.as_ref().map(|a| {
        AudioTimeline::new(AudioFormat {
            channels: a.formato.canales,
            sample_rate: a.formato.muestras_por_segundo,
        })
    });
    loop {
        if timing.discard.load(Ordering::Acquire) {
            encoder.cancel();
            return Ok(None);
        }
        // Keep the newest frame and reuse it at output cadence while the source is static.
        while let Ok(frame) = receiver.try_recv() {
            if let Some(old) = picture.replace(frame.bgra) {
                recycled.devolver(old);
            }
            timing.frames.fetch_add(1, Ordering::Relaxed);
        }
        if let (Some(source), Some(timeline)) = (audio.as_ref(), timeline.as_mut()) {
            loop {
                match source.trozos.try_recv() {
                    Ok(chunk) => {
                        let sample = ((chunk.desde_el_inicio.max(0) as u128
                            * source.formato.muestras_por_segundo as u128
                            + 5_000_000)
                            / 10_000_000) as u64;
                        timeline.push(sample, &crate::record::audio::a_pcm16(&chunk.datos))?;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Err(AppError::Msg(
                            "The selected audio device stopped delivering audio.".into(),
                        ))
                    }
                }
            }
        }
        let stopping = timing.stop.load(Ordering::Acquire);
        if stopping {
            let duration = timing.finish_ms.load(Ordering::Acquire);
            if duration > 0 {
                active = Duration::from_millis(duration);
            }
        } else if !timing.pause.load(Ordering::Acquire) {
            active = timing
                .started
                .elapsed()
                .saturating_sub(Duration::from_millis(
                    timing.paused_ms.load(Ordering::Acquire),
                ));
        }
        let due = stream_encoder::frames_at(active, timing.fps).max(1);
        if due.saturating_sub(sent) > u64::from(timing.fps) * 2 && picture.is_some() && sent > 0 {
            return Err(AppError::Msg("The encoder fell more than two seconds behind. Choose a faster preset, hardware encoder, or lower frame rate.".into()));
        }
        if let Some(picture) = picture.as_ref() {
            // FFmpeg opens/probes its inputs sequentially. Prime the actual initial
            // audio before a large first video write can fill the TCP send buffer.
            // Keep this short startup data in memory, never insert artificial lead-in.
            if sent == 0 && timeline.is_some() && active < Duration::from_millis(150) && !stopping {
                encoder.check()?;
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            // Audio is held for 150ms to absorb WASAPI packet/silence-fill jitter.
            // Finalization pads/trims to exactly the same active-time clock as video.
            if let Some(timeline) = timeline.as_mut() {
                let time = if stopping || sent == 0 {
                    active
                } else {
                    active.saturating_sub(Duration::from_millis(150))
                };
                let target = timeline.frames_at(time);
                while timeline.cursor < target {
                    encoder.audio(timeline.next(target))?;
                }
            }
            if stopping {
                encoder.end_audio();
            }
            while sent < due {
                if !encoder.try_video(picture)? {
                    break;
                }
                sent += 1;
            }
        } else if stopping || timing.started.elapsed() > Duration::from_secs(10) {
            return Err(AppError::Msg(
                "Windows did not deliver a video frame. Nothing could be recorded.".into(),
            ));
        }
        timing.bytes.store(encoder.bytes(), Ordering::Relaxed);
        encoder.check()?;
        if stopping && sent >= due {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Stop the endpoint threads before closing the encoder's sockets.
    drop(audio);
    encoder.finish().map(Some)
}

#[cfg(windows)]
fn grosor_del_marco(region: Rect) -> i32 {
    let (cx, cy) = region.center();
    let scale = windows_mgr::monitor_de(cx, cy)
        .map(|m| m.escala)
        .unwrap_or(1.0);
    (2.0 * scale).round().max(1.0) as i32
}

#[cfg(not(windows))]
pub fn start(_app: &AppHandle, _region: Rect, _options: RecordOptions) -> Result<SessionInfo> {
    Err(AppError::Unsupported)
}
fn tick_de(recording: &RecordingState, saving: bool) -> RecordingTick {
    RecordingTick {
        elapsed_ms: recording.elapsed_ms(),
        frames: recording.frames.load(Ordering::Relaxed),
        bytes: recording.bytes.load(Ordering::Relaxed),
        paused: recording.pause.load(Ordering::Relaxed),
        saving,
        format: recording.cabecera.format.clone(),
        audio: recording.cabecera.audio,
        microphone: recording.cabecera.microphone,
        output_path: recording.output.to_string_lossy().into_owned(),
    }
}
fn avisar_barra(app: &AppHandle, tick: RecordingTick) {
    for (label, window) in app.webview_windows() {
        if label.starts_with(windows_mgr::RECORDER_PREFIX) {
            let _ = window.emit(EVENT_TICK, tick.clone());
        }
    }
}

fn spawn_ticker(app: AppHandle, output: PathBuf) {
    std::thread::spawn(move || loop {
        let tick = {
            let state = app.state::<AppState>();
            let guard = state.recording.lock();
            guard.as_ref().filter(|r| r.output == output).map(|r| {
                (
                    tick_de(r, false),
                    r.writer.as_ref().is_some_and(|w| w.is_finished()),
                )
            })
        };
        match tick {
            Some((_, true)) => {
                let _ = stop(&app);
                break;
            }
            Some((tick, false)) => avisar_barra(&app, tick),
            None => break,
        }
        std::thread::sleep(Duration::from_millis(200));
    });
}

fn finish(mut recording: RecordingState) -> Result<Option<PathBuf>> {
    recording
        .fin_ms
        .store(recording.elapsed_ms(), Ordering::Release);
    recording.stop.store(true, Ordering::Release);
    #[cfg(windows)]
    if let Some(control) = recording.control.take() {
        std::thread::spawn(move || {
            let _ = control.stop();
        });
    }
    #[cfg(windows)]
    drop(recording.marco.take());
    recording
        .writer
        .take()
        .ok_or_else(|| AppError::Msg("The recording writer is unavailable".into()))?
        .join()
        .map_err(|_| {
            AppError::Msg(format!(
                "The encoder thread stopped unexpectedly. Recording retained at {}",
                recording.output.display()
            ))
        })?
}

pub fn stop(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let recording = state.recording.lock().take().ok_or(AppError::NoRecording)?;
    avisar_barra(app, tick_de(&recording, true));
    let bar = recording.barra.clone();
    let play_sound = recording.play_sound;
    let saving = state.saving.clone();
    saving.fetch_add(1, Ordering::SeqCst);
    let app = app.clone();
    std::thread::spawn(move || {
        match finish(recording) {
            Ok(Some(path)) => {
                if play_sound {
                    crate::platform::sonido::obturador();
                }
                let _ = app.emit(
                    "winshotx://recording-saved",
                    path.to_string_lossy().into_owned(),
                );
            }
            Ok(None) => {}
            Err(error) => {
                use tauri_plugin_dialog::DialogExt;
                app.dialog()
                    .message(error.to_string())
                    .title("Recording could not be completed")
                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
        close_bar(&app, bar.as_deref());
        saving.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(())
}
fn close_bar(app: &AppHandle, bar: Option<&str>) {
    match bar {
        Some(label) => windows_mgr::close_recorder_label(app, label),
        None => windows_mgr::close_recorder(app),
    }
}
pub fn cancel(app: &AppHandle) -> Result<()> {
    let recording = app
        .state::<AppState>()
        .recording
        .lock()
        .take()
        .ok_or(AppError::NoRecording)?;
    recording.descartar.store(true, Ordering::Release);
    let bar = recording.barra.clone();
    let partial = recording.output.clone();
    let app = app.clone();
    std::thread::spawn(move || {
        match finish(recording) {
            Ok(Some(path)) => {
                let _ = std::fs::remove_file(path);
            }
            Err(error) => eprintln!("Recording stopped while discarding: {error}"),
            _ => {}
        }
        let _ = std::fs::remove_file(partial);
        close_bar(&app, bar.as_deref());
    });
    Ok(())
}
pub fn set_paused(app: &AppHandle, paused: bool) -> Result<()> {
    let state = app.state::<AppState>();
    let guard = state.recording.lock();
    let recording = guard.as_ref().ok_or(AppError::NoRecording)?;
    let mut pause_started = recording.pause_started.lock();
    if paused {
        if pause_started.is_none() {
            *pause_started = Some(Instant::now());
        }
    } else if let Some(started) = pause_started.take() {
        recording
            .paused_ms
            .fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    recording.pause.store(paused, Ordering::Relaxed);
    #[cfg(windows)]
    if let Some(marco) = recording.marco.as_ref() {
        marco.pausado(paused);
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod streaming_tests {
    use super::*;
    #[test]
    #[ignore = "Runs the actual recording writer and installed FFmpeg"]
    fn static_source_pause_resume_and_immediate_stop_have_no_spool() {
        use crate::record::win::{CapturedFrame, Reciclados};
        let dir =
            std::env::temp_dir().join(format!("winshotx-clock-test-{}", uuid::Uuid::new_v4()));
        let paths = OutputPaths::reserve(&dir).unwrap();
        let mut options =
            stream_encoder::streaming_options(&crate::settings::EncodingSettings::default());
        options.preset = "ultrafast".into();
        let encoder = Encoder::start(paths.clone(), &options, 320, 180, 30, None).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let paused_ms = Arc::new(AtomicU64::new(0));
        let finish_ms = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let timing = WriterTiming {
            started: Instant::now(),
            fps: 30,
            stop: stop.clone(),
            pause: pause.clone(),
            paused_ms: paused_ms.clone(),
            finish_ms: finish_ms.clone(),
            discard: Arc::new(AtomicBool::new(false)),
            frames: frames.clone(),
            bytes: bytes.clone(),
        };
        tx.send(CapturedFrame {
            bgra: vec![80; 320 * 180 * 4],
        })
        .unwrap();
        let writer = std::thread::spawn(move || {
            write_recording(encoder, rx, None, Reciclados::default(), timing)
        });
        std::thread::sleep(Duration::from_millis(600));
        pause.store(true, Ordering::Release);
        let pause_start = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        paused_ms.store(pause_start.elapsed().as_millis() as u64, Ordering::Release);
        pause.store(false, Ordering::Release);
        std::thread::sleep(Duration::from_millis(600));
        finish_ms.store(1200, Ordering::Release);
        stop.store(true, Ordering::Release);
        let path = writer.join().unwrap().unwrap().unwrap();
        drop(tx);
        assert_eq!(
            frames.load(Ordering::Relaxed),
            1,
            "The source should remain static"
        );
        let decoded = crate::encode::ffmpeg::command(&options.ffmpeg_path)
            .args(["-hide_banner", "-i"])
            .arg(&path)
            .args(["-progress", "pipe:1", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let progress = String::from_utf8_lossy(&decoded.stdout);
        let count: u32 = progress
            .lines()
            .filter_map(|l| l.strip_prefix("frame="))
            .last()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (36..=38).contains(&count),
            "Paused time leaked into the video: {count} frames"
        );
        assert!(!paths.partial.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "Opens the default playback endpoint and runs the actual live writer"]
    fn live_large_frames_with_wasapi_audio_pause_and_resume() {
        use crate::record::win::{CapturedFrame, Reciclados};
        let dir =
            std::env::temp_dir().join(format!("winshotx-audio-clock-{}", uuid::Uuid::new_v4()));
        let paths = OutputPaths::reserve(&dir).unwrap();
        let pause = Arc::new(AtomicBool::new(false));
        let audio = crate::record::audio::empezar_con_dispositivos(
            crate::record::audio::Fuentes {
                sistema: true,
                microfono: false,
            },
            pause.clone(),
            None,
            None,
        )
        .unwrap();
        let started = audio.started;
        let format = AudioFormat {
            channels: audio.formato.canales,
            sample_rate: audio.formato.muestras_por_segundo,
        };
        let mut options = crate::settings::EncodingSettings::default();
        options.preset = "ultrafast".into();
        let encoder = Encoder::start(paths.clone(), &options, 1280, 720, 30, Some(format)).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        tx.send(CapturedFrame {
            bgra: vec![80; 1280 * 720 * 4],
        })
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let paused_ms = Arc::new(AtomicU64::new(0));
        let finish_ms = Arc::new(AtomicU64::new(0));
        let timing = WriterTiming {
            started,
            fps: 30,
            stop: stop.clone(),
            pause: pause.clone(),
            paused_ms: paused_ms.clone(),
            finish_ms: finish_ms.clone(),
            discard: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            bytes: Arc::new(AtomicU64::new(0)),
        };
        let writer = std::thread::spawn(move || {
            write_recording(encoder, rx, Some(audio), Reciclados::default(), timing)
        });
        std::thread::sleep(Duration::from_millis(1000));
        pause.store(true, Ordering::Release);
        let pause_start = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        let pause_duration = pause_start.elapsed().as_millis() as u64;
        paused_ms.store(pause_duration, Ordering::Release);
        pause.store(false, Ordering::Release);
        std::thread::sleep(Duration::from_millis(1000));
        let active_ms = started.elapsed().as_millis() as u64 - pause_duration;
        finish_ms.store(active_ms, Ordering::Release);
        stop.store(true, Ordering::Release);
        let path = writer.join().unwrap().unwrap().unwrap();
        drop(tx);
        let decoded = crate::encode::ffmpeg::command(&options.ffmpeg_path)
            .args(["-hide_banner", "-i"])
            .arg(&path)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-progress",
                "pipe:1",
                "-f",
                "null",
                "-",
            ])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let progress = String::from_utf8_lossy(&decoded.stdout);
        let count: u64 = progress
            .lines()
            .filter_map(|l| l.strip_prefix("frame="))
            .last()
            .unwrap()
            .parse()
            .unwrap();
        let expected = active_ms * 30 / 1000;
        assert!(
            (expected..=expected + 2).contains(&count),
            "{active_ms}ms, {count} frames"
        );
        assert!(String::from_utf8_lossy(&decoded.stderr).contains("Audio: aac"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        println!("720p with real WASAPI audio: {active_ms}ms active, {count} frames, pause excluded, no spool");
        let _ = std::fs::remove_dir_all(dir);
    }
}
