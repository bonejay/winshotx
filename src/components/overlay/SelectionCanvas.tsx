import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence } from "framer-motion";
import { emit, listen } from "@tauri-apps/api/event";
import { cursorPosition, getCurrentWindow } from "@tauri-apps/api/window";
import {
  applyPreset,
  cancelCapture,
  captureAllScreens,
  captureStill,
  copyColor,
  freezeBytes,
  freezePng,
  getSettings,
  openSettings,
  overlayBootstrap,
  overlayListo,
  startRecording,
} from "../../lib/ipc";
import { clamp } from "../../lib/format";
import { useT } from "../../lib/i18n";
import {
  aPantalla,
  aVirtual,
  esDeEstaPantalla,
  ventanaBajoElPunto,
  ventanasDeEstaPantalla,
  type VentanaDelSistema,
} from "../../lib/pantallas";
import {
  EVENTS,
  type CaptureMode,
  type OverlayModeState,
  type OverlayPayload,
  type Rect,
  type StillAction,
} from "../../lib/types";
import { BootScreen } from "./BootScreen";
import { DimensionBadge } from "./DimensionBadge";
import { Magnifier } from "./Magnifier";
import { presetSummary } from "../../lib/presets";
import { ModeBar, SELECTOR_BARRA } from "./ModeBar";
import { ScreenPicker } from "./ScreenPicker";
import { SelectionHandles, type HandleId } from "./SelectionHandles";

type Mode =
  | { kind: "idle" }
  | { kind: "drawing"; originX: number; originY: number; candidate: Rect | null }
  | { kind: "moving"; grabX: number; grabY: number; base: Rect }
  | { kind: "resizing"; handle: HandleId; base: Rect };

const MIN_DRAG = 4; // below this, a drag counts as a click

/** Space taken by the magnifier and its caption, plus a gap from the cursor. */
const LUPA_ANCHO = 90;
const LUPA_ALTO = 112;
const LUPA_AIRE = 18;


/**
 * Where the magnifier goes so it never covers the pixel being inspected: right of the cursor,
 * else left; below, else above.
 */
function sitioDeLaLupa(x: number, y: number, ancho: number, alto: number) {
  const cabeDerecha = x + LUPA_AIRE + LUPA_ANCHO <= ancho;
  const cabeAbajo = y + LUPA_AIRE + LUPA_ALTO <= alto;
  return {
    left: clamp(
      cabeDerecha ? x + LUPA_AIRE : x - LUPA_AIRE - LUPA_ANCHO,
      0,
      Math.max(0, ancho - LUPA_ANCHO),
    ),
    top: clamp(
      cabeAbajo ? y + LUPA_AIRE : y - LUPA_AIRE - LUPA_ALTO,
      0,
      Math.max(0, alto - LUPA_ALTO),
    ),
  };
}

function normalize(ax: number, ay: number, bx: number, by: number): Rect {
  return {
    x: Math.min(ax, bx),
    y: Math.min(ay, by),
    width: Math.abs(bx - ax),
    height: Math.abs(by - ay),
  };
}

function applyHandle(base: Rect, handle: HandleId, x: number, y: number): Rect {
  let left = base.x;
  let top = base.y;
  let right = base.x + base.width;
  let bottom = base.y + base.height;
  if (handle.includes("w")) left = x;
  if (handle.includes("e")) right = x;
  if (handle.startsWith("n")) top = y;
  if (handle.startsWith("s")) bottom = y;
  return normalize(left, top, right, bottom);
}

/** Whether a pointer gesture started inside the mode bar. */
function enLaBarra(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(SELECTOR_BARRA) !== null;
}

function contains(rect: Rect, x: number, y: number): boolean {
  return x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height;
}

/**
 * The overlay background covers the whole screen, so there are two ways to load it, both over
 * IPC from Rust's memory: PNG normally, and an uncompressed BMP as a fallback.
 */
