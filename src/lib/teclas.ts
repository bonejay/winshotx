/**
 * From Tauri shortcut names to readable labels. Shortcuts are stored with physical key codes
 * (`CmdOrCtrl+Shift+KeyS`) because those do not change with the keyboard layout; the UI shows
 * `Ctrl Shift S`.
 */
const BONITOS: Record<string, string> = {
  CmdOrCtrl: "Ctrl",
  CommandOrControl: "Ctrl",
  Control: "Ctrl",
  Super: "Win",
  Meta: "Win",
  Escape: "Esc",
  Space: "Space",
  PrintScreen: "Print Screen",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Backslash: "\\",
  Slash: "/",
  Comma: ",",
  Period: ".",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
};

export function nombreDeTecla(code: string): string {
  if (code.startsWith("Digit")) return code.slice(5);
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Numpad")) return `Num ${code.slice(6)}`;
  return BONITOS[code] ?? code;
}

export function partesDeAtajo(atajo: string): string[] {
  return atajo.split("+").filter(Boolean).map(nombreDeTecla);
}
