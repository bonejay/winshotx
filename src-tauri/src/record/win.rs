use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Instant;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor as WcMonitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::capture::Rect;
use crate::error::{AppError, Result};

/// windows-capture identifica monitores por HMONITOR, asi que se lo pedimos a Win32.
pub(crate) fn monitor_at(x: i32, y: i32) -> Result<WcMonitor> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};

    let handle = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if handle.is_invalid() {
        return Err(AppError::Msg("No display at this position".into()));
    }
    Ok(WcMonitor::from_raw_hmonitor(handle.0))
}

/// Un fotograma recien capturado, ya recortado a la region y en BGRA.
pub struct CapturedFrame {
    pub bgra: Vec<u8>,
    pub ts_ms: u64,
}

#[derive(Clone)]
pub struct CaptureFlags {
    pub sender: SyncSender<CapturedFrame>,
    /// Recorte dentro del monitor: x1, y1, x2, y2.
    pub crop: (u32, u32, u32, u32),
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    /// Milisegundos acumulados en pausa, para que el reloj no cuente ese tiempo.
    pub paused_ms: Arc<AtomicU64>,
    pub min_interval: std::time::Duration,
    /// El cero del reloj de los fotogramas. Lo crea quien arranca la grabacion y lo
    /// comparte con el hilo que muestrea el raton: asi el rastro y la imagen van en el
    /// mismo tiempo. Antes cada uno tenia su `Instant::now()` y se separaban unos
    /// milisegundos que no se podian corregir.
    pub start: Instant,
    /// Los bufers ya usados que vuelven para el siguiente fotograma. Ver `Reciclados`.
    pub reciclados: Reciclados,
}

/// Bufers de fotograma que van y vuelven, para no reservar ocho megabytes nuevos sesenta
/// veces por segundo.
///
/// **Medido el 17 de septiembre de 2026** (`bench_anillo`, en release): copiar un fotograma
/// de 1920x1080 a un `Vec` recien reservado cuesta 3,4 ms, y copiarlo a uno que ya se uso
/// cuesta 0,5 ms. La diferencia no es la copia, es que Windows tiene que buscar y poner a
/// cero ocho megabytes de paginas nuevas cada vez, y el anillo hace eso toda la tarde.
///
/// Quien consume el fotograma lo devuelve con `devolver` cuando termina; la captura coge
/// uno con `coger` antes de copiar. Si no hay ninguno devuelto se reserva uno nuevo, como
/// antes: esto es un ahorro, nunca una espera.
#[derive(Clone, Default)]
pub struct Reciclados(Arc<parking_lot::Mutex<Vec<Vec<u8>>>>);

impl Reciclados {
    /// Cuantos se guardan como mucho. Los que sobren se sueltan: si el consumidor va por
    /// detras y devuelve de golpe, no hace falta quedarse con veinte fotogramas en memoria.
    const TOPE: usize = 4;

    /// Un bufer vacio, con la memoria de la vez anterior si la hay.
    pub fn coger(&self) -> Vec<u8> {
        self.0.lock().pop().unwrap_or_default()
    }

    /// Devuelve un bufer para que lo use el siguiente fotograma.
    pub fn devolver(&self, mut bufer: Vec<u8>) {
        bufer.clear();
        let mut guardados = self.0.lock();
        if guardados.len() < Self::TOPE {
            guardados.push(bufer);
        }
    }
}

fn frame_due(next: &mut Option<Instant>, now: Instant, interval: std::time::Duration) -> bool {
    if interval.is_zero() {
        return true;
    }
    let deadline = next.unwrap_or(now);
    let tolerance = (interval / 16).min(std::time::Duration::from_micros(500));
    if now + tolerance < deadline {
        return false;
    }
    let periods = now.saturating_duration_since(deadline).as_nanos() / interval.as_nanos() + 1;
    *next = Some(deadline + interval.mul_f64(periods as f64));
    true
}

pub struct RegionCapture {
    flags: CaptureFlags,
    next_emit: Option<Instant>,
    /// El apoyo que necesita el crate cuando la textura viene con relleno de fila. Se
    /// guarda entre fotogramas para no reservar ocho megabytes nuevos sesenta veces por
    /// segundo: la copia que se manda por el canal sigue siendo una, esta no cuenta.
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for RegionCapture {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            flags: ctx.flags,
            next_emit: None,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        if self.flags.stop.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }
        if self.flags.pause.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Keep a target cadence without rounding 120 fps to whole milliseconds or
        // halving delivery when the monitor runs at a different refresh rate.
        let now = Instant::now();
        if !frame_due(&mut self.next_emit, now, self.flags.min_interval) {
            return Ok(());
        }

