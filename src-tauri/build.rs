use std::path::Path;

fn main() {
    // The frontend is embedded in the binary, but Cargo only watches .rs files. Watching dist/
    // makes frontend-only changes relink the executable instead of shipping a stale UI.
    marcar(Path::new("../dist"));

    tauri_build::build()
}

/// Tell Cargo to watch this file or folder recursively.
fn marcar(ruta: &Path) {
    println!("cargo:rerun-if-changed={}", ruta.display());
    let Ok(entradas) = std::fs::read_dir(ruta) else {
        return;
    };
    for entrada in entradas.flatten() {
        let camino = entrada.path();
        if camino.is_dir() {
            marcar(&camino);
        } else {
            println!("cargo:rerun-if-changed={}", camino.display());
        }
    }
}
