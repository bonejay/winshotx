//! System audio (WASAPI loopback) and microphone capture for recordings.
//!
//! Loopback opens the playback device and records what is playing, without permissions or
//! rerouting. Captured chunks are converted to 16-bit PCM and streamed to FFmpeg by the
//! recorder.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

use crate::error::{AppError, Result};

/// Which audio sources to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fuentes {
    /// What plays through the speakers.
    pub sistema: bool,
    /// The microphone input.
    pub microfono: bool,
}

impl Fuentes {
    pub fn ninguna(&self) -> bool {
        !self.sistema && !self.microfono
    }
}

/// The capture format, needed to configure the encoder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Formato {
    pub canales: u16,
    pub muestras_por_segundo: u32,
    pub bits_por_muestra: u16,
}

impl Formato {
    /// Bytes per audio frame across all channels.
    pub fn bytes_por_instante(&self) -> u32 {
        u32::from(self.canales) * u32::from(self.bits_por_muestra) / 8
    }

    /// Duration of this many bytes in 100 ns units (the Windows media time unit).
    pub fn duracion_de(&self, bytes: usize) -> i64 {
        let por_instante = self.bytes_por_instante().max(1) as u64;
        let instantes = bytes as u64 / por_instante;
        (instantes * 10_000_000 / u64::from(self.muestras_por_segundo.max(1))) as i64
    }

    /// The same as a `Duration`, to compare with the wall clock.
    pub fn duracion(&self, bytes: usize) -> Duration {
        Duration::from_nanos(self.duracion_de(bytes).max(0) as u64 * 100)
    }
}

/// An audio chunk as delivered by Windows, with its start time.
pub struct Trozo {
    pub datos: Vec<u8>,
    /// Time since recording started, in 100 ns units.
    pub desde_el_inicio: i64,
}

/// Peak levels (0..1, as f32 bits) since the recording bar last read them, per source.
#[derive(Default)]
pub struct Niveles {
    pub sistema: std::sync::atomic::AtomicU32,
    pub microfono: std::sync::atomic::AtomicU32,
}

impl Niveles {
    fn subir(slot: &std::sync::atomic::AtomicU32, pico: f32) {
        // For non-negative floats the bit patterns sort like the values.
        slot.fetch_max(pico.max(0.0).to_bits(), Ordering::Relaxed);
    }
    /// Read and reset a level.
    pub fn tomar(slot: &std::sync::atomic::AtomicU32) -> f32 {
        f32::from_bits(slot.swap(0, Ordering::Relaxed)).clamp(0.0, 1.0)
    }
}

fn pico(muestras: impl Iterator<Item = f32>) -> f32 {
    muestras.fold(
        0.0f32,
        |m, x| if x.is_finite() { m.max(x.abs()) } else { m },
    )
}

fn pico_de_bytes(bytes: &[u8]) -> f32 {
    pico(
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
    )
}

/// A running capture. Dropping it stops the thread and closes everything.
pub struct Captura {
    pub niveles: Arc<Niveles>,
    parar: Arc<AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
    pub formato: Formato,
    pub started: Instant,
    pub trozos: Receiver<Trozo>,
}

impl Captura {}

impl Drop for Captura {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(hilo) = self.hilo.take() {
            let _ = hilo.join();
        }
    }
}

/// Polling interval. Shorter wastes wakeups; much longer lets the Windows buffer overflow.
const CADA: std::time::Duration = std::time::Duration::from_millis(10);

/// Buffer requested from Windows: 200 ms. If the thread falls further behind, Windows drops old
/// data and flags a discontinuity.
const COLCHON_100NS: i64 = 2_000_000;

/// How far written audio may lag the wall clock before silence is inserted. Below 100 ms it is
/// normal delivery jitter; above it the device has really gone quiet.
const SEQUIA: Duration = Duration::from_millis(100);

