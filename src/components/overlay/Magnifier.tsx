import { useEffect, useRef } from "react";
import { clamp } from "../../lib/format";

const SIZE = 88; // box size in CSS px
const ZOOM = 4; // each physical pixel is drawn as 4x4

interface Props {
  /** Offscreen canvas with the monitor's frozen image at physical resolution. */
  source: HTMLCanvasElement | null;
  /** Point under the cursor, in the monitor's physical pixels. */
  px: number;
  py: number;
  /** Box corner in overlay CSS coordinates. */
  left: number;
  top: number;
  hex: string;
}

export function Magnifier({ source, px, py, left, top, hex }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !source) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const span = Math.floor(SIZE / ZOOM); // visible physical pixels
    const half = Math.floor(span / 2);
    // Near an edge the source box stays inside the image; otherwise the browser clips source
    // and destination together and the sampled pixel no longer matches the crosshair.
    const sx = clamp(Math.round(px) - half, 0, Math.max(0, source.width - span));
    const sy = clamp(Math.round(py) - half, 0, Math.max(0, source.height - span));
    ctx.imageSmoothingEnabled = false;
    ctx.clearRect(0, 0, SIZE, SIZE);
    ctx.drawImage(source, sx, sy, span, span, 0, 0, SIZE, SIZE);

    // Outline the exact pixel being sampled.
    ctx.strokeStyle = "#3b82f6";
    ctx.lineWidth = 2;
    ctx.strokeRect((Math.round(px) - sx) * ZOOM, (Math.round(py) - sy) * ZOOM, ZOOM, ZOOM);
  }, [source, px, py]);

  return (
    <div
      style={{ left, top }}
      className="pointer-events-none absolute z-40 overflow-hidden rounded-lg border border-white/15 bg-neutral-900/90 shadow-2xl backdrop-blur-md"
    >
      <canvas ref={canvasRef} width={SIZE} height={SIZE} className="block" />
      <div className="flex items-center justify-between gap-2 border-t border-white/10 px-1.5 py-1 font-mono text-[10px] tabular-nums text-neutral-300">
        <span>
          {Math.round(px)}, {Math.round(py)}
        </span>
        <span className="flex items-center gap-1.5 uppercase">
          <span
            className="size-2.5 rounded-[3px] border border-white/20"
            style={{ backgroundColor: hex }}
          />
          {hex}
          {/* The copy-color key, shown next to the color. */}
          <kbd className="rounded border border-white/15 bg-white/10 px-1 text-[9px] text-neutral-400">
            C
          </kbd>
        </span>
      </div>
    </div>
  );
}
