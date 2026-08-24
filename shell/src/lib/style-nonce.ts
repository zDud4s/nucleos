import { setNonce } from "get-nonce";

/**
 * Handing the window's per-load style nonce to the libraries that inject stylesheets.
 *
 * # The problem this solves, which was invisible until a gate looked for it
 *
 * This app runs under `style-src 'self'` with no `unsafe-inline`. Several dependencies lock
 * scrolling or size a scrollbar gutter by building a `<style>` element at runtime — Radix's modal
 * dialog and dropdown do it through `react-remove-scroll`, and the ⌘K palette does it through the
 * dialog it lives in. `style-src-elem` refuses a `<style>` whether markup or a script made it, so
 * every one of those is refused in a packaged build.
 *
 * **And only in a packaged build.** `tauri.conf.json`'s `devCsp` carries `style-src
 * 'unsafe-inline'`, necessarily, because Vite serves CSS in development the same way. So this
 * shipped working perfectly every day and broken every install, and nothing said so until
 * `scripts/csp-gate.mjs` ran the real bundle under the real policy.
 *
 * # Why a nonce, and not the two easier answers
 *
 * Adding `'unsafe-inline'` to the production policy would fix it and would be the end of the
 * fence: `style-src` would then permit anything that reaches the DOM. Pinning hashes would not
 * work at all — the CSS these libraries emit contains the measured scrollbar width, so it differs
 * between machines.
 *
 * A nonce costs nothing, because Tauri already mints one per page load. At compile time it stamps
 * `nonce="…"` on every `<style>` element in the HTML; at serve time it replaces each stamp with a
 * fresh random value **and adds `'nonce-<value>'` to the `style-src` it sends**. That is a
 * capability the window already has and was not using.
 *
 * # Why `index.html` carries an empty `<style>`
 *
 * The nonce is only reachable through a tag Tauri stamped, and a Vite build emits its CSS as a
 * `<link>` rather than a `<style>` — so without one there is nothing stamped and nothing to read.
 * The empty element in `index.html` exists to be that channel and nothing else. It is documented
 * where it sits, because an empty `<style>` is exactly the kind of thing somebody tidies away.
 *
 * Nothing here weakens anything: a value that changes every load and is never written down cannot
 * be used by an attacker who could not already run script, and `script-src 'self'` is what stops
 * that.
 */

/**
 * Read the nonce off the tag Tauri stamped and give it to `get-nonce`.
 *
 * Called before React mounts, from both entries, because the first modal can open before any
 * effect this could otherwise hang off has run.
 *
 * Returns what it found, so the one test that can be written without a webview — *did we read the
 * attribute the way the browser exposes it* — has something to assert on. `null` means no stamped
 * tag, which is what a plain `vite dev` looks like and is not an error: there is no nonce in that
 * policy either, and `devCsp` allows the styles outright.
 */
export function adoptStyleNonce(doc: Document = document): string | null {
  const stamped = doc.querySelector("style[nonce]");
  if (stamped === null) return null;
  /*
    `.nonce` before `getAttribute`, and that order is load-bearing. Browsers implement "nonce
    hiding": once a document has a nonce-carrying CSP, the content attribute is blanked so that a
    CSS selector cannot exfiltrate the value, while the IDL property keeps it. Reading the
    attribute alone gets an empty string in exactly the situation this function exists for.
  */
  const nonce = (stamped as HTMLStyleElement).nonce || stamped.getAttribute("nonce") || "";
  if (nonce === "") return null;
  setNonce(nonce);
  return nonce;
}
