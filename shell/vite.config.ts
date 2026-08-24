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
