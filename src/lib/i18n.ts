/** Replaces {name} placeholders in a UI string. */
export function t(text: string, vars?: Record<string, string | number>): string {
  return Object.entries(vars ?? {}).reduce((value, [key, replacement]) =>
    value.split(`{${key}}`).join(String(replacement)), text);
}
export function useT() { return t; }
