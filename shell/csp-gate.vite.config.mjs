import { fileURLToPath } from "node:url";
import { defineConfig, mergeConfig } from "vite";

import appConfig from "./vite.config.ts";

/**
 * The CSP gate's build — the app's own config, with one entry swapped and somewhere else to land.
 *
 * **It imports `vite.config.ts` rather than restating it**, and that is the whole point of the
 * file: the gate's claim is *this is what ships*, so a plugin the app gains and the gate does not
 * would make the claim false without anything failing. Vite bundles a config's imports with
 * esbuild before running it, so the `.ts` is read directly — which also steps around the trap
 * `vite.config.ts` warns about at the top of itself, since nothing emits a `.js` beside THIS file.
 *
 * `dist-csp-gate/` and never `dist/`: `tauri::generate_context!()` embeds `dist/` at compile time,
 * so a build that wrote there would replace the app inside the next binary with a test page.
 *
 * Run from `shell/`, so `root` is `shell/` and `/src/csp-gate/main.tsx` in the HTML resolves the
 * same way `/src/main.tsx` does for the app.
 */
export default defineConfig(async (env) => {
  const base = typeof appConfig === "function" ? await appConfig(env) : appConfig;

  return mergeConfig(base, {
    build: {
      outDir: "dist-csp-gate",
      emptyOutDir: true,
      rollupOptions: {
        input: fileURLToPath(new URL("./csp-gate.html", import.meta.url)),
      },
    },
  });
});
