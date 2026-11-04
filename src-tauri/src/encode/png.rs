use std::path::Path;

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

use crate::error::Result;

/// PNG row filter. `Up` is both faster and smaller than the default `Adaptive` on screenshots
/// (1920x1200: 64 ms / 169 KB versus 182 ms / 177 KB).
const FILTRO: PngFilter = PngFilter::Up;

fn codificar(image: &RgbaImage, compresion: CompressionType) -> Result<Vec<u8>> {
    let mut salida = Vec::new();
    PngEncoder::new_with_quality(&mut salida, compresion, FILTRO).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgba8,
    )?;
    Ok(salida)
}

/// Save the capture as PNG, scaling it first if the user changed its size.
pub fn save(image: &RgbaImage, path: &Path, width: u32, height: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Files on disk use full compression.
    let bytes = if image.width() == width && image.height() == height {
        codificar(image, CompressionType::Default)?
    } else {
        let scaled = image::imageops::resize(
            image,
            width.max(1),
            height.max(1),
            image::imageops::FilterType::Lanczos3,
        );
        codificar(&scaled, CompressionType::Default)?
    };
    std::fs::write(path, bytes)?;
    Ok(())
}

/// In-memory PNG for the clipboard. Uses fast compression: size does not matter there, and an
/// uncompressed CF_DIB is copied alongside anyway.
pub fn to_bytes(image: &RgbaImage) -> Result<Vec<u8>> {
    codificar(image, CompressionType::Fast)
}
