//! Las fotos fijas por la captura moderna de Windows (Windows.Graphics.Capture).
//!
//! `xcap` fotografia con GDI, y GDI no siempre ve lo que se compone con la tarjeta grafica:
//! encima de una ventana de cristal con dos WebView2 dentro (Adeorq) salian fotos con trozos
//! en negro, medido el 18 de octubre de 2026 en 33 de cada 80, y por aqui en ninguna. Los
//! videos ya iban por esta captura (`record::win`), y por eso salian enteros.
//!
//! **Usa las piezas de `windows-capture`, pero no su arranque.** Ese arranque crea un
//! dispositivo de la tarjeta grafica nuevo cada vez, y en la maquina de Munir eso son unos
//! 100 ms: la foto pasaba de 51 ms por GDI a 175. En un video que dura minutos no se nota;
//! en una foto es todo el atajo. Aqui el dispositivo se crea una vez y se guarda.

use std::sync::mpsc::sync_channel;
use std::sync::OnceLock;
use std::time::Duration;

use image::RgbaImage;
use parking_lot::Mutex;
use windows::core::{factory, Interface, Ref};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows_capture::d3d11::{create_d3d_device, create_direct3d_device, StagingTexture};
use windows_capture::graphics_capture_api::GraphicsCaptureApi;

use super::MonitorInfo;
use crate::error::{AppError, Result};

/// Lo que se espera al primer fotograma antes de darlo por perdido. Windows lo entrega en
/// cuanto empieza la captura, aunque la pantalla este quieta, asi que esto solo se agota si
/// algo va mal de verdad; y entonces la foto sale por GDI, tarde pero sale.
const ESPERA: Duration = Duration::from_millis(500);

/// Lo que se espera a un fotograma mas fresco que el primero, en `Momento::ElMasFresco`.
/// Son 20 ms porque una pantalla de 60 Hz compone uno cada 16,7, y a 120 Hz cada 8,3: en
/// las dos cabe uno. Si no llega, vale el primero; esperar mas seria pagar por nada en la
/// pantalla quieta, que es el caso normal al hacer una foto.
const UN_FOTOGRAMA: Duration = Duration::from_millis(20);

/// Cuando se da por buena la foto.
///
/// **Existe para poder demostrar que el de siempre vale, no porque haga falta cambiarlo.**
/// Windows entrega un fotograma nada mas empezar la captura, y ese fotograma es lo que el
/// compositor ya tenia compuesto: sobre el papel, si la pantalla acaba de cambiar podria
/// ser el de justo antes. Medido el 21 de octubre de 2026 con `medir_el_retraso_de_la_foto`,
/// que pinta un color y mira si la foto lo trae: **las tres formas aciertan 20 de 20**, y
/// `ElPrimero` es la que menos tarda (34 ms, contra 39 y 65).
///
/// La razon es que montar la captura ya cuesta esos 34 ms, y para entonces el compositor
/// lleva rato habiendo pintado. Esperarle encima es pagar dos veces por lo mismo.
///
/// Lo que si parecia un retraso (1 foto de cada 80 en `medir_el_negro`) era del banco y no
/// de la app: comparaba la foto de una via con la de la otra, hechas en instantes
/// distintos, asi que a la via que mas tardaba le apuntaba como desfase el hueco entre las
/// dos. Por eso `el mas fresco`, que es la que mas espera por traer lo mas nuevo, salia la
/// peor de las tres.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Momento {
    /// El primero que entregue Windows, sin esperar a nada.
    ElPrimero,
    /// Se espera a que el compositor termine lo que tiene entre manos (`DwmFlush`) y
    /// entonces se empieza a capturar, asi que el primer fotograma ya lleva lo ultimo.
    TrasElCompositor,
    /// Se coge el primero y, si llega otro en lo que dura un fotograma de pantalla, ese.
    ElMasFresco,
}

/// Si este Windows puede hacer la foto como hace falta: sin el marco amarillo que pinta la
/// captura alrededor de la pantalla y sin el raton (la foto de GDI tampoco lo lleva). En
/// Windows 10 anterior a 2022 no se puede quitar el marco, y una foto que hace parpadear
/// la pantalla en amarillo es peor que la de siempre. Se pregunta una vez y se dice una vez.
pub fn disponible() -> bool {
    static SI: OnceLock<bool> = OnceLock::new();
    *SI.get_or_init(|| {
        let si = GraphicsCaptureApi::is_supported().unwrap_or(false)
            && GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false)
            && GraphicsCaptureApi::is_cursor_settings_supported().unwrap_or(false);
        if !si {
            eprintln!(
                "[captura] este Windows no deja hacer la foto con la captura moderna: las fotos van por GDI"
            );
        }
        si
    })
}

