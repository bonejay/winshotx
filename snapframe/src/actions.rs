//! Operations that do not depend on any window: delivering a screenshot, applying a preset,
//! saving settings, building the FFmpeg command preview. All callable from any thread.

use std::path::PathBuf;

use crate::capture::Rect;
use crate::encode::png;
use crate::error::Result;
use crate::settings::Settings;
use crate::state::AppState;

/// Copy a screenshot to the clipboard (as image and as a .png file, so pasting into File
/// Explorer creates a real file) and, if enabled, save it to the screenshot folder.
pub fn deliver_screenshot(state: &AppState, image: image::RgbaImage) -> Result<Option<PathBuf>> {
    let (width, height) = (image.width(), image.height());
    let (save, sound, directory) = {
        let s = state.settings.read();
        (s.save_screenshots, s.play_sound, s.save_directory.clone())
    };
    if sound {
        crate::platform::sonido::obturador();
    }
    let bytes = png::to_bytes(&image)?;
    let (file, saved) = if save {
        let path = crate::archivos::destino(std::path::Path::new(&directory), "png")?;
        png::save(&image, &path, width, height)?;
        (Some(path.clone()), Some(path))
    } else {
        // Explorer can only paste a file that exists: keep a hidden temporary copy.
        let file = crate::platform::clipboard::clipboard_file()
            .and_then(|path| Ok(std::fs::write(&path, &bytes).map(|_| path)?))
            .map_err(|e| eprintln!("could not write the clipboard file: {e}"))
            .ok();
        (file, None)
    };
    crate::platform::clipboard::copy_image(&image, &bytes, file.as_deref())?;
    Ok(saved)
}

/// Screenshot of a virtual-desktop region from the frozen screens.
pub fn capture_region(
    state: &AppState,
    freezes: &[crate::capture::Freeze],
    region: Rect,
) -> Result<Option<PathBuf>> {
    let image = crate::capture::crop_from_freeze(freezes, region)?;
    deliver_screenshot(state, image)
}

/// Apply a toolbar choice: a frame rate, and/or a quality level that switches to automatic
/// encoding. Saved, so the next recording starts with the same choice.
pub fn set_recording_choice(
    state: &AppState,
    fps: Option<u32>,
    level: Option<usize>,
) -> Result<Settings> {
    let snapshot = {
        let mut s = state.settings.write();
        if let Some(fps) = fps {
            s.fps = fps.clamp(5, 120);
        }
        if let Some(level) = level {
            s.encoding = crate::encode::auto::settings(level, &s.encoding.ffmpeg_path);
        }
        s.clone()
    };
    crate::settings::save(&snapshot)?;
    Ok(snapshot)
}

/// Validate, store and save new settings. Returns them normalised. Shortcut registration and
/// autostart are the caller's job (they need the UI thread / report errors).
pub fn store_settings(state: &AppState, mut settings: Settings) -> Result<Settings> {
    settings.encoding = crate::record::stream_encoder::streaming_options(&settings.encoding);
    crate::settings::fix_settings_directories(&mut settings);
    crate::encode::ffmpeg::validate(&settings.encoding)?;
    *state.settings.write() = settings.clone();
    crate::settings::save(&settings)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_choices_switch_to_automatic_encoding() {
        let state = AppState::new(Settings::default());
        state.settings.write().encoding.encoder = "libx264".into();
        let _ = set_recording_choice(&state, Some(30), None);
        assert_eq!(state.settings.read().fps, 30);
        assert_eq!(state.settings.read().encoding.encoder, "libx264", "fps keeps the encoder");
        let _ = set_recording_choice(&state, None, Some(2));
        let s = state.settings.read();
        assert!(crate::encode::auto::is_auto(&s.encoding));
        assert_eq!(s.encoding.preset, "high");
        assert_eq!(s.fps, 30);
    }
}