pub fn empezar_con_dispositivos(
    fuentes: Fuentes,
    pausa: Arc<AtomicBool>,
    system_device: Option<String>,
    microphone_device: Option<String>,
) -> Result<Captura> {
    let (formato_tx, formato_rx) = mpsc::channel::<Result<(Formato, Instant)>>();
    let (tx, rx) = mpsc::sync_channel::<Trozo>(100);
    let parar = Arc::new(AtomicBool::new(false));
    let bandera = Arc::clone(&parar);
    let niveles = Arc::new(Niveles::default());
    let medidor = Arc::clone(&niveles);

    // All COM work happens on this one thread: audio objects belong to the thread that created
    // them.
    let hilo = std::thread::spawn(move || {
        let resultado = capturar(
            fuentes,
            &formato_tx,
            &tx,
            &bandera,
            &pausa,
            system_device.as_deref(),
            microphone_device.as_deref(),
            &medidor,
        );
        if let Err(error) = resultado {
            // Failures after the format was reported cannot be sent back, so they are logged
            // and the recording continues silently.
            eprintln!("[winshotx] system audio capture stopped: {error}");
        }
    });

    let (formato, started) = formato_rx
        .recv()
        .map_err(|_| AppError::Msg("Could not start audio capture".into()))??;

    Ok(Captura {
        niveles,
        parar,
        hilo: Some(hilo),
        formato,
        trozos: rx,
        started,
    })
}

fn capturar(
    fuentes: Fuentes,
    formato_tx: &Sender<Result<(Formato, Instant)>>,
    tx: &SyncSender<Trozo>,
    parar: &AtomicBool,
    pausa: &AtomicBool,
    system_device: Option<&str>,
    microphone_device: Option<&str>,
    niveles: &Niveles,
) -> Result<()> {
    let nivel_maestro = if fuentes.sistema {
        &niveles.sistema
    } else {
        &niveles.microfono
    };
    // Guard that uninitializes COM no matter how the thread exits.
    let _com = Com::inicializar()?;

    // The master source sets the timing and format: system audio if enabled, otherwise the
    // microphone. Only the master emits chunks; the other source is mixed on top. Two
    // independent clocks would drift apart within seconds.
    let maestro = if fuentes.sistema {
        Dispositivo::Altavoz
    } else {
        Dispositivo::Microfono
    };
    let (cliente, captura, formato) = match unsafe {
        abrir_elegido(
            maestro,
            if fuentes.sistema {
                system_device
            } else {
                microphone_device
            },
        )
    } {
        Ok(abierto) => abierto,
        Err(error) => {
            let _ = formato_tx.send(Err(error));
            return Ok(());
        }
    };
    // Both requested sources must open successfully; never silently omit the microphone.
    let mut acompannante = if fuentes.sistema && fuentes.microfono {
        match unsafe { abrir_elegido(Dispositivo::Microfono, microphone_device) } {
            Ok((cliente_mic, captura_mic, formato_mic)) => match unsafe { cliente_mic.Start() } {
                Ok(()) => Some(Acompannante {
                    cliente: cliente_mic,
                    captura: captura_mic,
                    formato: formato_mic,
                    pendiente: Vec::new(),
                }),
                Err(error) => {
                    let _ = formato_tx.send(Err(AppError::Msg(format!(
                        "Could not start the selected microphone: {error}"
                    ))));
                    return Ok(());
                }
            },
            Err(error) => {
                let _ = formato_tx.send(Err(error));
                return Ok(());
            }
        }
    } else {
        None
    };

    unsafe { cliente.Start() }.map_err(|e| AppError::Msg(format!("Could not start audio: {e}")))?;

    let started = Instant::now();
    if formato_tx.send(Ok((formato, started))).is_err() {
        let _ = unsafe { cliente.Stop() };
        if let Some(microphone) = acompannante.as_ref() {
            let _ = unsafe { microphone.cliente.Stop() };
        }
        return Ok(());
    }

    // Wall-clock recording time excluding pauses. It takes over when the master goes quiet; see
    // `SEQUIA`.
    let mut reloj = RelojDeGrabacion::arrancar(started);
    let mut escritos: usize = 0;
    while !parar.load(Ordering::Relaxed) {
        std::thread::sleep(CADA);
        let en_pausa = pausa.load(Ordering::Relaxed);
        reloj.apuntar_pausa(en_pausa, Instant::now());

        // 1. Whatever the master delivered: its samples and timing take priority.
        let mut llego_algo = false;
        loop {
            let disponible = unsafe { captura.GetNextPacketSize() }
                .map_err(|e| AppError::Msg(format!("Could not read audio: {e}")))?;
            if disponible == 0 {
                break;
            }

            let mut datos = std::ptr::null_mut();
            let mut instantes = 0u32;
            let mut banderas = 0u32;
            unsafe { captura.GetBuffer(&mut datos, &mut instantes, &mut banderas, None, None) }
                .map_err(|e| AppError::Msg(format!("Could not acquire audio: {e}")))?;

            let bytes = instantes as usize * formato.bytes_por_instante() as usize;
            // AUDCLNT_BUFFERFLAGS_SILENT (2): Windows reports silence without filling the
            // buffer, so insert zeros manually to keep audio aligned with video.
            let trozo = if banderas & 0x2 != 0 || datos.is_null() {
                vec![0u8; bytes]
            } else {
                unsafe { std::slice::from_raw_parts(datos, bytes) }.to_vec()
            };
            unsafe { captura.ReleaseBuffer(instantes) }
                .map_err(|e| AppError::Msg(format!("Could not release audio: {e}")))?;
            llego_algo = true;

            // While paused, read data is discarded and `escritos` does not advance, so the
            // pause does not exist in the output, exactly as for video.
            if en_pausa {
                if let Some(mic) = acompannante.as_mut() {
                    let _ = unsafe { leer_todo(&mic.captura, mic.formato) };
                    mic.pendiente.clear();
                }
                continue;
            }

            if !emitir(
                trozo,
                formato,
                acompannante.as_mut(),
                &mut escritos,
                tx,
                nivel_maestro,
                &niveles.microfono,
            ) {
                // Nobody is listening any more: the recording has ended.
                break;
            }
        }
        if en_pausa {
            continue;
        }

        // 2. If the master has gone quiet, the wall clock takes over and the missing time is
        // written as silence, with the microphone mixed on top.
        //
        // WASAPI loopback delivers nothing when nothing is playing. Without this, narration
        // over a silent desktop would be lost and audio would fall behind video by the length
        // of every silence.
        if !llego_algo {
            let emitido = formato.duracion(escritos);
            if let Some(instantes) = instantes_que_faltan(
                reloj.transcurrido(Instant::now()),
                emitido,
                formato.muestras_por_segundo,
            ) {
                let silencio = vec![0u8; instantes * formato.bytes_por_instante() as usize];
                let _ = emitir(
                    silencio,
                    formato,
                    acompannante.as_mut(),
                    &mut escritos,
                    tx,
                    nivel_maestro,
                    &niveles.microfono,
                );
            }
        }
    }

    let _ = unsafe { cliente.Stop() };
    if let Some(mic) = acompannante.as_ref() {
        let _ = unsafe { mic.cliente.Stop() };
    }
    Ok(())
}

