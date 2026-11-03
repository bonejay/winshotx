// Legacy language values are accepted so existing settings migrate without a reset.
export type Idioma = "sistema" | "es" | "en" | "zh";
export function aplicarIdioma(_language: Idioma) {
  document.documentElement.lang = "en";
  try { localStorage.setItem("winshotx.idioma", "en"); } catch { /* optional cache */ }
}
export function idiomaActivo(): "en" { return "en"; }
export function t(text: string, vars?: Record<string, string | number>): string {
  const translated = text;
  return Object.entries(vars ?? {}).reduce((value, [key, replacement]) =>
    value.split(`{${key}}`).join(String(replacement)), translated);
}
export function useT() { return t; }
aplicarIdioma("en");