        let (x1, y1, x2, y2) = self.flags.crop;
        // Grabando una VENTANA, su tamanno puede cambiar mientras se graba, y el
        // codificador necesita el mismo tamanno en todos los fotogramas. Se coge lo que
        // haya y lo que falte se queda en negro; si la ventana se ha ido del todo, este
        // fotograma se salta.
        let (ancho_real, alto_real) = (frame.width(), frame.height());
        let (hasta_x, hasta_y) = (x2.min(ancho_real), y2.min(alto_real));
        if x1 >= hasta_x || y1 >= hasta_y {
            return Ok(());
        }
        let buffer = frame.buffer_crop(x1, y1, hasta_x, hasta_y)?;
        // El crate necesita un Vec de apoyo por si la textura viene con relleno de fila.
        // A un bufer devuelto si lo hay, que ya tiene sus paginas puestas; si no, a uno nuevo.
        let mut data = self.flags.reciclados.coger();
        let pedido = ((x2 - x1) as usize, (y2 - y1) as usize);
        let hay = ((hasta_x - x1) as usize, (hasta_y - y1) as usize);
        let sin_relleno = buffer.as_nopadding_buffer(&mut self.scratch);
        if hay == pedido {
            data.extend_from_slice(sin_relleno);
        } else {
            data.resize(pedido.0 * pedido.1 * 4, 0);
            for fila in 0..hay.1 {
                let desde = fila * hay.0 * 4;
                let hasta = desde + hay.0 * 4;
                let destino = fila * pedido.0 * 4;
                data[destino..destino + hay.0 * 4].copy_from_slice(&sin_relleno[desde..hasta]);
            }
        }

        let elapsed = now.duration_since(self.flags.start).as_millis() as u64;
        let ts_ms = elapsed.saturating_sub(self.flags.paused_ms.load(Ordering::Relaxed));
        // Bound queued raw frames so high-rate capture cannot grow memory without
        // limit when encoding or storage is slower. Timing retains the real gap.
        match self
            .flags
            .sender
            .try_send(CapturedFrame { bgra: data, ts_ms })
        {
            Ok(()) => {}
            Err(TrySendError::Full(frame)) => self.flags.reciclados.devolver(frame.bgra),
            Err(TrySendError::Disconnected(frame)) => {
                self.flags.reciclados.devolver(frame.bgra);
                self.flags.stop.store(true, Ordering::Relaxed);
                control.stop();
            }
        }
        Ok(())
    }

    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        self.flags.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
}

pub type Control = windows_capture::capture::CaptureControl<
    RegionCapture,
    Box<dyn std::error::Error + Send + Sync>,
>;

/// Arranca la captura de la region sobre el monitor que la contiene.
/// `monitor_origin` viene de la enumeracion de xcap, que si expone coordenadas.
pub fn start(
    region: Rect,
    monitor_origin: (i32, i32),
    capture_cursor: bool,
    fps: u32,
    flags_base: CaptureFlags,
) -> Result<Control> {
    let (cx, cy) = region.center();
    let monitor = monitor_at(cx, cy)?;

    let x1 = (region.x - monitor_origin.0).max(0) as u32;
    let y1 = (region.y - monitor_origin.1).max(0) as u32;
    let flags = CaptureFlags {
        crop: (x1, y1, x1 + region.width, y1 + region.height),
        min_interval: std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
        ..flags_base
    };

    let settings = Settings::new(
        monitor,
        if capture_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        },
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    RegionCapture::start_free_threaded(settings).map_err(|e| AppError::Msg(e.to_string()))
}

