import type { CaptureMode } from "../../lib/types";
import { useT } from "../../lib/i18n";

interface Props {
  /** This screen's number, starting at 1. */
  numero: number;
  /** How many screens there are. A single screen is not numbered. */
  total: number;
  modo: CaptureMode;
  ancho: number;
  alto: number;
}

const QUE_HACE: Record<CaptureMode, string> = {
  still: "capture this screen",
  video: "record this screen",
};

/**
 * The "this is screen 2" badge that captures the whole screen with one click, so multi-monitor
 * users can pick a screen without dragging corner to corner.
 */
export function ScreenPicker({ numero, total, modo, ancho, alto }: Props) {
  const t = useT();
  return (
    <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
      {/* Inset border: an outer one would be off-screen. */}
      <div className="absolute inset-[3px] rounded-lg border-2 border-blue-500/70" />

      <div className="flex flex-col items-center gap-3">
        {total > 1 && (
          <span className="flex size-[104px] items-center justify-center rounded-3xl border-2 border-blue-400/50 bg-neutral-900/85 text-[56px] leading-none font-semibold text-white tabular-nums shadow-2xl backdrop-blur-md">
            {numero}
          </span>
        )}
        <span className="flex items-center gap-2 rounded-full border border-white/10 bg-neutral-900/90 px-4 py-2 text-[15px] text-neutral-200 shadow-xl backdrop-blur-md">
          {total > 1 && (
            // Show the key, since the big number alone does not say it can be pressed, and the
            // cursor is often on another screen.
            <>
              <kbd className="rounded-[5px] border border-white/15 bg-white/10 px-1.5 py-0.5 text-[13px] leading-none font-medium">
                {numero}
              </kbd>
              <span className="text-neutral-500">{t("or click to")}</span>
            </>
          )}
          {total === 1 && <span>{t("Click to")}</span>}
          <span>{QUE_HACE[modo]}</span>
          <span className="text-neutral-500 tabular-nums">
            {ancho} × {alto}
          </span>
        </span>

        {/* Hint for the `0` key (capture all screens). Only with several screens and only for
           screenshots. */}
        {total > 1 && modo === "still" && (
          <span className="flex items-center gap-2 rounded-full border border-white/10 bg-neutral-900/70 px-3 py-1.5 text-[14px] text-neutral-400 shadow-lg backdrop-blur-md">
            <kbd className="rounded-[5px] border border-white/15 bg-white/10 px-1.5 py-0.5 text-[13px] leading-none font-medium text-neutral-200">
              0
            </kbd>
            <span>to capture all {total} screens at once</span>
          </span>
        )}
      </div>
    </div>
  );
}
