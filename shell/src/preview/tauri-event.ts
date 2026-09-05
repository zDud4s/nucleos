/**
 * `@tauri-apps/api/event`, for a page that is not inside Tauri.
 *
 * The sibling of `tauri.ts` and aliased the same way, for the same reason and
 * against the same failure. `preview.vite.config.mjs` used to alias
 * `@tauri-apps/api/core` alone, and the gap was invisible for as long as nobody
 * opened one of the three pages that listen: `Voice.tsx`, `Files.tsx` and
 * `data/conversation.ts` all call `listen()` from an effect, and outside Tauri
 * that reaches for `window.__TAURI_INTERNALS__.transformCallback`, which is not
 * there. The throw happens in a passive effect, so React's boundary catches it
 * and the whole window becomes "Something went wrong!" — a page that renders
 * two lines of apology has not been previewed, it has been missed.
 *
 * What it does is nothing, and that is the honest answer rather than a
 * convenience: these events are pushed by the Rust side — a transcription
 * arriving, a file changing under a watcher — and the preview has no Rust side.
 * A fixture that invented them would photograph a núcleo saying things no
 * daemon said. The pages are built to render the state before the first event,
 * and that state is exactly what is worth looking at here.
 *
 * The unlisten is a real function and not `undefined`: every caller stores it
 * and calls it on cleanup, and a promise resolving to nothing would turn a
 * tidy unmount into a second boundary.
 */

/** What `listen` hands back — the same shape the real module promises. */
export type UnlistenFn = () => void;

export async function listen<T>(
  _event: string,
  _handler: (event: { event: string; id: number; payload: T }) => void,
): Promise<UnlistenFn> {
  return () => {
    /* Nothing was ever subscribed, so there is nothing to undo. */
  };
}

/**
 * `once`, for completeness rather than for a caller.
 *
 * Nothing in `src/` uses it today. It is here because the alias replaces the
 * whole module: an import of `once` would fail to resolve at build time and
 * take the preview bundle down with it, which is a worse way to find out than
 * this.
 */
export async function once<T>(
  event: string,
  handler: (event: { event: string; id: number; payload: T }) => void,
): Promise<UnlistenFn> {
  return listen(event, handler);
}

/**
 * `emit`, refusing rather than pretending.
 *
 * `invoke` in `tauri.ts` throws for every command it does not know, and this
 * follows it: a preview that silently swallowed an emit would let a page look
 * as though it had told the núcleo something.
 */
export async function emit(event: string): Promise<void> {
  throw new Error(`the preview has no núcleo to emit("${event}") to`);
}