/// Emit a chunk with the microphone mixed in and advance the written-audio clock. Returns
/// `false` when nobody is listening any more.
fn emitir(
    trozo: Vec<u8>,
    formato: Formato,
    mic: Option<&mut Acompannante>,
    escritos: &mut usize,
    tx: &SyncSender<Trozo>,
    nivel_maestro: &std::sync::atomic::AtomicU32,
    nivel_mic: &std::sync::atomic::AtomicU32,
) -> bool {
    Niveles::subir(nivel_maestro, pico_de_bytes(&trozo));
    // `mezclar_encima` takes exactly this chunk's duration from the microphone; if the
    // microphone lags, the rest is silent and system audio is kept whole.
    let trozo = match mic {
        Some(mic) => mezclar_encima(trozo, formato, mic, nivel_mic),
        None => trozo,
    };
    let desde_el_inicio = formato.duracion_de(*escritos);
    *escritos += trozo.len();
    match tx.try_send(Trozo {
        datos: trozo,
        desde_el_inicio,
    }) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// How many frames of silence are needed for written audio to catch up with the clock, or
/// `None` while the gap is below `SEQUIA`. Pure so it can be tested.
fn instantes_que_faltan(
    reloj: Duration,
    emitido: Duration,
    muestras_por_segundo: u32,
) -> Option<usize> {
    let falta = reloj.checked_sub(emitido)?;
    if falta < SEQUIA {
        return None;
    }
    Some((falta.as_nanos() * u128::from(muestras_por_segundo) / 1_000_000_000) as usize)
}

/// Wall-clock recording time minus paused time. Instants are parameters so it can be tested.
struct RelojDeGrabacion {
    arranque: Instant,
    pausado: Duration,
    pausa_desde: Option<Instant>,
}

impl RelojDeGrabacion {
    fn arrancar(ahora: Instant) -> Self {
        Self {
            arranque: ahora,
            pausado: Duration::ZERO,
            pausa_desde: None,
        }
    }

    /// Told each iteration whether recording is paused; tracks when pauses start and end.
    fn apuntar_pausa(&mut self, en_pausa: bool, ahora: Instant) {
        match (en_pausa, self.pausa_desde) {
            (true, None) => self.pausa_desde = Some(ahora),
            (false, Some(desde)) => {
                self.pausado += ahora.saturating_duration_since(desde);
                self.pausa_desde = None;
            }
            _ => {}
        }
    }

    /// Actual recorded time, excluding finished pauses and a pause in progress.
    fn transcurrido(&self, ahora: Instant) -> Duration {
        let en_curso = self
            .pausa_desde
            .map(|desde| ahora.saturating_duration_since(desde))
            .unwrap_or_default();
        ahora
            .saturating_duration_since(self.arranque)
            .saturating_sub(self.pausado + en_curso)
    }
}

/// The microphone when it is mixed with system audio, plus leftover samples from the last
/// iteration.
struct Acompannante {
    cliente: IAudioClient,
    captura: IAudioCaptureClient,
    formato: Formato,
    /// Samples already converted to the master format and not used yet.
    pendiente: Vec<f32>,
}

/// Mix microphone samples onto the master chunk. The microphone is drained each time into
/// `pendiente` (already converted); exactly this chunk's duration is consumed so the voice
/// neither runs ahead nor accumulates delay.
fn mezclar_encima(
    trozo: Vec<u8>,
    formato: Formato,
    mic: &mut Acompannante,
    nivel: &std::sync::atomic::AtomicU32,
) -> Vec<u8> {
    if let Some(nuevas) = unsafe { leer_todo(&mic.captura, mic.formato) } {
        mic.pendiente
            .extend(super::mezcla::adaptar(&nuevas, mic.formato, formato));
    }
    // If the microphone clock runs slightly fast, the backlog would grow forever and the voice
    // would drift late. Keep at most half a second, dropping the oldest samples.
    let tope = formato.muestras_por_segundo as usize * usize::from(formato.canales.max(1)) / 2;
    if mic.pendiente.len() > tope {
        let sobra = mic.pendiente.len() - tope;
        mic.pendiente.drain(..sobra);
    }
    let mut muestras = super::mezcla::como_flotantes(&trozo);
    let cuantas = muestras.len().min(mic.pendiente.len());
    if cuantas == 0 {
        return trozo;
    }
    Niveles::subir(nivel, pico(mic.pendiente[..cuantas].iter().copied()));
    super::mezcla::sumar(&mut muestras, &mic.pendiente[..cuantas]);
    mic.pendiente.drain(..cuantas);
    super::mezcla::como_bytes(&muestras)
}

/// Drain a device's buffer and return its samples, or `None` if empty.
unsafe fn leer_todo(captura: &IAudioCaptureClient, formato: Formato) -> Option<Vec<f32>> {
    let mut todo: Vec<f32> = Vec::new();
    loop {
        let disponible = unsafe { captura.GetNextPacketSize() }.ok()?;
        if disponible == 0 {
            break;
        }
        let mut datos = std::ptr::null_mut();
        let mut instantes = 0u32;
        let mut banderas = 0u32;
        unsafe { captura.GetBuffer(&mut datos, &mut instantes, &mut banderas, None, None) }.ok()?;
        let bytes = instantes as usize * formato.bytes_por_instante() as usize;
        if banderas & 0x2 != 0 || datos.is_null() {
            todo.extend(std::iter::repeat_n(0.0, bytes / std::mem::size_of::<f32>()));
        } else {
            let crudo = unsafe { std::slice::from_raw_parts(datos, bytes) };
            todo.extend(super::mezcla::como_flotantes(crudo));
        }
        let _ = unsafe { captura.ReleaseBuffer(instantes) };
    }
    (!todo.is_empty()).then_some(todo)
}

/// Which device to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispositivo {
    /// The speakers in loopback mode: records what is playing.
    Altavoz,
    /// The microphone, recorded as a normal input.
    Microfono,
}

