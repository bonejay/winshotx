//! Mixing two audio streams with different sample rates and channel counts (for example 48 kHz
//! stereo system audio and a 44.1 kHz mono microphone).
//!
//! Everything here is pure functions so it can be tested without devices; device handling lives
//! in `audio.rs`.

use super::audio::Formato;

/// Convert a chunk between formats: channels first (exact), then sample rate (approximate), so
/// the approximation happens once.
pub fn adaptar(muestras: &[f32], de: Formato, a: Formato) -> Vec<f32> {
    let con_canales = cambiar_canales(muestras, de.canales, a.canales);
    remuestrear(
        &con_canales,
        a.canales.max(1) as usize,
        de.muestras_por_segundo,
        a.muestras_por_segundo,
    )
}

/// Mono to stereo duplicates the signal; stereo to mono averages it.
fn cambiar_canales(muestras: &[f32], de: u16, a: u16) -> Vec<f32> {
    let de = de.max(1) as usize;
    let a = a.max(1) as usize;
    if de == a {
        return muestras.to_vec();
    }
    let mut salida = Vec::with_capacity(muestras.len() / de * a);
    for instante in muestras.chunks_exact(de) {
        // The average of the input channels, repeated on every output channel.
        let media = instante.iter().sum::<f32>() / de as f32;
        for _ in 0..a {
            salida.push(media);
        }
    }
    salida
}

/// Change the sample rate with linear interpolation, which is good enough for voice and desktop
/// audio.
fn remuestrear(muestras: &[f32], canales: usize, de: u32, a: u32) -> Vec<f32> {
    if de == a || de == 0 || a == 0 || muestras.is_empty() {
        return muestras.to_vec();
    }
    let instantes = muestras.len() / canales;
    if instantes == 0 {
        return Vec::new();
    }
    let salida_instantes = ((instantes as u64 * a as u64) / de as u64).max(1) as usize;
    let paso = instantes as f32 / salida_instantes as f32;

    let mut salida = Vec::with_capacity(salida_instantes * canales);
    for i in 0..salida_instantes {
        let sitio = i as f32 * paso;
        let izquierda = sitio.floor() as usize;
        let derecha = (izquierda + 1).min(instantes - 1);
        let peso = sitio - izquierda as f32;
        for c in 0..canales {
            let a0 = muestras[izquierda * canales + c];
            let a1 = muestras[derecha * canales + c];
            salida.push(a0 + (a1 - a0) * peso);
        }
    }
    salida
}

/// Add the second signal onto the first, clamping to [-1, 1] so loud overlaps do not wrap
/// around into clicks when converted to integers (as in `audio::a_pcm16`). If the second signal
/// is shorter, the rest of the first is unchanged.
pub fn sumar(base: &mut [f32], encima: &[f32]) {
    for (destino, extra) in base.iter_mut().zip(encima) {
        *destino = (*destino + *extra).clamp(-1.0, 1.0);
    }
}

/// Read 32-bit float bytes as samples.
pub fn como_flotantes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|m| f32::from_le_bytes([m[0], m[1], m[2], m[3]]))
        .collect()
}

/// And back to bytes for the encoder.
pub fn como_bytes(muestras: &[f32]) -> Vec<u8> {
    let mut salida = Vec::with_capacity(muestras.len() * 4);
    for m in muestras {
        salida.extend_from_slice(&m.to_le_bytes());
    }
    salida
}
