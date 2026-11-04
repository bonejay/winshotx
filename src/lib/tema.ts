import type { Theme } from "./types";

/**
 * The window theme. The CSS defaults to dark and switches when `<html>` has `data-
 * tema="claro"`, so this only decides which one applies and sets the attribute.
 */

const CLAVE = "winshotx.tema";
const CONSULTA = "(prefers-color-scheme: dark)";

const sistema = window.matchMedia(CONSULTA);

/**
 * Read from local storage before the settings arrive from Rust, to avoid a dark flash on
 * startup for users who forced the light theme.
 */
let elegido = leerGuardado();

function leerGuardado(): Theme {
  try {
    const valor = localStorage.getItem(CLAVE);
    return valor === "claro" || valor === "oscuro" ? valor : "sistema";
  } catch {
    // Without storage it still works, just with the flash.
    return "sistema";
  }
}

function pintar() {
  const oscuro = elegido === "oscuro" || (elegido === "sistema" && sistema.matches);
  document.documentElement.dataset.tema = oscuro ? "oscuro" : "claro";
}

/** Change the window theme and remember it for the next start. */
export function aplicarTema(tema: Theme) {
  elegido = tema;
  try {
    localStorage.setItem(CLAVE, tema);
  } catch {
    // Failing to remember it does not prevent applying it now.
  }
  pintar();
}

// In automatic mode, follow Windows while the window is open, not only at startup.
sistema.addEventListener("change", pintar);

pintar();
