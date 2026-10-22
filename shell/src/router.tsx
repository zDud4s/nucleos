// §spec novo-frontend
import { Suspense, lazy, type ComponentType, type ReactNode } from "react";
import {
  Outlet,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  redirect,
  useRouterState,
} from "@tanstack/react-router";
import { AppShell } from "./app/AppShell";
import { RouteError } from "./app/RouteError";
import { NAV_ITEMS, type NavItem } from "./app/nav";
import { validateAnalyticsSearch } from "./data/analytics";
import { QuickCapture } from "./quick-capture/QuickCapture";
import { Agents } from "./pages/Agents";
import { Analytics } from "./pages/Analytics";
import { Calendar, validateCalendarSearch } from "./pages/Calendar";
import { Contacts } from "./pages/Contacts";
import { Feed, validateFeedSearch } from "./pages/Feed";
import { Home } from "./pages/Home";
import { Mail } from "./pages/Mail";
import { MailDetail } from "./pages/MailDetail";
import { Projects } from "./pages/Projects";
import { Roster } from "./pages/Roster";
import { NewProject } from "./pages/NewProject";
import { Workspace } from "./project/Workspace";
import { RunDetail } from "./pages/RunDetail";
import { Runs, validateRunSearch } from "./pages/Runs";
import { TeamRunDetail } from "./pages/TeamRunDetail";
import { Bench } from "./team/Bench";
import { Voice, validateVoiceSearch } from "./pages/Voice";
import { validateBrainSearch } from "./brain/search";
import { Waiting, validateWaitingSearch } from "./pages/Waiting";
import { BrowserRedirect, WebTabs } from "./pages/WebTabs";
import { validateSystemSearch } from "./pages/systemSearch";

/**
 * Pages that are not the landing route load on first visit rather than at startup. `Chats` alone is
 * thousands of lines and `Brain` pulls in d3-force; none of it is needed to paint Home. The
 * boundary lives here, around the page, so the shell (rail, kill switch) stays on screen while a
 * chunk loads. Chunks are local, so the fallback is empty rather than a spinner that would flash.
 */
function lazyPage(load: () => Promise<{ default: ComponentType }>): () => ReactNode {
  const Page = lazy(load);
  return function LazyPage() {
    return (
      <Suspense fallback={null}>
        <Page />
      </Suspense>
    );
  };
}

const Autopilot = lazyPage(() => import("./pages/Autopilot").then((m) => ({ default: m.Autopilot })));
const Brain = lazyPage(() => import("./pages/Brain").then((m) => ({ default: m.Brain })));
const Chats = lazyPage(() => import("./pages/Chats").then((m) => ({ default: m.Chats })));
const Council = lazyPage(() => import("./pages/Council").then((m) => ({ default: m.Council })));
const Files = lazyPage(() => import("./pages/Files").then((m) => ({ default: m.Files })));
const Fleet = lazyPage(() => import("./pages/Fleet").then((m) => ({ default: m.Fleet })));
const System = lazyPage(() => import("./pages/System").then((m) => ({ default: m.System })));
const Teams = lazyPage(() => import("./pages/Teams").then((m) => ({ default: m.Teams })));

/**
 * Memory history, in the real app as well as in the tests.
 *
 * This is a desktop window with no address bar and no back button of its own,
 * served from a `tauri://` origin whose path is the bundle rather than the
 * route. Browser history would make the app's location a property of a URL
 * nobody can see or type, and one stray reload would land on a path the
 * bundler never emitted a file for. Keeping the location in memory means the
 * route is state, which is what it actually is.
 */
/**
 * The pages that exist, by the path the nav gave them.
 *
 * A table rather than a chain of ternaries in the map below: this list grows by
 * one entry per slice, and a conditional expression that grows to fourteen
 * branches is a conditional expression nobody reads. Every nav path must be
 * here: a path that is not fails `createAppRouter` loudly rather than mounting
 * a route with nothing in it.
 *
 * `export`ed so the "every item has a page" invariant can be asserted directly
 * against this table, rather than by walking the rendered tree.
 */
export const PAGES: Record<string, () => ReactNode> = {
  "/": Home,
  "/fleet": Fleet,
  "/autopilot": Autopilot,
  "/waiting": Waiting,
  "/runs": Runs,
  "/analytics": Analytics,
  "/feed": Feed,
  "/projects": Roster,
  "/chats": Chats,
  "/teams": Teams,
  "/council": Council,
  "/agents": Agents,
  "/mail": Mail,
  "/contacts": Contacts,
  "/calendar": Calendar,
  "/voice": Voice,
  "/brain": Brain,
  "/web": WebTabs,
  "/files": Files,
  "/system": System,
};

