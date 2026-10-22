// §spec pilar-de-browser
// Module-level store for the integrated browser's tab state, plus the daemon-request handlers
// (open / close / focus) and the env-rebuilt handler. Nothing here is React: the shell root starts
// the bridge once, and BrowserPage only reads the snapshot through useBrowserStore().
import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  addAgentTab,
  findTabById,
  findTabBySessionId,
  EMPTY_PROJECT_TABS,
  listenBrowserEvents,
  newTab,
  removeTabById,
  scopeKey,
  setTabSessionId,
  tabClose,
  tabCreate,
  type Bounds,
  type NewWindow,
  type ProcessFailed,
  type Tab,
  type TabMap,
  type TabState,
} from "./browser-tabs";
import {
  agentTabUrl,
  ensureAgentEnv,
  mintNonce,
  reportAgentTabClosed,
  startAgentPoller,
} from "./browser-agent";
import { embeddedAck, type EmbeddedRequest } from "./browser-embedded";

/** `project` is a scope key: a project id or `NO_PROJECT`, never null. */
export type FocusIntent = { project: string; sessionId: string; nonce: number };

export type BrowserSnapshot = {
  tabMap: TabMap;
  /** Tabs whose native webview exists. */
  created: ReadonlySet<string>;
  focusIntent: FocusIntent | null;
  agentNotice: Record<string, string>;
  /** The WebView2 Runtime is not installed. */
  runtimeMissing: boolean;
};

const FALLBACK_BOUNDS: Bounds = { x: 0, y: 0, width: 800, height: 600 };

function initial(): BrowserSnapshot {
  return { tabMap: {}, created: new Set(), focusIntent: null, agentNotice: {}, runtimeMissing: false };
}

let state: BrowserSnapshot = initial();
let boundsProvider: (() => Bounds) | null = null;
let focusNonce = 0;
// Request ids whose open is being (or has been) handled.
const handling = new Set<string>();
const listeners = new Set<() => void>();

