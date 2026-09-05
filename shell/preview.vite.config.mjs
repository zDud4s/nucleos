import { fileURLToPath } from "node:url";
import { defineConfig, mergeConfig } from "vite";

import appConfig from "./vite.config.ts";

/**
 * The preview's build — the app's own config, with one entry swapped, somewhere
 * else to land, and Tauri's `invoke` aliased away.
 *
 * It imports `vite.config.ts` rather than restating it, for the reason
 * `csp-gate.vite.config.mjs` gives: the claim being made is *this is what
 * ships*, so a plugin the app gains and this does not would make the picture a
 * picture of something else.
 *
 * `dist-preview/` and never `dist/`: `tauri::generate_context!()` embeds `dist/`
 * at compile time, so a build that wrote there would replace the app inside the
 * next binary with a page full of fixtures.
 */
export default defineConfig(async (env) => {
  const base = typeof appConfig === "function" ? await appConfig(env) : appConfig;

  return mergeConfig(base, {
    resolve: {
      alias: {
        // The one `invoke` on the read path. Aliased rather than patched: it is
        // a named import resolved at build time, so there is no global to reach.
        "@tauri-apps/api/core": fileURLToPath(new URL("./src/preview/tauri.ts", import.meta.url)),
        // And the events beside it. Three pages call `listen()` from an effect —
        // Voice, Files, and the chat conversation — and outside Tauri that
        // throws inside a passive effect, which React's boundary turns into
        // "Something went wrong!" over the whole window. Both halves of the
        // bridge have to be stubbed or the preview photographs an apology.
        "@tauri-apps/api/event": fileURLToPath(
          new URL("./src/preview/tauri-event.ts", import.meta.url),
        ),
      },
    },
    build: {
      outDir: "dist-preview",
      emptyOutDir: true,
      rollupOptions: {
        input: fileURLToPath(new URL("./preview.html", import.meta.url)),
      },
    },
  });
});
