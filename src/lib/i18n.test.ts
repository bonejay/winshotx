import { describe, expect, it } from "vitest";
import { aplicarIdioma, idiomaActivo, t } from "./i18n";
describe("English-only interface", () => {
  it("migrates every legacy language selection to English", () => {
    for (const language of ["es", "zh", "sistema", "en"] as const) {
      aplicarIdioma(language);
      expect(idiomaActivo()).toBe("en");
      expect(document.documentElement.lang).toBe("en");
      expect(localStorage.getItem("winshotx.idioma")).toBe("en");
    }
  });
  it("keeps English text and interpolates values", () => {
    expect(t("Frame {frame} of {total}", { frame: 3, total: 8 })).toBe("Frame 3 of 8");
    expect(t("Capture")).toBe("Capture");
  });
});