/**
 * The pages whose *filters* are part of the location.
 *
 * A validator per path, in a table beside the components, for the same reason
 * the components are in one: this list grows with the slices, and a special
 * case bolted into the map below would be the first of fourteen. The validator
 * itself lives with the page that reads it — the page owns what its search
 * params mean, and the router only needs to know that it has some.
 */
const SEARCH_VALIDATORS: Record<string, (search: Record<string, unknown>) => object> = {
  "/runs": validateRunSearch,
  "/analytics": validateAnalyticsSearch,
  "/feed": validateFeedSearch,
  "/calendar": validateCalendarSearch,
  "/waiting": validateWaitingSearch,
  "/voice": validateVoiceSearch,
  "/brain": validateBrainSearch,
};

/**
 * The routes that are not navigation items.
 *
 * `/runs/$runId` is the first of them, and the shape is the general one: a
 * detail is reached *from* a list rather than from the rail, so it has no place
 * in the nav table and must be added here instead. Registered as a sibling of
 * `/runs` and not as its child, because opening a run replaces the index rather
 * than appearing beside it — a nested route would need the list to render an
 * `Outlet` and would keep fifty rows polling behind one open run.
 *
 * TanStack spells a parameter `$runId`; the page reads it back under that name.
 *
 * `/projects/$projectId/$view` is the second, and it carries a parameter that is
 * not an id: the mode a project is being looked at through — `state`, `map`,
 * `code`, `workflows` or `authority` (once `github`, which still resolves). It is in the location rather than in component state
 * because a project somebody is working in should survive a reload and be linkable, and
 * it is **not** a search param because it is not a filter: there is exactly one
 * of it and it always has a value.
 *
 * There is no validator for it. A route parameter is a string, anybody can type
 * one, and `Workspace` answers an unrecognised mode with `state` rather than a
 * dead end — a typo in a path is not a missing page. Registering a validator
 * here would move that decision away from the page that knows what the modes
 * are.
 *
 * These segments were `estado`, `mapa` and `codigo` until 2026-09-02 — the one
 * place in this app where a route segment was not English. This paragraph used
 * to argue for keeping them, and the argument was never that Portuguese was
 * right: it was that *renaming them later would break every link somebody kept*.
 * That is a real cost and it is the only one, so the rename came with the only
 * thing that answers it. `Workspace`'s `RENAMED` map still resolves all three,
 * an old link lands on the mode it always meant, and the tabs emit the new
 * segment so an address corrects itself on the first click. The window is meant
 * to be closed, and the map is where that decision is written down.
 *
 * `/system/$view` is the same idiom again: like `/projects/$projectId/$view`, the
 * view is a path parameter and not a search param — there is exactly one of it, it
 * always has a value, and it is not a filter. No `SEARCH_VALIDATORS` entry: its one search param,
 * `?channel=` (the notifications tab's scope), is validated by the `DETAIL_ROUTES` entry, and the
 * validator serves nothing else.
 *
 * `/team-runs/$runId` is the first detail route whose first segment is not
 * itself a nav path — `/teams/$teamId` is a detail of `/teams`, but a team run
 * is reached from inside a department rather than from its own list page.
 * Nothing else in the tree mentions `/team-runs`, so `router.test.tsx` asserts
 * it by name.
 *
 * `/teams/$teamId` is a `Bench` and NOT a second mounting of `Teams`, which it
 * used to be. That page served both routes at once in the Council pattern —
 * list on screen, detail added below — and the two altitudes turned out to want
 * different things: `/teams` is a console over every department, and a
 * department is a workbench that wants the page to itself. Two components is
 * what makes that possible, and this line is where it is decided.
 */
/**
 * A detail route may declare a search validator, which the nav-built routes have always been able
 * to. Added when the workspace needed one: the Code mode reviews a *run*, and which run that is
 * belongs in the location — a slot on the State mode links straight to it, and a review somebody
 * is in the middle of survives a reload.
 */
/** A search param that is really there. Empty and absent are the same claim. */
function someText(raw: unknown): string | undefined {
  return typeof raw === "string" && raw !== "" ? raw : undefined;
}

/** The Bench's tab values (`team/Bench.tsx`), the only ones `?tab=` may carry. */
const BENCH_TAB_VALUES: readonly string[] = ["work", "decisions", "roster", "routines", "charter"];

