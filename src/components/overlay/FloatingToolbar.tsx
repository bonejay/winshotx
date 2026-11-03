import { Check, Circle, Copy, Download, FolderOpen, Pin, Type, X } from "lucide-react";
import type { CaptureMode } from "../../lib/types";
import { GlassPanel } from "../ui/GlassPanel";
import { IconButton } from "../ui/IconButton";
import { useT } from "../../lib/i18n";

interface Props {
  left: number;
  top: number;
  /** La barra se pega arriba de la seleccion cuando abajo no cabe. */
  flipped: boolean;
  busy: boolean;
  /** Lo que se eligio arriba: manda en lo que puede hacerse con esta seleccion. */
  modo: CaptureMode;
  onCopy: () => void;
  onSave: () => void;
  /** Guardar donde diga el usuario, con el diálogo de Windows, en vez de en la carpeta de siempre. */
  onSaveAs: () => void;
  /** Deja la captura flotando encima de todo, en su sitio, hasta que se cierre. */
  onPin: () => void;
  /** Lee el texto de la captura y lo copia, sin la imagen. */
  onText: () => void;
  onRecord: () => void;
  onCancel: () => void;
}

/**
 * Qué hacer con la foto ya recortada.
 *
 * Grabar en GIF o en vídeo ya no está aquí: eso se elige arriba, antes de recortar, en la
 * misma barra que en el perfil "se copia sola". Tenerlo en los dos sitios significaba que
 * el icono de GIF eran unas chispas, que no dicen nada, al lado del de vídeo.
 */
export function FloatingToolbar({
  left,
  top,
  flipped,
  busy,
  modo,
  onCopy,
  onSave,
  onSaveAs,
  onPin,
  onText,
  onRecord,
  onCancel,
}: Props) {
  const t = useT();
  return (
    <div
      style={{ left, top }}
      className={`absolute z-40 -translate-x-1/2 ${flipped ? "-translate-y-full" : ""}`}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <GlassPanel className="flex items-center gap-0.5 rounded-xl p-1">
        {modo === "still" ? (
          <>
            <IconButton
              icon={Copy}
              label={t("Copy")}
              shortcut="Enter"
              showLabel
              accent
              onClick={onCopy}
              disabled={busy}
            />
            <IconButton
              icon={Download}
              label={t("Save")}
              shortcut="Ctrl+S"
              onClick={onSave}
              disabled={busy}
            />
            <IconButton
              icon={FolderOpen}
              label={t("Save to…")}
              shortcut={t("Ctrl+Shift+S")}
              onClick={onSaveAs}
              disabled={busy}
            />
            <IconButton
              icon={Pin}
              label={t("Pin")}
              shortcut="A"
              onClick={onPin}
              disabled={busy}
            />
            <IconButton
              icon={Type}
              label={t("Copy the text")}
              shortcut="T"
              onClick={onText}
              disabled={busy}
            />
          </>
        ) : (
          // Un botón, y grande: aquí ya está elegido si es vídeo o GIF. Lo que falta es
          // dejar ajustar el recuadro antes de empezar, que grabando importa más que en
          // una foto porque lo que salga mal se descubre minutos después.
          <button
            type="button"
            onClick={onRecord}
            disabled={busy}
            className="flex h-8 items-center gap-2 rounded-lg bg-red-500 px-3.5 text-[15px] font-semibold text-white transition-colors hover:bg-red-400 disabled:opacity-50"
          >
            <Circle className="size-3 fill-current" />
            {t("Record video")}
          </button>
        )}
        <span className="mx-0.5 h-5 w-px bg-white/10" />
        <IconButton icon={X} label={t("Cancel")} shortcut="Esc" onClick={onCancel} danger />
      </GlassPanel>
      {busy && (
        <div className="mt-1.5 flex items-center gap-1.5 rounded-lg bg-neutral-900/90 px-2 py-1 text-[13px] text-neutral-300 shadow-lg">
          <Check className="size-3 text-emerald-400" /> {t("Working…")}
        </div>
      )}
    </div>
  );
}
