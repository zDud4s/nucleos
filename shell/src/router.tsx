import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { AppShell } from "./app/AppShell";
import { NAV_ITEMS, type NavItem } from "./app/nav";
import { Home } from "./pages/Home";
import { Placeholder } from "./pages/Placeholder";

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
  const routes = NAV_ITEMS.map((item) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path: item.path,
      component: item.path === "/" ? Home : placeholderFor(item),
    }),
  );

  return createRouter({
    routeTree: rootRoute.addChildren(routes),
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
