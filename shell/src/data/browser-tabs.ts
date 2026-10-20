// §spec pilar-de-browser
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * Thin typed wrapper over the native browser-tab commands and events the
 * Tauri shell exposes. No state lives here: one function per command, one
 * listener bundle for the `browser://` events.
 */

export type Role = "owner" | "agent";

export interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TabState {
  tabId: string;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  secure: boolean;
}

export interface NewWindow {
  fromTabId: string;
  url: string;
}

export interface ProcessFailed {
  tabId: string;
  kind: string;
}

export function envEnsure(project: string, role: Role): Promise<{ cdp_http: string | null }> {
  return invoke("browser_env_ensure", { project, role });
}

export function tabCreate(
  project: string,
  role: Role,
  tabId: string,
  url: string,
  bounds: Bounds,
): Promise<void> {
  return invoke("browser_tab_create", { project, role, tabId, url, bounds });
}

export function tabNavigate(tabId: string, url: string): Promise<void> {
  return invoke("browser_tab_navigate", { tabId, url });
}

export function tabBack(tabId: string): Promise<void> {
  return invoke("browser_tab_back", { tabId });
}

export function tabForward(tabId: string): Promise<void> {
  return invoke("browser_tab_forward", { tabId });
}

export function tabReload(tabId: string): Promise<void> {
  return invoke("browser_tab_reload", { tabId });
}

export function tabStop(tabId: string): Promise<void> {
  return invoke("browser_tab_stop", { tabId });
}

export function tabsShow(tabId: string | null, bounds: Bounds): Promise<void> {
  return invoke("browser_tabs_show", { tabId, bounds });
}

export function tabClose(tabId: string): Promise<void> {
  return invoke("browser_tab_close", { tabId });
}

export interface BrowserEventHandlers {
  onTabState?: (state: TabState) => void;
  onNewWindow?: (win: NewWindow) => void;
  onProcessFailed?: (failure: ProcessFailed) => void;
  onRuntimeMissing?: (payload: unknown) => void;
}

/** Subscribe to the `browser://` events; the returned function drops them all. */
export async function listenBrowserEvents(handlers: BrowserEventHandlers): Promise<() => void> {
  const unlisteners = await Promise.all([
    listen<TabState>("browser://tab-state", (e) => handlers.onTabState?.(e.payload)),
    listen<NewWindow>("browser://new-window", (e) => handlers.onNewWindow?.(e.payload)),
    listen<ProcessFailed>("browser://process-failed", (e) => handlers.onProcessFailed?.(e.payload)),
    listen<unknown>("browser://runtime-missing", (e) => handlers.onRuntimeMissing?.(e.payload)),
  ]);
  return () => {
    for (const un of unlisteners) un();
  };
}