/// El dispositivo de la tarjeta grafica, el mismo para todas las fotos.
#[derive(Clone)]
struct Tarjeta {
    dispositivo: ID3D11Device,
    contexto: ID3D11DeviceContext,
}

/// Se crea con la primera foto (o con `preparar`, al arrancar) y se tira si una foto
/// falla: si la tarjeta se ha reiniciado (un controlador nuevo, un portatil que cambia de
/// grafica), el dispositivo viejo ya no vale y la siguiente foto crea otro.
static TARJETA: Mutex<Option<Tarjeta>> = Mutex::new(None);

/// El contexto de un dispositivo no se puede usar desde dos hilos a la vez, y las pantallas
/// se fotografian a la vez. Solo lo que lo toca (copiar, abrir y leer) va de una en una.
static COPIANDO: Mutex<()> = Mutex::new(());

fn tarjeta() -> Result<Tarjeta> {
    let mut guardada = TARJETA.lock();
    if let Some(tarjeta) = guardada.as_ref() {
        return Ok(tarjeta.clone());
    }
    let (dispositivo, contexto) =
        create_d3d_device().map_err(|e| AppError::Msg(format!("No graphics device: {e}")))?;
    let tarjeta = Tarjeta {
        dispositivo,
        contexto,
    };
    *guardada = Some(tarjeta.clone());
    Ok(tarjeta)
}

/// Crea el dispositivo antes de que haga falta, para que la primera foto no pague sus
/// 100 ms. Se llama al arrancar, en un hilo aparte; si falla, ya lo intentara la foto.
pub fn preparar() {
    if disponible() {
        let _ = tarjeta();
    }
}

/// Los pixeles de la textura, que vienen en BGRA y con relleno al final de cada fila, a
/// RGBA sin relleno, en una sola pasada. El alfa se pone opaco: una pantalla lo es entera,
/// y `stitch_all` cuenta con eso para distinguirla de los huecos entre monitores.
fn bgra_a_rgba(crudo: &[u8], ancho: u32, alto: u32, paso: usize) -> Vec<u8> {
    let fila = ancho as usize * 4;
    let mut rgba = vec![0u8; fila * alto as usize];
    for (y, destino) in rgba.chunks_exact_mut(fila).enumerate() {
        let origen = &crudo[y * paso..y * paso + fila];
        for (d, s) in destino.chunks_exact_mut(4).zip(origen.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
    }
    rgba
}

/// La foto de esa pantalla, del tamanno que dice `monitor`, que es el que ya usa todo lo
/// demas (el overlay, el recorte, juntar pantallas). Si sale de otro tamanno es un error y
/// no una foto: quien llama va por GDI antes que descuadrar la seleccion.
pub fn fotografiar(monitor: &MonitorInfo) -> Result<RgbaImage> {
    // El primero, que es el mas rapido y acierta igual: ver `Momento`.
    fotografiar_en(monitor, Momento::ElPrimero)
}

/// Lo mismo, diciendo con que fotograma se queda. Existe para poder medir los tres caminos
/// uno contra otro en `bench_freeze`, que es de donde salio que el de siempre deja pasar
/// una foto vieja de cada ochenta.
pub(crate) fn fotografiar_en(monitor: &MonitorInfo, momento: Momento) -> Result<RgbaImage> {
    let resultado = fotografiar_con(&tarjeta()?, monitor, momento);
    if resultado.is_err() {
        *TARJETA.lock() = None;
    }
    resultado
}

fn fotografiar_con(
    tarjeta: &Tarjeta,
    monitor: &MonitorInfo,
    momento: Momento,
) -> Result<RgbaImage> {
    let de_windows = |e: windows::core::Error| AppError::Msg(e.to_string());
    let pantalla = crate::record::win::monitor_at(
        monitor.x + (monitor.width / 2) as i32,
        monitor.y + (monitor.height / 2) as i32,
    )?;
    let item: GraphicsCaptureItem = unsafe {
        factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .and_then(|interop| interop.CreateForMonitor(HMONITOR(pantalla.as_raw_hmonitor())))
    }
    .map_err(de_windows)?;
    let tamanno = item.Size().map_err(de_windows)?;
    if (tamanno.Width as u32, tamanno.Height as u32) != (monitor.width, monitor.height) {
        return Err(AppError::Msg(format!(
            "Windows reported {}x{} but the display is {}x{}",
            tamanno.Width, tamanno.Height, monitor.width, monitor.height
        )));
    }

    // Sin hilo propio ni bucle de mensajes: el fotograma llega por un hilo del sistema y
    // viaja por el canal hasta aqui.
    let directo =
        create_direct3d_device(&tarjeta.dispositivo).map_err(|e| AppError::Msg(e.to_string()))?;
    let piscina = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &directo,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        tamanno,
    )
    .map_err(de_windows)?;
    let (salida, llegada) = sync_channel::<Direct3D11CaptureFrame>(1);
    piscina
        .FrameArrived(&TypedEventHandler::new(
            move |piscina: Ref<Direct3D11CaptureFramePool>, _| {
                if let Some(piscina) = piscina.as_ref() {
                    if let Ok(fotograma) = piscina.TryGetNextFrame() {
                        // Si ya habia uno, este sobra: con el primero basta.
                        let _ = salida.try_send(fotograma);
                    }
                }
                Ok(())
            },
        ))
        .map_err(de_windows)?;
    let sesion = piscina.CreateCaptureSession(&item).map_err(de_windows)?;
    let foto = (|| {
        sesion.SetIsBorderRequired(false).map_err(de_windows)?;
        sesion
            .SetIsCursorCaptureEnabled(false)
            .map_err(de_windows)?;
        if momento == Momento::TrasElCompositor {
            // Espera a que el compositor termine el fotograma que esta componiendo. Sin
            // esto la captura empieza a media composicion, y lo primero que entrega puede
            // ser el fotograma de antes del ultimo cambio.
            let _ = unsafe { DwmFlush() };
        }
        sesion.StartCapture().map_err(de_windows)?;
        let mut fotograma = llegada
            .recv_timeout(ESPERA)
            .map_err(|_| AppError::Msg("No capture frame was received".into()))?;
        if momento == Momento::ElMasFresco {
            // Si en lo que dura un fotograma de pantalla llega otro, ese es mas nuevo. Y si
            // no llega, que es lo normal con la pantalla quieta, vale el primero: esperar
            // mas seria perder la foto por nada.
            if let Ok(siguiente) = llegada.recv_timeout(UN_FOTOGRAMA) {
                let _ = fotograma.Close();
                fotograma = siguiente;
            }
        }
        // Se copia ANTES de cerrar la piscina, que es la duenna de la textura.
        let foto = copiar(tarjeta, &fotograma);
        let _ = fotograma.Close();
        foto
    })();
    let _ = sesion.Close();
    let _ = piscina.Close();
    let foto = foto?;

    if foto.dimensions() != (monitor.width, monitor.height) {
        return Err(AppError::Msg(format!(
            "The captured frame is {}x{} but the display is {}x{}",
            foto.width(),
            foto.height(),
            monitor.width,
            monitor.height
        )));
    }
    Ok(foto)
}

