import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, mergeConfig } from "vite";

import appConfig from "./vite.config.ts";

const adapter = (file) => fileURLToPath(new URL(`./src/remote/adapters/${file}.ts`, import.meta.url));

/**
 * The browser build served by the remote gateway: the app's own config with the entry swapped,
 * somewhere else to land, and every Tauri module replaced by a browser adapter.
 *
 * It merges over `vite.config.ts` rather than restating it, for the reason `preview.vite.config.mjs`
 * gives: what is served must be what ships.
 *
 * `dist-remote/` and never `dist/`: `tauri::generate_context!()` embeds `dist/` at compile time, so
 * a build that wrote there would put the browser build inside the next desktop binary.
 *
 * The gateway serves `index.html` (it substitutes the CSP nonce in that file only), but Vite names
 * the output after the input, `remote.html`; the small plugin below renames it once written.
 */
function indexAsIndex(outDir) {
  return {
    name: "remote-index-name",
    apply: "build",
    closeBundle() {
      const from = path.resolve(outDir, "remote.html");
      if (fs.existsSync(from)) fs.renameSync(from, path.resolve(outDir, "index.html"));
    },
  };
}

export default defineConfig(async (env) => {
  const base = typeof appConfig === "function" ? await appConfig(env) : appConfig;
  const outDir = fileURLToPath(new URL("./dist-remote", import.meta.url));

  return mergeConfig(base, {
    plugins: [indexAsIndex(outDir)],
    resolve: {
      alias: {
        "@tauri-apps/api/core": adapter("core"),
        "@tauri-apps/api/event": adapter("event"),
        "@tauri-apps/api/window": adapter("window"),
        "@tauri-apps/api/path": adapter("path"),
        "@tauri-apps/plugin-opener": adapter("opener"),
        "@tauri-apps/plugin-notification": adapter("notification"),
        "@tauri-apps/plugin-dialog": adapter("dialog"),
        "@tauri-apps/plugin-process": adapter("process"),
        "@tauri-apps/plugin-updater": adapter("updater"),
      },
    },
    build: {
      outDir: "dist-remote",
      emptyOutDir: true,
      rollupOptions: {
        input: fileURLToPath(new URL("./remote.html", import.meta.url)),
      },
    },
  });
});