async function loadFreeze(monitorId: number): Promise<Blob> {
  try {
    const bytes = await freezePng(monitorId);
    if (!bytes || bytes.byteLength === 0) throw new Error("el PNG ha llegado vacio");
    return new Blob([bytes], { type: "image/png" });
  } catch (pngError) {
    console.warn("frozen-screen PNG failed, requesting uncompressed BMP", pngError);
    const bytes = await freezeBytes(monitorId);
    return new Blob([bytes], { type: "image/bmp" });
  }
}

export function SelectionCanvas({ monitorId }: { monitorId: number }) {
  const t = useT();
  const [payload, setPayload] = useState<OverlayPayload | null>(null);
  const [selection, setSelection] = useState<Rect | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: "idle" });
  const [cursor, setCursor] = useState({ x: 0, y: 0 });
  /**
   * Whether the cursor is on THIS screen. Only that overlay draws the magnifier, crosshair,
   * highlight, and mode bar; the others just show the dim layer.
   */
  const [cursorAqui, setCursorAqui] = useState(false);
  const [hex, setHex] = useState("#000000");
  // The key handler is registered once, so it reads the color through a ref instead of a stale
  // closure value.
  const hexRef = useRef(hex);
  hexRef.current = hex;
  /** The last copied color, shown briefly. */
  const [colorCopiado, setColorCopiado] = useState<string | null>(null);
  useEffect(() => {
    if (!colorCopiado) return;
    const id = setTimeout(() => setColorCopiado(null), 1600);
    return () => clearTimeout(id);
  }, [colorCopiado]);
  /** What will be done with the selection, chosen in the mode bar before selecting. */
  const [modo, setModo] = useState<CaptureMode>("still");
  /** Capture the whole screen with one click. */
  const [target, setTarget] = useState<import("../../lib/types").CaptureTarget>("rectangle");
  const targetRef = useRef(target);
  targetRef.current = target;
  const [pantallaEntera, setPantallaEntera] = useState(false);
  const modoRef = useRef<CaptureMode>("still");
  modoRef.current = modo;
  const pantallaRef = useRef(false);
  pantallaRef.current = pantallaEntera;

  /**
   * Apply a mode change on ALL screens. Each monitor's overlay has its own React tree; the
   * event also comes back to the sender, so the listener is the only code path.
   */
  const difundir = useCallback((cambio: Partial<OverlayModeState>) => {
    void emit(EVENTS.overlayMode, {
      target: cambio.target ?? targetRef.current,
      mode: cambio.mode ?? modoRef.current,
      fullScreen: cambio.fullScreen ?? pantallaRef.current,
    } satisfies OverlayModeState);
  }, []);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [bootError, setBootError] = useState<string | null>(null);
  const [source, setSource] = useState<HTMLCanvasElement | null>(null);
  const selectionRef = useRef<Rect | null>(null);
  selectionRef.current = selection;
  /** THIS screen's number, to know whether a by-number command is for us. */
  const numeroPantalla = useRef(0);

  /**
   * Where a press started on the mode bar, while it is undecided. Moving the pointer starts a
   * selection underneath the bar; releasing without moving is a button click.
   */
  const origenBarra = useRef<{ x: number; y: number } | null>(null);

  /**
   * Physical pixels per CSS pixel: the frozen image is authoritative; the webview may be DPI-
   * scaled.
   */
  const scale = useMemo(() => {
    if (!source) return 1;
    return source.width / Math.max(1, window.innerWidth);
  }, [source]);

  /**
   * Whether the background has been drawn into `source`. Until then, the boot screen is shown.
   */
  const [fondoListo, setFondoListo] = useState(false);
  /** Container for the canvas with the frozen screen (the visible background). */
  const fondoRef = useRef<HTMLDivElement>(null);

  /** System windows clipped to this monitor, in local CSS coordinates. */
  const snapTargets = useMemo(
    () =>
      payload
        ? ventanasDeEstaPantalla(
            payload.windows,
            payload.monitor,
            scale,
            window.innerWidth,
            window.innerHeight,
          )
        : [],
    [payload, scale],
  );

  /** The front-most window under the point, by Z order (see `ventanaBajoElPunto`). */
  const windowAt = useCallback(
    (x: number, y: number) => ventanaBajoElPunto(snapTargets, x, y),
    [snapTargets],
  );

  /** The highlighted window, in a ref so the keyboard handler can read it. */
  const hoveredRef = useRef<VentanaDelSistema | null>(null);

  const hovered = useMemo(() => {
    if (!cursorAqui || selection || mode.kind !== "idle" || pantallaEntera) return null;
    return windowAt(cursor.x, cursor.y);
  }, [windowAt, cursor, cursorAqui, selection, mode.kind, pantallaEntera]);
  hoveredRef.current = hovered;

  /**
   * Overlay windows are reused between captures (see `windows_mgr::open_overlays`) and are not
   * remounted, so boot cannot rely on the initial mount. `bootIdRef` identifies the current
   * boot so a late result from an older one cannot overwrite the new state.
   */
  const bootIdRef = useRef(0);

  const boot = useCallback(async (payloadListo?: OverlayPayload) => {
    const miId = ++bootIdRef.current;
    const vigente = () => bootIdRef.current === miId;

    // Clear the previous capture BEFORE requesting anything, so a reused window never flashes
    // the old image.
    setPayload(null);
    setFondoListo(false);
    setSource(null);
    setSelection(null);
    setMode({ kind: "idle" });
    setPantallaEntera(false);
    setBusy(false);
    // `conBarra` is not reset here: the payload sets it from the settings below, and resetting
    // it first would briefly show the toolbar to users who turned it off.
    setError(null);
    setBootError(null);
    setCursorAqui(false);

    try {
      // For reused windows the backend sends the payload in the event (see EVENTS.overlayShow).
      // It is only requested with invoke on the first mount.
      const data = payloadListo ?? (await overlayBootstrap(monitorId));
      if (!vigente()) return;
      setPayload(data);
      numeroPantalla.current = data.screenNumber;
      // Where the cursor is at open time: without movement no `pointermove` arrives, and no
      // screen would know it has the cursor.
      void cursorPosition()
        .then((p) => {
          const m = data.monitor;
          const fuera =
            p.x < m.x || p.y < m.y || p.x >= m.x + m.width || p.y >= m.y + m.height;
          if (!vigente() || fuera) return;
          const escala = window.devicePixelRatio || 1;
          setCursor({ x: (p.x - m.x) / escala, y: (p.y - m.y) / escala });
          setCursorAqui(true);
        })
        .catch(() => {});
      // The record shortcut opens the overlay in video mode.
      setModo(data.intent === "record" ? "video" : "still");
      const initialTarget = (data.intent === "record" ? data.settings.recordTarget : data.settings.captureTarget) ?? "rectangle";
      setTarget(initialTarget);
      setPantallaEntera(initialTarget === "screen");
      void getCurrentWindow().setFocus();

      // Use `data.monitor.id` (this capture's id), NEVER the `monitorId` prop from the URL: the
      // window is reused and that prop was fixed at creation, which may no longer match if
      // monitor order changed.
      const blob = await loadFreeze(data.monitor.id);
      if (!vigente()) return;

      // One canvas serves as both background and magnifier source, so the image is decoded
      // once.
      const bitmap = await createImageBitmap(blob);
      if (!vigente()) return;
      const canvas = document.createElement("canvas");
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      canvas.className = "pointer-events-none absolute inset-0 h-full w-full";
      canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
      bitmap.close?.();
      setSource(canvas);
      setFondoListo(true);
    } catch (e) {
      if (vigente()) setBootError(String(e));
    }
  }, [monitorId]);

  useEffect(() => {
    void boot();
  }, [boot]);

  // The frozen-screen canvas is attached to the DOM manually so React never redraws it and the
  // magnifier reads the same element that is shown.
  useLayoutEffect(() => {
    const hueco = fondoRef.current;
    if (!hueco || !source || !fondoListo) return;
    hueco.replaceChildren(source);
    return () => {
      hueco.replaceChildren();
    };
  }, [source, fondoListo]);

  /**
   * With the background attached, tell Rust it can show this window (it stays parked off-screen
   * until then). Waits for the next animation frame, which contains the new canvas; a parked
   * webview may not paint frames, so it notifies anyway after 30 ms.
   */
  useEffect(() => {
    if (!fondoListo || !payload) return;
    const { id } = payload.monitor;
    const generacion = payload.generation;
    let avisado = false;
    const avisar = () => {
      if (avisado) return;
      avisado = true;
      void overlayListo(id, generacion);
    };
    const uno = requestAnimationFrame(avisar);
    const plazo = window.setTimeout(avisar, 30);
    return () => {
      cancelAnimationFrame(uno);
      window.clearTimeout(plazo);
    };
  }, [fondoListo, payload]);

  // Reused windows get no remount, so the backend sends a show event with the new payload.
  //
  // `target` is NOT optional here: without it `listen` registers for every event, including
  // those for other windows, and all overlays ended up showing the same screen. The label is
  // passed as a string so both sides use `AnyLabel` with the same label.
  useEffect(() => {
    const etiqueta = getCurrentWindow().label;
    const unlisten = listen<OverlayPayload>(EVENTS.overlayShow, (e) => void boot(e.payload), {
      target: etiqueta,
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [boot]);

  const toPhysical = useCallback(
    (rect: Rect): Rect => aVirtual(rect, payload!.monitor, scale),
    [payload, scale],
  );

  const capturarRegion = useCallback(
    async (rect: Rect, action: StillAction) => {
      if (!payload || busy) return;
      setBusy(true);
      setError(null);
      try {
        await captureStill(toPhysical(rect), action);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [payload, busy, toPhysical],
  );

  /**
   * The last captured region in THIS screen's CSS coordinates, or null if it was on another
   * screen. Judged by the center, like Rust when choosing which screen to crop from.
   */
  const ultimaRegion = useMemo(() => {
    const r = payload?.lastRegion;
    if (!r || !payload) return null;
    if (!esDeEstaPantalla(r, payload.monitor)) return null;
    return aPantalla(r, payload.monitor, scale);
  }, [payload, scale]);

  const ultimaRegionRef = useRef<Rect | null>(null);
  ultimaRegionRef.current = ultimaRegion;

  const capturarTodasLasPantallas = useCallback(
    async (action: StillAction) => {
      if (busy) return;
      setBusy(true);
      setError(null);
      try {
        await captureAllScreens(action);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [busy],
  );

  /**
   * Record THE WINDOW under the cursor rather than the screen area it occupies, so anything
   * passing in front of it stays out of the video.
   */
  const grabarVentana = useCallback(
    async (ventana: VentanaDelSistema, format: "video") => {
      if (!payload || busy || !ventana.hwnd) return;
      setBusy(true);
      setError(null);
      try {
        await startRecording(toPhysical(ventana.rect), {
          format,
          audio: payload.settings.recordAudio && format === "video",
          microphone: payload.settings.recordMicrophone && format === "video",
          window: ventana.hwnd,
        });
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [payload, busy, toPhysical],
  );

  const grabarRegion = useCallback(
    async (rect: Rect, format: "video") => {
      if (!payload || busy) return;
      setBusy(true);
      setError(null);
      try {
        await startRecording(toPhysical(rect), {
          format,
          audio: payload.settings.recordAudio && format === "video",
          microphone: payload.settings.recordMicrophone && format === "video",
        });
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [payload, busy, toPhysical],
  );


  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          // One Escape always exits, with or without a selection; a new selection can be
          // dragged without clearing the old one.
          void cancelCapture();
          return;
      }
      const key = e.key.toLowerCase();
      if (key === "a" && e.ctrlKey) {
        e.preventDefault();
        const todo = { x: 0, y: 0, width: window.innerWidth, height: window.innerHeight };
        if (modo !== "still") void grabarRegion(todo, modo);
        else void capturarRegion(todo, "copy");
      } else if (key === "c") {
        // Copy the color under the cursor. Does not close the selection, since people often
        // pick several colors.
        void copyColor(hexRef.current)
          .then(() => setColorCopiado(hexRef.current))
          .catch((err) => setError(String(err)));
      } else if (key === "p") {
        difundir({ fullScreen: !pantallaRef.current });
      } else if (key === "f") {
        difundir({ mode: "still" });
      } else if (key === "v") {
        difundir({ mode: "video" });
      } else if (key === "w") {
        // Record ONLY the window under the cursor, even if covered. A key rather than a button
        // because it only makes sense while a window is highlighted.
        const debajo = hoveredRef.current;
        if (!debajo?.hwnd) return;
        e.preventDefault();
        void grabarVentana(debajo, "video");
      } else if (key === "r") {
        // Repeat the last region without dragging. Only the screen where that region was
        // responds.
        const previa = ultimaRegionRef.current;
        if (!previa) return;
        e.preventDefault();
        if (modoRef.current !== "still") void grabarRegion(previa, modoRef.current);
        else void capturarRegion(previa, "copy");
      } else if (key === "0") {
        // All screens at once, in one image. `0` goes with the screen numbers and means "all".
        // Rust stitches the frozen screens, so the focused overlay requests it directly.
        // Screenshots only; does nothing in video mode.
        if (modoRef.current !== "still") return;
        e.preventDefault();
        void capturarTodasLasPantallas("copy");
      } else if (key >= "1" && key <= "9") {
        // The number shown on each screen is the key that captures it. Sent as an event because
        // keys only reach the focused screen, which is rarely the one wanted.
        e.preventDefault();
        void emit(EVENTS.overlayTakeScreen, Number(key));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    modo,
    capturarRegion,
    capturarTodasLasPantallas,
    grabarRegion,
    difundir,
  ]);

  const readHex = useCallback(
    (cssX: number, cssY: number) => {
      if (!source) return;
      const ctx = source.getContext("2d", { willReadFrequently: true });
      if (!ctx) return;
      const px = clamp(Math.floor(cssX * scale), 0, source.width - 1);
      const py = clamp(Math.floor(cssY * scale), 0, source.height - 1);
      const data = ctx.getImageData(px, py, 1, 1).data;
      const toHex = (c: number) => c.toString(16).padStart(2, "0");
      setHex("#" + toHex(data[0]) + toHex(data[1]) + toHex(data[2]));
    },
    [source, scale],
  );

  // During a gesture listen on window, not the div, so the drag is not lost if the webview
  // denies pointer capture.
  useEffect(() => {
    if (mode.kind === "idle") return;

    const onMove = (e: PointerEvent) => {
      const x = e.clientX;
      const y = e.clientY;
      setCursor({ x, y });
      if (mode.kind === "drawing") {
        // The magnifier stays visible while dragging, so the color follows the cursor.
        readHex(x, y);
        setSelection(normalize(mode.originX, mode.originY, x, y));
      } else if (mode.kind === "moving") {
        setSelection({
          x: clamp(x - mode.grabX, 0, window.innerWidth - mode.base.width),
          y: clamp(y - mode.grabY, 0, window.innerHeight - mode.base.height),
          width: mode.base.width,
          height: mode.base.height,
        });
      } else if (mode.kind === "resizing") {
        setSelection(applyHandle(mode.base, mode.handle, x, y));
      }
    };

    const onUp = () => {
      if (mode.kind === "drawing") {
        const drawn = selectionRef.current;
        const dibujado = drawn && drawn.width >= MIN_DRAG && drawn.height >= MIN_DRAG;
        // Plain click: if there is a window underneath, select all of it.
        if (!dibujado) setSelection(mode.candidate);
        const elegido = dibujado ? drawn : mode.candidate;
        // On release: start recording, or copy the screenshot to the clipboard.
        if (elegido) {
          if (modo !== "still") void grabarRegion(elegido, modo);
          else void capturarRegion(elegido, "copy");
        }
      }
      setMode({ kind: "idle" });
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
    };
  }, [mode, readHex, modo, capturarRegion, grabarRegion]);

  /** Take this whole screen using the current mode: screenshot or video. */
  const llevarsePantalla = useCallback(async () => {
    const todo = {
      x: 0,
      y: 0,
      width: window.innerWidth,
      height: window.innerHeight,
    };
    difundir({ fullScreen: false });
    if (modoRef.current !== "still") await grabarRegion(todo, modoRef.current);
    else await capturarRegion(todo, "copy");
  }, [difundir, capturarRegion, grabarRegion]);

  // Commands from other screens: a mode change, or capture this whole screen because its number
  // was pressed.
  useEffect(() => {
    const modos = listen<OverlayModeState>(EVENTS.overlayMode, ({ payload: p }) => {
      setModo(p.mode);
      setPantallaEntera(p.fullScreen);
      // Another screen applied a preset: refresh the settings this overlay records with.
      if (p.preset !== undefined) void getSettings().then((s) => setPayload((old) => (old ? { ...old, settings: s } : old)));
      setTarget(p.fullScreen ? "screen" : p.target === "window" ? "window" : "rectangle");
      if (p.fullScreen) setSelection(null);
    });
    const numeros = listen<number>(EVENTS.overlayTakeScreen, ({ payload: n }) => {
      if (n === numeroPantalla.current) void llevarsePantalla();
    });
    return () => {
      void modos.then((fn) => fn());
      void numeros.then((fn) => fn());
    };
  }, [llevarsePantalla]);

  const onPointerDown = (e: React.PointerEvent) => {
    if (busy) return;
    // A press on the mode bar is undecided: remember where it started and wait to see whether
    // the pointer moves (select underneath) or is released (button click). See `origenBarra`.
    if (enLaBarra(e.target)) {
      origenBarra.current = { x: e.clientX, y: e.clientY };
      return;
    }
    // In full-screen mode there is nothing to drag: the clicked screen is captured immediately.
    if (pantallaEntera) {
      void llevarsePantalla();
      return;
    }
    const x = e.clientX;
    const y = e.clientY;
    if (target === "window") {
      const candidate = windowAt(x, y);
      if (!candidate) return;
      setSelection(candidate.rect);
      if (modo !== "still") void grabarRegion(candidate.rect, modo);
      else void capturarRegion(candidate.rect, "copy");
      return;
    }
    const current = selectionRef.current;
    if (current && contains(current, x, y)) {
      setMode({ kind: "moving", grabX: x - current.x, grabY: y - current.y, base: current });
      return;
    }
    setMode({ kind: "drawing", originX: x, originY: y, candidate: windowAt(x, y)?.rect ?? null });
    setSelection({ x, y, width: 0, height: 0 });
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const desdeLaBarra = origenBarra.current;
    if (desdeLaBarra) {
      // `buttons` is zero once released: that was a mode-bar click and this move must not start
      // a selection.
      if (e.buttons === 0 || pantallaEntera || busy) {
        origenBarra.current = null;
      } else if (
        Math.abs(e.clientX - desdeLaBarra.x) >= MIN_DRAG ||
        Math.abs(e.clientY - desdeLaBarra.y) >= MIN_DRAG
      ) {
        origenBarra.current = null;
        // No window snapping: dragging from the mode bar means a custom selection.
        setMode({
          kind: "drawing",
          originX: desdeLaBarra.x,
          originY: desdeLaBarra.y,
          candidate: null,
        });
        setSelection(normalize(desdeLaBarra.x, desdeLaBarra.y, e.clientX, e.clientY));
        return;
      }
    }
    setCursorAqui(true);
    if (mode.kind !== "idle") return;
    setCursor({ x: e.clientX, y: e.clientY });
    readHex(e.clientX, e.clientY);
  };

  if (!payload || !fondoListo) {
    return <BootScreen error={bootError} />;
  }

  const active =
    selection !== null && selection.width >= MIN_DRAG && selection.height >= MIN_DRAG
      ? selection
      : null;
  const highlight = !active && hovered ? hovered : null;
  const magnifierVisible =
    cursorAqui &&
    payload.settings.showMagnifier &&
    !pantallaEntera &&
    (!active || mode.kind === "drawing");

  return (
    <div
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerEnter={() => setCursorAqui(true)}
      onPointerLeave={() => setCursorAqui(false)}
      className="relative h-screen w-screen overflow-hidden"
      style={{ cursor: pantallaEntera ? "pointer" : active ? "default" : "crosshair" }}
    >
      {/* The background: the frozen-screen canvas attached by the effect above. A canvas rather
         than an <img> so the image is decoded once. */}
      <div ref={fondoRef} className="pointer-events-none absolute inset-0" />

      {/* No selection: uniform dim layer. With a selection, the box's shadow draws it. */}
      {!active && <div className="pointer-events-none absolute inset-0 bg-black/45" />}

      {/* The window under the cursor, with its title. */}
      {highlight && (
        <div
          style={{
            left: highlight.rect.x,
            top: highlight.rect.y,
            width: highlight.rect.width,
            height: highlight.rect.height,
          }}
          className="pointer-events-none absolute rounded-[3px] border border-white/45 bg-white/[0.06]"
        >
          {highlight.title && (
            <span
              // Outside the box when it fits; inside it would overlap the window's title bar.
              className={`absolute left-0 max-w-[min(360px,100%)] truncate rounded-md bg-neutral-900/90 px-2 py-1 text-[13px] text-neutral-200 shadow-lg backdrop-blur-sm ${
                highlight.rect.y > 28 ? "-top-7" : "top-1.5 left-1.5"
              }`}
            >
              {highlight.title}
              {/* Key hint for recording just this window, only in video mode and when Windows
                 provides the window handle. */}
              {modo !== "still" && highlight.hwnd > 0 && (
                <span className="ml-2 rounded bg-white/15 px-1.5 py-0.5 text-[12px] font-semibold text-white">
                  {t("W: record just this window")}
                </span>
              )}
            </span>
          )}
        </div>
      )}

      {active && (
        <>
          <div
            style={{
              left: active.x,
              top: active.y,
              width: active.width,
              height: active.height,
              boxShadow: "0 0 0 100vmax rgba(0,0,0,0.45)",
            }}
            className="pointer-events-none absolute border border-blue-500/90"
          />
          <SelectionHandles
            rect={active}
            onGrab={(handle) => setMode({ kind: "resizing", handle, base: active })}
          />
          <DimensionBadge
            width={active.width * scale}
            height={active.height * scale}
            left={active.x + 2}
            top={active.y > 26 ? active.y - 26 : active.y + 6}
          />
        </>
      )}

      {/* Crosshair guides through the cursor, for aligning a selection edge with something
         across the screen. Shown together with the magnifier and hidden once a selection
         exists. */}
      {magnifierVisible && (
        <div className="pointer-events-none absolute inset-0 z-30">
          <div
            className="absolute inset-x-0 h-px bg-white/25 mix-blend-difference"
            style={{ top: cursor.y }}
          />
          <div
            className="absolute inset-y-0 w-px bg-white/25 mix-blend-difference"
            style={{ left: cursor.x }}
          />
        </div>
      )}

      {magnifierVisible && source && (
        <Magnifier
          source={source}
          px={cursor.x * scale}
          py={cursor.y * scale}
          {...sitioDeLaLupa(cursor.x, cursor.y, window.innerWidth, window.innerHeight)}
          hex={hex}
        />
      )}

      <AnimatePresence>
      </AnimatePresence>

      {/* The copied color, shown like errors: a brief notice that does not cover the screen. */}
      {colorCopiado && !error && (
        <div className="pointer-events-none absolute inset-x-0 top-8 flex justify-center">
          <div className="flex items-center gap-2 rounded-xl border border-white/15 bg-neutral-900/90 px-4 py-2.5 text-xs text-neutral-200 shadow-2xl backdrop-blur-md">
            <span
              className="size-3 rounded-[4px] border border-white/25"
              style={{ backgroundColor: colorCopiado }}
            />
            <span className="font-mono uppercase">{colorCopiado}</span>
            <span className="text-neutral-400">{t("copied")}</span>
          </div>
        </div>
      )}

      {error && (
        <div className="pointer-events-none absolute inset-x-0 top-8 flex justify-center">
          <div className="max-w-xl rounded-xl border border-red-500/30 bg-red-950/90 px-4 py-2.5 text-xs text-red-200 shadow-2xl backdrop-blur-md">
            {/* Error text from Rust is shown as-is. */}
            {t(error)}
          </div>
        </div>
      )}

      {/* Outline of the last capture with its key (R), to repeat it. Hidden once dragging
         starts. */}
      {cursorAqui &&
        ultimaRegion &&
        !selection &&
        mode.kind === "idle" &&
        !pantallaEntera &&
        !hovered && (
        <div
          className="pointer-events-none absolute rounded-[3px] border border-dashed border-white/35"
          style={{
            left: ultimaRegion.x,
            top: ultimaRegion.y,
            width: ultimaRegion.width,
            height: ultimaRegion.height,
          }}
        >
          <span className="absolute -top-7 left-0 flex items-center gap-1.5 rounded-full border border-white/10 bg-neutral-900/85 px-2.5 py-1 text-[13px] whitespace-nowrap text-neutral-400 shadow-lg backdrop-blur-md">
            <kbd className="rounded-[4px] border border-white/15 bg-white/10 px-1.5 py-0.5 text-[12px] leading-none font-medium text-neutral-200">
              R
            </kbd>
            previous region
          </span>
        </div>
      )}

      {pantallaEntera && !active && (
        <ScreenPicker
          numero={payload.screenNumber}
          total={payload.screenCount}
          modo={modo}
          ancho={payload.monitor.width}
          alto={payload.monitor.height}
        />
      )}

      {/* The mode bar, only on the cursor's screen; in full-screen mode it shows on every
         screen so one can be picked. */}
      {(cursorAqui || pantallaEntera) && (
        <ModeBar
          value={modo}
          onChange={(m) => difundir({ mode: m })}
          target={target}
          onTarget={v => { setSelection(null); difundir({ target: v, fullScreen: v === "screen" }); }}
          pantallaEntera={pantallaEntera}
          onPantallaEntera={(v) => difundir({ fullScreen: v })}
          presets={(payload.settings.presets ?? []).map((p) => ({ name: p.name, summary: presetSummary(p) }))}
          activePreset={payload.settings.activePreset}
          activeSummary={presetSummary(payload.settings)}
          onPreset={(name) => {
            void applyPreset(name)
              .then((s) => {
                setPayload((p) => (p ? { ...p, settings: s } : p));
                difundir({ preset: name });
              })
              .catch((e) => setError(String(e)));
          }}
          onAjustes={() => void openSettings()}
          onCancel={() => void cancelCapture()}
          dimmed={mode.kind !== "idle"}
        />
      )}
    </div>
  );
}
