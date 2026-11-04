/**
 * Frontend test setup. Components talk to Rust through `invoke`; here a test double answers
 * instead.
 */
import "@testing-library/jest-dom/vitest";
import { afterEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { Storage } from "happy-dom";

/**
 * Use happy-dom's `localStorage` instead of Node's experimental global one, which shadows it
 * (undefined in Node 26 without `--localstorage-file`, throws in Node 24 with the flag).
 */
for (const almacen of ["localStorage", "sessionStorage"]) {
  Object.defineProperty(globalThis, almacen, {
    value: new Storage(),
    writable: true,
    configurable: true,
  });
}

/** What each Rust command returns in this test. Filled with `responde`. */
const respuestas = new Map<string, unknown>();

/** Commands called, in order. */
export const llamadas: { comando: string; args: unknown }[] = [];

/** Commands that will fail, with their error message. Filled with `falla`. */
const fallos = new Map<string, string>();

/** Listeners per Tauri event. Filled automatically by `listen`. */
const oyentes = new Map<string, ((evento: { payload: unknown }) => void)[]>();

/** Set what a Rust command returns during the test. */
export function responde(comando: string, valor: unknown) {
  respuestas.set(comando, valor);
}

/**
 * Make a Rust command fail with this message. A failed `invoke` rejects with the error text,
 * not an `Error`.
 */
export function falla(comando: string, mensaje: string) {
  fallos.set(comando, mensaje);
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (comando: string, args: unknown) => {
    llamadas.push({ comando, args });
    const fallo = fallos.get(comando);
    if (fallo !== undefined) return Promise.reject(fallo);
    return Promise.resolve(respuestas.get(comando) ?? null);
  },
  // Frozen screens are requested through this protocol, not `invoke`.
  convertFileSrc: (ruta: string) => `asset://${ruta}`,
}));

/** Emit a Tauri event to its listeners, as Rust would. */
export function emite(evento: string, payload: unknown) {
  for (const oyente of oyentes.get(evento) ?? []) oyente({ payload });
}

vi.mock("@tauri-apps/api/event", () => ({
  listen: (evento: string, oyente: (e: { payload: unknown }) => void) => {
    oyentes.set(evento, [...(oyentes.get(evento) ?? []), oyente]);
    return Promise.resolve(() => {
      oyentes.set(evento, (oyentes.get(evento) ?? []).filter((cual) => cual !== oyente));
    });
  },
  emit: () => Promise.resolve(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    close: () => Promise.resolve(),
    hide: () => Promise.resolve(),
    show: () => Promise.resolve(),
    setFocus: () => Promise.resolve(),
    label: "prueba",
  }),
  // Inside the test screen, which starts at (0, 0): the cursor is on it.
  cursorPosition: () => Promise.resolve({ x: 5, y: 5 }),
}));

afterEach(() => {
  cleanup();
  respuestas.clear();
  fallos.clear();
  oyentes.clear();
  llamadas.length = 0;
  localStorage.clear();
});
