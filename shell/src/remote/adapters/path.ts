import { HostOnlyError } from "./core";

/** Browser stand-in for `@tauri-apps/api/path`: the host's directories are not reachable. */
export function localDataDir(): Promise<string> {
  return Promise.reject(new HostOnlyError("localDataDir is unavailable remotely"));
}
