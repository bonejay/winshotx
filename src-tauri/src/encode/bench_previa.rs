//! Lo que cuesta el MP4 de vista previa, que es lo que el editor espera para dejar darle
//! al play.
//! `cargo test --release --lib medir_la_previa -- --ignored --nocapture --test-threads=1`
//!
//! El anillo de los ultimos segundos no puede escribir su MP4 mientras gira (hasta que
//! alguien pulsa la tecla no se sabe donde empieza), asi que se escribe entero al rescatar
//! y el boton de play esta apagado hasta que termina. Este banco parte ese tiempo para
//! poder atacarlo: cuanto cuesta por fotograma segun la medida y el ritmo.
#[cfg(all(test, windows))]
mod tests {
    use std::time::Instant;

    use image::RgbaImage;

    use crate::encode::mp4::{encode, Mp4Options};

    /// Una pantalla de mentira parecida a una de verdad: casi todo quieto y una ventana
    /// con texto, que es lo que de verdad se graba.
    fn fotograma(ancho: u32, alto: u32, paso: u32) -> RgbaImage {
        let mut img = RgbaImage::from_fn(ancho, alto, |x, y| {
            let ventana = x > ancho / 6 && x < ancho * 5 / 6 && y > alto / 8 && y < alto * 7 / 8;
            let texto = ventana && (x / 3 + y / 11) % 17 == 0;
            if texto {
                image::Rgba([32, 34, 38, 255])
            } else if ventana {
                image::Rgba([250, 250, 252, 255])
            } else {
                image::Rgba([(x * 255 / ancho) as u8, (y * 255 / alto) as u8, 120, 255])
            }
        });
        let cx = (paso * 7) % (ancho - 24);
        let cy = (paso * 3) % (alto - 24);
        for y in cy..cy + 20 {
            for x in cx..cx + 12 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        img
    }

    /// Codifica `cuantos` fotogramas ya listos en memoria y devuelve lo que ha costado.
    ///
    /// Los fotogramas se preparan ANTES de arrancar el reloj: aqui se mide el codificador
    /// y nada mas, no leer del cache ni encoger.
    fn medir(ancho: u32, alto: u32, fps: u32, cuantos: usize) -> (u128, f64, u64) {
        let listos: Vec<RgbaImage> =
            (0..cuantos.min(60)).map(|i| fotograma(ancho, alto, i as u32)).collect();
        let indices: Vec<usize> = (0..cuantos).collect();
        let paso = (1000 / fps.max(1)) as u32;
        let retardos: Vec<u32> = vec![paso; cuantos];
        let dir = std::env::temp_dir().join("winshotx-bench-previa");
        std::fs::create_dir_all(&dir).unwrap();
        let ruta = dir.join(format!("previa-{ancho}x{alto}-{fps}.mp4"));

        let mut cargar = |i: usize| Ok(listos[i % listos.len()].clone());
        let reloj = Instant::now();
        encode(
            &indices,
            &retardos,
            &mut cargar,
            &ruta,
            &Mp4Options { width: ancho, height: alto, fps, quality: 75, bitrate_override: None },
            None,
            |_, _, _| {},
        )
        .expect("el codificador tiene que escribir el mp4");
        let ms = reloj.elapsed().as_millis();
        let bytes = std::fs::metadata(&ruta).map(|m| m.len()).unwrap_or(0);
        (ms, ms as f64 / cuantos as f64, bytes)
    }

    #[test]
    #[ignore = "mide, no comprueba: pide release y maquina tranquila"]
    fn medir_la_previa() {
        // Treinta segundos de anillo, que es lo que se rescata, al ritmo de la vista previa.
        let casos = [
            ("hoy: 720p a 30", 1280u32, 720u32, 30u32, 900usize),
            ("540p a 30", 960, 540, 30, 900),
            ("720p a 24", 1280, 720, 24, 720),
            ("540p a 24", 960, 540, 24, 720),
            ("480p a 30", 854, 480, 30, 900),
            // Y lo que cuesta el codificador solo a tamanno de exportar, para saber cuanto
            // del camino de exportar es el y cuanto es lo demas.
            ("exportar: 1080p a 30", 1920, 1080, 30, 300),
        ];
        println!("\n| caso | fotogramas | total | por fotograma | tamanno |");
        println!("|---|---|---|---|---|");
        for (nombre, ancho, alto, fps, cuantos) in casos {
            let (ms, por, bytes) = medir(ancho, alto, fps, cuantos);
            println!(
                "| {nombre} | {cuantos} | {:.1} s | {por:.1} ms | {:.1} MB |",
                ms as f64 / 1000.0,
                bytes as f64 / 1_048_576.0
            );
        }
    }
    /// Cuanto de ese tiempo es dar la vuelta a los colores y a las filas, que es trabajo
    /// de CPU puro que se hace en este lado antes de entregar el fotograma.
    #[test]
    #[ignore = "mide, no comprueba: pide release"]
    fn medir_la_vuelta_de_los_colores() {
        use crate::encode::mp4::rgba_to_bgra_bottom_up;

        for (ancho, alto) in [(1280u32, 720u32), (960, 540)] {
            let img = fotograma(ancho, alto, 3);
            let reloj = Instant::now();
            let vueltas = 60;
            for _ in 0..vueltas {
                std::hint::black_box(rgba_to_bgra_bottom_up(&img));
            }
            let ms = reloj.elapsed().as_secs_f64() * 1000.0 / f64::from(vueltas);
            println!("[voltear] {ancho}x{alto}: {ms:.2} ms por fotograma");
        }
    }
    /// **Que tamannos acepta el codificador de Windows, probados uno a uno.**
    ///
    /// `cargo test --release --lib medir_el_tamanno_minimo -- --ignored --nocapture`
    ///
    /// Existe por el fallo del 17 de octubre de 2026: grabar un trozo pequenno dejaba el
    /// editor diciendo «Sin vista previa», y el error de Windows era «Puntero no valido»,
    /// que no dice nada. Probando tamannos uno a uno salio la raya: por debajo de 64
    /// pixeles de lado, Media Foundation no codifica. De ahi sale `LADO_MINIMO`.
    #[test]
    #[ignore = "mide, no comprueba"]
    fn medir_el_tamanno_minimo() {
        use crate::encode::mp4::{encode, Mp4Options};
        let casos = [
            // Los cuatro que marcan la raya, medidos el 17 de octubre de 2026: con el
            // lado corto por debajo de 64 el codificador contesta «Puntero no valido».
            ("48x320, el lado corto se queda corto", 48u32, 320u32, 2usize),
            ("96x48, lo mismo por el otro lado", 96, 48, 2),
            ("96x64, el primero que pasa", 96, 64, 2),
            ("64x160, y por el otro lado igual", 64, 160, 2),
            ("1920x1080, lo de siempre", 1920, 1080, 2),
        ];
        for (nombre, ancho, alto, cuantos) in casos {
            let img = fotograma(ancho, alto, 1);
            let indices: Vec<usize> = (0..cuantos).collect();
            let retardos: Vec<u32> = vec![33; cuantos];
            let dir = std::env::temp_dir().join("winshotx-aislar-mp4");
            std::fs::create_dir_all(&dir).unwrap();
            let ruta = dir.join(format!("{ancho}x{alto}-{cuantos}.mp4"));
            let mut cargar = |_: usize| Ok(img.clone());
            let salida = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode(
                &indices,
                &retardos,
                &mut cargar,
                &ruta,
                &Mp4Options { width: ancho, height: alto, fps: 30, quality: 75, bitrate_override: None },
                None,
                |_, _, _| {},
            )));
            let salida = match salida {
                Ok(r) => r,
                Err(_) => {
                    println!("[REVIENTA] {nombre}: el codificador ha entrado en panico");
                    continue;
                }
            };
            match salida {
                Ok(()) => println!(
                    "[ok] {nombre}: {} bytes",
                    std::fs::metadata(&ruta).map(|m| m.len()).unwrap_or(0)
                ),
                Err(e) => println!("[FALLA] {nombre}: {e}"),
            }
        }
    }
}