const DETAIL_ROUTES: {
  path: string;
  component: () => ReactNode;
  validateSearch?: (search: Record<string, unknown>) => unknown;
}[] = [
  { path: "/runs/$runId", component: RunDetail },
  /*
    Adding a project has a route but draws a dialog: the roster, with the wizard on the shared
    `Modal` over it (owner's call 2026-10-09, overriding the earlier page-not-dialog reading of
    §3.2). The route keeps `?path=` and every link to it working, and closing the dialog goes back
    to `/projects`. It sits under `/projects/` because that is what it is about, and cannot be confused with a
    project called `new`: that one would be `/projects/new/state`, three segments rather than two.
  */
  { path: "/projects/new", component: NewProject },
  {
    path: "/projects/$projectId/$view",
    component: Workspace,
    /*
      One optional number. Not validated into a range or checked against the daemon — a run id in a
      URL is a claim, and the page answers a claim it cannot honour with a refusal rather than the
      router answering it with a dead end. Anything unparseable becomes `undefined`, which is the
      same as not asking.
    */
    validateSearch: (search) => {
      const raw = search.run;
      const run = typeof raw === "number" ? raw : Number(raw);
      return Number.isInteger(run) && run > 0 ? { run } : {};
    },
  },
  /**
   * The read-only inspector, on a path of its own until the Code mode replaces it.
   *
   * It used to share `/projects/$projectId/$view` with nothing else, and the
   * workspace took that path over. Moving it here rather than deleting it is
   * deliberate: `browse`, `search` and `diff` are superseded by the Code mode
   * and `rules` is superseded by the config editor, and neither of those exists
   * yet. Removing a working capability because its replacement is designed is
   * how a rewrite loses things quietly.
   *
   * `/projects` is `Roster` and this is `Projects`, and until 2026-08-24 they
   * were one component doing both: the roster drew itself above the inspector on
   * every visit, so choosing a project to look inside meant carrying
   * twenty-five rows down the page with you. Two questions, two pages. The way
   * IN is a link from the Código mode, which is where somebody already reading
   * this project's code would go looking for a file tree.
   */
  {
    path: "/projects/$projectId/inspect/$view",
    component: Projects,
    /*
      Where in the tree, and what was searched for. The page's header has always
      claimed the views are in the route "so a folder somebody is looking at
      survives a reload and can be linked to" — but only the VIEW ever was, and
      the folder, the open file and the query were component state that died on
      the first refresh. A search that found the thing could not be sent to
      anybody. Now it can.

      Strings, unvalidated, and deliberately so: a path in a URL is a claim, and
      the núcleo already refuses one that leaves the project's folder with a
      sentence explaining why. A router that answered it with a dead end would
      take that explanation away from the page that gives it. Empty becomes
      absent, so a cleared field leaves no `?path=` behind it.
    */
    validateSearch: (search) => ({
      path: someText(search.path),
      file: someText(search.file),
      q: someText(search.q),
      under: someText(search.under),
    }),
  },
  { path: "/chats/$chatId", component: Chats },
  { path: "/council/$councilId", component: Council },
  {
    path: "/teams/$teamId",
    component: Bench,
    /*
      Which Bench tab is open, so "3 decisions" can link to `?tab=decisions` and a reload or Back
      keeps the tab. Only the Bench's real tabs count; anything else is dropped, which is the same
      as not asking (the Bench then opens on Work).
    */
    validateSearch: (search) => {
      const raw = search.tab;
      return typeof raw === "string" && BENCH_TAB_VALUES.includes(raw) ? { tab: raw } : {};
    },
  },
  { path: "/team-runs/$runId", component: TeamRunDetail },
  { path: "/mail/$emailId", component: MailDetail },
  { path: "/web/pages/$pageId", component: WebTabs },
  // The Archive tab of the Web page (Sessions is /web itself); a route of its own so the tab
  // survives a reload.
  { path: "/web/archive", component: WebTabs },
  // The Sessions tab's address until 2026-10-08, when Sessions became /web; it only forwards.
  { path: "/web/sessions", component: BrowserRedirect },
  // Old bookmarks and feed rows still point at /browser; it now only forwards to the tab.
  { path: "/browser", component: BrowserRedirect },
  { path: "/system/$view", component: System, validateSearch: validateSystemSearch },
];

/** The quick-capture window's page. Not a nav item: only the window the capture chord opens shows it. */
export const QUICK_CAPTURE_PATH = "/quick-capture";

/** Whether this document is the quick-capture window (`quick_capture.rs` loads `?window=quick-capture`). */
function isQuickCaptureWindow(): boolean {
  return new URLSearchParams(window.location.search).get("window") === "quick-capture";
}

