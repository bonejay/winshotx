//! GPU RGB -> limited-range BT.709 NV12, then a small CPU download for x264.
//! This is the software-encoder branch of the OBS design: 1.5 bytes/pixel cross
//! to CPU memory instead of 4 bytes/pixel, with no CPU RGB color conversion.
use crate::error::{AppError, Result};
use std::mem::ManuallyDrop;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

pub struct Converter {
    context: ID3D11DeviceContext,
    video: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input: ID3D11Texture2D,
    clear: ID3D11RenderTargetView,
    input_view: ID3D11VideoProcessorInputView,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let description = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            OutputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&description)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };
        let mut description = D3D11_TEXTURE2D_DESC {
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
        let mut input = None;
        unsafe { device.CreateTexture2D(&description, None, Some(&mut input))? };
        let input = input.unwrap();
        let mut clear = None;
        unsafe { device.CreateRenderTargetView(&input, None, Some(&mut clear))? };
        let mut input_view = None;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        unsafe {
            video_device.CreateVideoProcessorInputView(
                &input,
                &enumerator,
                &input_desc,
                Some(&mut input_view),
            )?
        };
        description.Format = DXGI_FORMAT_NV12;
        let mut output = None;
        unsafe { device.CreateTexture2D(&description, None, Some(&mut output))? };
        let output = output.unwrap();
        let mut output_view = None;
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        unsafe {
            video_device.CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &output_desc,
                Some(&mut output_view),
            )?
        };
        description.Usage = D3D11_USAGE_STAGING;
        description.BindFlags = 0;
        description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        unsafe { device.CreateTexture2D(&description, None, Some(&mut staging))? };
        // Disable enhancement/deinterlacing heuristics: screen pixels need an exact
        // color conversion, not denoising, dynamic contrast, sharpening or scaling.
        let modern: ID3D11VideoContext1 = video.cast()?;
        unsafe {
            video.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            modern.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            modern.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
        }
        Ok(Self {
            context: context.clone(),
            video,
            processor,
            input,
            clear: clear.unwrap(),
            input_view: input_view.unwrap(),
            output,
            output_view: output_view.unwrap(),
            staging: staging.unwrap(),
            width,
            height,
        })
    }

    pub fn convert(
        &self,
        source: &ID3D11Texture2D,
        crop: D3D11_BOX,
        data: &mut Vec<u8>,
    ) -> Result<()> {
        self.capture(source, crop);
        self.render()?;
        self.download(data)
    }

    pub fn surface(&self) -> &ID3D11Texture2D {
        &self.output
    }

    pub fn capture(&self, source: &ID3D11Texture2D, crop: D3D11_BOX) {
        unsafe {
            if crop.right - crop.left != self.width || crop.bottom - crop.top != self.height {
                self.context
                    .ClearRenderTargetView(&self.clear, &[0.0, 0.0, 0.0, 1.0]);
            }
            self.context
                .CopySubresourceRegion(&self.input, 0, 0, 0, 0, source, 0, Some(&crop));
        }
    }

    pub fn render(&self) -> Result<()> {
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ..Default::default()
        };
        let result = unsafe {
            self.video.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result?;
        Ok(())
    }

    fn download(&self, data: &mut Vec<u8>) -> Result<()> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(&self.staging, &self.output);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let rows = self.height as usize * 3 / 2;
        let stride = mapped.RowPitch as usize;
        if mapped.pData.is_null() || stride < self.width as usize {
            unsafe { self.context.Unmap(&self.staging, 0) };
            return Err(AppError::Msg(
                "The GPU returned an invalid NV12 surface.".into(),
            ));
        }
        let pixels = unsafe {
            std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                (rows - 1) * stride + self.width as usize,
            )
        };
        data.clear();
        data.reserve(self.width as usize * rows);
        for row in 0..rows {
            data.extend_from_slice(&pixels[row * stride..row * stride + self.width as usize]);
        }
        unsafe { self.context.Unmap(&self.staging, 0) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Uses the actual D3D11 video processor and checks limited-range BT.709 colors"]
    fn gpu_conversion_has_correct_size_range_and_color() {
        let (device, context) = windows_capture::d3d11::create_d3d_device().unwrap();
        let converter = Converter::new(&device, &context, 64, 32).unwrap();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 64,
            Height: 32,
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
        let mut source = None;
        unsafe {
            device
                .CreateTexture2D(&desc, None, Some(&mut source))
                .unwrap()
        };
        let source = source.unwrap();
        let mut view = None;
        unsafe {
            device
                .CreateRenderTargetView(&source, None, Some(&mut view))
                .unwrap()
        };
        for (rgb, expected) in [
            ([0.0, 0.0, 0.0, 1.0], [16, 128, 128]),
            ([1.0, 1.0, 1.0, 1.0], [235, 128, 128]),
            ([1.0, 0.0, 0.0, 1.0], [63, 102, 240]),
        ] {
            unsafe { context.ClearRenderTargetView(view.as_ref().unwrap(), &rgb) };
            let mut data = Vec::new();
            converter
                .convert(
                    &source,
                    D3D11_BOX {
                        left: 0,
                        top: 0,
                        right: 64,
                        bottom: 32,
                        front: 0,
                        back: 1,
                    },
                    &mut data,
                )
                .unwrap();
            assert_eq!(data.len(), 64 * 32 * 3 / 2);
            for (value, expected) in [data[16 * 64 + 32], data[64 * 32 + 16], data[64 * 32 + 17]]
                .into_iter()
                .zip(expected)
            {
                assert!(
                    (i32::from(value) - expected).abs() <= 3,
                    "{rgb:?}: got {value}, expected {expected}"
                );
            }
        }
    }
}
