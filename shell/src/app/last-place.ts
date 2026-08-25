/**
 * Where the window was when it was last closed.
 *
 * The router deliberately keeps its location in memory (see `router.tsx`): this is a desktop window
 * with no address bar, served from a `tauri://` origin whose path is the bundle rather than the
 * route, so a browser history would make the app's position a property of a URL nobody can see and
 * one stray reload would land on a path the bundler never emitted a file for.
 *
 * All of that stays true. What it never justified was STARTING AT `/` EVERY TIME. A route held in
 * memory is state, and state a desktop app throws away on every launch is state the person has to
 * rebuild by hand — you close the window in the middle of a conversation and come back to the Home
 * page, with the conversation somewhere in a list.
 *
 * So the location is remembered here and handed back as the router's first entry. `localStorage`
 * and not the daemon: this is a property of THIS WINDOW on THIS MACHINE, it must be readable
 * before the first request, and a daemon that is still starting up must not be able to make the app
 * open somewhere unexpected.
 */

const KEY = "nucleos.last-place";

/**
 * The paths a remembered location is allowed to be.
 *
 * Checked rather than trusted, because this string is read from storage a person can edit and is
 * handed straight to the router as a path. The rule is narrow on purpose — an absolute path, no
 * scheme, no host, no `..` — and anything else falls back to Home rather than being repaired.
 * A remembered place is a convenience; there is no version of this worth a broken launch.
 */
function looksLikeAPath(value: string): boolean {
  if (!value.startsWith("/")) return false;
  if (value.startsWith("//")) return false;
  if (value.includes("..")) return false;
  if (value.length > 512) return false;
  return true;
}

/**
 * Where to open, or `/` when there is nothing sensible to reopen.
 *
 * A storage that throws — private mode, a locked-down webview, a quota — is not an error path here.
 * The whole feature is a convenience, and an app that refuses to start because it could not
 * remember where it was would be trading something that matters for something that does not.
 */
export function lastPlace(): string {
  try {
    const stored = window.localStorage.getItem(KEY);
    if (stored === null || !looksLikeAPath(stored)) return "/";
    return stored;
  } catch {
    return "/";
  }
}

/** Remembers where the window is now. Silent on a storage that refuses, for the reason above. */
export function rememberPlace(path: string): void {
  try {
    if (looksLikeAPath(path)) window.localStorage.setItem(KEY, path);
  } catch {
    // Nothing to do and nothing worth saying: the next launch opens on Home, which is where it
    // opened before this file existed.
  }
}
