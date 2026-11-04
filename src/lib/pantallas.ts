/**
 * Conversions between a screen's coordinates and the virtual desktop.
 *
 * Windows places monitors on one plane: the primary starts at (0, 0) and monitors left of or
 * above it have NEGATIVE coordinates, so clamping to zero (`Math.max(0, x)`) moves a region to
 * the wrong screen.
 *
 * Two units are involved:
 * - Physical: used by Rust and the virtual desktop; real pixels.
 * - CSS: what the webview sees, divided by the screen's display scale.
 * The `escala` (scale) is physical pixels per CSS pixel.
 */
import type { MonitorInfo, Rect } from "./types";

/** A monitor's origin, taken separately so tests do not need a full monitor. */
export type Origen = Pick<MonitorInfo, "x" | "y" | "width" | "height">;

/**
 * From this screen's CSS coordinates to virtual-desktop physical pixels. Width and height are
 * at least two pixels: a one-pixel selection is a slip, and encoders reject zero sizes.
 */
export function aVirtual(rect: Rect, monitor: Origen, escala: number): Rect {
  return {
    x: Math.round(monitor.x + rect.x * escala),
    y: Math.round(monitor.y + rect.y * escala),
    width: Math.max(2, Math.round(rect.width * escala)),
    height: Math.max(2, Math.round(rect.height * escala)),
  };
}

/** The reverse: from virtual-desktop physical pixels to this screen's CSS coordinates. */
export function aPantalla(rect: Rect, monitor: Origen, escala: number): Rect {
  return {
    x: (rect.x - monitor.x) / escala,
    y: (rect.y - monitor.y) / escala,
    width: rect.width / escala,
    height: rect.height / escala,
  };
}

/**
 * Whether a virtual-desktop region belongs to this screen, judged by its CENTER, so a region
 * spanning two monitors belongs to exactly one. Rust uses the same rule when choosing which
 * screen to crop from.
 */
export function esDeEstaPantalla(rect: Rect, monitor: Origen): boolean {
  const cx = rect.x + Math.floor(rect.width / 2);
  const cy = rect.y + Math.floor(rect.height / 2);
  return (
    cx >= monitor.x &&
    cy >= monitor.y &&
    cx < monitor.x + monitor.width &&
    cy < monitor.y + monitor.height
  );
}

/** A system window as sent by Rust. */
export interface VentanaDelSistema {
  title: string;
  rect: Rect;
  /** Its HWND, so the window itself can be recorded. */
  hwnd: number;
}

/**
 * System windows visible on this screen, in CSS coordinates. Windows not on this screen and
 * those smaller than eight pixels (often invisible system windows) are skipped.
 */
export function ventanasDeEstaPantalla(
  ventanas: VentanaDelSistema[],
  monitor: Origen,
  escala: number,
  anchoCss: number,
  altoCss: number,
): VentanaDelSistema[] {
  return ventanas
    .map((v) => ({ ...v, rect: aPantalla(v.rect, monitor, escala) }))
    .filter(
      (v) =>
        v.rect.width > 8 &&
        v.rect.height > 8 &&
        v.rect.x < anchoCss &&
        v.rect.y < altoCss &&
        v.rect.x + v.rect.width > 0 &&
        v.rect.y + v.rect.height > 0,
    );
}

/**
 * The topmost window under a point. Rust sends windows in `EnumWindows` order (Z order, front
 * first), so the first match is the visible one.
 */
export function ventanaBajoElPunto(
  ventanas: VentanaDelSistema[],
  x: number,
  y: number,
): VentanaDelSistema | null {
  return (
    ventanas.find(
      (v) =>
        x >= v.rect.x &&
        y >= v.rect.y &&
        x < v.rect.x + v.rect.width &&
        y < v.rect.y + v.rect.height,
    ) ?? null
  );
}
