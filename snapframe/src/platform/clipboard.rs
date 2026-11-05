use image::RgbaImage;

use crate::error::Result;

/// BITMAPINFOHEADER plus pixels (CF_DIB), composited over white because many apps ignore alpha
/// in a 32-bit DIB.
#[cfg(windows)]
fn build_dib(image: &RgbaImage) -> Vec<u8> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let row_size = (width as usize) * 3;
    let padded_row = (row_size + 3) & !3;
    let pixel_bytes = padded_row * height as usize;

    let mut out = Vec::with_capacity(40 + pixel_bytes);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes()); // positive: bottom-up rows
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // 72 ppp
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // Row by row into preallocated memory, skipping the blend for opaque pixels (all of them in
    // a screenshot).
    let source = image.as_raw();
    let fila_origen = width as usize * 4;
    let cabecera = out.len();
    out.resize(cabecera + pixel_bytes, 0);
    let cuerpo = &mut out[cabecera..];
    for (n, destino) in cuerpo.chunks_exact_mut(padded_row).enumerate() {
        let y = height as usize - 1 - n;
        let origen = &source[y * fila_origen..(y + 1) * fila_origen];
        for (d, s) in destino[..row_size]
            .chunks_exact_mut(3)
            .zip(origen.chunks_exact(4))
        {
            if s[3] == 255 {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
            } else {
                let alpha = s[3] as u32;
                let blend = |c: u8| -> u8 {
                    ((c as u32 * alpha + 255 * (255 - alpha)) / 255).min(255) as u8
                };
                d[0] = blend(s[2]);
                d[1] = blend(s[1]);
                d[2] = blend(s[0]);
            }
        }
    }
    out
}

/// Copy an image to the clipboard as PNG (with alpha, for modern apps), CF_DIB (Paint, Office,
/// older apps) and, when `file` is given, as that PNG file (CF_HDROP), so pasting into File
/// Explorer or a chat upload creates a real .png file instead of nothing.
#[cfg(windows)]
pub fn copy_image(
    image: &RgbaImage,
    png_bytes: &[u8],
    file: Option<&std::path::Path>,
) -> Result<()> {
    use clipboard_win::{formats, raw, register_format, Clipboard};

    let _guard = Clipboard::new_attempts(10)
        .map_err(|e| crate::error::AppError::Msg(format!("Clipboard is busy: {e}")))?;
    raw::empty().map_err(|e| crate::error::AppError::Msg(e.to_string()))?;

    if let Some(format) = register_format("PNG") {
        raw::set_without_clear(format.get(), png_bytes)
            .map_err(|e| crate::error::AppError::Msg(e.to_string()))?;
    }
    let dib = build_dib(image);
    raw::set_without_clear(formats::CF_DIB, &dib)
        .map_err(|e| crate::error::AppError::Msg(e.to_string()))?;
    if let Some(file) = file {
        raw::set_file_list(&[file.to_string_lossy()])
            .map_err(|e| crate::error::AppError::Msg(e.to_string()))?;
        // Paste copies the file instead of moving it.
        if let Some(format) = register_format("Preferred DropEffect") {
            let _ = raw::set_without_clear(format.get(), &1u32.to_le_bytes());
        }
    }
    Ok(())
}

/// Copy plain text, e.g. the OCR result of a capture.
#[cfg(windows)]
pub fn copy_text(texto: &str) -> Result<()> {
    use clipboard_win::{formats, Clipboard, Setter};

    let _guard = Clipboard::new_attempts(10)
        .map_err(|e| crate::error::AppError::Msg(format!("Clipboard is busy: {e}")))?;
    clipboard_win::raw::empty().map_err(|e| crate::error::AppError::Msg(e.to_string()))?;
    formats::Unicode
        .write_clipboard(&texto)
        .map_err(|e| crate::error::AppError::Msg(e.to_string()))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn copy_image(
    _image: &RgbaImage,
    _png_bytes: &[u8],
    _file: Option<&std::path::Path>,
) -> Result<()> {
    Err(crate::error::AppError::Unsupported)
}

#[cfg(not(windows))]
pub fn copy_text(_texto: &str) -> Result<()> {
    Err(crate::error::AppError::Unsupported)
}

/// The hidden temporary file behind a copied screenshot when it is not saved, so pasting into
/// File Explorer still creates a .png. Only the newest few are kept.
pub fn clipboard_file() -> Result<std::path::PathBuf> {
    const KEEP: usize = 10;
    let dir = std::env::temp_dir()
        .join("winshotx-english")
        .join("clipboard");
    std::fs::create_dir_all(&dir)?;
    let mut old: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    old.sort();
    for (_, path) in old.iter().rev().skip(KEEP - 1) {
        let _ = std::fs::remove_file(path);
    }
    let name = format!(
        "Screenshot {}",
        chrono::Local::now().format("%Y-%m-%d %H%M%S")
    );
    Ok(crate::archivos::nombre_libre(&dir, &name, "png"))
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    #[ignore = "Writes to the real clipboard"]
    fn copied_screenshot_pastes_as_a_png_file() {
        let image = image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]));
        let bytes = crate::encode::png::to_bytes(&image).unwrap();
        let path =
            std::env::temp_dir().join(format!("snapframe-clip-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, &bytes).unwrap();
        super::copy_image(&image, &bytes, Some(&path)).unwrap();
        let mut files: Vec<String> = Vec::new();
        clipboard_win::get_clipboard(clipboard_win::formats::FileList)
            .map(|f: Vec<String>| files = f)
            .unwrap();
        assert_eq!(files, vec![path.to_string_lossy().into_owned()]);
        assert_eq!(image::open(&path).unwrap().width(), 4);
        let _ = std::fs::remove_file(&path);
    }
}
