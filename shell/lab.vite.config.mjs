import { fileURLToPath } from "node:url";
import { defineConfig, mergeConfig } from "vite";

import appConfig from "./vite.config.ts";

/**
 * The sidebar lab's build — the app's own config, with one entry swapped and
 * somewhere else to land.
 *
 * It imports `vite.config.ts` rather than restating it, for the reason
 * `preview.vite.config.mjs` and `csp-gate.vite.config.mjs` both give: the claim
 * being made is *this is what the app's document does to a pasted component*, so
 * a plugin the app has and this does not would make the answer a picture of
 * somewhere else. In particular `@tailwindcss/vite` has to be the same one, or
 * the whole point — which classes survive the cleared namespaces in
 * `tailwind.css` — is measured against the wrong stylesheet.
 *
 * Unlike the preview's, nothing is aliased away. The lab mounts no router, calls
 * no `invoke` and talks to no daemon; it renders one component twice.
 *
 * `dist-lab/` and never `dist/`: `tauri::generate_context!()` embeds `dist/` at
 * compile time, so a build that wrote there would replace the app inside the next
 * binary with a bench.
 *
 * Building is optional — `npm run dev` serves `sidebar-lab.html` directly, which
 * is the normal way to look at it. This exists so the audit is reproducible: the
 * nine dead classes listed in `src/lab/prompt-design.css` were found by building
 * with that file's import commented out and grepping the emitted CSS for each of
 * the 195 class tokens in the paste. Re-run it after any change to `tailwind.css`
 * and the list can be checked rather than believed.
 */
export default defineConfig(async (env) => {
  const base = typeof appConfig === "function" ? await appConfig(env) : appConfig;

  return mergeConfig(base, {
    build: {
      outDir: "dist-lab",
      emptyOutDir: true,
      rollupOptions: {
        input: fileURLToPath(new URL("./sidebar-lab.html", import.meta.url)),
      },
    },
  });
});
