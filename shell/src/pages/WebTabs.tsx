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
 * Only the header swaps per tab; the tab list and panels sit in one stable tree, because a
 * list that was remounted on every switch dropped keyboard focus from the tab just selected.
 */
export function webTabOf(pathname: string): "archive" | "sessions" {
  return pathname === "/web/sessions" || pathname.startsWith("/web/sessions/") ? "sessions" : "archive";
}

export function WebTabs() {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const navigate = useNavigate();
  const tab = webTabOf(pathname);

  return (
    <Tabs value={tab} onValueChange={(next) => void navigate({ to: next === "sessions" ? "/web/sessions" : "/web" })}>
      {tab === "archive" ? <WebHeader /> : <SessionsHeader />}
      <TabsList aria-label="Web views">
        <TabsTrigger value="archive">Archive</TabsTrigger>
        <TabsTrigger value="sessions">Sessions</TabsTrigger>
      </TabsList>
      <TabsContent value="archive">
        <Web embedded />
      </TabsContent>
      <TabsContent value="sessions">
        <Browser embedded />
      </TabsContent>
    </Tabs>
  );
}

export function BrowserRedirect() {
  return <Navigate to="/web/sessions" replace />;
}
