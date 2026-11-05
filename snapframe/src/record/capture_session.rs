//! Free-threaded WGC delivery, separate from GPU readback and encoding.
//!
//! The callback only hands off a frame handle. A three-buffer frame pool keeps Windows
//! delivering while the worker maps a reusable staging texture. No GPU wait, allocation,
//! row packing, or encoder work runs on the Windows delivery thread.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{factory, Interface, Ref};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows_capture::d3d11::{create_d3d_device, create_direct3d_device, StagingTexture};

use super::win::{frame_due, CaptureFlags, CapturedFrame};
use crate::error::{AppError, Result};

pub enum Target {
    Monitor(isize),
    Window(isize),
}

pub struct Control {
    halt: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Control {
    pub fn stop(mut self) -> Result<()> {
        self.halt.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| AppError::Msg("The Windows capture thread stopped unexpectedly".into()))?
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.halt.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start(target: Target, cursor: bool, flags: CaptureFlags) -> Result<Control> {
    let halt = Arc::new(AtomicBool::new(false));
    let done = halt.clone();
    let (ready, started) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = run(target, cursor, &flags, &done, &ready);
        if let Err(error) = &result {
            *flags.stats.capture_error.lock() = Some(error.to_string());
            flags.stop.store(true, Ordering::Release);
            let _ = ready.try_send(Err(error.to_string()));
        }
        result
    });
    let control = Control {
        halt,
        worker: Some(worker),
    };
    match started.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(control),
        Ok(Err(error)) => Err(AppError::Msg(error)),
        Err(_) => Err(AppError::Msg(
            "Windows capture did not start in time".into(),
        )),
    }
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        // WGC schedules callbacks after Close. Keep one implicit MTA for the process;
        // tearing down the final apartment between sessions can unload
        // GraphicsCapture.dll while its callbacks still execute. No capture device,
        // frame pool or image buffers are retained by this lifetime anchor.
        static MTA: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        MTA.get_or_init(|| {
            unsafe { CoIncrementMTAUsage() }
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| AppError::Msg(e.clone()))?;
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

fn run(
    target: Target,
    cursor: bool,
    flags: &CaptureFlags,
    halt: &AtomicBool,
    ready: &mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    let _apartment = Apartment::new()?;
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe {
        match target {
            Target::Monitor(handle) => interop.CreateForMonitor(HMONITOR(handle as *mut _))?,
            Target::Window(handle) => interop.CreateForWindow(HWND(handle as *mut _))?,
        }
    };
    let (device, context) = create_d3d_device().map_err(|e| AppError::Msg(e.to_string()))?;
    let direct = create_direct3d_device(&device).map_err(|e| AppError::Msg(e.to_string()))?;
    let mut size = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &direct,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        3,
        size,
    )?;
    let session = pool.CreateCaptureSession(&item)?;
    session.SetIsCursorCaptureEnabled(cursor)?;
    // Request source-driven delivery. Newer Windows versions otherwise apply their
    // default capture interval independently of our output cadence.
    #[cfg(test)]
    let previous_interval = session.MinUpdateInterval().ok();
    let interval_result =
        session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 0 });
    #[cfg(test)]
    println!("WGC default interval: {previous_interval:?}; requesting 0: {interval_result:?}");
    if let Err(error) = interval_result {
        // E_NOINTERFACE is expected on Windows versions predating this property.
        if error.code() != windows::core::HRESULT(0x80004002u32 as i32) {
            return Err(AppError::Msg(format!(
                "Could not remove Windows' capture rate limit: {error}"
            )));
        }
    }
    // Older Windows versions do not expose the border switch.
    let _ = session.SetIsBorderRequired(false);
    let (tx, rx) = mpsc::sync_channel::<(Direct3D11CaptureFrame, Instant)>(2);
    let stats = flags.stats.clone();
    let delivery_stop = flags.stop.clone();
    let pause = flags.pause.clone();
    let last_arrival = parking_lot::Mutex::new(None::<Instant>);
    let callback_gate = Arc::new(parking_lot::Mutex::new(()));
    let delivery_gate = callback_gate.clone();
    let closing = Arc::new(AtomicBool::new(false));
    let delivery_closing = closing.clone();
    let token = pool.FrameArrived(&TypedEventHandler::new(
        move |sender: Ref<Direct3D11CaptureFramePool>, _| {
            let _callback = delivery_gate.lock();
            if delivery_closing.load(Ordering::Acquire) {
                return Ok(());
            }
            let result = (|| -> windows::core::Result<()> {
                let Some(sender) = sender.as_ref() else {
                    return Ok(());
                };
                let frame = sender.TryGetNextFrame()?;
                let now = Instant::now();
                if delivery_stop.load(Ordering::Acquire) || pause.load(Ordering::Acquire) {
                    frame.Close()?;
                    return Ok(());
                }
                stats.arrived.fetch_add(1, Ordering::Relaxed);
                if let Some(previous) = last_arrival.lock().replace(now) {
                    stats.gap(now - previous);
                }
                match tx.try_send((frame, now)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full((frame, _))) => {
                        stats.capture_full.fetch_add(1, Ordering::Relaxed);
                        frame.Close()?;
                    }
                    Err(mpsc::TrySendError::Disconnected((frame, _))) => {
                        frame.Close()?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                *stats.capture_error.lock() = Some(error.to_string());
                delivery_stop.store(true, Ordering::Release);
            }
            Ok(())
        },
    ))?;
    let closed_stop = flags.stop.clone();
    let closed = item.Closed(&TypedEventHandler::new(move |_, _| {
        closed_stop.store(true, Ordering::Release);
        Ok(())
    }))?;
    // Always close the session and revoke callbacks, including errors during readback.
    let result = (|| -> Result<()> {
        let converter = if flags.format != super::stream_encoder::PixelFormat::Bgra {
            Some(super::nv12::Converter::new(
                &device,
                &context,
                flags.crop.2 - flags.crop.0,
                flags.crop.3 - flags.crop.1,
            )?)
        } else {
            None
        };
        let native = flags
            .encoding
            .as_ref()
            .map(|options| {
                super::nvenc::Encoder::new(
                    &device,
                    &context,
                    converter.as_ref().unwrap().surface(),
                    options,
                    flags.crop.2 - flags.crop.0,
                    flags.crop.3 - flags.crop.1,
                    (1.0 / flags.min_interval.as_secs_f64()).round() as u32,
                )
            })
            .transpose()?;
        session.StartCapture()?;
        let _ = ready.send(Ok(()));
        if let Some(native) = native {
            return run_native(
                &rx,
                flags,
                halt,
                converter.as_ref().unwrap(),
                native,
                &pool,
                &direct,
                &callback_gate,
                size,
            );
        }
        let mut staging: Option<StagingTexture> = None;
        let mut staging_size = (0, 0);
        let mut next = None;
        while !halt.load(Ordering::Acquire) && !flags.stop.load(Ordering::Acquire) {
            let (frame, arrived) = match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(frame) => frame,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            };
            let copied = (|| -> Result<()> {
                if flags.pause.load(Ordering::Acquire) {
                    return Ok(());
                }
                if !frame_due(&mut next, arrived, flags.min_interval) {
                    flags.stats.throttled.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                let texture: ID3D11Texture2D = unsafe {
                    frame
                        .Surface()?
                        .cast::<IDirect3DDxgiInterfaceAccess>()?
                        .GetInterface()?
                };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { texture.GetDesc(&mut desc) };
                let (x1, y1, x2, y2) = flags.crop;
                let (right, bottom) = (x2.min(desc.Width), y2.min(desc.Height));
                if right <= x1 || bottom <= y1 {
                    return Ok(());
                }
                let available = (right - x1, bottom - y1);
                let started = Instant::now();
                flags.stats.first_frame.lock().get_or_insert(arrived);
                let mut data = flags.reciclados.coger();
                let crop = D3D11_BOX {
                    left: x1,
                    top: y1,
                    right,
                    bottom,
                    front: 0,
                    back: 1,
                };
                if let Some(converter) = converter.as_ref() {
                    converter.convert(&texture, crop, &mut data)?;
                } else {
                    if staging_size != available {
                        staging = Some(
                            StagingTexture::new(&device, available.0, available.1, desc.Format)
                                .map_err(|e| AppError::Msg(e.to_string()))?,
                        );
                        staging_size = available;
                    }
                    readback(
                        &context,
                        &texture,
                        staging.as_ref().unwrap().texture(),
                        crop,
                        (x2 - x1, y2 - y1),
                        &mut data,
                    )?;
                }
                flags
                    .stats
                    .copy_max_us
                    .fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                match flags.sender.try_send(CapturedFrame { pixels: data }) {
                    Ok(()) => {
                        flags.stats.queued.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::TrySendError::Full(frame)) => {
                        flags.stats.queue_full.fetch_add(1, Ordering::Relaxed);
                        flags.reciclados.devolver(frame.pixels);
                    }
                    Err(mpsc::TrySendError::Disconnected(frame)) => {
                        flags.reciclados.devolver(frame.pixels);
                        flags.stop.store(true, Ordering::Release);
                    }
                }
                Ok(())
            })();
            let content = frame.ContentSize()?;
            frame.Close()?;
            copied?;
            if content != size && content.Width > 0 && content.Height > 0 {
                let _callback = callback_gate.lock();
                // Release queued frames before recreating the pool on window resize.
                while let Ok((frame, _)) = rx.try_recv() {
                    let _ = frame.Close();
                }
                pool.Recreate(
                    &direct,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    3,
                    content,
                )?;
                size = content;
            }
        }
        Ok(())
    })();
    closing.store(true, Ordering::Release);
    let _ = pool.RemoveFrameArrived(token);
    // Revoking an event does not wait for an in-flight callback. Never close a frame
    // pool concurrently with TryGetNextFrame/Close on the Windows delivery thread.
    let _callback = callback_gate.lock();
    let _ = item.RemoveClosed(closed);
    while let Ok((frame, _)) = rx.try_recv() {
        let _ = frame.Close();
    }
    drop(_callback);
    let _ = session.Close();
    let _ = pool.Close();
    result
}

#[allow(clippy::too_many_arguments)]
fn run_native(
    rx: &mpsc::Receiver<(Direct3D11CaptureFrame, Instant)>,
    flags: &CaptureFlags,
    halt: &AtomicBool,
    converter: &super::nv12::Converter,
    mut encoder: super::nvenc::Encoder,
    pool: &Direct3D11CaptureFramePool,
    direct: &windows::Graphics::DirectX::Direct3D11::IDirect3DDevice,
    callback_gate: &parking_lot::Mutex<()>,
    mut size: windows::Graphics::SizeInt32,
) -> Result<()> {
    // Like OBS, request fine Windows sleeps only while recording. The final short
    // part of each absolute deadline is spun, so sleep overshoot never accumulates.
    struct Timer;
    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::Media::timeEndPeriod(1);
            }
        }
    }
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }
    let _timer = Timer;
    let mut next: Option<Instant> = None;
    let mut index = 0u64;
    let mut changed = false;
    let mut transport = super::transport::Transport::new(
        flags.format == super::stream_encoder::PixelFormat::Hevc,
        (1.0 / flags.min_interval.as_secs_f64()).round() as u32,
    );
    // B-frames make NVENC return 0..n packets per picture, in decode order.
    let mut send = |packets: Vec<super::nvenc::Packet>| -> Result<()> {
        for packet in packets {
            let pixels = transport.packet(&packet);
            if flags.sender.try_send(CapturedFrame { pixels }).is_err() {
                flags.stats.queue_full.fetch_add(1, Ordering::Relaxed);
                return Err(AppError::Msg("The recording muxer stopped accepting encoded frames. Completed fragments are retained.".into()));
            }
            flags.stats.queued.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    };
    while !halt.load(Ordering::Acquire) && !flags.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        let remaining = next
            .map(|at| at.saturating_duration_since(now))
            .unwrap_or(Duration::from_millis(5));
        let wait = remaining
            .saturating_sub(Duration::from_micros(250))
            .min(Duration::from_millis(5));
        let received = if wait.is_zero() {
            match rx.try_recv() {
                Ok(frame) => Some(frame),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(wait) {
                Ok(frame) => Some(frame),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            }
        };
        if let Some((mut frame, mut arrived)) = received {
            // Keep only the newest available capture. The persistent GPU input
            // texture remains valid even when the screen stops changing.
            while let Ok((newer, time)) = rx.try_recv() {
                frame.Close()?;
                frame = newer;
                arrived = time;
            }
            let content = frame.ContentSize()?;
            let copied = (|| -> Result<()> {
                let texture: ID3D11Texture2D = unsafe {
                    frame
                        .Surface()?
                        .cast::<IDirect3DDxgiInterfaceAccess>()?
                        .GetInterface()?
                };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    texture.GetDesc(&mut desc);
                }
                let (left, top, right, bottom) = flags.crop;
                if left < desc.Width && top < desc.Height {
                    converter.capture(
                        &texture,
                        D3D11_BOX {
                            left,
                            top,
                            right: right.min(desc.Width),
                            bottom: bottom.min(desc.Height),
                            front: 0,
                            back: 1,
                        },
                    );
                    changed = true;
                    if next.is_none() {
                        flags.stats.first_frame.lock().replace(arrived);
                        next = Some(Instant::now());
                    }
                }
                Ok(())
            })();
            frame.Close()?;
            copied?;
            if content != size && content.Width > 0 && content.Height > 0 {
                let _callback = callback_gate.lock();
                while let Ok((frame, _)) = rx.try_recv() {
                    let _ = frame.Close();
                }
                pool.Recreate(
                    direct,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    3,
                    content,
                )?;
                size = content;
            }
        }
        let Some(deadline) = next else { continue };
        if Instant::now() < deadline {
            if deadline.saturating_duration_since(Instant::now()) < Duration::from_micros(250) {
                std::hint::spin_loop();
            }
            continue;
        }
        if Instant::now().saturating_duration_since(deadline) > Duration::from_secs(2) {
            return Err(AppError::Msg("NVENC fell more than two seconds behind. Choose a faster preset or lower frame rate.".into()));
        }
        let began = Instant::now();
        converter.render()?;
        let packets = encoder.encode(index)?;
        flags
            .stats
            .copy_max_us
            .fetch_max(began.elapsed().as_micros() as u64, Ordering::Relaxed);
        if !changed {
            flags.stats.source_repeat.fetch_add(1, Ordering::Relaxed);
        }
        changed = false;
        send(packets)?;
        index += 1;
        next = Some(deadline + flags.min_interval);
    }
    // Stopped: collect the pictures NVENC still holds for B-frame reordering.
    send(encoder.finish()?)
}

