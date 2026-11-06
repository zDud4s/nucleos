import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import type { Toast } from "../data/desktop-notify";
import { isRemote } from "../remote/host";

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
    // A browser only lets a click ask; a prompt out of the blue is refused or ignored, and it is
    // rude. So a remote tab uses what a click in System already got, and never asks here.
    if (isRemote()) return false;
    return (await requestPermission()) === "granted";
  })();
  return permission;
}

/**
 * Shows one toast through the operating system, or through the browser's Web Notifications on a
 * remote device. Elsewhere (the browser preview, a test) it does nothing.
 *
 * Without permission, or without the API, nothing is shown and nothing fails: the in-app prompt
 * the notifier renders is the notice. With no tab open there is no page to notify from; that case
 * is the channels' (Telegram, mail), not this one's.
 *
 * A failure is swallowed: a notification is a nudge, and the thing it nudges about is on screen
 * the moment the window opens. The permission is asked again after a failure, so a plugin that was
 * not ready on the first call is not written off for the whole run.
 */
export async function sendDesktopToast(toast: Toast): Promise<void> {
  if (!inTauri() && !isRemote()) return;
  try {
    // A remote answer can change while the tab lives (the person clicks Allow), so it is not cached.
    if (isRemote()) permission = null;
    if (!(await allowed())) return;
    // `to` is read only by the browser stand-in; the desktop call stays exactly as it was.
    sendNotification(
      isRemote()
        ? ({ title: toast.title, body: toast.body, to: toast.to } as Parameters<typeof sendNotification>[0])
        : { title: toast.title, body: toast.body },
    );
  } catch {
    permission = null;
  }
}

/** What this browser has decided about notifications, for the button that asks. */
export type BrowserNotificationState = "granted" | "denied" | "default" | "unsupported";

export function browserNotificationState(): BrowserNotificationState {
  if (!isRemote() || typeof Notification === "undefined") return "unsupported";
  return Notification.permission;
}

/**
 * Asks the browser for notification permission. Call it only from a click handler. Resolves to the
 * answer and never rejects: a denial or a missing API is an answer, not an error.
 */
export async function askBrowserNotifications(): Promise<BrowserNotificationState> {
  if (!isRemote()) return "unsupported";
  try {
    const answer = await requestPermission();
    permission = null;
    return answer;
  } catch {
    return "denied";
  }
}
