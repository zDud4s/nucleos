/**
 * `@tauri-apps/api/core`, for a page that is not inside Tauri.
 *
 * The preview build aliases the real module to this one. There is exactly one
 * `invoke` on the app's read path — `get_daemon_token` in `data/client.ts` —
 * and without it every query fails as `ApiUnavailable("token")` and the window
 * shows the authorisation gate instead of the page being looked at.
 *
 * Aliasing rather than patching a global, because `invoke` is a named import
 * resolved at build time: there is no `window.invoke` to reach for.
 */
export async function invoke<T>(command: string): Promise<T> {
  if (command === "get_daemon_token") return "preview" as T;
  throw new Error(`the preview has no answer for invoke("${command}")`);
}