/** The path of a location string, without its search or hash. */
function pathOf(location: string): string {
  return location.split(/[?#]/, 1)[0] ?? location;
}

/**
 * The location a window starts at. The quick-capture window always starts on its box, whatever it
 * was handed; any other window that is handed the box (the remembered place is shared storage, and
 * the quick window resolves to it too) starts on Home, so the main window never restores it.
 */
function startPath(initialPath: string): string {
  if (isQuickCaptureWindow()) return QUICK_CAPTURE_PATH;
  return pathOf(initialPath) === QUICK_CAPTURE_PATH ? "/" : initialPath;
}

/**
 * The root: the shell around every page, except the quick-capture box, which is the whole window
 * and has no rail, connection line or kill switch around it.
 */
function Root() {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  return pathname === QUICK_CAPTURE_PATH ? <Outlet /> : <AppShell />;
}

export function createAppRouter(initialPath = "/") {
  /**
   * Built inside the factory rather than at module scope, and that matters:
   * `createRouter` initialises the route objects in place, so two routers
   * sharing one tree would be two routers fighting over the same instances.
   * Every test that mounts the app gets its own tree.
   *
   * No `errorComponent` here, and that is the decision rather than the omission.
   * This route's component IS the shell — the rail, the connection line, the
   * kill switch, the `<Outlet />`. An error boundary at this level would have
   * nothing left to render inside, so it would draw `RouteError` bare on the
   * window; and `RouteError` renders a `<Link>`, which resolves against the
   * router whose root just failed. The child routes below carry the boundary
   * instead, which is what keeps the shell standing around a page that threw.
   */
  const rootRoute = createRootRoute({ component: Root });

  /**
   * One route per navigation item, from the same table the sidebar reads.
   *
   * The nav is the route list. A page cannot be added to the app without
   * appearing in the sidebar, and cannot appear in the sidebar without being
   * reachable — the two ways an app grows a dead link are both closed here
   * rather than by anybody remembering.
   */
  const routes = NAV_ITEMS.map((item) => {
    const validateSearch = SEARCH_VALIDATORS[item.path];
    return createRoute({
      getParentRoute: () => rootRoute,
      path: item.path,
      component: pageFor(item),
      // Declared per route rather than only on the router, so the boundary sits
      // as deep as it can: inside `AppShell`'s outlet, where the page that threw
      // is the only thing replaced.
      errorComponent: RouteError,
      // Spread rather than passed as `undefined`: the router treats the key's
      // presence as the declaration, and a route that declares a validator and
      // has none would strip every search param it is given.
      ...(validateSearch === undefined ? {} : { validateSearch }),
    });
  });

  const details = DETAIL_ROUTES.map((detail) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path: detail.path,
      component: detail.component,
      // For the reason the nav routes above give. It matters most here: a detail
      // route is the one reached with an id in hand, and an id the daemon has
      // nothing for is the commonest way one of these throws.
      errorComponent: RouteError,
      // Spread rather than passed as `undefined`, for the reason the nav routes above give: the
      // router treats the key's presence as the declaration, and a route that declares a validator
      // and has none strips every search param it is given.
      ...(detail.validateSearch === undefined
        ? {}
        : { validateSearch: detail.validateSearch }),
    }),
  );

  // The Learned page now lives in the Brain; old links and bookmarks land there.
  const learned = createRoute({
    getParentRoute: () => rootRoute,
    path: "/learned",
    beforeLoad: () => {
      throw redirect({ to: "/brain" });
    },
  });

  const quickCapture = createRoute({
    getParentRoute: () => rootRoute,
    path: QUICK_CAPTURE_PATH,
    component: QuickCapture,
    errorComponent: RouteError,
  });

  return createRouter({
    routeTree: rootRoute.addChildren([...routes, ...details, learned, quickCapture]),
    history: createMemoryHistory({ initialEntries: [startPath(initialPath)] }),
    /**
     * Nothing here has a loader, so there is nothing to keep warm and nothing
     * to wait for. Turning preloading off keeps a hover over the rail from
     * firing twenty route loads on a machine that is already polling.
     */
    defaultPreload: false,
    /**
     * The belt under the braces. Every child route above names `RouteError`
     * explicitly; this catches the one somebody adds without it, which is the
     * only way a blank window comes back. The library's own default here is a
     * bare pre-formatted dump with no shell around it.
     */
    defaultErrorComponent: RouteError,
  });
}

/** The page for a nav item. Every item has one, so a missing entry is a bug in `PAGES`. */
function pageFor(item: NavItem): () => ReactNode {
  const page = PAGES[item.path];
  if (page === undefined) throw new Error(`no page for nav path ${item.path}`);
  return page;
}

export type AppRouter = ReturnType<typeof createAppRouter>;