/// La textura del fotograma vive en la tarjeta grafica: se copia a una que el procesador
/// pueda leer, se abre, y se pasa a RGBA.
fn copiar(tarjeta: &Tarjeta, fotograma: &Direct3D11CaptureFrame) -> Result<RgbaImage> {
    let de_windows = |e: windows::core::Error| AppError::Msg(e.to_string());
    let textura: ID3D11Texture2D = fotograma
        .Surface()
        .and_then(|superficie| superficie.cast::<IDirect3DDxgiInterfaceAccess>())
        .and_then(|acceso| unsafe { acceso.GetInterface() })
        .map_err(de_windows)?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { textura.GetDesc(&mut desc) };
    let legible = StagingTexture::new(&tarjeta.dispositivo, desc.Width, desc.Height, desc.Format)
        .map_err(|e| AppError::Msg(e.to_string()))?;

    let _turno = COPIANDO.lock();
    let mut abierta = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        tarjeta.contexto.CopyResource(legible.texture(), &textura);
        tarjeta
            .contexto
            .Map(legible.texture(), 0, D3D11_MAP_READ, 0, Some(&mut abierta))
            .map_err(de_windows)?;
    }
    let paso = abierta.RowPitch as usize;
    // Map acaba de dar `RowPitch * alto` bytes legibles en `pData`, que valen hasta el Unmap.
    let crudo = unsafe {
        std::slice::from_raw_parts(abierta.pData as *const u8, paso * desc.Height as usize)
    };
    let rgba = bgra_a_rgba(crudo, desc.Width, desc.Height, paso);
    unsafe { tarjeta.contexto.Unmap(legible.texture(), 0) };

    RgbaImage::from_raw(desc.Width, desc.Height, rgba)
        .ok_or_else(|| AppError::Msg("Capture frame dimensions do not match".into()))
}

