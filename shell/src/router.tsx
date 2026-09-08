// §spec novo-frontend
import type { ReactNode } from "react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { AppShell } from "./app/AppShell";
import { RouteError } from "./app/RouteError";
import { NAV_ITEMS, type NavItem } from "./app/nav";
import { Agents } from "./pages/Agents";
import { Autopilot } from "./pages/Autopilot";
import { Browser } from "./pages/Browser";
import { Calendar, validateCalendarSearch } from "./pages/Calendar";
import { Chats } from "./pages/Chats";
import { Contacts } from "./pages/Contacts";
import { Council } from "./pages/Council";
import { Errands } from "./pages/Errands";
import { Feed, validateFeedSearch } from "./pages/Feed";
import { Files } from "./pages/Files";
import { Fleet } from "./pages/Fleet";
import { Home } from "./pages/Home";
import { Learned } from "./pages/Learned";
import { Mail } from "./pages/Mail";
import { MailDetail } from "./pages/MailDetail";
import { Placeholder } from "./pages/Placeholder";
import { Projects } from "./pages/Projects";
import { Roster } from "./pages/Roster";
import { NewProject } from "./pages/NewProject";
import { Workspace } from "./project/Workspace";
import { RunDetail } from "./pages/RunDetail";
import { Runs, validateRunSearch } from "./pages/Runs";
import { System } from "./pages/System";
import { TeamRunDetail } from "./pages/TeamRunDetail";
import { Teams } from "./pages/Teams";
import { Bench } from "./team/Bench";
import { Voice } from "./pages/Voice";
import { Waiting, validateWaitingSearch } from "./pages/Waiting";
import { Web } from "./pages/Web";

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
 * branches is a conditional expression nobody reads. A path that is not here
 * gets the placeholder, which is what makes an unbuilt page a *stated* absence
 * rather than a missing route.
 *
 * `export`ed so the "no placeholder left" invariant can be asserted directly
 * against this table, rather than by walking the rendered tree.
 */
export const PAGES: Record<string, () => ReactNode> = {
  "/": Home,
  "/fleet": Fleet,
  "/autopilot": Autopilot,
  "/waiting": Waiting,
  "/runs": Runs,
  "/feed": Feed,
  "/projects": Roster,
  "/learned": Learned,
  "/chats": Chats,
  "/errands": Errands,
  "/teams": Teams,
  "/council": Council,
  "/agents": Agents,
  "/mail": Mail,
  "/contacts": Contacts,
  "/calendar": Calendar,
  "/voice": Voice,
  "/web": Web,
  "/browser": Browser,
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
  "/feed": validateFeedSearch,
  "/calendar": validateCalendarSearch,
  "/waiting": validateWaitingSearch,
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
 * not an id: the mode a project is being looked at through — `state`, `code`,
 * `workflows` or `github`. It is in the location rather than in component state
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
 * always has a value, and it is not a filter. No `SEARCH_VALIDATORS` entry.
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

const DETAIL_ROUTES: {
  path: string;
  component: () => ReactNode;
  validateSearch?: (search: Record<string, unknown>) => unknown;
}[] = [
  { path: "/runs/$runId", component: RunDetail },
  /*
    Adding a project is a page and not a dialog, for the same reason the eject guard is a panel:
    §3.2 of the frontend spec. It is also three steps long and one of them is a folder path somebody
    may want to go and look up — a modal that had to be dismissed to do that would lose the other
    two. It sits under `/projects/` because that is what it is about, and cannot be confused with a
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
  { path: "/errands/$errandId", component: Errands },
  { path: "/council/$councilId", component: Council },
  { path: "/teams/$teamId", component: Bench },
  { path: "/team-runs/$runId", component: TeamRunDetail },
  { path: "/mail/$emailId", component: MailDetail },
  { path: "/web/pages/$pageId", component: Web },
  { path: "/system/$view", component: System },
];

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
  const rootRoute = createRootRoute({ component: AppShell });

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
      component: PAGES[item.path] ?? placeholderFor(item),
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

  return createRouter({
    routeTree: rootRoute.addChildren([...routes, ...details]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
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

/**
 * A component per item rather than one component reading the route.
 *
 * The item is closed over, so the placeholder knows which page it stands in for
 * without a route-parameter dance — and when a real page replaces it, the
 * change is one line in the map above.
 */
function placeholderFor(item: NavItem) {
  return function PlaceholderRoute() {
    return <Placeholder item={item} />;
  };
}

export type AppRouter = ReturnType<typeof createAppRouter>;
