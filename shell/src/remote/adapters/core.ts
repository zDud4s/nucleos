/**
 * Browser stand-in for `@tauri-apps/api/core`. Every host command is unavailable remotely.
 */
export class HostOnlyError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HostOnlyError";
  }
}

/** Rejects: there is no desktop host behind a browser tab. */
export function invoke<T = unknown>(command: string, _args?: unknown, _options?: unknown): Promise<T> {
  return Promise.reject(new HostOnlyError(`${command} is unavailable remotely`));
}

/** Always false: the remote page is never inside the desktop host. */
export function isTauri(): boolean {
  return false;
}
