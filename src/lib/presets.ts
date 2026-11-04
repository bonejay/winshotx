import type { EncodingSettings } from "./types";

/** Short description of a preset (or the current settings), as shown in the capture bar. */
export function presetSummary(p: { fps: number; encoding?: EncodingSettings }): string {
  const e = p.encoding;
  const encoder = e?.encoder ?? "libx264";
  const quality = e?.rateControl === "bitrate" ? `${e.bitrateMbps} Mbps` : `q ${e?.crf ?? 20}`;
  return `${p.fps} fps · ${encoder} · ${quality}`;
}
