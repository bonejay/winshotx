import { motion, useReducedMotion } from "framer-motion";
import type { ReactNode } from "react";

interface Props {
  children: ReactNode;
  className?: string;
  /** macOS-style entrance: a short scale and a 4px rise. */
  animate?: boolean;
}

export function GlassPanel({ children, className = "", animate = true }: Props) {
  // With reduced motion the panel neither scales nor fades; it simply appears.
  const quieto = useReducedMotion();
  const base =
    "rounded-2xl border border-linea-fuerte bg-flotante/90 backdrop-blur-xl shadow-2xl";
  if (!animate || quieto) return <div className={`${base} ${className}`}>{children}</div>;
  return (
    <motion.div
      initial={{ opacity: 0, scale: 0.92, y: 4 }}
      animate={{ opacity: 1, scale: 1, y: 0 }}
      exit={{ opacity: 0, scale: 0.96, y: 2 }}
      transition={{ type: "spring", stiffness: 520, damping: 34, mass: 0.6 }}
      className={`${base} ${className}`}
    >
      {children}
    </motion.div>
  );
}