fn readback(
    context: &ID3D11DeviceContext,
    source: &ID3D11Texture2D,
    staging: &ID3D11Texture2D,
    crop: D3D11_BOX,
    output: (u32, u32),
    data: &mut Vec<u8>,
) -> Result<()> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context.CopySubresourceRegion(staging, 0, 0, 0, 0, source, 0, Some(&crop));
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    struct Mapping<'a>(&'a ID3D11DeviceContext, &'a ID3D11Texture2D);
    impl Drop for Mapping<'_> {
        fn drop(&mut self) {
            unsafe { self.0.Unmap(self.1, 0) }
        }
    }
    let _mapping = Mapping(context, staging);
    let height = (crop.bottom - crop.top) as usize;
    let row = (crop.right - crop.left) as usize * 4;
    let pitch = mapped.RowPitch as usize;
    // Only read the actual pixel bytes, including on the final row (padding is not needed).
    let bytes = unsafe {
        std::slice::from_raw_parts(mapped.pData.cast::<u8>(), (height - 1) * pitch + row)
    };
    pack_rows(bytes, pitch, row, height, output, data);
    Ok(())
}

fn pack_rows(
    bytes: &[u8],
    pitch: usize,
    row: usize,
    height: usize,
    output: (u32, u32),
    data: &mut Vec<u8>,
) {
    let stride = output.0 as usize * 4;
    data.clear();
    if row == stride && height == output.1 as usize {
        data.reserve(stride * height);
        if pitch == row {
            data.extend_from_slice(&bytes[..row * height]);
        } else {
            for y in 0..height {
                data.extend_from_slice(&bytes[y * pitch..y * pitch + row]);
            }
        }
    } else {
        data.resize(stride * output.1 as usize, 0);
        for y in 0..height {
            data[y * stride..y * stride + row].copy_from_slice(&bytes[y * pitch..y * pitch + row]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn padded_rows_and_resized_windows_keep_fixed_output_and_black_remainder() {
        let bytes = [1, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8];
        let mut packed = vec![255; 24];
        pack_rows(&bytes, 8, 4, 2, (1, 2), &mut packed);
        assert_eq!(packed, [1, 2, 3, 4, 5, 6, 7, 8]);
        pack_rows(&bytes, 8, 4, 2, (2, 3), &mut packed);
        assert_eq!(
            packed,
            [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }
}
