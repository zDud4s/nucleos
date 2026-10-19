import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { Toast } from "../data/desktop-notify";

/**
 * Whether the page is running inside the desktop host.
 *
 * Read off the global Tauri sets rather than imported from `@tauri-apps/api/core`: this answer
 * gates a side effect, and a test that mocks `core` for its own reasons must not have to know it.
 */
export function inTauri(): boolean {
  return (globalThis as { isTauri?: boolean }).isTauri === true;
}

/** Asked once per run: the operating system remembers the answer, and nagging helps nobody. */
let permission: Promise<boolean> | null = null;

function allowed(): Promise<boolean> {
  permission ??= (async () => {
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === "granted";
  })();
  return permission;
}

/**
 * Shows one toast through the operating system. Outside the desktop host (the browser preview, a
 * test) it does nothing.
 *
 * A failure is swallowed: a notification is a nudge, and the thing it nudges about is on screen
 * the moment the window opens. The permission is asked again after a failure, so a plugin that was
 * not ready on the first call is not written off for the whole run.
 */
export async function sendDesktopToast(toast: Toast): Promise<void> {
  if (!inTauri()) return;
  try {
    if (!(await allowed())) return;
    sendNotification({ title: toast.title, body: toast.body });
  } catch {
    permission = null;
  }
}
