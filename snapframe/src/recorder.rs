use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::Rect;
use crate::error::{AppError, Result};
use crate::record::stream_encoder::{self, AudioFormat, AudioTimeline, Encoder, OutputPaths};
use crate::state::{AppState, Cabecera, RecordingState};
use std::path::PathBuf;

/// How the UI hears about a recording. The recorder never touches windows itself.
pub trait RecorderEvents: Send + Sync {
    /// About five times a second while recording.
    fn tick(&self, tick: RecordingTick);
    /// The recording ended: saved, discarded, or failed. The toolbar should close.
    fn finished(&self, outcome: Outcome);
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Saved(PathBuf),
    Discarded,
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct RecordOptions {
    pub audio: bool,
    pub microphone: bool,
    /// Record this window (HWND) instead of the screen area.
    pub window: Option<isize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordingTick {
    pub elapsed_ms: u64,
    pub frames: u64,
    pub bytes: u64,
    pub paused: bool,
    pub saving: bool,
    pub format: String,
    pub audio: bool,
    pub microphone: bool,
    /// Peak level since the previous tick, 0..1.
    pub audio_level: f32,
    pub microphone_level: f32,
    pub output_path: String,
}

/// What was started, so the UI can place the toolbar and the border.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub region: Rect,
    /// The concrete encoding used (automatic settings resolved for this region).
    pub encoding: crate::settings::EncodingSettings,
    pub fps: u32,
}

fn monitor_origin(state: &AppState, region: Rect) -> (i32, i32) {
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
pub fn start(
    state: &Arc<AppState>,
    events: Arc<dyn RecorderEvents>,
    region: Rect,
    options: RecordOptions,
) -> Result<SessionInfo> {
    use crate::record::win::{self, CaptureFlags};
    use std::sync::mpsc::sync_channel;
    let mut slot = state.recording.lock();
    if slot.is_some() {
        return Err(AppError::Msg("A recording is already in progress".into()));
    }
    if state.encoder_testing.load(Ordering::Acquire) {
        return Err(AppError::Msg(
            "Wait for the encoder test to finish before starting a recording.".into(),
        ));
    }
    let settings = state.settings.read().clone();
    let region = region.to_even();
    // The active settings decide the frame rate (presets change it), not the caller.
    let fps = settings.fps.clamp(5, 120);
    let encoding = crate::encode::auto::resolve_here(
        &stream_encoder::streaming_options(&settings.encoding),
        region.width,
        region.height,
        fps,
    );
    crate::encode::preflight::recording(&encoding, region.width, region.height, fps)?;
    crate::audio_devices::stop(None);
    let origin = monitor_origin(state, region);
    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let paused_ms = Arc::new(AtomicU64::new(0));
    let fin_ms = Arc::new(AtomicU64::new(0));
    let lead_us = Arc::new(AtomicU64::new(u64::MAX));
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
    let niveles = audio.as_ref().map(|a| a.niveles.clone());
    let started = audio
        .as_ref()
        .map(|a| a.started)
        .unwrap_or_else(Instant::now);
    let audio_format = audio.as_ref().map(|a| AudioFormat {
        channels: a.formato.canales,
        sample_rate: a.formato.muestras_por_segundo,
    });
    let paths = OutputPaths::reserve(std::path::Path::new(&settings.video_directory))?;
    let stats = Arc::new(crate::record::stats::Stats::new());
    let gpu = crate::record::nvenc::supported(&encoding);
    let pixel_format = if gpu {
        if encoding.encoder == "hevc_nvenc" {
            stream_encoder::PixelFormat::Hevc
        } else {
            stream_encoder::PixelFormat::H264
        }
    } else {
        stream_encoder::PixelFormat::Nv12
    };
    let timing = WriterTiming {
        started,
        fps,
        stop: stop.clone(),
        pause: pause.clone(),
        paused_ms: paused_ms.clone(),
        finish_ms: fin_ms.clone(),
        lead_us: lead_us.clone(),
        discard: discard.clone(),
        frames: frames.clone(),
        bytes: bytes.clone(),
        stats: stats.clone(),
    };
    let writer_stop = stop.clone();
    let encoder = match Encoder::start_for(
        paths.clone(),
        &encoding,
        region.width,
        region.height,
        fps,
        audio_format,
        pixel_format,
    ) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = std::fs::remove_file(&paths.partial);
            return Err(error);
        }
    };
    let (sender, receiver) = sync_channel(if gpu { 32 } else { win::FRAME_QUEUE });
    let recycled = win::Reciclados::default();
    let flags = CaptureFlags {
        sender,
        format: pixel_format,
        encoding: gpu.then(|| encoding.clone()),
        crop: (0, 0, region.width, region.height),
        stop: stop.clone(),
        pause: pause.clone(),
        min_interval: Duration::ZERO,
        reciclados: recycled.clone(),
        stats: stats.clone(),
    };
    let capture = match options.window.filter(|h| *h != 0) {
        Some(hwnd) => win::start_window(hwnd, settings.capture_cursor, fps, flags),
        None => win::start(region, origin, settings.capture_cursor, fps, flags),
    };
    let control = match capture {
        Ok(control) => control,
        Err(error) => {
            encoder.cancel();
            return Err(error);
        }
    };
    let path = encoder.path().to_owned();
    let writer = std::thread::spawn(move || {
        let result = write_recording(encoder, receiver, audio, recycled, timing);
        #[cfg(test)]
        if let Err(error) = &result {
            eprintln!("Recording failed: {error}");
        }
        writer_stop.store(true, Ordering::Release);
        result.map_err(|e| {
            AppError::Msg(format!(
                "{e}\n\nRecording fragments retained at {}",
                path.display()
            ))
        })
    });
    let control = Some(control);
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
        lead_us,
        frames,
        bytes,
        cabecera: Cabecera {
            format: "video".into(),
            audio: sources.sistema,
            microphone: sources.microfono,
        },
        niveles,
        descartar: discard,
        control,
        gpu,
        marco,
        writer: Some(writer),
    });
    drop(slot);
    spawn_ticker(Arc::clone(state), events, paths.partial.clone());
    Ok(SessionInfo {
        region,
        encoding,
        fps,
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
    lead_us: Arc<AtomicU64>,
    discard: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    stats: Arc<crate::record::stats::Stats>,
}

#[cfg(windows)]
fn write_recording(
    mut encoder: Encoder,
    receiver: std::sync::mpsc::Receiver<crate::record::win::CapturedFrame>,
    audio: Option<crate::record::audio::Captura>,
    recycled: crate::record::win::Reciclados,
    timing: WriterTiming,
) -> Result<Option<PathBuf>> {
    // Frames captured but not written yet, oldest first, and the last frame written (repeated
    // while the screen is static).
    let mut queue: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut last: Option<Vec<u8>> = None;
    /// At most this many frames wait for the encoder (~100 ms at 60 fps).
    const QUEUE: usize = 6;
    // Hardware codec initialization can briefly stall FFmpeg after its input sockets
    // connect. Retain up to 16 real startup frames, then return to the normal bound.
    const STARTUP_QUEUE: usize = 16;
    let mut sent = 0u64;
    let mut active = Duration::ZERO;
    // The clock starts when the first frame arrives (see `RecordingState::lead_us`).
    let mut lead: Option<Duration> = None;
    let mut pacer = stream_encoder::Pacer::default();
    let stats = timing.stats.clone();
    let mut log = crate::record::stats::Log::start(&format!(
        "Snapframe recording log · target {} fps · {}",
        timing.fps,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    // Audio that arrived before the first frame, kept until the clock starts.
    let mut early_audio: Vec<crate::record::audio::Trozo> = Vec::new();
    let compressed = encoder.compressed();
    let mut capture_finished = false;
    let mut drain_started = None;
    let mut audio_started = false;
    let mut timeline = audio.as_ref().map(|a| {
        AudioTimeline::new(AudioFormat {
            channels: a.formato.canales,
            sample_rate: a.formato.muestras_por_segundo,
        })
    });
    loop {
        if let Some(error) = stats.capture_error.lock().take() {
            return Err(AppError::Msg(format!("Windows capture failed: {error}")));
        }
        if timing.discard.load(Ordering::Acquire) {
            encoder.cancel();
            return Ok(None);
        }
        // Queue new frames for the encoder; repeat the last one while the source is static.
        loop {
            let frame = match receiver.try_recv() {
                Ok(frame) => frame,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    capture_finished = true;
                    break;
                }
            };
            if lead.is_none() {
                let at = stats
                    .first_frame
                    .lock()
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(timing.started);
                lead = Some(at);
                timing
                    .lead_us
                    .store(at.as_micros() as u64, Ordering::Release);
            }
            queue.push_back(frame.pixels);
            let limit = if compressed {
                64
            } else if sent < u64::from(timing.fps) {
                STARTUP_QUEUE
            } else {
                QUEUE
            };
            if queue.len() > limit {
                if compressed {
                    return Err(AppError::Msg("The MP4 muxer fell behind native encoding. Completed fragments are retained; choose a faster disk or lower recording bitrate.".into()));
                }
                // The encoder is too far behind: drop the oldest waiting frame.
                if let Some(old) = queue.pop_front() {
                    recycled.devolver(old);
                }
                stats.replaced.fetch_add(1, Ordering::Relaxed);
            }
            stats.received.fetch_add(1, Ordering::Relaxed);
            pacer.fresh = queue.len() as u64;
            timing.frames.fetch_add(1, Ordering::Relaxed);
        }
        if let (Some(source), Some(timeline)) = (audio.as_ref(), timeline.as_mut()) {
            loop {
                match source.trozos.try_recv() {
                    Ok(chunk) => early_audio.push(chunk),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Err(AppError::Msg(
                            "The selected audio device stopped delivering audio.".into(),
                        ))
                    }
                }
            }
            if let Some(lead) = lead {
                let rate = u128::from(source.formato.muestras_por_segundo);
                let lead_samples = (lead.as_nanos() * rate / 1_000_000_000) as i64;
                let align = usize::from(source.formato.canales) * 2;
                for chunk in early_audio.drain(..) {
                    let sample = ((chunk.desde_el_inicio.max(0) as u128 * rate + 5_000_000)
                        / 10_000_000) as i64
                        - lead_samples;
                    let pcm = crate::record::audio::a_pcm16(&chunk.datos);
                    // Drop audio from before the first video frame.
                    let skip = ((-sample).max(0) as usize * align).min(pcm.len());
                    if skip < pcm.len() {
                        timeline.push(sample.max(0) as u64, &pcm[skip..])?;
                    }
                }
            }
        }
        let stopping = timing.stop.load(Ordering::Acquire);
        if compressed && stopping {
            let since = *drain_started.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(10) {
                return Err(AppError::Msg(
                    "Native capture did not finish in time.".into(),
                ));
            }
        }
        if stopping {
            let duration = timing.finish_ms.load(Ordering::Acquire);
            if duration > 0 {
                active = Duration::from_millis(duration);
            }
        } else if let (false, Some(lead)) = (timing.pause.load(Ordering::Acquire), lead) {
            active = timing
                .started
                .elapsed()
                .saturating_sub(lead)
                .saturating_sub(Duration::from_millis(
                    timing.paused_ms.load(Ordering::Acquire),
                ));
        }
        let due = stream_encoder::frames_at(active, timing.fps).max(1);
        let has_picture = last.is_some() || !queue.is_empty();
        if due.saturating_sub(sent) > u64::from(timing.fps) * 2 && has_picture && sent > 0 {
            return Err(AppError::Msg("The encoder fell more than two seconds behind. Choose a faster preset, hardware encoder, or lower frame rate.".into()));
        }
        if has_picture || (compressed && sent > 0) {
            // Video and audio have independent socket workers. Start video immediately;
            // withholding it for 150 ms overflows the six-frame capture queue at 60 fps.
            // Video is written first, and audio only once FFmpeg has opened its audio input:
            // FFmpeg opens it after reading the first video packets, so blocking on audio
            // before then (B-frames delay the first packet by ~11 frames) deadlocks.
            // Until then audio stays in the timeline.
            let first = !audio_started;
            let (fresh, repeats) = if compressed {
                (queue.len() as u64, 0)
            } else {
                pacer.wanted(sent, due, stopping)
            };
            'write: {
                for _ in 0..fresh {
                    let Some(frame) = queue.front() else { break };
                    if !encoder.try_video(frame)? {
                        stats.busy.fetch_add(1, Ordering::Relaxed);
                        break 'write;
                    }
                    sent += 1;
                    stats.fresh.fetch_add(1, Ordering::Relaxed);
                    let picture = queue.pop_front().unwrap_or_default();
                    if !compressed {
                        if let Some(old) = last.replace(picture) {
                            recycled.devolver(old);
                        }
                    }
                }
                for _ in 0..repeats {
                    let Some(frame) = last.as_ref() else { break };
                    if !encoder.try_video(frame)? {
                        stats.busy.fetch_add(1, Ordering::Relaxed);
                        break 'write;
                    }
                    sent += 1;
                    stats.repeat.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Audio is held for 150ms to absorb WASAPI packet/silence-fill jitter.
            // Finalization pads/trims to exactly the same active-time clock as video.
            if let Some(timeline) = timeline.as_mut().filter(|_| encoder.audio_ready()) {
                audio_started = true;
                let time = if stopping || first {
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
            pacer.fresh = queue.len() as u64;
            log.tick(&stats, timing.fps);
        } else if stopping || timing.started.elapsed() > Duration::from_secs(10) {
            return Err(AppError::Msg(
                "Windows did not deliver a video frame. Nothing could be recorded.".into(),
            ));
        }
        timing.bytes.store(encoder.bytes(), Ordering::Relaxed);
        encoder.check()?;
        if stopping
            && ((!compressed && sent >= due)
                || (compressed && capture_finished && queue.is_empty()))
        {
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
    let scale = crate::platform::window_style::monitor_at(cx, cy)
        .map(|m| m.scale)
        .unwrap_or(1.0);
    (2.0 * scale).round().max(1.0) as i32
}

#[cfg(not(windows))]
pub fn start(
    _state: &Arc<AppState>,
    _events: Arc<dyn RecorderEvents>,
    _region: Rect,
    _options: RecordOptions,
) -> Result<SessionInfo> {
    Err(AppError::Unsupported)
}
fn tick_de(recording: &RecordingState, saving: bool) -> RecordingTick {
    #[cfg(windows)]
    let (audio_level, microphone_level) = recording
        .niveles
        .as_ref()
        .map(|n| {
            use crate::record::audio::Niveles;
            (Niveles::tomar(&n.sistema), Niveles::tomar(&n.microfono))
        })
        .unwrap_or_default();
    #[cfg(not(windows))]
    let (audio_level, microphone_level) = (0.0, 0.0);
    RecordingTick {
        audio_level,
        microphone_level,
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
fn spawn_ticker(state: Arc<AppState>, events: Arc<dyn RecorderEvents>, output: PathBuf) {
    std::thread::spawn(move || loop {
        let tick = {
            let guard = state.recording.lock();
            guard.as_ref().filter(|r| r.output == output).map(|r| {
                (
                    tick_de(r, false),
                    r.writer.as_ref().is_some_and(|w| w.is_finished()),
                )
            })
        };
        match tick {
            // The writer ended by itself (encoder error, window closed): finish like Stop.
            Some((_, true)) => {
                let _ = stop(&state, events);
                break;
            }
            Some((tick, false)) => events.tick(tick),
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

pub fn stop(state: &Arc<AppState>, events: Arc<dyn RecorderEvents>) -> Result<()> {
    let recording = state.recording.lock().take().ok_or(AppError::NoRecording)?;
    events.tick(tick_de(&recording, true));
    let play_sound = recording.play_sound;
    let saving = state.saving.clone();
    saving.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        let outcome = match finish(recording) {
            Ok(Some(path)) => {
                if play_sound {
                    crate::platform::sonido::obturador();
                }
                Outcome::Saved(path)
            }
            Ok(None) => Outcome::Discarded,
            Err(error) => Outcome::Failed(error.to_string()),
        };
        events.finished(outcome);
        saving.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(())
}

pub fn cancel(state: &Arc<AppState>, events: Arc<dyn RecorderEvents>) -> Result<()> {
    let recording = state.recording.lock().take().ok_or(AppError::NoRecording)?;
    recording.descartar.store(true, Ordering::Release);
    let partial = recording.output.clone();
    std::thread::spawn(move || {
        match finish(recording) {
            Ok(Some(path)) => {
                let _ = std::fs::remove_file(path);
            }
            Err(error) => eprintln!("Recording stopped while discarding: {error}"),
            _ => {}
        }
        let _ = std::fs::remove_file(partial);
        events.finished(Outcome::Discarded);
    });
    Ok(())
}

#[allow(dead_code)]
pub fn set_paused(state: &AppState, paused: bool) -> Result<()> {
    let guard = state.recording.lock();
    let recording = guard.as_ref().ok_or(AppError::NoRecording)?;
    #[cfg(windows)]
    if recording.gpu {
        return Err(AppError::Msg(
            "Pausing desktop GPU recording is not available. Stop and start a new recording."
                .into(),
        ));
    }
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
    struct SilentEvents;
    impl RecorderEvents for SilentEvents {
        fn tick(&self, _: RecordingTick) {}
        fn finished(&self, _: Outcome) {}
    }

    #[test]
    #[ignore = "Records and decodes GPU and CPU captures, including 4K60, through the public recorder"]
    fn obs_style_gpu_and_cpu_recording_smoke() {
        for (width, height, encoder, preset, audio) in [
            (2560, 1440, "h264_nvenc", "p5", true),
            (3840, 2160, "h264_nvenc", "p5", false),
            (3840, 2160, "h264_nvenc", "p5", true),
            (1920, 1080, "libx264", "veryfast", false),
            (1920, 1080, "libx264", "veryfast", true),
            (1920, 1080, "hevc_nvenc", "p5", true),
        ] {
            record_production(width, height, encoder, preset, audio, false, 5);
        }
    }

    #[test]
    #[ignore = "Records 30-second 1080p60 NVENC window and screen clips with and without WASAPI audio"]
    fn sustained_1080p60_nvenc_with_and_without_audio() {
        for (audio, screen) in [(false, false), (true, false), (true, true)] {
            record_production(1920, 1080, "h264_nvenc", "p5", audio, !screen, 30);
        }
    }

    #[test]
    #[ignore = "Records a 60-second 4K60 screen clip with NVENC and WASAPI audio; needs a 4K display"]
    fn sustained_4k60_nvenc_screen_with_audio() {
        record_production(3840, 2160, "h264_nvenc", "p5", true, false, 60);
    }

    #[test]
    #[ignore = "Records a 30-second 1080p60 screen clip with GPU conversion, x264 veryfast and WASAPI audio"]
    fn sustained_cpu_1080p60_with_audio() {
        record_production(1920, 1080, "libx264", "veryfast", true, false, 30);
    }

    fn record_production(
        width: u32,
        height: u32,
        encoder_name: &str,
        preset: &str,
        audio: bool,
        window: bool,
        seconds: u64,
    ) -> PathBuf {
        record_production_at(
            width,
            height,
            encoder_name,
            preset,
            audio,
            window,
            seconds,
            60,
        )
    }

    #[test]
    #[ignore = "Records an odd-sized rectangle at 30 fps with automatic encoding and audio"]
    fn automatic_odd_rectangle_30fps_with_audio() {
        record_production_at(1234, 566, "auto", "medium", true, false, 5, 30);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_production_at(
        width: u32,
        height: u32,
        encoder_name: &str,
        preset: &str,
        audio: bool,
        window: bool,
        seconds: u64,
        fps: u32,
    ) -> PathBuf {
        let animation = crate::record::validation::Animation::new(width, height);
        let dir =
            std::env::temp_dir().join(format!("snapframe-obs-style-{}", uuid::Uuid::new_v4()));
        let settings = crate::settings::Settings {
            fps,
            capture_cursor: false,
            video_directory: dir.to_string_lossy().into_owned(),
            encoding: crate::settings::EncodingSettings {
                encoder: encoder_name.into(),
                preset: preset.into(),
                crf: 28,
                ..Default::default()
            },
            ..Default::default()
        };
        let ffmpeg = settings.encoding.ffmpeg_path.clone();
        let state = Arc::new(AppState::new(settings));
        start(
            &state,
            Arc::new(SilentEvents),
            animation.region(),
            RecordOptions {
                audio,
                window: window.then_some(animation.hwnd),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            state.recording.lock().as_ref().unwrap().gpu,
            encoder_name.ends_with("_nvenc")
                || (encoder_name == "auto" && crate::encode::auto::gpu_available())
        );
        let deadline = Instant::now() + Duration::from_secs(seconds + 15);
        let mut grew_live = false;
        loop {
            let guard = state.recording.lock();
            let recording = guard.as_ref().expect("Recording ended unexpectedly");
            assert!(
                !recording.writer.as_ref().unwrap().is_finished(),
                "Recording ended early"
            );
            if recording.elapsed_ms() >= seconds * 500 {
                assert!(
                    std::fs::metadata(&recording.output).unwrap().len() > 4096,
                    "The destination did not grow during recording"
                );
                grew_live = true;
            }
            if recording.elapsed_ms() >= seconds * 1000 {
                break;
            }
            assert!(Instant::now() < deadline, "Recording did not advance");
            drop(guard);
            std::thread::sleep(Duration::from_millis(10));
        }
        let recording = state.recording.lock().take().unwrap();
        assert!(grew_live);
        let elapsed = recording.elapsed_ms();
        let path = finish(recording).unwrap().unwrap();
        let decoded = crate::encode::ffmpeg::command(&ffmpeg)
            .args(["-hide_banner", "-i"])
            .arg(&path)
            .args([
                "-map",
                "0:v:0",
                "-vf",
                "crop=256:16:0:0,scale=16:1:flags=neighbor,format=gray",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(decoded.stdout.len() % 16, 0);
        let ids: Vec<u16> = decoded
            .stdout
            .chunks_exact(16)
            .map(|cells| {
                cells
                    .iter()
                    .enumerate()
                    .fold(0u16, |id, (bit, luma)| id | (u16::from(*luma > 128) << bit))
            })
            .collect();
        let count = ids.len() as u64;
        let expected = elapsed * u64::from(fps) / 1000;
        assert!(
            (expected.saturating_sub(2)..=expected + 12).contains(&count),
            "Wrong media duration: {count} frames for {elapsed}ms"
        );
        let steady: Vec<_> = ids
            .iter()
            .take(expected as usize)
            .skip(fps as usize)
            .copied()
            .collect();
        let repeats = steady.windows(2).filter(|pair| pair[0] == pair[1]).count();
        println!("{width}x{height} {encoder_name}/{preset} audio={audio} window={window}: {elapsed}ms, {count} frames, {repeats}/{} repeated visible IDs after warmup; {}", steady.len(), path.display());
        assert!(
            repeats * 100 <= steady.len().max(1),
            "More than 1% of motion frames repeated after warmup"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "Unexpected partial/spool files"
        );
        path
    }
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
            lead_us: Arc::new(AtomicU64::new(u64::MAX)),
            discard: Arc::new(AtomicBool::new(false)),
            frames: frames.clone(),
            bytes: bytes.clone(),
            stats: Arc::new(crate::record::stats::Stats::new()),
        };
        tx.send(CapturedFrame {
            pixels: vec![80; 320 * 180 * 4],
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
            pixels: vec![80; 1280 * 720 * 4],
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
            lead_us: Arc::new(AtomicU64::new(u64::MAX)),
            discard: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            bytes: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(crate::record::stats::Stats::new()),
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
