use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::capture::Rect;
use crate::error::{AppError, Result};
use crate::record::{self, FrameCache, SessionData};
use crate::state::{AppState, Cabecera, RecordingState};
use crate::windows_mgr;

pub const EVENT_TICK: &str = "winshotx://recording-tick";
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOptions {
    pub format: String,
    pub fps: u32,
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
pub(crate) fn bgra_a_rgba_en_sitio(pixeles: &mut [u8]) {
    for pixel in pixeles.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}
fn flip_rows_en(input: &[u8], width: u32, height: u32, out: &mut Vec<u8>) {
    let stride = width as usize * 4;
    out.clear();
    out.resize(input.len(), 0);
    for y in 0..height as usize {
        let src = y * stride;
        let dst = (height as usize - 1 - y) * stride;
        if src + stride <= input.len() && dst + stride <= out.len() {
            out[dst..dst + stride].copy_from_slice(&input[src..src + stride]);
        }
    }
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
struct SalidaDeAudio {
    captura: Option<crate::record::audio::Captura>,
    archivo: Option<std::io::BufWriter<std::fs::File>>,
    escritos: u64,
}

#[cfg(windows)]
impl SalidaDeAudio {
    fn abrir(captura: Option<crate::record::audio::Captura>, ruta: std::path::PathBuf) -> Self {
        let archivo = captura.as_ref().and_then(|_| {
            std::fs::File::create(ruta)
                .ok()
                .map(std::io::BufWriter::new)
        });
        Self {
            captura,
            archivo,
            escritos: 0,
        }
    }

    fn formato(&self) -> Option<crate::record::audio::Formato> {
        self.captura.as_ref().map(|c| c.formato)
    }
    fn volcar(&mut self, encoder: &mut Option<windows_capture::encoder::VideoEncoder>) {
        use std::io::Write;
        let Some(captura) = self.captura.as_ref() else {
            return;
        };
        while let Ok(trozo) = captura.trozos.try_recv() {
            let pcm = crate::record::audio::a_pcm16(&trozo.datos);
            if let Some(f) = self.archivo.as_mut() {
                if f.write_all(&pcm).is_ok() {
                    self.escritos += pcm.len() as u64;
                }
            }
            let Some(codificador) = encoder.as_mut() else {
                continue;
            };
            if codificador
                .send_audio_buffer(&pcm, trozo.desde_el_inicio)
                .is_err()
            {
                *encoder = None;
            }
        }
    }
    fn cerrar(
        mut self,
        encoder: &mut Option<windows_capture::encoder::VideoEncoder>,
    ) -> Option<record::AudioInfo> {
        use std::io::Write;
        self.volcar(encoder);
        let captura = self.captura.take()?;
        let formato = captura.formato;
        captura.parar();
        if let Some(mut f) = self.archivo.take() {
            let _ = f.flush();
        }
        (self.escritos > 0).then_some(record::AudioInfo {
            channels: formato.canales,
            sample_rate: formato.muestras_por_segundo,
        })
    }
}
#[cfg(windows)]
pub fn start(app: &AppHandle, region: Rect, options: RecordOptions) -> Result<SessionInfo> {
    use std::sync::mpsc::sync_channel;

    use crate::record::win::{self, CaptureFlags};

    if options.format != "video" {
        return Err(AppError::Msg(
            "Only MP4 video recording is supported.".into(),
        ));
    }
    let state = app.state::<AppState>();
    if state.is_recording() {
        return Err(AppError::Msg("A recording is already in progress".into()));
    }

    crate::audio_devices::stop(None);
    let region = region.to_even();
    let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let dir = state.session_dir(&id);
    std::fs::create_dir_all(&dir)?;

    let fps = options.fps.clamp(5, 120);
    let origin = monitor_origin(app, region);
    let ventana = options.window.filter(|h| *h != 0);

    let session_seed = SessionData {
        id: id.clone(),
        dir: dir.clone(),
        region,
        fps,
        format: options.format.clone(),
        has_audio: false, // se pone a cierto abajo, si el altavoz llega a abrirse
        width: region.width,
        height: region.height,
        mp4_path: Some(dir.join("recording.mp4")),
        audio: None,
        frames: Vec::new(),
    };

    let stop = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let descartar = Arc::new(AtomicBool::new(false));
    let paused_ms = Arc::new(AtomicU64::new(0));
    let reloj_inicio = Instant::now();
    let frames_counter = Arc::new(AtomicU64::new(0));
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let fuentes = crate::record::audio::Fuentes {
        sistema: options.audio,
        microfono: options.microphone,
    };
    let audio = if fuentes.ninguna() {
        None
    } else {
        let audio_settings = state.settings.read().clone();
        Some(crate::record::audio::empezar_con_dispositivos(
            fuentes,
            pause.clone(),
            audio_settings.system_audio_device,
            audio_settings.microphone_device,
        )?)
    };
    windows_mgr::close_overlays(app);
    let cabecera = Cabecera {
        format: options.format.clone(),
        audio: audio.is_some() && fuentes.sistema,
        microphone: audio.is_some() && fuentes.microfono,
    };

    let (sender, receiver) = sync_channel::<win::CapturedFrame>(2);
    let writer_frames = frames_counter.clone();
    let writer_bytes = bytes_counter.clone();
    let writer_stop = stop.clone();
    let fin_ms = Arc::new(AtomicU64::new(0));
    let writer_fin = fin_ms.clone();
    let writer_descartar = descartar.clone();
    let reciclados = win::Reciclados::default();
    let reciclados_escritor = reciclados.clone();
    let encoding = state.settings.read().encoding.clone();
    let writer = std::thread::spawn(move || -> Result<SessionData> {
        let mut session = session_seed;
        session.has_audio = audio.is_some();
        let (width, height) = (session.width, session.height);
        let mut cache = FrameCache::new(&session.dir)?;
        let mut salida = SalidaDeAudio::abrir(audio, session.audio_path());
        let mut encoder = build_recording_encoder(&session, salida.formato(), &encoding);
        let mut last_ts = 0u64;
        let mut dado_la_vuelta: Vec<u8> = Vec::new();
        loop {
            let frame = match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(frame) => frame,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if writer_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    salida.volcar(&mut encoder);
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            last_ts = frame.ts_ms;
            let para_vista_previa = encoder.is_some();
            if para_vista_previa {
                flip_rows_en(&frame.bgra, width, height, &mut dado_la_vuelta);
            }
            let mut rgba = frame.bgra;
            bgra_a_rgba_en_sitio(&mut rgba);

            writer_frames.fetch_add(1, Ordering::Relaxed);
            if cache.push_rgba(&rgba, width, height, frame.ts_ms)? {
                writer_bytes.store(cache.bytes_written(), Ordering::Relaxed);
            }
            if let Some(enc) = encoder.as_mut().filter(|_| para_vista_previa) {
                if let Err(error) =
                    enc.send_frame_buffer(&dado_la_vuelta, frame.ts_ms as i64 * 10_000)
                {
                    eprintln!("[winshotx] se ha caido la vista previa en vivo: {error}");
                    encoder = None;
                }
            }
            reciclados_escritor.devolver(rgba);
            salida.volcar(&mut encoder);
        }
        let grabado_ms = writer_fin.load(Ordering::Relaxed).max(last_ts);
        if grabado_ms > last_ts && !dado_la_vuelta.is_empty() {
            if let Some(enc) = encoder.as_mut() {
                if let Err(error) =
                    enc.send_frame_buffer(&dado_la_vuelta, grabado_ms as i64 * 10_000)
                {
                    eprintln!("[winshotx] la vista previa no ha podido cerrar el final: {error}");
                }
            }
        }

        session.audio = salida.cerrar(&mut encoder);
        session.has_audio = session.audio.is_some();
        let vista_previa_cerrada = encoder.take().is_some_and(|enc| enc.finish().is_ok());
        if !vista_previa_cerrada {
            session.mp4_path = None;
        }

        session.frames = cache.finish(grabado_ms, session.fps)?;
        if writer_descartar.load(Ordering::Relaxed) {
            return Ok(session);
        }
        session.persist()?;
        Ok(session)
    });

    let flags_base = CaptureFlags {
        sender,
        crop: (0, 0, 0, 0),
        stop: stop.clone(),
        pause: pause.clone(),
        paused_ms: paused_ms.clone(),
        min_interval: std::time::Duration::ZERO,
        start: reloj_inicio,
        reciclados,
    };
    let control = match ventana {
        Some(hwnd) => {
            win::start_window(hwnd, state.settings.read().capture_cursor, fps, flags_base)?
        }
        None => win::start(
            region,
            origin,
            state.settings.read().capture_cursor,
            fps,
            flags_base,
        )?,
    };
    let marco = match crate::platform::marco::Marco::abrir(region, grosor_del_marco(region)) {
        Ok(marco) => Some(marco),
        Err(error) => {
            eprintln!("[winshotx] sin marco alrededor de la grabacion: {error}");
            None
        }
    };

    let recording = RecordingState {
        settings: state.settings.read().clone(),
        started: reloj_inicio,
        stop: stop.clone(),
        pause: pause.clone(),
        paused_ms: paused_ms.clone(),
        pause_started: parking_lot::Mutex::new(None),
        fin_ms,
        frames: frames_counter,
        bytes: bytes_counter,
        cabecera,
        control: Some(control),
        marco,
        descartar,
        barra: None,
        writer: Some(writer),
    };

    let info = SessionInfo {
        id: id.clone(),
        region,
        fps,
        frame_count: 0,
        duration_ms: 0,
        has_audio: false,
        format: options.format,
        mp4_path: None,
    };

    *state.recording.lock() = Some(recording);
    crate::audio_devices::stop(None);
    match windows_mgr::open_recorder(app, region) {
        Ok(label) => {
            if let Some(recording) = state.recording.lock().as_mut() {
                recording.barra = Some(label);
            }
        }
        Err(error) => eprintln!("no se ha podido abrir la barra de grabacion: {error}"),
    }
    spawn_ticker(app.clone(), stop);
    Ok(info)
}
#[cfg(windows)]
fn grosor_del_marco(region: Rect) -> i32 {
    let (cx, cy) = region.center();
    let escala = windows_mgr::monitor_de(cx, cy)
        .map(|m| m.escala)
        .unwrap_or(1.0);
    (2.0 * escala).round().max(1.0) as i32
}

#[cfg(not(windows))]
pub fn start(_app: &AppHandle, _region: Rect, _options: RecordOptions) -> Result<SessionInfo> {
    Err(AppError::Unsupported)
}

#[cfg(windows)]
fn build_recording_encoder(
    session: &SessionData,
    audio: Option<crate::record::audio::Formato>,
    encoding: &crate::settings::EncodingSettings,
) -> Option<windows_capture::encoder::VideoEncoder> {
    if encoding.engine == "ffmpeg" {
        return None;
    }
    use windows_capture::encoder::{
        AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
        VideoSettingsSubType,
    };

    let path = session.mp4_path.as_ref()?;
    if crate::encode::mp4::medida_que_acepta(session.width, session.height)
        != (session.width, session.height)
    {
        eprintln!(
            "[winshotx] {}x{} es demasiado pequenno para el codificador en vivo: la vista previa se escribe al terminar",
            session.width, session.height
        );
        return None;
    }
    let options = crate::encode::mp4::Mp4Options {
        width: session.width,
        height: session.height,
        fps: session.fps,
        quality: encoding.quality,
        bitrate_override: None,
    };
    VideoEncoder::new(
        VideoSettingsBuilder::new(session.width, session.height)
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(session.fps)
            .bitrate(encoding.bitrate().unwrap_or_else(|| options.bitrate())),
        match audio {
            Some(formato) => AudioSettingsBuilder::default()
                .channel_count(u32::from(formato.canales))
                .sample_rate(formato.muestras_por_segundo)
                .bit_per_sample(16)
                .disabled(false),
            None => AudioSettingsBuilder::default().disabled(true),
        },
        ContainerSettingsBuilder::default(),
        path,
    )
    .inspect_err(|error| {
        eprintln!(
            "[winshotx] sin vista previa en vivo ({}x{} a {} fps): {error}",
            session.width, session.height, session.fps
        );
    })
    .ok()
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
    }
}
fn avisar_barra(app: &AppHandle, tick: RecordingTick) {
    for (label, window) in app.webview_windows() {
        if label.starts_with(windows_mgr::RECORDER_PREFIX) {
            let _ = window.emit(EVENT_TICK, tick.clone());
        }
    }
}

fn spawn_ticker(app: AppHandle, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let tick = {
                let state = app.state::<AppState>();
                let guard = state.recording.lock();
                guard.as_ref().map(|recording| tick_de(recording, false))
            };
            let Some(tick) = tick else { break };
            avisar_barra(&app, tick);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}
fn terminar(mut recording: RecordingState) -> Result<SessionData> {
    recording
        .fin_ms
        .store(recording.elapsed_ms(), Ordering::Relaxed);
    recording.stop.store(true, Ordering::Relaxed);
    #[cfg(windows)]
    if let Some(control) = recording.control.take() {
        std::thread::spawn(move || {
            let _ = control.stop();
        });
    }
    #[cfg(windows)]
    drop(recording.marco.take());

    let writer = recording
        .writer
        .take()
        .ok_or_else(|| AppError::Msg("The recording writer is unavailable".into()))?;
    writer
        .join()
        .map_err(|_| AppError::Msg("The recording writer stopped unexpectedly".into()))?
}
pub fn stop(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let Some(recording) = state.recording.lock().take() else {
        return Err(AppError::NoRecording);
    };
    avisar_barra(app, tick_de(&recording, true));
    let barra = recording.barra.clone();
    let settings = recording.settings.clone();
    let saving = state.saving.clone();
    saving.fetch_add(1, Ordering::SeqCst);
    let handle = app.clone();
    std::thread::spawn(move || {
        match terminar(recording) {
            Ok(session) if session.frames.is_empty() => {
                eprintln!("no se ha capturado ningÃºn fotograma; prueba a bajar los fps");
                let _ = std::fs::remove_dir_all(&session.dir);
                cerrar_esta_barra(&handle, barra.as_deref());
            }
            Ok(session) => entregar(&handle, session, barra.as_deref(), &settings),
            Err(error) => {
                use tauri_plugin_dialog::DialogExt;
                handle
                    .dialog()
                    .message(error.to_string())
                    .title("Recording failed")
                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                    .show(|_| {});
                cerrar_esta_barra(&handle, barra.as_deref());
            }
        }
        saving.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(())
}
fn cerrar_esta_barra(app: &AppHandle, barra: Option<&str>) {
    match barra {
        Some(label) => windows_mgr::close_recorder_label(app, label),
        None => windows_mgr::close_recorder(app),
    }
}
fn entregar(
    app: &AppHandle,
    session: SessionData,
    barra: Option<&str>,
    settings: &crate::settings::Settings,
) {
    use tauri_plugin_dialog::DialogExt;
    match crate::exporter::save_recording(&session, settings) {
        Ok(path) => {
            if settings.play_sound {
                crate::platform::sonido::obturador();
            }
            let _ = app.emit(
                "winshotx://recording-saved",
                path.to_string_lossy().to_string(),
            );
            let _ = std::fs::remove_dir_all(&session.dir);
        }
        Err(error) => {
            let _ = std::fs::write(session.dir.join("keep-on-error"), error.to_string());
            app.dialog()
                .message(format!(
                    "{error}\n\nTemporary recording retained at {}",
                    session.dir.display()
                ))
                .title("Recording could not be saved")
                .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                .show(|_| {});
        }
    }
    cerrar_esta_barra(app, barra);
}

fn cerrar_barra(app: &AppHandle, barra: Option<String>) {
    let handle = app.clone();
    std::thread::spawn(move || cerrar_esta_barra(&handle, barra.as_deref()));
}
pub fn cancel(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let Some(recording) = state.recording.lock().take() else {
        cerrar_barra(app, None);
        return Err(AppError::NoRecording);
    };
    recording.descartar.store(true, Ordering::Relaxed);
    cerrar_barra(app, recording.barra.clone());
    std::thread::spawn(move || match terminar(recording) {
        Ok(session) => {
            let _ = std::fs::remove_dir_all(&session.dir);
        }
        Err(error) => eprintln!("fallo al descartar: {error}"),
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
mod pruebas_de_audio {
    #[test]
    #[ignore]
    fn el_mp4_sale_con_pista_de_sonido() {
        use windows_capture::encoder::{
            AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
            VideoSettingsSubType,
        };

        let (ancho, alto, fps) = (320u32, 240u32, 30u32);
        let unico = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let destino = std::env::temp_dir().join(format!("winshotx-audio-{unico}.mp4"));

        let mut encoder = VideoEncoder::new(
            VideoSettingsBuilder::new(ancho, alto)
                .sub_type(VideoSettingsSubType::H264)
                .frame_rate(fps)
                .bitrate(2_000_000),
            AudioSettingsBuilder::default()
                .channel_count(2)
                .sample_rate(48_000)
                .bit_per_sample(16)
                .disabled(false),
            ContainerSettingsBuilder::default(),
            &destino,
        )
        .expect("no se ha podido crear el codificador");
        let bgra = vec![90u8; (ancho * alto) as usize * 4];
        let por_fotograma = vec![0u8; (48_000 / 30) * 2 * 2];
        for i in 0..fps {
            encoder
                .send_frame_buffer(&bgra, i as i64 * (10_000_000 / fps as i64))
                .expect("no se ha podido enviar el fotograma");
            encoder
                .send_audio_buffer(&por_fotograma, 0)
                .expect("no se ha podido enviar el sonido");
        }
        encoder.finish().expect("no se ha podido cerrar el MP4");

        let bytes = std::fs::read(&destino).expect("no se ha escrito el MP4");
        let _ = std::fs::remove_file(&destino);
        println!("MP4 de un segundo con sonido: {} bytes", bytes.len());
        let tiene_pista = bytes.windows(4).any(|v| v == b"mp4a");
        assert!(tiene_pista, "el MP4 ha salido sin pista de sonido");
        assert!(bytes.len() > 5_000, "el MP4 ha salido demasiado pequeÃ±o");
    }
}
