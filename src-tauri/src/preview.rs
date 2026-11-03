//! Generates an editor preview only after an explicit recording has stopped.
#[cfg(not(windows))]
use crate::error::AppError;
use crate::error::Result;
use crate::record::{self, SessionData};
use crate::state::AppState;
use tauri::{AppHandle, Emitter, Manager};
const ALTO_VISTA_PREVIA: u32 = 540;
const FPS_VISTA_PREVIA: u32 = 30;

pub(crate) fn preparar_por_detras(app: &AppHandle, id: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let Some(mut session) = state.sessions.read().get(&id).cloned() else {
            return;
        };
        // El porcentaje se manda cada cinco, no en cada fotograma: son novecientos, y
        // novecientos avisos para mover un numero de sitio es trabajo tirado.
        let mut ultimo = 0;
        let avisador = |hechos: usize, total: usize| {
            let ahora = (hechos * 100 / total.max(1)) as u32;
            if ahora >= ultimo + 5 {
                ultimo = ahora;
                let _ = app.emit(
                    crate::EVENT_SESSION_PREVIEW,
                    AvisoVistaPrevia::yendo(&id, ahora),
                );
            }
        };
        if let Err(error) = escribir_vista_previa(&mut session, avisador) {
            // Sin vista previa la sesion sigue siendo buena: se ve y se exporta igual, solo
            // que sin reproducir. Mucho mejor eso que perder lo que se acaba de rescatar.
            eprintln!("[preview] sin vista previa: {error}");
            let _ = app.emit(crate::EVENT_SESSION_PREVIEW, AvisoVistaPrevia::fallida(&id));
            return;
        }
        let _ = session.persist();
        state.sessions.write().insert(id.clone(), session.clone());
        eprintln!("[preview] vista previa lista: {}", ruta_de(&session));
        let _ = app.emit(crate::EVENT_SESSION_PREVIEW, AvisoVistaPrevia::lista(&id));

        // Y ahora la tira. Lo que viene de una grabacion normal ya las trae hechas, asi
        // que no se repiten: son cuatro milisegundos por fotograma tirados.
        if session.frames.iter().any(|f| f.thumb_path.is_empty()) {
            match record::generate_thumbnails(&mut session) {
                Ok(()) => {
                    let _ = session.persist();
                    state.sessions.write().insert(id.clone(), session);
                    let _ = app.emit(crate::recorder::EVENT_SESSION_THUMBS, id.clone());
                }
                // Sin miniaturas la tira ensenna huecos, pero el video y la exportacion
                // van igual: no es motivo para tirar nada.
                Err(error) => eprintln!("[preview] sin miniaturas para la tira: {error}"),
            }
        }
    });
}

fn ruta_de(session: &SessionData) -> String {
    session
        .mp4_path
        .as_ref()
        .map(|r| r.display().to_string())
        .unwrap_or_else(|| "sin ruta".into())
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvisoVistaPrevia {
    pub session_id: String,
    pub por_ciento: u32,
    pub listo: bool,
    pub fallida: bool,
}

impl AvisoVistaPrevia {
    fn yendo(id: &str, por_ciento: u32) -> Self {
        Self {
            session_id: id.to_string(),
            por_ciento,
            listo: false,
            fallida: false,
        }
    }

    fn lista(id: &str) -> Self {
        Self {
            session_id: id.to_string(),
            por_ciento: 100,
            listo: true,
            fallida: false,
        }
    }

    fn fallida(id: &str) -> Self {
        Self {
            session_id: id.to_string(),
            por_ciento: 0,
            listo: false,
            fallida: true,
        }
    }
}

fn a_ritmo(duraciones_origen: &[u32], fps: u32) -> (Vec<usize>, Vec<u32>) {
    let paso = (1000 / fps.max(1)).max(1);
    let mut indices: Vec<usize> = Vec::new();
    let mut duraciones: Vec<u32> = Vec::new();
    // El reloj de lo grabado y el instante en que toca guardar el siguiente. El objetivo
    // avanza de paso en paso y NO se recoloca sobre el reloj: asi el reparto no se va
    // acumulando el sobrante y de sesenta por segundo salen treinta, no veinte.
    let mut reloj: u32 = 0;
    let mut objetivo: u32 = 0;
    for (i, dura) in duraciones_origen.iter().copied().enumerate() {
        match duraciones.last_mut() {
            // Lo que cae entre dos pasos no se codifica: su tiempo se le suma al ultimo
            // que si entro, que es lo que mantiene la duracion del video clavada.
            Some(ultima) if reloj < objetivo => *ultima += dura,
            _ => {
                indices.push(i);
                duraciones.push(dura);
                objetivo += paso;
            }
        }
        reloj += dura;
    }
    (indices, duraciones)
}

#[cfg(windows)]
fn escribir_vista_previa(
    session: &mut SessionData,
    mut avisar: impl FnMut(usize, usize),
) -> Result<()> {
    let ruta = session.dir.join("preview.mp4");
    let duraciones: Vec<u32> = session.frames.iter().map(|f| f.duration_ms).collect();
    let (indices, retardos) = a_ritmo(&duraciones, FPS_VISTA_PREVIA);
    let (ancho, alto) =
        crate::encode::escalar::medida_para(session.width, session.height, ALTO_VISTA_PREVIA)
            .unwrap_or((session.width, session.height));

    let sonido = session.audio.and_then(|info| {
        std::fs::read(session.audio_path())
            .ok()
            .map(|datos| crate::encode::mp4::Pista {
                channels: info.channels,
                sample_rate: info.sample_rate,
                datos,
            })
    });

    // En orden y sin volver a empezar: los fotogramas se piden del primero al ultimo, que
    // es justo el caso para el que existe `LectorEnOrden`. El encogido va aqui y no dentro
    // del codificador porque el suyo es un Lanczos que cuesta 46 ms por fotograma, diez
    // veces mas que el filtro de caja de `reducir`.
    //
    // **Y se lee en otro hilo, dos fotogramas por delante del codificador.** Leer uno
    // (descomprimir el QOI, pegar sus zonas, encogerlo) y codificarlo son dos trabajos
    // que no se pisan: mientras Media Foundation mastica uno, aqui ya esta el siguiente.
    // Medido el 17 de septiembre de 2026 sobre el anillo de verdad: de 37 a 21 ms por
    // fotograma, y un rescate de treinta segundos a 60 fps son novecientos.
    let mut lector =
        record::LectorPorDelante::nuevo_con(session.clone(), indices.clone(), move |fotograma| {
            if fotograma.dimensions() == (ancho, alto) {
                fotograma
            } else {
                crate::encode::escalar::reducir(&fotograma, ancho, alto)
            }
        });
    let mut cargar = |indice: usize| lector.en(indice);
    let resultado = crate::encode::mp4::encode(
        &indices,
        &retardos,
        &mut cargar,
        &ruta,
        &crate::encode::mp4::Mp4Options {
            width: ancho,
            height: alto,
            fps: session.fps.clamp(1, FPS_VISTA_PREVIA),
            // La vista previa no es el archivo que se lleva nadie: se mira y se recorta.
            // Setenta y cinco es lo mismo que usa la grabacion normal para lo suyo.
            quality: 75,
            bitrate_override: None,
        },
        sonido,
        |_, hechos, total| avisar(hechos, total),
    );
    resultado?;
    session.mp4_path = Some(ruta);
    Ok(())
}

#[cfg(not(windows))]
fn escribir_vista_previa(
    _session: &mut SessionData,
    _avisar: impl FnMut(usize, usize),
) -> Result<()> {
    Err(AppError::Unsupported)
}
