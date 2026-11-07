/**
 * Browser stand-in for `@tauri-apps/api/event`: nothing is ever delivered, nothing fails.
 *
 * Every event the shell listens for is emitted by the host, and each is classified here (task 5.3):
 * - `files://drag-enter|drag-leave|dropped`: replaced by the page's DOM drop and the File API
 *   (pages/Files.tsx); ContextRefs cannot take a drop remotely and says so.
 * - `voice://start|stop`: global dictation chords, host-only; the Voice page's in-page shortcuts and
 *   the capture buttons answer instead (task 5.2).
 * - `voice://conversation-toggle`: the global conversation chord, host-only. Nothing waits on it: the
 *   Voice page's own button starts a conversation.
 * - kill-switch engage: the tray item, host-only. State is read from the daemon (`useKillSwitch`),
 *   and Ctrl+Alt+K and the button are page-level, so nothing waits on the event.
 * - notch mode: the notch is a host window; exception.
 * - `browser://*`: host webviews (task 5.5); exception.
 * - window moved/resized (screen-line): a host window; exception.
 * Anything that changes shared state is read from the daemon's queries and fleet stream, not here.
 */
export type UnlistenFn = () => void;

export interface Event<T> {
  event: string;
  id: number;
  payload: T;
}

const noop: UnlistenFn = () => {};

export function listen<T>(
  _event: string,
  _handler: (event: Event<T>) => void,
  _options?: unknown,
): Promise<UnlistenFn> {
  return Promise.resolve(noop);
}

export function once<T>(
  _event: string,
  _handler: (event: Event<T>) => void,
  _options?: unknown,
): Promise<UnlistenFn> {
  return Promise.resolve(noop);
}

export function emit(_event: string, _payload?: unknown): Promise<void> {
  return Promise.resolve();
}
