import { HostOnlyError } from "./core";

/** Browser stand-in for `@tauri-apps/plugin-process`. */
export function relaunch(): Promise<void> {
  return Promise.reject(new HostOnlyError("relaunch is unavailable remotely"));
}