function set(next: BrowserSnapshot) {
  state = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getBrowserState(): BrowserSnapshot {
  return state;
}

export function useBrowserStore(): BrowserSnapshot {
  return useSyncExternalStore(subscribe, getBrowserState, getBrowserState);
}

export function setTabMap(updater: (m: TabMap) => TabMap): void {
  const tabMap = updater(state.tabMap);
  if (tabMap === state.tabMap) return;
  set({ ...state, tabMap });
}

export function markCreated(id: string): void {
  if (state.created.has(id)) return;
  set({ ...state, created: new Set(state.created).add(id) });
}

export function unmarkCreated(id: string): void {
  if (!state.created.has(id)) return;
  const created = new Set(state.created);
  created.delete(id);
  set({ ...state, created });
}

export function setAgentNotice(project: string, msg: string | null): void {
  const agentNotice = { ...state.agentNotice };
  if (msg === null) {
    if (!(project in agentNotice)) return;
    delete agentNotice[project];
  } else {
    agentNotice[project] = msg;
  }
  set({ ...state, agentNotice });
}

export function requestFocus(project: string, sessionId: string): void {
  focusNonce += 1;
  set({ ...state, focusIntent: { project, sessionId, nonce: focusNonce } });
}

export function consumeFocus(): void {
  if (state.focusIntent === null) return;
  set({ ...state, focusIntent: null });
}

/** The page registers where the native webview goes; null when it unmounts. */
export function setBoundsProvider(fn: (() => Bounds) | null): void {
  boundsProvider = fn;
}

export function readBounds(): Bounds {
  return boundsProvider?.() ?? FALLBACK_BOUNDS;
}

export function resetBrowserStore(): void {
  state = initial();
  boundsProvider = null;
  focusNonce = 0;
  handling.clear();
  agentProjects.clear();
  for (const listener of listeners) listener();
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export async function handleAgentOpen(
  req: EmbeddedRequest & { url?: string },
): Promise<void> {
  if (handling.has(req.request_id)) return;
  handling.add(req.request_id);
  const tabId = crypto.randomUUID();
  const nonce = mintNonce();
  const url = agentTabUrl(nonce);
  // The daemon sends `project: null` for the no-project scope; the tab map keys it as NO_PROJECT.
  const key = scopeKey(req.project);
  setTabMap((map) => addAgentTab(map, key, newTab(tabId, url, "agent")));
  try {
    await tabCreate(key, "agent", tabId, url, readBounds());
    // Skip a tab closed while it was being created.
    if (findTabById(state.tabMap, tabId) === undefined) return;
    markCreated(tabId);
    const { session_id } = await embeddedAck(req.request_id, nonce);
    setTabMap((map) => setTabSessionId(map, tabId, session_id));
  } catch (e: unknown) {
    setAgentNotice(key, message(e));
    setTabMap((map) => removeTabById(map, tabId).map);
    unmarkCreated(tabId);
    void tabClose(tabId).catch(() => {});
  } finally {
    // Trim the de-dupe set once the request is done; the poller's own capped set covers re-delivery.
    handling.delete(req.request_id);
  }
}

// Scope keys (project ids and NO_PROJECT) whose agent env the shell has prepared; re-run after
// the daemon drops the registration.
const agentProjects = new Set<string>();

/**
 * Prepare (once per call) the agent browser env for a scope key (a project id or NO_PROJECT) and
 * surface a failure as a notice.
 */
export async function ensureProjectAgent(
  key: string,
  opts?: { fresh?: boolean },
): Promise<void> {
  agentProjects.add(key);
  // Only a forced rebuild passes options: callers and tests see the plain one-argument call.
  const result = await (opts ? ensureAgentEnv(key, opts) : ensureAgentEnv(key));
  setAgentNotice(key, result.ok ? null : result.reason);
}

/** The daemon lost our registration (requests failed or 404/409/410): redo prepare/ensure/register. */
export function reregisterAgentProjects(): void {
  for (const project of agentProjects) {
    void ensureProjectAgent(project).catch(() => {});
  }
}

export function handleAgentClose(req: { session_id: string }): void {
  const found = findTabBySessionId(state.tabMap, req.session_id);
  if (found === undefined) return;
  const id = found.tab.id;
  void tabClose(id).catch(() => {});
  unmarkCreated(id);
  setTabMap((map) => removeTabById(map, id).map);
}

export function handleAgentFocus(req: { session_id: string }): void {
  const found = findTabBySessionId(state.tabMap, req.session_id);
  if (found === undefined) return;
  const { project, tab } = found;
  setTabMap((map) => {
    const entry = map[project];
    if (entry === undefined || entry.activeId === tab.id) return map;
    return { ...map, [project]: { ...entry, activeId: tab.id } };
  });
  requestFocus(project, req.session_id);
}

/** `project` is `null` for the no-project scope. */
export type EnvRebuilt = { project: string | null; role: string; closedTabIds: string[] };

export function handleEnvRebuilt(ev: EnvRebuilt): void {
  for (const id of ev.closedTabIds) {
    const { map, removed } = removeTabById(state.tabMap, id);
    if (removed === undefined) {
      unmarkCreated(id);
      continue;
    }
    setTabMap(() => map);
    unmarkCreated(id);
    if (removed.tab.role === "agent" && removed.tab.sessionId !== undefined) {
      void reportAgentTabClosed(removed.tab.sessionId).catch(() => {});
    }
  }
}

/** The native side tore down the no-project agent env after its last tab closed. */
export type EnvRetired = { project: string | null; role: string };

/**
 * A retired agent env is gone together with the cdp endpoint the daemon was told about, so a scope
 * the shell had prepared is prepared again (a fresh env, registered anew). The run is fresh: an
 * ensure already in flight may register the retired env's dead endpoint, so a new run follows it
 * instead of joining it.
 */
export function handleEnvRetired(ev: EnvRetired): void {
  if (ev.role !== "agent") return;
  const key = scopeKey(ev.project);
  if (!agentProjects.has(key)) return;
  void ensureProjectAgent(key, { fresh: true }).catch(() => {});
}

/** Merge a patch into the tab with this id, wherever it lives. */
export function patchTab(tabId: string, patch: Partial<Tab>): void {
  setTabMap((map) => {
    const next: TabMap = {};
    for (const [id, entry] of Object.entries(map)) {
      next[id] = entry.tabs.some((tab) => tab.id === tabId)
        ? { ...entry, tabs: entry.tabs.map((tab) => (tab.id === tabId ? { ...tab, ...patch } : tab)) }
        : entry;
    }
    return next;
  });
}

/** Drop the daemon session id of a tab (its session was reported closed). */
function clearTabSessionId(tabId: string): void {
  setTabMap((map) => {
    const found = findTabById(map, tabId);
    if (found === undefined || found.tab.sessionId === undefined) return map;
    const entry = map[found.project];
    return {
      ...map,
      [found.project]: {
        ...entry,
        tabs: entry.tabs.map((tab) => {
          if (tab.id !== tabId) return tab;
          const { sessionId: _dropped, ...rest } = tab;
          return rest;
        }),
      },
    };
  });
}

/**
 * Open an owner tab in a scope (a project id or NO_PROJECT) and create its native webview. A creation failure removes the
 * tab again and rejects, so the caller can surface the message.
 */
export async function openOwnerTab(project: string, url: string): Promise<void> {
  const id = crypto.randomUUID();
  setTabMap((map) => {
    const entry = map[project] ?? EMPTY_PROJECT_TABS;
    return { ...map, [project]: { tabs: [...entry.tabs, newTab(id, url)], activeId: id } };
  });
  try {
    await tabCreate(project, "owner", id, url, readBounds());
  } catch (e: unknown) {
    setTabMap((map) => {
      const entry = map[project] ?? EMPTY_PROJECT_TABS;
      const tabs = entry.tabs.filter((tab) => tab.id !== id);
      return {
        ...map,
        [project]: {
          tabs,
          activeId: entry.activeId === id ? (tabs[tabs.length - 1]?.id ?? null) : entry.activeId,
        },
      };
    });
    throw e;
  }
  // Skip a tab closed while it was being created.
  if (findTabById(state.tabMap, id) !== undefined) markCreated(id);
}

export function handleTabState(s: TabState): void {
  patchTab(s.tabId, {
    url: s.url,
    title: s.title,
    loading: s.loading,
    canGoBack: s.canGoBack,
    canGoForward: s.canGoForward,
    secure: s.secure,
  });
}

export function handleNewWindow(w: NewWindow): void {
  const found = findTabById(state.tabMap, w.fromTabId);
  // An agent tab's popups are dropped: the agent owns that tab's navigation.
  if (found === undefined || found.tab.role === "agent") return;
  void openOwnerTab(found.project, w.url).catch(() => {});
}

export function handleProcessFailed(f: ProcessFailed): void {
  patchTab(f.tabId, { failed: true });
  const found = findTabById(state.tabMap, f.tabId);
  if (found?.tab.role === "agent" && found.tab.sessionId !== undefined) {
    // Once the session is reported closed the tab no longer belongs to it.
    void reportAgentTabClosed(found.tab.sessionId)
      .catch(() => {})
      .then(() => clearTabSessionId(f.tabId));
  }
}

export function setRuntimeMissing(): void {
  if (state.runtimeMissing) return;
  set({ ...state, runtimeMissing: true });
}

/**
 * Start the 1 s request poller and the `browser://` listeners (tab state, new window, process
 * failure, runtime missing, env rebuilt, env retired) for the whole shell session; returns a stop
 * function.
 */
export function startBrowserBridge(): () => void {
  const stopPoller = startAgentPoller({
    onOpen: handleAgentOpen,
    onClose: handleAgentClose,
    onFocus: handleAgentFocus,
    onLost: reregisterAgentProjects,
  });
  let stopped = false;
  const unlistens: Array<() => void> = [];
  const keep = (un: () => void) => {
    if (stopped) un();
    else unlistens.push(un);
  };
  void listen<EnvRebuilt>("browser://env-rebuilt", (e) => handleEnvRebuilt(e.payload)).then(
    keep,
    () => {},
  );
  void listen<EnvRetired>("browser://env-retired", (e) => handleEnvRetired(e.payload)).then(
    keep,
    () => {},
  );
  void listenBrowserEvents({
    onTabState: handleTabState,
    onNewWindow: handleNewWindow,
    onProcessFailed: handleProcessFailed,
    onRuntimeMissing: setRuntimeMissing,
  }).then(keep, () => {});
  return () => {
    if (stopped) return;
    stopped = true;
    stopPoller();
    for (const un of unlistens.splice(0)) un();
  };
}
