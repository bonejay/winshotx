/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// The version is written once, in package.json.
const paquete = JSON.parse(readFileSync(resolve(__dirname, "package.json"), "utf8"));

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: { __VERSION__: JSON.stringify(paquete.version) },
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  // Frontend tests run on the same Vite config as the app, with the same aliases,
  // `__VERSION__`, and Tailwind.
  test: {
    environment: "happy-dom",
    globals: true,
    setupFiles: ["./src/test/preparar.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
  build: {
    target: "chrome110",
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "overlay.html"),
        recorder: resolve(__dirname, "recorder.html"),
        cuenta: resolve(__dirname, "cuenta.html"),
      },
    },
  },
});
