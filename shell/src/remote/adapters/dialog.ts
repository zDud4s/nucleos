import { HostOnlyError } from "./core";

/** Browser stand-in for `@tauri-apps/plugin-dialog`: native pickers do not exist remotely. */
export function open(_options?: unknown): Promise<string | string[] | null> {
  return Promise.reject(new HostOnlyError("open is unavailable remotely"));
}
