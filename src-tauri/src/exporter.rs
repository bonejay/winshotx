//! Save completed recordings directly. There is no editing or session UI.
use crate::error::{AppError, Result};
use crate::record::SessionData;
use crate::settings::Settings;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub fn save_recording(session: &SessionData, settings: &Settings) -> Result<PathBuf> {
    use crate::encode::{ffmpeg, mp4};
    if session.frames.is_empty() {
        return Err(AppError::Msg("No video frames were captured.".into()));
    }
    let options = &settings.encoding;
    if options.engine == "ffmpeg" && !ffmpeg::available_at(&options.ffmpeg_path) {
        return Err(AppError::Msg(
            "FFmpeg could not be started. Check its executable path in Recording settings.".into(),
        ));
    }
    let destination = crate::archivos::destino(Path::new(&settings.save_directory), "mp4")?;
    if options.engine == "native" {
        if let Some(source) = session.mp4_path.as_ref().filter(|p| p.is_file()) {
            std::fs::copy(source, &destination)?;
            return Ok(destination);
        }
    }
    let intermediate = session.dir.join("encoding-source.mp4");
    let native_destination = if options.engine == "ffmpeg" {
        &intermediate
    } else {
        &destination
    };
    let indices: Vec<usize> = (0..session.frames.len()).collect();
    let delays: Vec<u32> = session.frames.iter().map(|f| f.duration_ms).collect();
    let mut reader = crate::record::LectorPorDelante::nuevo(session.clone(), indices.clone());
    let audio = session.audio.and_then(|info| {
        std::fs::read(session.audio_path())
            .ok()
            .map(|datos| mp4::Pista {
                channels: info.channels,
                sample_rate: info.sample_rate,
                datos,
            })
    });
    let (width, height) = mp4::medida_que_acepta((session.width / 2) * 2, (session.height / 2) * 2);
    mp4::encode(
        &indices,
        &delays,
        &mut |index| reader.en(index),
        native_destination,
        &mp4::Mp4Options {
            width,
            height,
            fps: session.fps,
            quality: if options.engine == "ffmpeg" {
                100
            } else {
                options.quality
            },
            bitrate_override: if options.engine == "native" {
                options.bitrate()
            } else {
                None
            },
        },
        audio,
        |_, _, _| {},
    )?;
    if options.engine == "ffmpeg" {
        ffmpeg::mp4_from_video(
            &intermediate,
            &destination,
            session.fps,
            width,
            height,
            options.quality,
            options,
        )?;
    }
    Ok(destination)
}
#[cfg(not(windows))]
pub fn save_recording(_session: &SessionData, _settings: &Settings) -> Result<PathBuf> {
    Err(AppError::Unsupported)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Uses installed Windows encoder and FFmpeg"]
    fn saves_native_and_ffmpeg_recordings_without_an_editor() {
        let dir =
            std::env::temp_dir().join(format!("winshotx-direct-save-{}", uuid::Uuid::new_v4()));
        let mut cache = crate::record::FrameCache::new(&dir).unwrap();
        for i in 0..15 {
            let image = image::RgbaImage::from_pixel(320, 180, image::Rgba([i * 10, 40, 90, 255]));
            cache
                .push_rgba(image.as_raw(), 320, 180, i as u64 * 33)
                .unwrap();
        }
        let session = SessionData {
            id: "test".into(),
            dir: dir.clone(),
            region: crate::capture::Rect {
                x: 0,
                y: 0,
                width: 320,
                height: 180,
            },
            fps: 30,
            format: "video".into(),
            has_audio: true,
            width: 320,
            height: 180,
            mp4_path: None,
            audio: Some(crate::record::AudioInfo {
                channels: 2,
                sample_rate: 48000,
            }),
            frames: cache.finish(500, 30).unwrap(),
        };
        std::fs::write(session.audio_path(), vec![0u8; 48000 * 2]).unwrap();
        let mut settings = Settings::default();
        settings.save_directory = dir.join("saved").to_string_lossy().into_owned();
        let native = save_recording(&session, &settings).unwrap();
        let native_bytes = std::fs::read(&native).unwrap();
        assert_eq!(&native_bytes[4..8], b"ftyp");
        assert!(native_bytes.windows(4).any(|b| b == b"mp4a"));
        let mut streaming_session = session.clone();
        streaming_session.mp4_path = Some(native.clone());
        let copied = save_recording(&streaming_session, &settings).unwrap();
        assert_eq!(std::fs::read(copied).unwrap(), native_bytes);
        settings.encoding.engine = "ffmpeg".into();
        let ffmpeg = save_recording(&session, &settings).unwrap();
        let output = std::fs::read(ffmpeg).unwrap();
        assert_eq!(&output[4..8], b"ftyp");
        assert!(output.windows(4).any(|b| b == b"mp4a"));
        settings.encoding.extra_args = r#"["-preset", "invalid-preset"]"#.into();
        assert!(save_recording(&session, &settings).is_err());
        assert!(session.dir.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
