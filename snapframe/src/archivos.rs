use std::path::{Path, PathBuf};

/// Current time to the second, formatted for file names.
pub fn marca_de_tiempo() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// A file name that does not exist yet in the folder: `name.png`, then `name-2.png`,
/// `name-3.png`, and so on. The suffix goes before the extension. Gives up after 1000 attempts
/// so a broken folder cannot hang a save.
pub fn nombre_libre(dir: &Path, base: &str, extension: &str) -> PathBuf {
    let candidato = dir.join(format!("{base}.{extension}"));
    if !candidato.exists() {
        return candidato;
    }
    for n in 2..1000 {
        let otro = dir.join(format!("{base}-{n}.{extension}"));
        if !otro.exists() {
            return otro;
        }
    }
    // Never return a name that might overwrite an existing file.
    dir.join(format!(
        "{base}-{}.{extension}",
        chrono::Local::now().format("%S%3f")
    ))
}

/// Path for the next saved capture, with its folder created.
pub fn destino(dir: &Path, extension: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    Ok(nombre_libre(
        dir,
        &format!("winshotx-{}", marca_de_tiempo()),
        extension,
    ))
}