/// Open the requested device and return the format it uses.
unsafe fn abrir_elegido(
    que: Dispositivo,
    device_id: Option<&str>,
) -> Result<(IAudioClient, IAudioCaptureClient, Formato)> {
    let enumerador: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| AppError::Msg(format!("Could not enumerate audio devices: {e}")))?;
    // `eRender` is output (speakers), `eCapture` is input (microphone). `eConsole` follows the
    // user's default device in Windows.
    let (flujo, nombre) = match que {
        Dispositivo::Altavoz => (eRender, "speaker"),
        Dispositivo::Microfono => (eCapture, "microphone"),
    };
    let dispositivo = match device_id.filter(|id| !id.is_empty()) {
        Some(id) => {
            use windows::core::{Interface, HSTRING};
            use windows::Win32::Media::Audio::IMMEndpoint;
            let device = enumerador
                .GetDevice(&HSTRING::from(id))
                .map_err(|e| AppError::Msg(format!("Could not open the selected {nombre}: {e}")))?;
            if device.cast::<IMMEndpoint>()?.GetDataFlow()? != flujo {
                return Err(AppError::Msg(format!(
                    "The selected device is not a {nombre}."
                )));
            }
            device
        }
        None => enumerador
            .GetDefaultAudioEndpoint(flujo, eConsole)
            .map_err(|e| AppError::Msg(format!("No default {nombre}: {e}")))?,
    };

    let cliente: IAudioClient = dispositivo
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| AppError::Msg(format!("Could not open {nombre}: {e}")))?;

    let formato_ptr = cliente
        .GetMixFormat()
        .map_err(|e| AppError::Msg(format!("Could not determine the audio format: {e}")))?;
    // Windows allocates the format and it must be freed either way: copy what is needed and
    // free it immediately.
    let formato = Formato {
        canales: (*formato_ptr).nChannels,
        muestras_por_segundo: (*formato_ptr).nSamplesPerSec,
        bits_por_muestra: (*formato_ptr).wBitsPerSample,
    };

    // Loopback only exists for outputs; inputs are recorded directly.
    let banderas = match que {
        Dispositivo::Altavoz => AUDCLNT_STREAMFLAGS_LOOPBACK,
        Dispositivo::Microfono => 0,
    };
    let inicio = cliente.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        banderas,
        COLCHON_100NS,
        0,
        formato_ptr,
        None,
    );
    CoTaskMemFree(Some(formato_ptr as *const _));
    inicio.map_err(|e| AppError::Msg(format!("Could not capture {nombre}: {e}")))?;

    let captura: IAudioCaptureClient = cliente
        .GetService()
        .map_err(|e| AppError::Msg(format!("Could not read from {nombre}: {e}")))?;

    Ok((cliente, captura, formato))
}

/// COM inicializado mientras viva, y cerrado al soltarlo.
pub(crate) struct Com;

impl Com {
    pub(crate) fn inicializar() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| AppError::Msg(format!("Could not prepare audio: {e}")))?;
        Ok(Self)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Convert 32-bit float samples (the Windows mixer format) to 16-bit integers for the encoder.
pub fn a_pcm16(flotantes: &[u8]) -> Vec<u8> {
    let mut salida = Vec::with_capacity(flotantes.len() / 2);
    for muestra in flotantes.chunks_exact(4) {
        let valor = f32::from_le_bytes([muestra[0], muestra[1], muestra[2], muestra[3]]);
        // Clamp to [-1, 1] before scaling; values above 1 would wrap around and click.
        let recortado = valor.clamp(-1.0, 1.0);
        let entero = (recortado * i16::MAX as f32) as i16;
        salida.extend_from_slice(&entero.to_le_bytes());
    }
    salida
}
