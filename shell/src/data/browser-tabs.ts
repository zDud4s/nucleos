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

/**
 * Tab-map key of the "No project" scope. It starts with `@`, so it can never be a project id;
 * it is translated to `null` at every `invoke`/`apiFetch` boundary (the wire never carries it).
 */
export const NO_PROJECT = "@none";

/** The tab-map key of a scope: the project id, or {@link NO_PROJECT} for no project. */
export function scopeKey(project: string | null): string {
  return project === null ? NO_PROJECT : project;
}

/** The wire project of a tab-map key: `null` for {@link NO_PROJECT}, the project id otherwise. */
export function scopeProject(key: string): string | null {
  return key === NO_PROJECT ? null : key;
}

/** `key` is a scope key ({@link scopeKey}); the native side receives `null` for no project. */
export function envEnsure(key: string, role: Role): Promise<{ cdp_http: string | null }> {
  return invoke("browser_env_ensure", { project: scopeProject(key), role });
}

/** `key` is a scope key ({@link scopeKey}); the native side receives `null` for no project. */
export function tabCreate(
  key: string,
  role: Role,
  tabId: string,
  url: string,
  bounds: Bounds,
): Promise<void> {
  return invoke("browser_tab_create", { project: scopeProject(key), role, tabId, url, bounds });
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

export interface Tab {
  id: string;
  title: string;
  url: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  secure: boolean;
  failed: boolean;
  role: Role;
  sessionId?: string;
}

export interface ProjectTabs {
  tabs: Tab[];
  activeId: string | null;
}

export type TabMap = Record<string, ProjectTabs>;

export const EMPTY_PROJECT_TABS: ProjectTabs = { tabs: [], activeId: null };

export function newTab(id: string, url: string, role: Role = "owner", sessionId?: string): Tab {
  const tab: Tab = {
    id,
    title: "",
    url,
    loading: false,
    canGoBack: false,
    canGoForward: false,
    secure: false,
    failed: false,
    role,
  };
  if (sessionId !== undefined) tab.sessionId = sessionId;
  return tab;
}

/** Append an agent tab; it only becomes active when the project had no active tab. */
export function addAgentTab(map: TabMap, project: string, tab: Tab): TabMap {
  const current = map[project] ?? EMPTY_PROJECT_TABS;
  return {
    ...map,
    [project]: {
      tabs: [...current.tabs, tab],
      activeId: current.activeId ?? tab.id,
    },
  };
}

function findTab(
  map: TabMap,
  pred: (tab: Tab) => boolean,
): { project: string; tab: Tab } | undefined {
  for (const [project, entry] of Object.entries(map)) {
    const tab = entry.tabs.find(pred);
    if (tab !== undefined) return { project, tab };
  }
  return undefined;
}

export function findTabById(map: TabMap, tabId: string): { project: string; tab: Tab } | undefined {
  return findTab(map, (tab) => tab.id === tabId);
}

export function findTabBySessionId(
  map: TabMap,
  sessionId: string,
): { project: string; tab: Tab } | undefined {
  return findTab(map, (tab) => tab.sessionId === sessionId);
}

export function setTabSessionId(map: TabMap, tabId: string, sessionId: string): TabMap {
  const found = findTabById(map, tabId);
  if (found === undefined) return map;
  const entry = map[found.project];
  return {
    ...map,
    [found.project]: {
      ...entry,
      tabs: entry.tabs.map((tab) => (tab.id === tabId ? { ...tab, sessionId } : tab)),
    },
  };
}

export function removeTabById(
  map: TabMap,
  tabId: string,
): { map: TabMap; removed?: { project: string; tab: Tab } } {
  const found = findTabById(map, tabId);
  if (found === undefined) return { map };
  const entry = map[found.project];
  const index = entry.tabs.findIndex((tab) => tab.id === tabId);
  const tabs = entry.tabs.filter((tab) => tab.id !== tabId);
  const activeId =
    entry.activeId === tabId ? (tabs[Math.min(index, tabs.length - 1)]?.id ?? null) : entry.activeId;
  return { map: { ...map, [found.project]: { tabs, activeId } }, removed: found };
}

export function removeTabBySessionId(
  map: TabMap,
  sessionId: string,
): { map: TabMap; removed?: { project: string; tab: Tab } } {
  const found = findTabBySessionId(map, sessionId);
  if (found === undefined) return { map };
  return removeTabById(map, found.tab.id);
}
