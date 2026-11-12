/**
 * Browser stand-in for `@tauri-apps/plugin-updater`.
 *
 * `check` resolves null ("no update"). data/updater.ts already gates on `isTauri()` (false here), so
 * `check` is normally never reached; resolving null instead of rejecting keeps the UI quiet if it is.
 */
export interface Update {
  version: string;
  download(): Promise<void>;
  install(): Promise<void>;
}

export function check(_options?: unknown): Promise<Update | null> {
  return Promise.resolve(null);
}
