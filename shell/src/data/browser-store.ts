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
  newTab,
  removeTabById,
  setTabSessionId,
  tabClose,
  tabCreate,
  type Bounds,
  type TabMap,
} from "./browser-tabs";
import {
  agentTabUrl,
  mintNonce,
  reportAgentTabClosed,
  startAgentPoller,
} from "./browser-agent";
import { embeddedAck, type EmbeddedRequest } from "./browser-embedded";

export type FocusIntent = { project: string; sessionId: string; nonce: number };

export type BrowserSnapshot = {
  tabMap: TabMap;
  /** Tabs whose native webview exists. */
  created: ReadonlySet<string>;
  focusIntent: FocusIntent | null;
  agentNotice: Record<string, string>;
};

const FALLBACK_BOUNDS: Bounds = { x: 0, y: 0, width: 800, height: 600 };

function initial(): BrowserSnapshot {
  return { tabMap: {}, created: new Set(), focusIntent: null, agentNotice: {} };
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
  setTabMap((map) => addAgentTab(map, req.project, newTab(tabId, url, "agent")));
  try {
    await tabCreate(req.project, "agent", tabId, url, readBounds());
    // Skip a tab closed while it was being created.
    if (findTabById(state.tabMap, tabId) === undefined) return;
    markCreated(tabId);
    const { session_id } = await embeddedAck(req.request_id, nonce);
    setTabMap((map) => setTabSessionId(map, tabId, session_id));
  } catch (e: unknown) {
    setAgentNotice(req.project, message(e));
    setTabMap((map) => removeTabById(map, tabId).map);
    unmarkCreated(tabId);
    void tabClose(tabId).catch(() => {});
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

export type EnvRebuilt = { project: string; role: string; closedTabIds: string[] };

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

/** Start the 1 s request poller and the env-rebuilt listener; returns a stop function. */
export function startBrowserBridge(): () => void {
  const stopPoller = startAgentPoller({
    onOpen: handleAgentOpen,
    onClose: handleAgentClose,
    onFocus: handleAgentFocus,
  });
  let stopped = false;
  let unlisten: (() => void) | null = null;
  void listen<EnvRebuilt>("browser://env-rebuilt", (e) => handleEnvRebuilt(e.payload)).then(
    (un) => {
      if (stopped) un();
      else unlisten = un;
    },
    () => {},
  );
  return () => {
    if (stopped) return;
    stopped = true;
    stopPoller();
    unlisten?.();
    unlisten = null;
  };
}
