/**
 * Browser stand-in for `@tauri-apps/plugin-notification`. Uses the Web Notifications API only when
 * the browser has already granted permission; everything else degrades silently.
 *
 * `requestPermission` is only ever called from a click (System, Notifications): `sendNotification`
 * and `isPermissionGranted` never ask. Web Push is out of scope, so a closed tab shows nothing and
 * the channels carry the notice.
 */
export type Permission = "granted" | "denied" | "default";

export interface Options {
  title: string;
  body?: string;
  /** An in-app path to open when the notification is clicked. Not part of the Tauri options. */
  to?: string;
}

function supported(): boolean {
  return typeof Notification !== "undefined";
}

export function isPermissionGranted(): Promise<boolean> {
  return Promise.resolve(supported() && Notification.permission === "granted");
}

export async function requestPermission(): Promise<Permission> {
  if (!supported()) return "denied";
  try {
    return (await Notification.requestPermission()) as Permission;
  } catch {
    return "denied";
  }
}

export function sendNotification(options: Options | string): void {
  try {
    if (!supported() || Notification.permission !== "granted") return;
    const opts = typeof options === "string" ? { title: options } : options;
    const shown = new Notification(opts.title, opts.body === undefined ? undefined : { body: opts.body });
    // Acting on the notice brings the tab forward and goes where the toast points. Only an in-app
    // path is followed: never `//host` or a scheme, whatever the sender put in `to`.
    const route = opts.to;
    shown.onclick = () => {
      window.focus();
      if (route !== undefined && route.startsWith("/") && !route.startsWith("//")) {
        window.location.assign(route);
      }
      shown.close?.();
    };
  } catch {
    // A notification is a nudge; failing to show one is not an error.
  }
}
