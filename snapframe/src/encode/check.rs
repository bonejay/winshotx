//! Resolution-aware synthetic throughput check. Uses the production conversion,
//! encoder, transport and muxer, without reading screens or opening audio devices.
use crate::error::{AppError, Result};
use crate::settings::EncodingSettings;
use std::time::{Duration, Instant};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderCheck {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub measured_fps: f64,
    /// What was tested, e.g. "GPU · NVENC H.264 P5".
    pub plan: String,
}

/// Test choices are explicit; a profile never changes recording dimensions.
pub fn dimensions(choice: i32) -> (u32, u32) {
    match choice {
        1 => (2560, 1440),
        2 => (3840, 2160),
        _ => (1920, 1080),
    }
}

pub fn run(options: &EncodingSettings, width: u32, height: u32, fps: u32) -> Result<EncoderCheck> {
    let options = super::auto::resolve_here(
        &crate::record::stream_encoder::streaming_options(options),
        width,
        height,
        fps,
    );
    super::preflight::recording(&options, width, height, fps)?;
    #[cfg(windows)]
    return run_windows(&options, width, height, fps)
        .map_err(|e| AppError::Msg(format!("Encoder test failed: {e}")));
    #[cfg(not(windows))]
    Err(AppError::Unsupported)
}

#[cfg(windows)]
fn run_windows(
    options: &EncodingSettings,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<EncoderCheck> {
    use crate::record::{
        nv12::Converter,
        nvenc,
        stream_encoder::{AudioFormat, Encoder, OutputPaths, PixelFormat},
        transport::Transport,
    };
    use windows::core::Interface;
    use windows::Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    };
    let directory =
        std::env::temp_dir().join(format!("snapframe-encoder-check-{}", uuid::Uuid::new_v4()));
    // Every owned resource is dropped before removing temporary output on success/error.
    let result = (|| {
        let native_gpu = nvenc::supported(options);
        let format = if native_gpu {
            if options.encoder == "hevc_nvenc" {
                PixelFormat::Hevc
            } else {
                PixelFormat::H264
            }
        } else {
            PixelFormat::Nv12
        };
        let mut mux = Encoder::start_for(
            OutputPaths::reserve(&directory)?,
            options,
            width,
            height,
            fps,
            Some(AudioFormat {
                channels: 2,
                sample_rate: 48_000,
            }),
            format,
        )?;
        let (device, context) = windows_capture::d3d11::create_d3d_device()
            .map_err(|e| AppError::Msg(e.to_string()))?;
        let convert = Converter::new(&device, &context, width, height)?;
        let mut native = if native_gpu {
            Some(nvenc::Encoder::new(
                &device,
                &context,
                convert.surface(),
                options,
                width,
                height,
                fps,
            )?)
        } else {
            None
        };
        let mut transport = Transport::new(format == PixelFormat::Hevc, fps);
        // A detailed desktop-like pattern with moving panels/stripes. The input is
        // generated, so a successful check does not certify capture under GPU load.
        let mut pattern = vec![0u8; width as usize * height as usize * 4];
        for (i, pixel) in pattern.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            let line = if x % 13 == 0 || y % 19 == 0 { 35 } else { 0 };
            pixel.copy_from_slice(&[
                (40 + (x / 8 % 96) + line) as u8,
                (50 + (y / 8 % 96) + line) as u8,
                (55 + ((x / 96 + y / 64) % 4) * 30) as u8,
                255,
            ]);
        }
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let pixels = D3D11_SUBRESOURCE_DATA {
            pSysMem: pattern.as_ptr().cast(),
            SysMemPitch: width * 4,
            ..Default::default()
        };
        let mut source = None;
        unsafe {
            device.CreateTexture2D(&description, Some(&pixels), Some(&mut source))?;
        }
        drop(pattern);
        let source = source.unwrap();
        let mut target = None;
        unsafe {
            device.CreateRenderTargetView(&source, None, Some(&mut target))?;
        }
        let target = target.unwrap();
        let drawing: ID3D11DeviceContext1 = context.cast()?;
        let crop = D3D11_BOX {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
            front: 0,
            back: 1,
        };
        let frames = fps * 3;
        let mut pcm = Vec::with_capacity(3 * 48_000 * 4);
        for sample in 0..3 * 48_000 {
            let value =
                ((sample as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 5000.0) as i16;
            pcm.extend_from_slice(&value.to_le_bytes());
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        let began = Instant::now();
        let deadline = began + Duration::from_secs(20);
        mux.audio(pcm)?;
        mux.end_audio();
        let mut downloaded = Vec::new();
        for index in 0..frames {
            if Instant::now() >= deadline {
                return Err(AppError::Msg(
                    "The encoder test exceeded 20 seconds. Choose a faster preset or smaller size."
                        .into(),
                ));
            }
            for panel in 0..16 {
                let x = ((index * 17 + panel * width / 16) % width) as i32;
                let y = ((index * 7 + panel * height / 16) % height) as i32;
                let color = (index + panel * 5) as f32 / frames as f32;
                unsafe {
                    drawing.ClearView(
                        &target,
                        &[0.1 + color.fract() * 0.7, 0.5, 0.25, 1.0],
                        Some(&[RECT {
                            left: x,
                            top: y,
                            right: (x + (width / 8) as i32).min(width as i32),
                            bottom: (y + (height / 8) as i32).min(height as i32),
                        }]),
                    );
                }
            }
            if let Some(native) = native.as_mut() {
                convert.capture(&source, crop);
                convert.render()?;
                for packet in native.encode(u64::from(index))? {
                    mux.video(&transport.packet(&packet))?;
                }
            } else {
                convert.convert(&source, crop, &mut downloaded)?;
                mux.video(&downloaded)?;
            }
        }
        if let Some(native) = native.as_mut() {
            for packet in native.finish()? {
                mux.video(&transport.packet(&packet))?;
            }
        }
        let path = mux.finish_by(deadline)?;
        let elapsed = began.elapsed();
        let measured_fps = f64::from(frames) / elapsed.as_secs_f64();
        if measured_fps < f64::from(fps) * 1.1 {
            return Err(AppError::Msg(format!("{width}×{height} at {fps} fps needs more encoding headroom: the synthetic test reached {measured_fps:.1} fps (requires {:.0} fps including 10% margin). Choose a faster preset, hardware encoder, smaller selection or lower frame rate.", f64::from(fps) * 1.1)));
        }
        Ok(EncoderCheck {
            width,
            height,
            fps,
            bytes: std::fs::metadata(path)?.len(),
            elapsed_ms: elapsed.as_millis() as u64,
            measured_fps,
            plan: super::auto::describe(options),
        })
    })();
    let _ = std::fs::remove_dir_all(directory);
    result
}
