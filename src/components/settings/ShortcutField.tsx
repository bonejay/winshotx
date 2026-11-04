import { useEffect, useRef, useState } from "react";
import { partesDeAtajo } from "../../lib/teclas";
import { useT } from "../../lib/i18n";

/** Modifier-only keys: they cannot form a shortcut by themselves. */
const MODIFIERS = new Set([
  "Control",
  "Shift",
  "Alt",
  "Meta",
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

interface Props {
  value: string;
  onChange: (value: string) => void;
  /** false when the system refused to register it. */
  active: boolean;
}

export function ShortcutField({ value, onChange, active }: Props) {
  const t = useT();
  const [recording, setRecording] = useState(false);
  const boxRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!recording) return;

    const onKey = (e: KeyboardEvent) => {
      if (e.type === "keyup" && e.code !== "PrintScreen" && e.key !== "PrintScreen") return;
      e.preventDefault();
      e.stopPropagation();

      if (e.key === "Escape") {
        setRecording(false);
        return;
      }
      if (MODIFIERS.has(e.code) || MODIFIERS.has(e.key)) return;

      const parts: string[] = [];
      // The Windows key is Super, not Ctrl; mixing them saved Win+X as Ctrl+X.
      if (e.ctrlKey) parts.push("CmdOrCtrl");
      if (e.metaKey) parts.push("Super");
      if (e.shiftKey) parts.push("Shift");
      if (e.altKey) parts.push("Alt");
      // A global shortcut without modifiers would take over the key system-wide; Print Screen
      // alone is the one exception, since taking it over is the point.
      if (parts.length === 0 && e.code !== "PrintScreen" && e.key !== "PrintScreen") return;
      // Without a physical key code the shortcut would be "CmdOrCtrl+Shift+", which Windows
      // cannot register (happens with the on-screen keyboard and synthetic key events).
      if (!e.code && e.key !== "PrintScreen") return;

      parts.push(e.code || e.key);
      // Stop listening BEFORE saving, so a failed save does not leave the field waiting for
      // keys forever.
      setRecording(false);
      onChange(parts.join("+"));
    };

    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    return () => { window.removeEventListener("keydown", onKey, true); window.removeEventListener("keyup", onKey, true); };
  }, [recording, onChange]);

  useEffect(() => {
    if (!recording) return;
    const onClickOutside = (e: MouseEvent) => {
      if (!boxRef.current?.contains(e.target as Node)) setRecording(false);
    };
    window.addEventListener("mousedown", onClickOutside);
    return () => window.removeEventListener("mousedown", onClickOutside);
  }, [recording]);

  const parts = partesDeAtajo(value);

  return (
    <button
      ref={boxRef}
      type="button"
      onClick={() => setRecording((r) => !r)}
      title={
        recording
          ? t("Press the combination · the ones Windows keeps never reach here")
          : t("Click to change the shortcut")
      }
      className={`flex h-8 min-w-[132px] items-center justify-end gap-1 rounded-lg border px-2 transition-colors ${
        recording
          ? "animate-pulse border-blue-500 bg-blue-500/10"
          : active
            ? "border-linea-fuerte bg-hueco hover:border-linea-viva"
            : "border-red-500/40 bg-red-500/10 hover:border-red-400"
      }`}
    >
      {recording ? (
        <span className="w-full text-center text-[13px] text-blue-300">{t("Press the keys…")}</span>
      ) : (
        parts.map((part, index) => (
          <kbd
            key={`${part}-${index}`}
            className="rounded-[5px] border border-linea-fuerte bg-realce px-1.5 py-0.5 text-[13px] leading-none font-medium text-texto"
          >
            {part}
          </kbd>
        ))
      )}
    </button>
  );
}
