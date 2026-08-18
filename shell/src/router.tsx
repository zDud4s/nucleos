import type { ReactNode } from "react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { AppShell } from "./app/AppShell";
import { NAV_ITEMS, type NavItem } from "./app/nav";
import { Autopilot } from "./pages/Autopilot";
import { Chats } from "./pages/Chats";
import { Council } from "./pages/Council";
import { Feed, validateFeedSearch } from "./pages/Feed";
import { Fleet } from "./pages/Fleet";
import { Home } from "./pages/Home";
import { Placeholder } from "./pages/Placeholder";
import { Projects } from "./pages/Projects";
import { RunDetail } from "./pages/RunDetail";
import { Runs, validateRunSearch } from "./pages/Runs";
import { Waiting } from "./pages/Waiting";

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
 */
const PAGES: Record<string, () => ReactNode> = {
  "/": Home,
  "/fleet": Fleet,
  "/autopilot": Autopilot,
  "/waiting": Waiting,
  "/runs": Runs,
  "/feed": Feed,
  "/projects": Projects,
  "/chats": Chats,
  "/council": Council,
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
 * not an id: the view a project is being looked at through — `browse`, `search`,
 * `diff` or `rules`. It is in the location rather than in component state
 * because a folder somebody is reading should survive a reload and be
 * linkable, and it is **not** a search param because it is not a filter: there
 * is exactly one of it and it always has a value.
 *
 * There is no validator for it. A route parameter is a string, anybody can type
 * one, and `Projects` answers an unrecognised view with `browse` rather than a
 * dead end — a typo in a path is not a missing page. Registering a validator
 * here would move that decision away from the page that knows what the views
 * are.
 */
const DETAIL_ROUTES: { path: string; component: () => ReactNode }[] = [
  { path: "/runs/$runId", component: RunDetail },
  { path: "/projects/$projectId/$view", component: Projects },
  { path: "/chats/$chatId", component: Chats },
  { path: "/council/$councilId", component: Council },
];

export function createAppRouter(initialPath = "/") {
  /**
   * Built inside the factory rather than at module scope, and that matters:
   * `createRouter` initialises the route objects in place, so two routers
   * sharing one tree would be two routers fighting over the same instances.
   * Every test that mounts the app gets its own tree.
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
