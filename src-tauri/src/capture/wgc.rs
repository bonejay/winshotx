//! Still screenshots through Windows.Graphics.Capture. GDI (used by `xcap`) sometimes misses
//! GPU-composited content and returned partly black images over some windows; this path does
//! not. Recordings use the same API (`record::win`).
//!
//! Uses pieces of `windows-capture` but not its startup, which creates a new GPU device every
//! time (~100 ms). Here the device is created once and kept.

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

/// How long to wait for the first frame. Windows delivers one as soon as capture starts, so
/// this only expires when something is wrong; the screenshot then falls back to GDI.
const ESPERA: Duration = Duration::from_millis(500);

/// How long to wait for a fresher frame in `Momento::ElMasFresco`: 20 ms fits one frame at 60
/// Hz and at 120 Hz.
const UN_FOTOGRAMA: Duration = Duration::from_millis(20);

/// Which frame to keep. Measured: all three variants returned the current screen 20 out of 20
/// times, and `ElPrimero` is the fastest (34 ms versus 39 and 65), because setting up the
/// capture already takes long enough for the compositor to have caught up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Momento {
    /// The first frame Windows delivers.
    ElPrimero,
    /// Wait for the compositor (`DwmFlush`), then start capturing.
    TrasElCompositor,
    /// Take the first frame, or a newer one if it arrives within one display frame.
    ElMasFresco,
}

/// Whether this Windows can capture without the yellow capture border and without the cursor.
/// Windows 10 before 2022 cannot remove the border, so GDI is used there. Checked once.
pub fn disponible() -> bool {
    static SI: OnceLock<bool> = OnceLock::new();
    *SI.get_or_init(|| {
        let si = GraphicsCaptureApi::is_supported().unwrap_or(false)
            && GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false)
            && GraphicsCaptureApi::is_cursor_settings_supported().unwrap_or(false);
        if !si {
            eprintln!(
                "[capture] Windows.Graphics.Capture is unavailable for screenshots; using GDI"
            );
        }
        si
    })
}

/// The GPU device shared by all screenshots.
#[derive(Clone)]
struct Tarjeta {
    dispositivo: ID3D11Device,
    contexto: ID3D11DeviceContext,
}

/// Created with the first screenshot (or `preparar` at startup) and dropped when a screenshot
/// fails, so a reset GPU gets a new device next time.
static TARJETA: Mutex<Option<Tarjeta>> = Mutex::new(None);

/// A device context cannot be used from two threads at once, but screens are captured in
/// parallel; only the parts that touch it are serialized.
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

/// Create the device ahead of time so the first screenshot does not pay ~100 ms. Called at
/// startup on a separate thread.
pub fn preparar() {
    if disponible() {
        let _ = tarjeta();
    }
}

/// Convert padded BGRA texture rows into unpadded RGBA in one pass. Alpha is set opaque;
/// `stitch_all` relies on that to tell screens from gaps.
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

/// Screenshot of that monitor at the size `monitor` reports. A different size is an error so
/// the caller falls back to GDI instead of misaligning the selection.
pub fn fotografiar(monitor: &MonitorInfo) -> Result<RgbaImage> {
    // The first frame: fastest and just as accurate (see `Momento`).
    fotografiar_en(monitor, Momento::ElPrimero)
}

/// The same, with an explicit frame choice.
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

    // No own thread or message loop: the frame arrives on a system thread and is sent through
    // the channel.
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
                        // Only the first frame is needed.
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
            // Wait for the compositor to finish the frame in progress so capture does not start
            // mid-composition.
            let _ = unsafe { DwmFlush() };
        }
        sesion.StartCapture().map_err(de_windows)?;
        let mut fotograma = llegada
            .recv_timeout(ESPERA)
            .map_err(|_| AppError::Msg("No capture frame was received".into()))?;
        if momento == Momento::ElMasFresco {
            // Use a newer frame if one arrives within a display frame; otherwise the first one
            // is fine.
            if let Ok(siguiente) = llegada.recv_timeout(UN_FOTOGRAMA) {
                let _ = fotograma.Close();
                fotograma = siguiente;
            }
        }
        // Copy before closing the frame pool, which owns the texture.
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

/// The frame texture lives on the GPU: copy it into a CPU-readable texture, map it, and convert
/// to RGBA.
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
    // Map returned `RowPitch * height` readable bytes in `pData`, valid until Unmap.
    let crudo = unsafe {
        std::slice::from_raw_parts(abierta.pData as *const u8, paso * desc.Height as usize)
    };
    let rgba = bgra_a_rgba(crudo, desc.Width, desc.Height, paso);
    unsafe { tarjeta.contexto.Unmap(legible.texture(), 0) };

    RgbaImage::from_raw(desc.Width, desc.Height, rgba)
        .ok_or_else(|| AppError::Msg("Capture frame dimensions do not match".into()))
}
