// §spec pilar-de-browser
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft, ArrowRight, Bot, Lock, LockOpen, Plus, RotateCw, Square, X } from "lucide-react";
import { useProjects } from "../data/system";
import {
  EMPTY_PROJECT_TABS as EMPTY,
  findTabById,
  findTabBySessionId,
  listenBrowserEvents,
  newTab,
  tabBack,
  tabClose,
  tabCreate,
  tabForward,
  tabNavigate,
  tabReload,
  tabStop,
  tabsShow,
  type Bounds,
  type NewWindow,
  type ProcessFailed,
  type Tab,
  type TabMap,
  type TabState,
} from "../data/browser-tabs";
import { ensureAgentEnv, reportAgentTabClosed } from "../data/browser-agent";
import {
  consumeFocus,
  getBrowserState,
  markCreated,
  setAgentNotice,
  setBoundsProvider,
  setTabMap,
  unmarkCreated,
  useBrowserStore,
} from "../data/browser-store";
import { AgentControlBar } from "./AgentControlBar";
import { Button, ErrorNote, IconButton } from "../ui";
import { useOverlayOpen } from "../ui/overlay";
import { resolveOmnibox } from "./browser-omnibox";
import "./browser-page.css";

const NO_BOUNDS: Bounds = { x: 0, y: 0, width: 0, height: 0 };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * The integrated browser: a tab strip and toolbar in React, and a placeholder that the native
 * WebView2 child window is laid over. Tabs are kept per project; the native webviews survive
 * leaving the page, they are only hidden.
 */
