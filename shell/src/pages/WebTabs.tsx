import { Navigate, useNavigate, useRouterState } from "@tanstack/react-router";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../ui";
import { Browser, SessionsHeader } from "./Browser";
import { Web, WebHeader } from "./Web";

/**
 * Web and Browser as one page with two real tabs.
 *
 * The owner decided on 2026-10-03 to merge the two pages under one rail entry. The tabs
 * are Radix Tabs driven by the URL, rather than the Link strip System.tsx uses, because
 * these are two whole pages with their own data and a tab must keep working on a reload
 * and on a deep link such as /web/pages/42. /browser survives only as a redirect, so
 * old bookmarks and feed rows that still point at it land on the Sessions tab.
 *
 * Sessions is the page's front, at /web, and the archive sits behind it at /web/archive
 * (owner, 2026-10-08): what somebody opens Web for is a browser. /web/sessions was the
 * Sessions tab's own address until then and now forwards to /web, as /browser does.
 *
 * Only the header swaps per tab; the tab list and panels sit in one stable tree, because a
 * list that was remounted on every switch dropped keyboard focus from the tab just selected.
 */
export function webTabOf(pathname: string): "archive" | "sessions" {
  return pathname === "/web/archive" || pathname.startsWith("/web/pages/") ? "archive" : "sessions";
}

export function WebTabs() {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const navigate = useNavigate();
  const tab = webTabOf(pathname);

  return (
    <Tabs className="web-tabs" value={tab} onValueChange={(next) => void navigate({ to: next === "sessions" ? "/web" : "/web/archive" })}>
      {tab === "archive" ? <WebHeader /> : <SessionsHeader />}
      <TabsList aria-label="Web views">
        <TabsTrigger value="sessions">Sessions</TabsTrigger>
        <TabsTrigger value="archive">Archive</TabsTrigger>
      </TabsList>
      <TabsContent value="sessions">
        <Browser embedded />
      </TabsContent>
      <TabsContent value="archive">
        <Web embedded />
      </TabsContent>
    </Tabs>
  );
}

export function BrowserRedirect() {
  return <Navigate to="/web" replace />;
}
