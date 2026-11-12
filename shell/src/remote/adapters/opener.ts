import { HostOnlyError } from "./core";

/** Browser stand-in for `@tauri-apps/plugin-opener`. Only http(s) URLs can be opened, in a new tab. */
export function openUrl(url: string | URL, _openWith?: string): Promise<void> {
  const text = String(url);
  let protocol = "";
  try {
    protocol = new URL(text).protocol;
  } catch {
    // Unparseable: falls through to the rejection below.
  }
  if (protocol !== "http:" && protocol !== "https:") {
    return Promise.reject(new HostOnlyError("openUrl is unavailable remotely for non-http(s) URLs"));
  }
  window.open(text, "_blank", "noopener,noreferrer");
  return Promise.resolve();
}

export function openPath(_path: string, _openWith?: string): Promise<void> {
  return Promise.reject(new HostOnlyError("openPath is unavailable remotely"));
}

export function revealItemInDir(_path: string | string[]): Promise<void> {
  return Promise.reject(new HostOnlyError("revealItemInDir is unavailable remotely"));
}
