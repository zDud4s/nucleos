import { Navigate, useNavigate, useRouterState } from "@tanstack/react-router";
import { useState } from "react";
import { useBrowserHealth, useBrowserSessions } from "../data/browser";
import { Count, StateBadge } from "../ui";
import { Browser } from "./Browser";
import { BrowserPage } from "./BrowserPage";
import { Web } from "./Web";

/**
 * The Web page: the integrated browser IS the page, and the Archive (or the legacy Sessions view)
 * is a side panel beside it.
 *
 * There is no header band: the browser's tab strip is the first row, and the panel toggles sit at
 * the end of its address bar. The page title stays for assistive technology only. The sidecar's
 * state is drawn only when it is not `ok`, since a healthy pillar has nothing to say there.
 *
 * Routes are unchanged: /web is the browser alone, /web/archive and /web/pages/$pageId open the
 * browser with the Archive panel shown (the detail selected as before). /browser and
 * /web/sessions only redirect to /web. The panel sits in a flex row BESIDE the browser
 * placeholder, never over it: native webviews paint above the DOM, so an overlapping panel would
 * be hidden by them.
 *
 * Archive visibility follows the URL so a reload and a deep link keep working; the Sessions
 * panel is local state. Sessions stays mounted (hidden) for the whole page life: its "keep
 * these?" question lives in local state and outranks everything, because the window it asks
 * about has already closed, so toggling panels must not throw it away. A session asking for the
 * wheel is counted on its toggle. On Windows the Sessions panel lists no sessions: it hosts only
 * the open bar, the chain dialogue and the site grants.
 */
export function webPanelOf(pathname: string): "archive" | null {
  return pathname === "/web/archive" || pathname.startsWith("/web/pages/") ? "archive" : null;
}

export function WebTabs() {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const navigate = useNavigate();
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const archiveOpen = webPanelOf(pathname) === "archive";
  const showSessions = sessionsOpen && !archiveOpen;
  const asking = (useBrowserSessions().data ?? []).filter((row) => row.mode === "wheel-requested").length;
  const subsystem = useBrowserHealth().data?.subsystem ?? null;
  const unwell = subsystem !== null && subsystem.status !== "ok";

  const toggleArchive = () => {
    setSessionsOpen(false);
    void navigate({ to: archiveOpen ? "/web" : "/web/archive" });
  };
  const toggleSessions = () => {
    setSessionsOpen(!showSessions);
    if (archiveOpen) void navigate({ to: "/web" });
  };

  const panels = (
    <div className="web-panel-toggles" role="toolbar" aria-label="Web panels">
      {unwell ? <StateBadge domain="pillar" state={subsystem.status} /> : null}
      <button type="button" className="web-panel-toggle" aria-pressed={archiveOpen} onClick={toggleArchive}>
        Archive
      </button>
      <button type="button" className="web-panel-toggle" aria-pressed={showSessions} onClick={toggleSessions}>
        Sessions
        {asking > 0 && (
          <>
            {" "}
            <Count n={asking} noun="asking" />
          </>
        )}
      </button>
    </div>
  );

  return (
    <div className="web-split">
      <h1 className="sr-only">Web</h1>
      <div className="web-split-row">
        <div className="web-split-main">
          <BrowserPage actions={panels} />
        </div>
        <aside className="web-side" aria-label="Side panel" hidden={!archiveOpen && !showSessions}>
          <div hidden={!showSessions}>
            <Browser embedded />
          </div>
          {archiveOpen && <Web embedded />}
        </aside>
      </div>
    </div>
  );
}

export function BrowserRedirect() {
  return <Navigate to="/web" replace />;
}
