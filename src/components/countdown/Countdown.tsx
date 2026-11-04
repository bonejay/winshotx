import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Camera } from "lucide-react";
import { EVENTS } from "../../lib/types";

/** The first value arrives in the URL, before the page can listen to events. */
function deLaUrl(clave: string): number {
  const numero = Number(new URLSearchParams(window.location.search).get(clave));
  return Number.isFinite(numero) && numero > 0 ? Math.floor(numero) : 0;
}

/**
 * Seconds left before a delayed capture, in a small window that never takes focus.
 * Rust does the real timing and sends 0 when it is done; this only counts down visually.
 */
export function Countdown() {
  const [segundos, setSegundos] = useState(() => deLaUrl("segundos"));

  // The window is hidden and reused between captures, so new values arrive as events.
  useEffect(() => {
    const unlisten = listen<number>(EVENTS.countdown, (e) => setSegundos(e.payload), {
      target: getCurrentWindow().label,
    });
    return () => void unlisten.then((fn) => fn());
  }, []);

  useEffect(() => {
    if (segundos <= 0) return;
    const tic = window.setTimeout(() => setSegundos((n) => n - 1), 1000);
    return () => window.clearTimeout(tic);
  }, [segundos]);

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-[#161618] text-white">
      {segundos > 0 ? (
        <span className="text-6xl font-light tabular-nums">{segundos}</span>
      ) : (
        <Camera className="size-12 text-neutral-400" />
      )}
    </div>
  );
}
