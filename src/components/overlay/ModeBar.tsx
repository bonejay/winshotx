import { AppWindow, Camera, RectangleHorizontal, Monitor, Settings, Video, X } from "lucide-react";
import type { CaptureMode } from "../../lib/types";
import { useT } from "../../lib/i18n";

/**
 * How the canvas recognizes a gesture that starts on the bar. The bar does not capture
 * `pointerdown`, so selecting can start underneath it; this marker lets the canvas ignore
 * button clicks.
 */
export const SELECTOR_BARRA = "[data-barra-modos]";

interface Props {
  target?: import("../../lib/types").CaptureTarget;
  onTarget?: (v: import("../../lib/types").CaptureTarget) => void;
  value: CaptureMode;
  onChange: (mode: CaptureMode) => void;
  /** Capture the whole screen with one click. */
  pantallaEntera: boolean;
  onPantallaEntera: (valor: boolean) => void;
  /** Recording presets and the active one, shown in video mode before recording starts. */
  presets?: { name: string; summary: string }[];
  activePreset?: string;
  activeSummary?: string;
  onPreset?: (name: string) => void;
  /** Open the settings, closing the capture. */
  onAjustes: () => void;
  onCancel: () => void;
  /** Moves out of the way while dragging so it does not cover the selection. */
  dimmed: boolean;
}

/**
 * Screenshot or video, chosen before selecting. Top-center, where Windows puts its own capture
 * bar. Icons only; labels are in the tooltips.
 */
export function ModeBar({
  target = "rectangle",
  onTarget,
  value,
  onChange,
  pantallaEntera,
  onPantallaEntera,
  presets = [],
  activePreset = "",
  activeSummary = "",
  onPreset,
  onAjustes,
  onCancel,
  dimmed,
}: Props) {
  const t = useT();
  return (
    <div
      data-barra-modos
      className={`group absolute top-6 left-1/2 z-50 -translate-x-1/2 transition-opacity duration-150 ${
        dimmed ? "pointer-events-none opacity-0" : "pointer-events-auto opacity-100"
      }`}
    >
      {/* On hover the bar fades to an outline so it does not cover the top-center area being
         captured. It stays clickable and drags pass through it (see SELECTOR_BARRA). */}
      <div className="flex items-center gap-1 rounded-2xl border border-white/10 bg-neutral-900/90 p-1.5 shadow-2xl backdrop-blur-xl transition-[background-color,border-color,box-shadow,backdrop-filter] duration-150 ">
        <Boton
          activo={value === "still"}
          onClick={() => onChange("still")}
          etiqueta="Photo"
          titulo="Photo of the crop · F"
        >
          <Camera className="size-[19px]" />
        </Boton>

        <Boton
          activo={value === "video"}
          onClick={() => onChange("video")}
          etiqueta="Video"
          titulo="Record the crop to MP4 · V"
        >
          <Video className="size-[19px]" />
        </Boton>

        <span className="mx-1 h-6 w-px bg-white/10" />

        {/* Recording preset, chosen before the recording starts. */}
        {value === "video" && presets.length > 0 && (
          <>
            <label className="flex items-center gap-2 px-1 text-[12px] text-neutral-300" title={activeSummary}>
              <select
                aria-label="Recording preset"
                value={activePreset}
                onChange={(e) => onPreset?.(e.target.value)}
                className="max-w-[150px] rounded-lg border border-white/10 bg-neutral-800 px-2 py-1 text-[12px] text-neutral-100 outline-none"
              >
                {!activePreset && <option value="">Custom</option>}
                {presets.map((p) => (
                  <option key={p.name} value={p.name}>{p.name}</option>
                ))}
              </select>
              <span className="whitespace-nowrap tabular-nums text-neutral-400">{activeSummary}</span>
            </label>
            <span className="mx-1 h-6 w-px bg-white/10" />
          </>
        )}

        {/* Where to capture from: with several screens each shows its number and can be taken
           with one click. */}
        <Boton activo={!pantallaEntera && target === "rectangle"} onClick={() => onTarget?.("rectangle")} etiqueta="Rectangle" titulo="Select a rectangle"><RectangleHorizontal className="size-[19px]" /></Boton>
        <Boton activo={!pantallaEntera && target === "window"} onClick={() => onTarget?.("window")} etiqueta="Window" titulo="Select a window"><AppWindow className="size-[19px]" /></Boton>
        <Boton
          activo={pantallaEntera}
          onClick={() => onTarget ? onTarget(pantallaEntera ? "rectangle" : "screen") : onPantallaEntera(!pantallaEntera)}
          etiqueta="Whole screen"
          titulo="Whole screen, one click · P"
        >
          <Monitor className="size-[19px]" />
        </Boton>

        <span className="mx-1 h-6 w-px bg-white/10" />

        {/* No `aria-pressed`: this opens a window instead of toggling. It is here because a
           full-screen capture otherwise hides the tray icon. */}
        <button
          type="button"
          onClick={onAjustes}
          title={t("winshotx settings")}
          aria-label={t("Settings")}
          className="flex size-9 items-center justify-center rounded-xl text-neutral-300 transition-colors hover:bg-white/10 hover:text-white"
        >
          <Settings className="size-[19px]" />
        </button>

        <span className="mx-1 h-6 w-px bg-white/10" />

        <button
          type="button"
          onClick={onCancel}
          title={t("Leave without capturing · Esc")}
          aria-label={t("Leave without capturing")}
          className="flex size-9 items-center justify-center rounded-xl text-neutral-400 transition-colors hover:bg-red-500/20 hover:text-red-300"
        >
          <X className="size-[19px]" />
        </button>
      </div>
    </div>
  );
}


function Boton({
  activo,
  onClick,
  etiqueta,
  titulo,
  children,
}: {
  activo: boolean;
  onClick: () => void;
  etiqueta: string;
  titulo: string;
  children: React.ReactNode;
}) {
  const t = useT();
  return (
    <button
      type="button"
      onClick={onClick}
      title={t(titulo)}
      aria-label={t(etiqueta)}
      aria-pressed={activo}
      className={`flex size-9 items-center justify-center rounded-xl transition-colors ${
        activo
          ? "bg-blue-500 text-white shadow-sm"
          : "text-neutral-300 hover:bg-white/10 hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}