/// Pone el proceso de las pruebas a ver las pantallas como la app. La app las ve por
/// monitor y en pixeles fisicos (lo pone tao al arrancar), y un proceso de pruebas no sabe
/// de escalados: sin esto, en la pantalla al 125 % GDI fotografia otra zona y cualquier
/// comparacion con la captura moderna miente.
#[cfg(test)]
pub(crate) fn como_la_app() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una textura de 3x2 con cuatro bytes de relleno al final de cada fila, que es como la
    /// entrega la tarjeta grafica cuando el ancho no le cuadra. Si el relleno se colara, la
    /// segunda fila saldria corrida y la foto entera torcida.
    #[test]
    fn el_relleno_de_fila_no_se_cuela_y_los_canales_se_dan_la_vuelta() {
        let paso = 3 * 4 + 4;
        let mut crudo = vec![0xEEu8; paso * 2];
        for y in 0..2usize {
            for x in 0..3usize {
                let i = y * paso + x * 4;
                // B, G, R y un alfa a cero, que tiene que salir opaco.
                crudo[i..i + 4].copy_from_slice(&[x as u8, 10 + y as u8, 100 + x as u8, 0]);
            }
        }

        let rgba = bgra_a_rgba(&crudo, 3, 2, paso);

        assert_eq!(
            rgba.len(),
            3 * 2 * 4,
            "sin relleno: tres pixeles por dos filas"
        );
        assert!(!rgba.contains(&0xEE), "se ha colado el relleno de la fila");
        // El pixel x=2, y=1: R=102, G=11, B=2.
        assert_eq!(&rgba[(3 + 2) * 4..(3 + 2) * 4 + 4], &[102, 11, 2, 255]);
    }

    /// **La foto nueva es la misma pantalla que la de siempre**: del mismo tamanno en cada
    /// monitor (tambien en el que esta al 125 %), con los colores en su sitio y sin nada
    /// corrido. Todo en memoria.
    ///
    /// No basta con comparar una foto de cada: GDI no coincide ni consigo mismo dos veces
    /// seguidas encima de una ventana de cristal (es justo el fallo que esto arregla) y una
    /// pantalla con un video cambia entre foto y foto. Asi que se hacen GDI, moderna y GDI,
    /// y se mira solo donde las dos de GDI coinciden: ahi la moderna tiene que dar lo mismo.
    ///
    ///     cargo test --lib la_foto_moderna_es_la_misma -- --ignored --nocapture
    #[test]
    #[ignore = "fotografia las pantallas de verdad: se corre a mano"]
    fn la_foto_moderna_es_la_misma_pantalla_que_la_de_gdi() {
        como_la_app();
        let parecido = |a: &[u8], b: &[u8]| (0..3).all(|c| a[c].abs_diff(b[c]) <= 2);
        let monitores = xcap::Monitor::all().unwrap();
        for (i, monitor) in monitores.iter().enumerate() {
            let info = crate::capture::describir(i, monitor).unwrap();
            let antes = monitor.capture_image().unwrap();
            let moderna = fotografiar(&info).unwrap();
            let despues = monitor.capture_image().unwrap();
            assert_eq!(
                moderna.dimensions(),
                antes.dimensions(),
                "pantalla {i} al {:.0}%: la moderna no mide lo que la de GDI",
                info.scale * 100.0
            );
            let (mut quietos, mut iguales) = (0u64, 0u64);
            for ((a, m), d) in antes
                .as_raw()
                .chunks_exact(4)
                .zip(moderna.as_raw().chunks_exact(4))
                .zip(despues.as_raw().chunks_exact(4))
            {
                if parecido(a, d) {
                    quietos += 1;
                    if parecido(a, m) {
                        iguales += 1;
                    }
                }
            }
            let total = (info.width * info.height) as f64;
            let parte = iguales as f64 * 100.0 / quietos.max(1) as f64;
            println!(
                "pantalla {i}: {}x{} al {:.0}%; GDI quieto en el {:.1}% de la pantalla, y ahi la moderna coincide en el {parte:.1}%",
                info.width,
                info.height,
                info.scale * 100.0,
                quietos as f64 * 100.0 / total
            );
            // Un fallo de verdad se queda muy lejos de esto: con los canales al reves salia
            // un 3 %, y con la escala o la posicion corridas, menos de un 40 %. Lo que falta
            // hasta el 100 es lo que cambia en pantalla entre las tres fotos (un video).
            assert!(
                parte > 90.0,
                "pantalla {i}: donde GDI esta quieto, la moderna solo coincide en el {parte:.1}%"
            );
        }
    }
}