export function BrowserPage() {
  const projects = useProjects().data ?? [];
  const overlayOpen = useOverlayOpen();
  const [picked, setPicked] = useState<string | null>(null);
  const project = picked ?? projects[0]?.project_id ?? null;

  const { tabMap, created, focusIntent, agentNotice } = useBrowserStore();
  const [highlightControl, setHighlightControl] = useState(false);
  const [runtimeMissing, setRuntimeMissing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const placeholder = useRef<HTMLDivElement | null>(null);
  const lastBounds = useRef<Bounds>(NO_BOUNDS);

  const current = project === null ? EMPTY : (tabMap[project] ?? EMPTY);
  const active = current.tabs.find((tab) => tab.id === current.activeId) ?? null;
  // `created` (from the store) holds the tabs whose native webview exists; a tab is only shown
  // once its creation has resolved.
  const activeReady = active !== null && created.has(active.id);
  const shownId = overlayOpen || runtimeMissing || !activeReady ? null : (active?.id ?? null);
  const tabCount = current.tabs.length;
  const failedActive = active?.failed ?? false;

  const readBounds = useCallback((): Bounds => {
    const el = placeholder.current;
    if (el !== null) {
      const rect = el.getBoundingClientRect();
      lastBounds.current = { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
    }
    return lastBounds.current;
  }, []);

  // Tell the store where the native webview goes, so agent tabs opened while this page is not
  // mounted are created with real bounds once it is.
  useEffect(() => {
    setBoundsProvider(readBounds);
    return () => setBoundsProvider(null);
  }, [readBounds]);

  // An agent focus request: switch to its project and tab, and draw the eye to the control bar.
  useEffect(() => {
    if (focusIntent === null) return;
    const found = findTabBySessionId(getBrowserState().tabMap, focusIntent.sessionId);
    if (found !== undefined) {
      setPicked(focusIntent.project);
      const tabId = found.tab.id;
      setTabMap((map) => {
        const entry = map[focusIntent.project];
        if (entry === undefined || entry.activeId === tabId) return map;
        return { ...map, [focusIntent.project]: { ...entry, activeId: tabId } };
      });
      setHighlightControl(true);
    }
    consumeFocus();
  }, [focusIntent]);

  useEffect(() => {
    if (!highlightControl) return;
    const timer = setTimeout(() => setHighlightControl(false), 3000);
    return () => clearTimeout(timer);
  }, [highlightControl]);

  const guard = useCallback((work: Promise<unknown>) => {
    work.then(
      () => setError(null),
      (e: unknown) => setError(message(e)),
    );
  }, []);

  // Keep the native webview over the placeholder, and hide everything while an overlay is open.
  useEffect(() => {
    const sync = () => {
      const bounds = readBounds();
      // A collapsed placeholder has nothing to lay a webview over: hide instead.
      const usable = bounds.width > 0 && bounds.height > 0;
      guard(tabsShow(usable ? shownId : null, bounds));
    };
    sync();
    const el = placeholder.current;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(sync);
    if (el !== null) observer?.observe(el);
    window.addEventListener("resize", sync);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", sync);
    };
    // error / failedActive / tabCount shift the placeholder without resizing it, which the
    // ResizeObserver does not see, so they re-run the sync.
  }, [shownId, runtimeMissing, error, failedActive, tabCount, readBounds, guard]);

  // Leaving the page hides the webviews; they stay alive.
  useEffect(
    () => () => {
      void tabsShow(null, lastBounds.current).catch(() => {});
    },
    [],
  );

  // Show the address of the tab in front.
  useEffect(() => {
    setDraft(active?.url ?? "");
  }, [active?.id, active?.url]);

  const patchTab = useCallback((tabId: string, patch: Partial<Tab>) => {
    setTabMap((map) => {
      const next: TabMap = {};
      for (const [id, entry] of Object.entries(map)) {
        next[id] = entry.tabs.some((tab) => tab.id === tabId)
          ? { ...entry, tabs: entry.tabs.map((tab) => (tab.id === tabId ? { ...tab, ...patch } : tab)) }
          : entry;
      }
      return next;
    });
  }, []);

  const openTab = useCallback(
    (projectId: string, url: string) => {
      const id = crypto.randomUUID();
      setTabMap((map) => {
        const entry = map[projectId] ?? EMPTY;
        return { ...map, [projectId]: { tabs: [...entry.tabs, newTab(id, url)], activeId: id } };
      });
      tabCreate(projectId, "owner", id, url, readBounds()).then(
        () => {
          // Skip a tab closed while it was being created.
          const open = Object.values(getBrowserState().tabMap).some((entry) =>
            entry.tabs.some((tab) => tab.id === id),
          );
          if (open) markCreated(id);
        },
        (e: unknown) => {
        setError(message(e));
        setTabMap((map) => {
          const entry = map[projectId] ?? EMPTY;
          const tabs = entry.tabs.filter((tab) => tab.id !== id);
          return {
            ...map,
            [projectId]: {
              tabs,
              activeId: entry.activeId === id ? (tabs[tabs.length - 1]?.id ?? null) : entry.activeId,
            },
          };
        });
        },
      );
    },
    [readBounds],
  );

  // The handlers read through a ref so one subscription lives as long as the page.
  const handlers = useRef({ openTab, patchTab });
  handlers.current = { openTab, patchTab };

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    listenBrowserEvents({
      onTabState: (s: TabState) =>
        handlers.current.patchTab(s.tabId, {
          url: s.url,
          title: s.title,
          loading: s.loading,
          canGoBack: s.canGoBack,
          canGoForward: s.canGoForward,
          secure: s.secure,
        }),
      onNewWindow: (w: NewWindow) => {
        // An agent tab's popups are dropped: the agent owns that tab's navigation.
        if (findTabById(getBrowserState().tabMap, w.fromTabId)?.tab.role === "agent") return;
        const owner = Object.entries(getBrowserState().tabMap).find(([, entry]) =>
          entry.tabs.some((tab) => tab.id === w.fromTabId),
        );
        if (owner !== undefined) handlers.current.openTab(owner[0], w.url);
      },
      onProcessFailed: (f: ProcessFailed) => {
        handlers.current.patchTab(f.tabId, { failed: true });
        const found = findTabById(getBrowserState().tabMap, f.tabId);
        if (found?.tab.role === "agent" && found.tab.sessionId !== undefined) {
          void reportAgentTabClosed(found.tab.sessionId);
        }
      },
      onRuntimeMissing: () => setRuntimeMissing(true),
    }).then(
      (un) => {
        if (gone) un();
        else unlisten = un;
      },
      (e: unknown) => setError(message(e)),
    );
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);

  // Prepare the agent browser environment once per project; never blocks the owner's tabs.
  const ensured = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (project === null || ensured.current.has(project)) return;
    ensured.current.add(project);
    const target = project;
    void ensureAgentEnv(target).then((result) => {
      if (!result.ok) setAgentNotice(target, result.reason);
    });
  }, [project]);

  function switchProject(next: string) {
    guard(tabsShow(null, readBounds()));
    setPicked(next);
  }

  function activate(id: string) {
    if (project === null) return;
    setTabMap((map) => ({ ...map, [project]: { ...(map[project] ?? EMPTY), activeId: id } }));
  }

  function closeTab(id: string) {
    if (project === null) return;
    const closing = current.tabs.find((tab) => tab.id === id);
    const index = current.tabs.findIndex((tab) => tab.id === id);
    const tabs = current.tabs.filter((tab) => tab.id !== id);
    const activeId =
      current.activeId === id ? (tabs[Math.min(index, tabs.length - 1)]?.id ?? null) : current.activeId;
    setTabMap((map) => ({ ...map, [project]: { tabs, activeId } }));
    unmarkCreated(id);
    guard(tabClose(id));
    if (closing?.role === "agent" && closing.sessionId !== undefined) {
      void reportAgentTabClosed(closing.sessionId);
    }
  }

  function openInMyTabs(tab: Tab) {
    if (project === null) return;
    const url = tab.url === "" || tab.url.startsWith("about:blank") ? "about:blank" : tab.url;
    openTab(project, url);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (active === null) return;
    const url = resolveOmnibox(draft);
    if (url === null) return;
    guard(tabNavigate(active.id, url));
  }

  function reloadFailed(id: string) {
    patchTab(id, { failed: false });
    guard(tabReload(id));
  }

  return (
    <section className="browser-page">
      <header className="browser-bar">
        <select
          className="browser-project"
          aria-label="Project"
          value={project ?? ""}
          onChange={(e) => switchProject(e.target.value)}
        >
          {projects.map((p) => (
            <option key={p.project_id} value={p.project_id}>
              {p.project_id}
            </option>
          ))}
        </select>
        <div className="browser-tabs" role="tablist">
          {current.tabs.map((tab) => (
            <div
              key={tab.id}
              className="browser-tab"
              data-active={tab.id === current.activeId}
              data-role={tab.role}
            >
              {tab.role === "agent" ? (
                <Bot className="browser-tab-badge" aria-label="Agent tab" role="img" size={14}>
                  <title>Agent tab</title>
                </Bot>
              ) : null}
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === current.activeId}
                className="browser-tab-title"
                onClick={() => activate(tab.id)}
              >
                {tab.title || tab.url || "New tab"}
              </button>
              <IconButton
                label={`Close ${tab.title || tab.url || "tab"}`}
                icon={X}
                onClick={() => closeTab(tab.id)}
              />
            </div>
          ))}
          <IconButton
            label="New tab"
            icon={Plus}
            disabled={project === null}
            onClick={() => project !== null && openTab(project, "about:blank")}
          />
        </div>
      </header>

      <div className="browser-toolbar">
        <IconButton
          label="Back"
          icon={ArrowLeft}
          disabled={active === null || !active.canGoBack}
          onClick={() => active && guard(tabBack(active.id))}
        />
        <IconButton
          label="Forward"
          icon={ArrowRight}
          disabled={active === null || !active.canGoForward}
          onClick={() => active && guard(tabForward(active.id))}
        />
        {active?.loading ? (
          <IconButton label="Stop" icon={Square} onClick={() => guard(tabStop(active.id))} />
        ) : (
          <IconButton
            label="Reload"
            icon={RotateCw}
            disabled={active === null}
            onClick={() => active && guard(tabReload(active.id))}
          />
        )}
        <form className="browser-omnibox" onSubmit={submit}>
          {active?.secure ? (
            <Lock className="browser-padlock" aria-label="Secure connection" role="img" size={14} />
          ) : (
            <LockOpen className="browser-padlock" aria-label="Connection not secure" role="img" size={14} />
          )}
          <input
            type="text"
            aria-label="Address"
            placeholder="Search or enter address"
            value={draft}
            disabled={active === null}
            onChange={(e) => setDraft(e.target.value)}
          />
        </form>
      </div>

      {error !== null ? <ErrorNote>{error}</ErrorNote> : null}
      {active?.failed ? (
        <p className="browser-crashed" role="alert">
          <span>This tab crashed</span>
          <Button type="button" onClick={() => reloadFailed(active.id)}>
            Reload
          </Button>
        </p>
      ) : null}
      {active?.role === "agent" ? (
        <AgentControlBar
          key={active.id}
          tab={active}
          highlight={highlightControl}
          onOpenInMyTabs={openInMyTabs}
        />
      ) : null}
      {project !== null && agentNotice[project] !== undefined ? (
        <p className="browser-agent-notice">
          Agent browser unavailable: {agentNotice[project]}. Your own tabs still work.
        </p>
      ) : null}

      <div className="browser-content">
        {runtimeMissing ? (
          <p className="browser-message" role="alert">
            The Microsoft WebView2 Runtime is missing, so the browser cannot show pages. Install it
            and restart the app.
          </p>
        ) : (
          <>
            <div ref={placeholder} className="browser-placeholder" data-testid="browser-placeholder" />
            {active === null ? (
              <div className="browser-empty">
                <p>{project === null ? "No project to browse in yet." : "No tabs open in this project."}</p>
                {project !== null ? (
                  <Button type="button" onClick={() => openTab(project, "about:blank")}>
                    New tab
                  </Button>
                ) : null}
              </div>
            ) : null}
          </>
        )}
      </div>
    </section>
  );
}
