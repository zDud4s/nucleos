/**
 * Browser stand-in for `@tauri-apps/plugin-notification`. Uses the Web Notifications API only when
 * the browser has already granted permission; everything else degrades silently.
 */
export type Permission = "granted" | "denied" | "default";

export interface Options {
  title: string;
  body?: string;
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
    new Notification(opts.title, opts.body === undefined ? undefined : { body: opts.body });
  } catch {
    // A notification is a nudge; failing to show one is not an error.
  }
}
