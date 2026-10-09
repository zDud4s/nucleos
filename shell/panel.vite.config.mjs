import { fileURLToPath } from "node:url";
import { defineConfig, mergeConfig } from "vite";

import appConfig from "./vite.config.ts";

/**
 * The browser panel's build: one IIFE the sidecar embeds and injects into the
 * page's isolated world. Same Tauri aliases as the preview, for the same reason:
 * the chat components it reuses import `invoke`, which must not reach a page.
 */
export default defineConfig(async (env) => {
  const base = typeof appConfig === "function" ? await appConfig(env) : appConfig;

  return mergeConfig(base, {
    resolve: {
      alias: {
        "@tauri-apps/api/core": fileURLToPath(new URL("./src/preview/tauri.ts", import.meta.url)),
        "@tauri-apps/api/event": fileURLToPath(
          new URL("./src/preview/tauri-event.ts", import.meta.url),
        ),
      },
    },
    define: { "process.env.NODE_ENV": '"production"' },
    build: {
      outDir: "../sidecars/browser/panelui",
      emptyOutDir: false,
      lib: {
        entry: fileURLToPath(new URL("./src/panel/main.tsx", import.meta.url)),
        formats: ["iife"],
        name: "NucleosPanel",
        fileName: () => "panel.js",
      },
    },
  });
});
