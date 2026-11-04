interface Props {
  /** Already in physical pixels, which is what the user expects to read. */
  width: number;
  height: number;
  /** Position in overlay CSS coordinates. */
  left: number;
  top: number;
  editable?: boolean;
}

export function DimensionBadge({ width, height, left, top }: Props) {
  return (
    <div
      style={{ left, top }}
      className="pointer-events-none absolute z-30 rounded-md border border-white/10 bg-neutral-900/90 px-2 py-1 font-mono text-[13px] tabular-nums text-white shadow-lg backdrop-blur-md"
    >
      {Math.round(width)} × {Math.round(height)}
    </div>
  );
}
