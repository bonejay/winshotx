//! D3D11 NVENC session with B-frames, structured like OBS's `plugins/obs-nvenc`
//! (`nvenc.c`, `nvenc-d3d11.c`). The capture callback never enters this code; the recording
//! worker owns conversion, submission and bitstream collection.
//!
//! With B-frames NVENC holds pictures back until a later reference frame arrives, so, as in
//! OBS: each submitted picture is copied into its own registered texture from a ring of
//! `max(4, 4 × frameIntervalP)` slots, each with its own bitstream; output is collected once
//! `ring − 1` pictures are queued (or all of them after the end-of-stream picture); the
//! presentation time comes back from NVENC (`outputTimeStamp`) and the decode time is the next
//! submitted timestamp from a FIFO minus the B-frame count.
//!
//! B-frames are used only below the pixel rate of 1440p60. Each P-frame submission makes
//! NVENC encode the held B-frames in one burst; at 1440p60 and 4K60 that burst delayed
//! Windows' delivery of the next captured frame (measured 1–10 repeated frames per 240 with
//! B-frames, 0 without), while 1080p60 stayed at 0 repeats in 30-second runs.
use super::nvenc_sys::api as nv;
use crate::error::{AppError, Result};
use crate::settings::EncodingSettings;
use std::collections::VecDeque;
use std::ffi::{c_void, CStr};
use windows::core::{s, w, Interface};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_TEXTURE2D_DESC,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

pub fn supported(options: &EncodingSettings) -> bool {
    matches!(options.encoder.as_str(), "h264_nvenc" | "hevc_nvenc")
        && options.gpu.trim().is_empty()
        && serde_json::from_str::<Vec<String>>(&options.extra_args).is_ok_and(|a| a.is_empty())
        && (options.preset.is_empty()
            || matches!(
                options.preset.as_str(),
                "p1" | "p2" | "p3" | "p4" | "p5" | "p6" | "p7"
            ))
}

/// Whether this machine can record with native NVENC: a session is opened once on the
/// default adapter, exactly as recordings do, and the answer is cached. Takes ~0.1–0.3 s the
/// first time; `main` warms it up on a background thread.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let probe = || -> Result<()> {
            let (device, context) = windows_capture::d3d11::create_d3d_device()
                .map_err(|e| AppError::Msg(e.to_string()))?;
            let convert = super::nv12::Converter::new(&device, &context, 256, 144)?;
            let options = EncodingSettings {
                encoder: "h264_nvenc".into(),
                preset: "p5".into(),
                ..Default::default()
            };
            Encoder::new(&device, &context, convert.surface(), &options, 256, 144, 30).map(drop)
        };
        probe().is_ok()
    })
}

/// B-frames between reference frames when the GPU supports them (OBS's default), for
/// recordings below [`B_FRAME_LIMIT`] pixels per second.
const B_FRAMES: u32 = 2;
const B_FRAME_LIMIT: u64 = 2560 * 1440 * 60;

/// One encoded access unit. Timestamps count frames: `pts` is when it is shown, `dts` when it
/// must be decoded (negative for the first `B_FRAMES` packets).
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: u64,
    pub dts: i64,
    pub keyframe: bool,
}

/// One ring entry: an NV12 copy of a submitted picture and the buffer its packet lands in.
struct Slot {
    texture: ID3D11Texture2D,
    registered: nv::NV_ENC_REGISTERED_PTR,
    mapped: nv::NV_ENC_INPUT_PTR,
    bitstream: nv::NV_ENC_OUTPUT_PTR,
}

