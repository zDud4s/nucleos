import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * The two Content-Security-Policies, read as the configuration rather than as a running page.
 *
 * `scripts/csp-gate.mjs` is the real measurement and this is not trying to be it: a policy is
 * enforced by an engine, and only a browser can say what one refuses. What a browser cannot say
 * anything about is `devCsp`, because the gate builds the app and runs it under the shipped `csp` —
 * so the policy that `tauri dev` serves, which is the one somebody develops against all day, has no
 * measurement anywhere. This file is the cheap always-on half: it reads both policies and asserts
 * the one property that was found missing by spending a session on its consequences.
 */
/* From the runner's root — `shell/` — and not from `import.meta.url`, which in a transformed module
   is not a `file:` URL and cannot be turned into a path. */
const CONFIG = JSON.parse(
  readFileSync(resolve(process.cwd(), "src-tauri/tauri.conf.json"), "utf8"),
) as { app: { security: { csp: Record<string, string>; devCsp: Record<string, string> } } };

const POLICIES: [string, Record<string, string>][] = [
  ["csp", CONFIG.app.security.csp],
  ["devCsp", CONFIG.app.security.devCsp],
];

describe.each(POLICIES)("the %s policy", (_name, policy) => {
  /**
   * Silero VAD runs in onnxruntime-web, which is WebAssembly, and Chromium — which is what WebView2
   * is — refuses to compile any WebAssembly at all unless `script-src` grants it. Measured on
   * 2026-09-19 in headless Chrome under this repository's own policy:
   *
   * > `CompileError: WebAssembly.instantiate(): Compiling or instantiating WebAssembly module
   * > violates the following Content Security policy directive because 'unsafe-eval' is not an
   * > allowed source of script in the following Content Security Policy directive: "script-src
   * > 'self'"`
   *
   * With `'wasm-unsafe-eval'` added and nothing else changed, the same page loaded the same model and
   * scored the same silence at 0.0017.
   *
   * The cost of not having this is what makes it worth a test: `loadSileroSession` falls back to an
   * energy threshold, the app keeps working, and the only sign is one line of status text saying it
   * is listening by loudness. A fan, a fridge and a hard drive all clear an energy threshold.
   *
   * `'wasm-unsafe-eval'` and NOT `'unsafe-eval'`: the narrow one permits WebAssembly compilation and
   * nothing else, where the broad one would hand `eval` back to every script on the page.
   */
  it("lets the app compile WebAssembly, which is what Silero VAD is", () => {
    const scriptSrc = policy["script-src"].split(/\s+/);

    expect(scriptSrc).toContain("'wasm-unsafe-eval'");
    expect(scriptSrc).not.toContain("'unsafe-eval'");
  });
});