/// Arranca la captura de UNA VENTANA, se vea o no se vea.
///
/// Es lo que no se puede hacer capturando la pantalla y recortando: ahi lo que se graba es
/// lo que hay delante, asi que cualquier cosa que pase por encima (un aviso, el explorador,
/// otra ventana) acaba dentro del video. Windows compone cada ventana por su cuenta, y
/// pidiendole ESA, lo de delante no sale.
///
/// **El tamanno se clava al que tiene al empezar.** Una ventana se puede redimensionar
/// mientras se graba y el codificador necesita que todos los fotogramas midan igual: si
/// crece, se graba el trozo de siempre; si encoge, lo que falta se queda en negro.
pub fn start_window(
    hwnd: isize,
    capture_cursor: bool,
    fps: u32,
    flags_base: CaptureFlags,
) -> Result<Control> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};

    let ventana = HWND(hwnd as *mut core::ffi::c_void);
    // El tamanno que se le pide a Windows es el del marco REAL (el que compone el
    // escritorio), no el de `GetWindowRect`, que incluye el borde invisible de las sombras
    // y dejaria franjas transparentes a los lados.
    let mut marco = RECT::default();
    let medida = unsafe {
        DwmGetWindowAttribute(
            ventana,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut marco).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if medida.is_err() {
        return Err(AppError::Msg("The selected window no longer exists".into()));
    }
    let ancho = (marco.right - marco.left).max(1) as u32;
    let alto = (marco.bottom - marco.top).max(1) as u32;

    let flags = CaptureFlags {
        crop: (0, 0, ancho, alto),
        min_interval: std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
        ..flags_base
    };

    let settings = Settings::new(
        windows_capture::window::Window::from_raw_hwnd(ventana.0),
        if capture_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        },
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    RegionCapture::start_free_threaded(settings).map_err(|e| AppError::Msg(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, ShowWindow,
        SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    };

    const CLASE: &[u16] = &[
        b'w' as u16,
        b's' as u16,
        b'x' as u16,
        b'-' as u16,
        b'p' as u16,
        b'r' as u16,
        b'u' as u16,
        b'e' as u16,
        b'b' as u16,
        b'a' as u16,
        0,
    ];

    /// Lo unico que hace esta ventana es dejarse pintar del color de su clase.
    unsafe extern "system" fn procedimiento(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, w, l) }
    }

    /// Un rectangulo magenta, encima de todo y que no se puede pulsar, en la esquina de la
    /// pantalla principal. El magenta no sale en ningun escritorio por accidente.
    fn ventana_magenta(region: Rect) -> isize {
        let instancia = unsafe { GetModuleHandleW(None) }.expect("sin instancia");
        let clase = WNDCLASSW {
            hInstance: instancia.into(),
            lpszClassName: PCWSTR(CLASE.as_ptr()),
            lpfnWndProc: Some(procedimiento),
            hbrBackground: unsafe { CreateSolidBrush(COLORREF(0x00FF_00FF)) },
            ..Default::default()
        };
        let _ = unsafe { RegisterClassW(&clase) };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                PCWSTR(CLASE.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                region.x,
                region.y,
                region.width as i32,
                region.height as i32,
                None,
                None,
                Some(instancia.into()),
                None,
            )
        }
        .expect("no se ha podido abrir la ventana de prueba");
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        hwnd.0 as isize
    }

    /// Graba la region medio segundo y dice si el ultimo fotograma lleva magenta.
    fn sale_el_magenta(region: Rect, ventana: isize) -> bool {
        // Windows solo entrega un fotograma cuando algo cambia en el monitor. Con la
        // pantalla quieta llegaba UNO y luego nada, y esta prueba dependia de que el
        // terminal estuviera escribiendo en esa pantalla para pasar. Asi que la propia
        // ventana se mueve un pixel de un lado a otro mientras dura la captura, y los
        // fotogramas llegan porque hay motivo, no por suerte.
        //
        // La mueve ESTE hilo, que es el que la creo: desde otro hilo `SetWindowPos` manda
        // el mensaje al dueño, que estaria aqui esperando fotogramas sin atenderlo, y se
        // queda colgado para siempre. Ya paso.
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let hwnd = HWND(ventana as *mut core::ffi::c_void);
        let mut paso = 0;
        let (sender, receiver) = sync_channel::<CapturedFrame>(2);
        let stop = Arc::new(AtomicBool::new(false));
        let control = start(
            region,
            (0, 0),
            false,
            30,
            CaptureFlags {
                sender,
                crop: (0, 0, 0, 0),
                stop: stop.clone(),
                pause: Arc::new(AtomicBool::new(false)),
                paused_ms: Arc::new(AtomicU64::new(0)),
                min_interval: std::time::Duration::ZERO,
                start: Instant::now(),
                reciclados: Reciclados::default(),
            },
        )
        .expect("no se ha podido arrancar la captura");

        // El ULTIMO de medio segundo, no el primero: los primeros fotogramas pueden venir
        // de antes de que el compositor haya pintado el cambio.
        let hasta = Instant::now() + Duration::from_millis(600);
        let mut ultimo = None;
        while Instant::now() < hasta {
            paso = (paso + 1) % 2;
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    region.x + paso,
                    region.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            if let Ok(frame) = receiver.recv_timeout(Duration::from_millis(30)) {
                ultimo = Some(frame);
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = control.stop();

        let frame = ultimo.expect("la captura no ha dado ni un fotograma");
        // BGRA: magenta es azul al maximo, verde a cero y rojo al maximo.
        frame
            .bgra
            .chunks_exact(4)
            .any(|p| p[0] > 200 && p[1] < 60 && p[2] > 200)
    }

    /// Graba LA VENTANA medio segundo y dice si lo que sale es suyo.
    ///
    /// Igual que `sale_el_magenta`, pero pidiendole a Windows esa ventana en vez de un
    /// trozo de pantalla. Se mueve desde este hilo por lo mismo: es el que la creo.
    fn graba_la_ventana(ventana: isize, region: Rect) -> Option<CapturedFrame> {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let hwnd = HWND(ventana as *mut core::ffi::c_void);
        let mut paso = 0;
        let (sender, receiver) = sync_channel::<CapturedFrame>(2);
        let stop = Arc::new(AtomicBool::new(false));
        let control = start_window(
            ventana,
            false,
            30,
            CaptureFlags {
                sender,
                crop: (0, 0, 0, 0),
                stop: stop.clone(),
                pause: Arc::new(AtomicBool::new(false)),
                paused_ms: Arc::new(AtomicU64::new(0)),
                min_interval: std::time::Duration::ZERO,
                start: Instant::now(),
                reciclados: Reciclados::default(),
            },
        )
        .expect("no se ha podido arrancar la captura de la ventana");

        let hasta = Instant::now() + Duration::from_millis(600);
        let mut ultimo = None;
        while Instant::now() < hasta {
            paso = (paso + 1) % 2;
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    region.x + paso,
                    region.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            if let Ok(frame) = receiver.recv_timeout(Duration::from_millis(30)) {
                ultimo = Some(frame);
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = control.stop();
        ultimo
    }

    /// Cuantos pixeles de ese fotograma son magenta, en tanto por ciento.
    fn cuanto_magenta(frame: &CapturedFrame) -> f32 {
        let total = frame.bgra.len() / 4;
        if total == 0 {
            return 0.0;
        }
        let magenta = frame
            .bgra
            .chunks_exact(4)
            .filter(|p| p[0] > 200 && p[1] < 60 && p[2] > 200)
            .count();
        magenta as f32 * 100.0 / total as f32
    }

    /// **Lo que promete grabar una ventana: que lo de delante no sale.**
    ///
    /// Se abren dos ventanas en el mismo sitio, una magenta debajo y una verde encima que
    /// la tapa entera. Grabando la PANTALLA ahi solo se ve verde, que es el problema de
    /// siempre: un aviso que salta encima se cuela en el video. Grabando la VENTANA
    /// magenta, Windows la compone aparte y lo que sale es magenta.
    ///
    ///     cargo test --lib grabar_una_ventana -- --ignored --nocapture
    #[test]
    #[ignore = "abre dos ventanas en la pantalla y graba un trozo: se corre a mano"]
    fn grabar_una_ventana_tapada_la_graba_a_ella_y_no_a_lo_de_delante() {
        let region = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 120,
        };
        let abajo = ventana_magenta(region);
        std::thread::sleep(Duration::from_millis(300));

        let suyo = graba_la_ventana(abajo, region).expect("sin fotogramas de la ventana");
        let antes = cuanto_magenta(&suyo);
        println!("sin nada delante: {antes:.1}% magenta");
        assert!(
            antes > 50.0,
            "la ventana no se graba ni estando a la vista: {antes:.1}%"
        );

        // Y ahora una verde del mismo tamanno encima, que la tapa entera.
        let encima = ventana_tapadera(region);
        std::thread::sleep(Duration::from_millis(400));

        // La pantalla de esa esquina ya no tiene magenta: lo de delante manda.
        let de_la_pantalla = sale_el_magenta(region, abajo);

        let tapada = graba_la_ventana(abajo, region).expect("sin fotogramas con la tapadera");
        let despues = cuanto_magenta(&tapada);
        println!("con la verde delante: {despues:.1}% magenta");

        unsafe {
            let _ = DestroyWindow(HWND(encima as *mut core::ffi::c_void));
            let _ = DestroyWindow(HWND(abajo as *mut core::ffi::c_void));
        }

        assert!(
            !de_la_pantalla,
            "la tapadera no esta tapando: esta prueba no prueba nada asi"
        );
        assert!(
            despues > 50.0,
            "tapada, la grabacion de la ventana ha salido con {despues:.1}% de lo suyo: \
             se esta grabando lo de delante y no la ventana"
        );
    }

    /// Una ventana verde del mismo tamanno, para ponerla encima.
    fn ventana_tapadera(region: Rect) -> isize {
        const CLASE_VERDE: &[u16] = &[
            b'w' as u16,
            b's' as u16,
            b'x' as u16,
            b't' as u16,
            b'a' as u16,
            b'p' as u16,
            0,
        ];
        let instancia = unsafe { GetModuleHandleW(None) }.expect("sin instancia");
        let clase = WNDCLASSW {
            hInstance: instancia.into(),
            lpszClassName: PCWSTR(CLASE_VERDE.as_ptr()),
            lpfnWndProc: Some(procedimiento),
            hbrBackground: unsafe { CreateSolidBrush(COLORREF(0x0000_FF00)) },
            ..Default::default()
        };
        let _ = unsafe { RegisterClassW(&clase) };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                PCWSTR(CLASE_VERDE.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                region.x,
                region.y,
                region.width as i32,
                region.height as i32,
                None,
                None,
                Some(instancia.into()),
                None,
            )
        }
        .expect("no se ha podido abrir la tapadera");
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        hwnd.0 as isize
    }

    /// La prueba de que el arreglo sirve de algo: una ventana marcada como fuera de la
    /// captura NO sale en lo que graba winshotx, y la misma ventana sin marcar SI sale.
    ///
    /// Esta ignorada porque abre una ventana en la pantalla de quien la corra y graba
    /// medio segundo de esa esquina: no es para una maquina de integracion, es para
    /// comprobar a mano que Windows sigue cumpliendo.
    ///
    ///     cargo test --lib una_ventana_excluida -- --ignored --nocapture
    #[test]
    #[ignore = "abre una ventana en la pantalla y graba un trozo: se corre a mano"]
    fn una_ventana_excluida_no_sale_en_lo_que_se_graba() {
        let region = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 120,
        };
        let ventana = ventana_magenta(region);
        std::thread::sleep(Duration::from_millis(300));

        assert!(
            sale_el_magenta(region, ventana),
            "sin excluirla tiene que salir, o esta prueba no prueba nada"
        );

        assert!(
            crate::platform::window_style::fuera_de_la_captura_hwnd(ventana, true),
            "Windows no ha aceptado sacarla de la captura"
        );
        std::thread::sleep(Duration::from_millis(300));
        let sigue = sale_el_magenta(region, ventana);

        unsafe {
            let _ = DestroyWindow(HWND(ventana as *mut core::ffi::c_void));
        }
        assert!(
            !sigue,
            "la ventana excluida se sigue colando en la grabacion"
        );
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    #[test]
    fn a_144_hz_source_can_deliver_a_120_fps_target() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..1440)
            .filter(|i| {
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 144.0),
                    interval,
                )
            })
            .count();
        assert!(
            (1199..=1201).contains(&accepted),
            "{accepted} frames in ten seconds"
        );
    }
    #[test]
    fn a_240_hz_source_is_limited_to_120_fps_without_phase_drift() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..2400)
            .filter(|i| {
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 240.0),
                    interval,
                )
            })
            .count();
        assert!(
            (1199..=1201).contains(&accepted),
            "{accepted} frames in ten seconds"
        );
    }
    #[test]
    fn small_delivery_jitter_does_not_drop_every_other_frame() {
        let start = Instant::now();
        let mut next = None;
        let interval = std::time::Duration::from_secs_f64(1.0 / 120.0);
        let accepted = (0..1200)
            .filter(|i| {
                let jitter = if i % 2 == 0 { 0.0001 } else { 0.0 };
                frame_due(
                    &mut next,
                    start + std::time::Duration::from_secs_f64(*i as f64 / 120.0 + jitter),
                    interval,
                )
            })
            .count();
        assert_eq!(accepted, 1200);
    }
}