pub struct Encoder {
    library: HMODULE,
    api: nv::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut c_void,
    slots: Vec<Slot>,
    /// Next slot to submit into, and oldest slot whose packet is still pending.
    next: usize,
    current: usize,
    queued: usize,
    /// Pictures queued before the first packet is collected (OBS: `buf_count - 1`).
    output_delay: usize,
    b_frames: u32,
    /// Submitted timestamps in submission order; the decode time of each packet in turn.
    dts: VecDeque<u64>,
    started: bool,
    // Driver registrations do not own COM references.
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    source: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Encoder {
    /// `source` is the converter's NV12 surface; each frame is copied out of it, so it can be
    /// overwritten while NVENC still holds earlier frames.
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        source: &ID3D11Texture2D,
        options: &EncodingSettings,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self> {
        let library =
            unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)? };
        let mut value = Self {
            library,
            api: nv::NV_ENCODE_API_FUNCTION_LIST::default(),
            session: std::ptr::null_mut(),
            slots: Vec::new(),
            next: 0,
            current: 0,
            queued: 0,
            output_delay: 0,
            b_frames: 0,
            dts: VecDeque::new(),
            started: false,
            _device: device.clone(),
            context: context.clone(),
            source: source.clone(),
            width,
            height,
        };
        let create = unsafe { GetProcAddress(library, s!("NvEncodeAPICreateInstance")) }
            .ok_or_else(|| AppError::Msg("The NVIDIA driver has no NVENC entry point.".into()))?;
        let create: unsafe extern "C" fn(*mut nv::NV_ENCODE_API_FUNCTION_LIST) -> nv::NVENCSTATUS =
            unsafe { std::mem::transmute(create) };
        value.api.version = nv::NV_ENCODE_API_FUNCTION_LIST_VER;
        let status = unsafe { create(&mut value.api) };
        value.check(status, "load the NVENC API")?;
        let mut open = nv::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: nv::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: nv::NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX,
            device: device.as_raw(),
            apiVersion: nv::NVENCAPI_VERSION,
            ..Default::default()
        };
        let status =
            unsafe { value.api.nvEncOpenEncodeSessionEx.unwrap()(&mut open, &mut value.session) };
        value.check(status, "open the D3D11 encoder")?;
        let codec = if options.encoder == "hevc_nvenc" {
            nv::NV_ENC_CODEC_HEVC_GUID
        } else {
            nv::NV_ENC_CODEC_H264_GUID
        };
        let preset = match options.preset.as_str() {
            "p1" => nv::NV_ENC_PRESET_P1_GUID,
            "p2" => nv::NV_ENC_PRESET_P2_GUID,
            "p3" => nv::NV_ENC_PRESET_P3_GUID,
            "p4" => nv::NV_ENC_PRESET_P4_GUID,
            "p6" => nv::NV_ENC_PRESET_P6_GUID,
            "p7" => nv::NV_ENC_PRESET_P7_GUID,
            _ => nv::NV_ENC_PRESET_P5_GUID,
        };
        let tuning = nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY;
        let mut preset_config = nv::NV_ENC_PRESET_CONFIG {
            version: nv::NV_ENC_PRESET_CONFIG_VER,
            presetCfg: nv::NV_ENC_CONFIG {
                version: nv::NV_ENC_CONFIG_VER,
                ..Default::default()
            },
            ..Default::default()
        };
        value.check(
            unsafe {
                value.api.nvEncGetEncodePresetConfigEx.unwrap()(
                    value.session,
                    codec,
                    preset,
                    tuning,
                    &mut preset_config,
                )
            },
            "read the encoder preset",
        )?;
        let mut config = preset_config.presetCfg;
        config.gopLength = fps * 2;
        // B-frames as in OBS: two when the GPU supports them (below the size limit above), no
        // lookahead (OBS's default when the preset does not enable it).
        let mut max_b = 0i32;
        let mut caps = nv::NV_ENC_CAPS_PARAM {
            version: nv::NV_ENC_CAPS_PARAM_VER,
            capsToQuery: nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_MAX_BFRAMES,
            ..Default::default()
        };
        let status = unsafe {
            value.api.nvEncGetEncodeCaps.unwrap()(value.session, codec, &mut caps, &mut max_b)
        };
        let rate = u64::from(width) * u64::from(height) * u64::from(fps);
        value.b_frames = if status == nv::NVENCSTATUS::NV_ENC_SUCCESS && rate < B_FRAME_LIMIT {
            B_FRAMES.min(max_b.max(0) as u32)
        } else {
            0
        };
        config.frameIntervalP = value.b_frames as i32 + 1;
        config.rcParams.set_enableLookahead(0);
        config.rcParams.lookaheadDepth = 0;
        config.rcParams.set_zeroReorderDelay(0);
        let ring = (4usize).max(config.frameIntervalP as usize * 4).min(64);
        value.output_delay = ring - 1;
        if options.rate_control == "bitrate" {
            config.rcParams.rateControlMode = nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
            config.rcParams.averageBitRate = options.bitrate().unwrap();
            config.rcParams.maxBitRate = options.bitrate().unwrap();
        } else {
            config.rcParams.rateControlMode = nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_VBR;
            config.rcParams.averageBitRate = 0;
            // Capped constant quality, with a two-second buffer as in the FFmpeg route.
            let peak = (options.max_bitrate_mbps.max(0.0) * 1_000_000.0) as u32;
            config.rcParams.maxBitRate = peak;
            config.rcParams.vbvBufferSize = peak.saturating_mul(2);
            config.rcParams.targetQuality = options.crf;
            config.rcParams.targetQualityLSB = 0;
        }
        unsafe {
            let vui = if options.encoder == "hevc_nvenc" {
                config.encodeCodecConfig.hevcConfig.set_repeatSPSPPS(1);
                config.encodeCodecConfig.hevcConfig.idrPeriod = fps * 2;
                config.encodeCodecConfig.hevcConfig.useBFramesAsRef =
                    nv::NV_ENC_BFRAME_REF_MODE::NV_ENC_BFRAME_REF_MODE_DISABLED;
                &mut config.encodeCodecConfig.hevcConfig.hevcVUIParameters
            } else {
                config.encodeCodecConfig.h264Config.set_repeatSPSPPS(1);
                config.encodeCodecConfig.h264Config.idrPeriod = fps * 2;
                config.encodeCodecConfig.h264Config.useBFramesAsRef =
                    nv::NV_ENC_BFRAME_REF_MODE::NV_ENC_BFRAME_REF_MODE_DISABLED;
                &mut config.encodeCodecConfig.h264Config.h264VUIParameters
            };
            vui.videoSignalTypePresentFlag = 1;
            vui.videoFormat = nv::NV_ENC_VUI_VIDEO_FORMAT::NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
            vui.videoFullRangeFlag = 0;
            vui.colourDescriptionPresentFlag = 1;
            vui.colourPrimaries = nv::NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
            vui.transferCharacteristics =
                nv::NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
            vui.colourMatrix = nv::NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
        }
        let mut init = nv::NV_ENC_INITIALIZE_PARAMS {
            version: nv::NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: codec,
            presetGUID: preset,
            encodeWidth: width,
            encodeHeight: height,
            darWidth: width,
            darHeight: height,
            frameRateNum: fps,
            frameRateDen: 1,
            enablePTD: 1,
            encodeConfig: &mut config,
            maxEncodeWidth: width,
            maxEncodeHeight: height,
            tuningInfo: tuning,
            bufferFormat: nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
            ..Default::default()
        };
        value.check(
            unsafe { value.api.nvEncInitializeEncoder.unwrap()(value.session, &mut init) },
            "initialize NVENC",
        )?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        desc.MiscFlags = 0;
        for _ in 0..ring {
            let mut texture = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
            let texture = texture.ok_or_else(|| AppError::Msg("No NV12 ring texture".into()))?;
            let mut register = nv::NV_ENC_REGISTER_RESOURCE {
                version: nv::NV_ENC_REGISTER_RESOURCE_VER,
                resourceType: nv::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
                width,
                height,
                resourceToRegister: texture.as_raw(),
                bufferFormat: nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12,
                bufferUsage: nv::NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE,
                ..Default::default()
            };
            value.check(
                unsafe { value.api.nvEncRegisterResource.unwrap()(value.session, &mut register) },
                "register an NV12 texture",
            )?;
            let mut output = nv::NV_ENC_CREATE_BITSTREAM_BUFFER {
                version: nv::NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
                ..Default::default()
            };
            let created = value.check(
                unsafe {
                    value.api.nvEncCreateBitstreamBuffer.unwrap()(value.session, &mut output)
                },
                "create an encoded packet buffer",
            );
            value.slots.push(Slot {
                texture,
                registered: register.registeredResource,
                mapped: std::ptr::null_mut(),
                bitstream: output.bitstreamBuffer,
            });
            created?;
        }
        Ok(value)
    }

    #[cfg(test)]
    pub fn b_frames(&self) -> u32 {
        self.b_frames
    }

    /// Submit the converter's current picture as frame `index`; returns the packets that
    /// became ready (usually zero or one).
    pub fn encode(&mut self, index: u64) -> Result<Vec<Packet>> {
        let slot = self.next;
        unsafe {
            self.context
                .CopyResource(&self.slots[slot].texture, &self.source);
        }
        let mut map = nv::NV_ENC_MAP_INPUT_RESOURCE {
            version: nv::NV_ENC_MAP_INPUT_RESOURCE_VER,
            registeredResource: self.slots[slot].registered,
            ..Default::default()
        };
        self.check(
            unsafe { self.api.nvEncMapInputResource.unwrap()(self.session, &mut map) },
            "map the NV12 texture",
        )?;
        self.slots[slot].mapped = map.mappedResource;
        self.dts.push_back(index);
        let mut picture = nv::NV_ENC_PIC_PARAMS {
            version: nv::NV_ENC_PIC_PARAMS_VER,
            inputWidth: self.width,
            inputHeight: self.height,
            inputPitch: self.width,
            inputTimeStamp: index,
            inputDuration: 1,
            frameIdx: index as u32,
            inputBuffer: map.mappedResource,
            outputBitstream: self.slots[slot].bitstream,
            bufferFmt: map.mappedBufferFmt,
            pictureStruct: nv::NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
            ..Default::default()
        };
        let status = unsafe { self.api.nvEncEncodePicture.unwrap()(self.session, &mut picture) };
        if status != nv::NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT {
            self.check(status, "encode a frame")?;
        }
        self.started = true;
        self.queued += 1;
        self.next = (self.next + 1) % self.slots.len();
        // As in OBS: with `ring - 1` pictures queued the oldest packet is certainly encoded,
        // so the blocking lock in `collect` returns at once.
        if self.queued < self.output_delay {
            return Ok(Vec::new());
        }
        self.collect(1)
    }

    /// End the stream and return every packet NVENC still holds.
    pub fn finish(&mut self) -> Result<Vec<Packet>> {
        if !self.started {
            return Ok(Vec::new());
        }
        self.started = false;
        let mut end = nv::NV_ENC_PIC_PARAMS {
            version: nv::NV_ENC_PIC_PARAMS_VER,
            encodePicFlags: nv::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_EOS as u32,
            ..Default::default()
        };
        let status = unsafe { self.api.nvEncEncodePicture.unwrap()(self.session, &mut end) };
        if status != nv::NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT {
            self.check(status, "end the stream")?;
        }
        self.collect(self.queued)
    }

    /// Lock, copy and release the oldest `count` pending packets, in decode order.
    fn collect(&mut self, count: usize) -> Result<Vec<Packet>> {
        let mut packets = Vec::with_capacity(count);
        for _ in 0..count {
            let slot = self.current;
            let mut output = nv::NV_ENC_LOCK_BITSTREAM {
                version: nv::NV_ENC_LOCK_BITSTREAM_VER,
                outputBitstream: self.slots[slot].bitstream,
                ..Default::default()
            };
            self.check(
                unsafe { self.api.nvEncLockBitstream.unwrap()(self.session, &mut output) },
                "collect the encoded packet",
            )?;
            let data = unsafe {
                std::slice::from_raw_parts(
                    output.bitstreamBufferPtr.cast::<u8>(),
                    output.bitstreamSizeInBytes as usize,
                )
            }
            .to_vec();
            let pts = output.outputTimeStamp;
            let keyframe = output.pictureType == nv::NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR;
            self.check(
                unsafe {
                    self.api.nvEncUnlockBitstream.unwrap()(self.session, self.slots[slot].bitstream)
                },
                "release the packet",
            )?;
            let mapped = std::mem::replace(&mut self.slots[slot].mapped, std::ptr::null_mut());
            if !mapped.is_null() {
                self.check(
                    unsafe { self.api.nvEncUnmapInputResource.unwrap()(self.session, mapped) },
                    "unmap the texture",
                )?;
            }
            self.current = (self.current + 1) % self.slots.len();
            self.queued -= 1;
            let dts = self.dts.pop_front().unwrap_or(pts) as i64 - i64::from(self.b_frames);
            packets.push(Packet {
                data,
                pts,
                dts,
                keyframe,
            });
        }
        Ok(packets)
    }

    fn check(&self, status: nv::NVENCSTATUS, action: &str) -> Result<()> {
        if status == nv::NVENCSTATUS::NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = if !self.session.is_null() {
            self.api
                .nvEncGetLastErrorString
                .and_then(|f| {
                    let p = unsafe { f(self.session) };
                    (!p.is_null())
                        .then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        Err(AppError::Msg(format!(
            "Could not {action}: {status:?}. {detail}"
        )))
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                // Like OBS: end the stream so the driver releases held pictures first.
                if self.started {
                    let _ = self.finish();
                }
                for slot in &self.slots {
                    if !slot.mapped.is_null() {
                        let _ =
                            self.api.nvEncUnmapInputResource.unwrap()(self.session, slot.mapped);
                    }
                    if !slot.bitstream.is_null() {
                        let _ = self.api.nvEncDestroyBitstreamBuffer.unwrap()(
                            self.session,
                            slot.bitstream,
                        );
                    }
                    if !slot.registered.is_null() {
                        let _ = self.api.nvEncUnregisterResource.unwrap()(
                            self.session,
                            slot.registered,
                        );
                    }
                }
                let _ = self.api.nvEncDestroyEncoder.unwrap()(self.session);
            }
            let _ = FreeLibrary(self.library);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{nv12::Converter, stream_encoder, transport::Transport};
    use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};

    #[test]
    fn advanced_options_keep_the_ffmpeg_compatibility_route() {
        let mut options = EncodingSettings {
            encoder: "h264_nvenc".into(),
            preset: "p5".into(),
            ..Default::default()
        };
        assert!(supported(&options));
        options.extra_args = r#"["-bf","2"]"#.into();
        assert!(!supported(&options));
        options.extra_args = "[]".into();
        options.gpu = "1".into();
        assert!(!supported(&options));
        options.gpu.clear();
        options.encoder = "av1_nvenc".into();
        assert!(!supported(&options));
    }

    #[test]
    #[ignore = "Runs real D3D11/NVENC and decodes synchronized video/tone markers through the compressed muxer"]
    fn live_native_gpu_video_and_audio_timestamps() {
        for codec in ["h264_nvenc", "hevc_nvenc"] {
            let dir =
                std::env::temp_dir().join(format!("snapframe-native-av-{}", uuid::Uuid::new_v4()));
            let paths = stream_encoder::OutputPaths::reserve(&dir).unwrap();
            let options = EncodingSettings {
                encoder: codec.into(),
                preset: "p5".into(),
                rate_control: "bitrate".into(),
                bitrate_mbps: 6.0,
                ..Default::default()
            };
            let format = if codec == "hevc_nvenc" {
                stream_encoder::PixelFormat::Hevc
            } else {
                stream_encoder::PixelFormat::H264
            };
            let mut mux = stream_encoder::Encoder::start_for(
                paths,
                &options,
                320,
                180,
                60,
                Some(stream_encoder::AudioFormat {
                    channels: 2,
                    sample_rate: 48_000,
                }),
                format,
            )
            .unwrap();
            let (device, context) = windows_capture::d3d11::create_d3d_device().unwrap();
            let convert = Converter::new(&device, &context, 320, 180).unwrap();
            let mut source = None;
            unsafe {
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: 320,
                            Height: 180,
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
                        },
                        None,
                        Some(&mut source),
                    )
                    .unwrap();
            }
            let source = source.unwrap();
            let mut view = None;
            unsafe {
                device
                    .CreateRenderTargetView(&source, None, Some(&mut view))
                    .unwrap();
            }
            let mut native =
                Encoder::new(&device, &context, convert.surface(), &options, 320, 180, 60).unwrap();
            assert_eq!(native.b_frames(), 2, "this GPU supports B-frames");
            let mut transport = Transport::new(codec == "hevc_nvenc", 60);
            let mut pcm = Vec::with_capacity(96_000 * 4);
            for sample in 0..96_000 {
                let value: i16 = if sample < 38_400 {
                    0
                } else {
                    ((sample as f64 * std::f64::consts::TAU * 440.0 / 48_000.0).sin() * 8_000.0)
                        as i16
                };
                pcm.extend_from_slice(&value.to_le_bytes());
                pcm.extend_from_slice(&value.to_le_bytes());
            }
            mux.audio(pcm).unwrap();
            mux.end_audio();
            for index in 0..120 {
                let value = if index < 48 { 0.0 } else { 1.0 };
                unsafe {
                    context
                        .ClearRenderTargetView(view.as_ref().unwrap(), &[value, value, value, 1.0]);
                }
                convert.capture(
                    &source,
                    D3D11_BOX {
                        left: 0,
                        top: 0,
                        right: 320,
                        bottom: 180,
                        front: 0,
                        back: 1,
                    },
                );
                convert.render().unwrap();
                for packet in native.encode(index).unwrap() {
                    mux.video(&transport.packet(&packet)).unwrap();
                }
            }
            for packet in native.finish().unwrap() {
                mux.video(&transport.packet(&packet)).unwrap();
            }
            let path = mux.finish().unwrap();
            let video = crate::encode::ffmpeg::command(&options.ffmpeg_path)
                .args(["-v", "error", "-i"])
                .arg(&path)
                .args([
                    "-map",
                    "0:v:0",
                    "-fps_mode",
                    "passthrough",
                    "-pix_fmt",
                    "gray",
                    "-f",
                    "rawvideo",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                video.status.success(),
                "{}",
                String::from_utf8_lossy(&video.stderr)
            );
            assert_eq!(video.stdout.len() / (320 * 180), 120);
            let marker = video
                .stdout
                .chunks_exact(320 * 180)
                .position(|frame| frame[0] > 128)
                .unwrap();
            assert_eq!(marker, 48);
            // Tone and picture both start at 0.8 s.
            let offset = stream_encoder::audio_minus_picture_ms(&options.ffmpeg_path, &path);
            println!(
                "{codec} 6 Mbps: 120 decoded frames, audio minus picture as played {offset:+.1} ms; {}",
                path.display()
            );
            assert!(offset.abs() < 25.0, "{codec}: A/V offset {offset:+.1} ms");
        }
    }
}
