import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// `@types/node` came in with the alias below, so `process` is typed now and the
// `@ts-expect-error` this line used to carry would itself be an error.
const host = process.env.TAURI_DEV_HOST;

/**
 * WARNING — this file is not the one Vite reads.
 *
 * `tsconfig.node.json` is a composite project that includes this file and does
 * not disable emit, so `tsc -b` writes a compiled `vite.config.js` beside it.
 * Vite resolves `vite.config.js` BEFORE `vite.config.ts`, which means the
 * compiled artifact wins. Editing this file and not running `tsc -b` leaves the
 * change out of the build, silently and with no error to read.
 *
 * `npx tsc -b` is already the shell's half of the gate, so the normal loop keeps
 * the two in step. It is only worth knowing about when a config change appears
 * to do nothing at all.
 */

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    react({
      babel: { plugins: [["babel-plugin-react-compiler", { target: "19" }]] },
    }),
    tailwindcss(),
  ],

  optimizeDeps: {
    /**
     * onnxruntime-web, unbundled, because pre-bundling breaks the URL of its own `.wasm`.
     *
     * The runtime locates its binary with `new URL("ort-wasm-simd-threaded.wasm", import.meta.url)`.
     * Pre-bundled, `import.meta.url` is the optimizer's output directory, so the URL becomes
     * `/node_modules/.vite/deps/ort-wasm-simd-threaded.wasm` — and the optimizer copies the `.mjs`
     * glue there without the binary beside it. Measured against this repo's own dev server on
     * 2026-09-19: that URL answers 200 with `text/html`, the SPA fallback, so the runtime is handed
     * `index.html` where it expected WebAssembly.
     *
     * The failure was invisible for exactly as long as `loadSileroSession` had an empty `catch`: the
     * app fell back to an energy threshold and said it was listening by loudness. Excluded here, Vite
     * serves the package's own file from `node_modules`, where the `.wasm` is.
     *
     * Development only — `vite build` emits the binary as an asset and rewrites the URL to it.
     */
    exclude: ["onnxruntime-web"],
  },

  resolve: {
    alias: {
      // What the registries' generated code imports by. Kept in step with the
      // `paths` entry in tsconfig.json — TypeScript resolves one, Vite the other,
      // and a rename that touches only one of them typechecks and then fails to
      // bundle.
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
