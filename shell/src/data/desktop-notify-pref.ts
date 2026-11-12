import { useCallback, useSyncExternalStore } from "react";

/**
 * Whether this computer shows desktop toasts — the owner's switch in System → Notifications.
 *
 * A shell preference and not a daemon setting: it is about THIS machine's window, and the daemon
 * has no part in a toast. Stored in the webview's `localStorage`; on by default, so an unreadable
 * or missing value means on — the same direction the notify policy fails in.
 */
const KEY = "nucleos.desktopNotify";

/** Same-window changes do not fire `storage`, so a write announces itself. */
const CHANGED = "nucleos:desktop-notify";

export function readDesktopNotify(): boolean {
  try {
    return localStorage.getItem(KEY) !== "false";
  } catch {
    return true;
  }
}

export function writeDesktopNotify(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? "true" : "false");
  } catch {
    // Storage can be blocked; the switch then lasts until the page reloads, which is all it can do.
  }
  window.dispatchEvent(new Event(CHANGED));
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener(CHANGED, onChange);
  window.addEventListener("storage", onChange);
  return () => {
    window.removeEventListener(CHANGED, onChange);
    window.removeEventListener("storage", onChange);
  };
}

/** The switch, kept current for every component that reads it. */
export function useDesktopNotify(): [boolean, (on: boolean) => void] {
  const on = useSyncExternalStore(subscribe, readDesktopNotify, () => true);
  const set = useCallback((next: boolean) => writeDesktopNotify(next), []);
  return [on, set];
}
