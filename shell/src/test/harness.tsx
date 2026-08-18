import type { ReactNode } from "react";
import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { render, type RenderResult } from "@testing-library/react";
import { NAV_PATHS } from "../app/nav";
import { createAppQueryClient } from "../app/queryClient";
import { createAppRouter } from "../router";
import type { BudgetView, ProjectSummary, Proposal } from "../data/system";

/**
 * The one test wrapper.
 *
 * Every component in this app that is worth testing needs two things it cannot
 * make for itself — a query cache and a router — and every test that builds
 * them by hand builds them slightly differently. One wrapper means one set of
 * answers to "how long do queries retry for in a test" and "what does the
 * router think the current path is", and it means a change to either is a
 * change in one file.
 *
 * **The API seam is `data/client.ts` and nothing below it.** Tests replace
 * `apiFetch` / `apiText` / `probeHealth` and let the real hooks, the real cache
 * and the real error classes run. Stubbing `fetch` instead would put the
 * client's own refusal parsing inside the thing under test; stubbing the hooks
 * instead would test a mock's opinion of react-query. The seam is where the
 * shell stops being ours and starts being the daemon's.
 *
 * `vi.mock` is hoisted per file and cannot be moved in here, so each test file
 * declares three lines of its own:
 *
 * ```ts
 * vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
 * const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
 * vi.mock("../data/client", async (original) => ({
 *   ...(await original<typeof import("../data/client")>()),
 *   ...daemon,
 * }));
 * ```
 *
 * Spreading the original keeps `ApiRefusal` and `ApiUnavailable` as the real
 * classes, so an `instanceof` in a component matches an error a test threw.
 */

/**
 * jsdom implements no scrolling, and the router scrolls to the top of the
 * document on every navigation. Left alone, a suite that navigates prints a
 * "Not implemented: Window's scrollTo()" line per navigation and buries real
 * output. The absence is a fact about jsdom, not a fault in the router — the
 * same reason `test-setup.ts` fills in `scrollIntoView`.
 */
window.scrollTo = () => {};

/** What the fake daemon is holding. Mutable — a POST in a test changes it. */
export interface DaemonState {
  kill: { engaged: boolean };
  budget: BudgetView;
  projects: ProjectSummary[];
  proposals: Proposal[];
}

export function daemonState(overrides: Partial<DaemonState> = {}): DaemonState {
  return {
    kill: { engaged: false },
    budget: {
      limit_usd: 5,
      period: "daily",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.25,
      time_cost_per_hour_usd: 0,
      window_spend_usd: 1.42,
      hourly_spend_usd: 0.1,
      paused: false,
      reason: null,
    },
    projects: [],
    proposals: [],
    ...overrides,
  };
}

export function project(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    project_id: "alpha",
    mode: "off",
    project_root: null,
    pending: 0,
    classes_ready: 0,
    classes_total: 0,
    promotable: false,
    open_proposals: 0,
    wip_limit: null,
    queue_full: false,
    ...overrides,
  };
}

export function proposal(overrides: Partial<Proposal> = {}): Proposal {
  return {
    id: 1,
    kind: "action-approval",
    status: "pending",
    run_id: null,
    session_id: null,
    project_id: null,
    errand_id: null,
    errand_name: null,
    tool_name: null,
    reasoning: "",
    tool_input: null,
    created_at: "2026-08-17T09:00:00Z",
    decided_at: null,
    ...overrides,
  };
}

/**
 * A stand-in for the núcleo's JSON routes, over mutable state.
 *
 * A responder rather than a pile of `mockResolvedValueOnce`: the shell polls,
 * so every route is asked repeatedly and in an order nobody controls, and a
 * queue of one-shot answers runs out halfway through the second tick.
 */
export function daemonFetch(state: DaemonState): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (init?.method === "POST") {
      // The one write the shell can make from the frame. Applied to the state so
      // that the refetch after the mutation reads back what was written.
      if (path === "/autopilot/kill" && typeof init.body === "string") {
        state.kill = JSON.parse(init.body) as { engaged: boolean };
      }
      return undefined;
    }

    switch (path) {
      case "/autopilot/kill":
        return state.kill;
      case "/autopilot/budget":
        return state.budget;
      case "/projects":
        return state.projects;
      case "/proposals":
        return state.proposals;
      default:
        return undefined;
    }
  };
}

export interface HarnessOptions {
  initialPath?: string;
  queryClient?: QueryClient;
}

export interface HarnessResult extends RenderResult {
  router: { state: { location: { pathname: string } } };
  queryClient: QueryClient;
}

export interface QueryHarnessResult extends RenderResult {
  queryClient: QueryClient;
}

/**
 * Mount a component that needs the cache but not the router.
 *
 * A fresh `QueryClient` per test, from the same factory the app uses — so the
 * retry policy under test is the app's policy and not a test-only one. Sharing
 * a client between tests would carry one test's answers into the next and turn
 * an ordering bug into a passing suite.
 */
export function renderWithQuery(
  ui: ReactNode,
  options: { queryClient?: QueryClient } = {},
): QueryHarnessResult {
  const queryClient = options.queryClient ?? createAppQueryClient();
  const result = render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>);
  return { ...result, queryClient };
}

/**
 * Mount one component inside a real router that knows every real route.
 *
 * The route tree is built from `NAV_PATHS`, so a link in the component under
 * test resolves against the same paths the app has — a test that navigates
 * proves the destination exists, rather than proving a stub route was
 * registered next to it. The routes themselves render a marker: this helper is
 * for the *rail*, not for the pages.
 */
export async function renderWithRouter(ui: ReactNode, options: HarnessOptions = {}): Promise<HarnessResult> {
  const queryClient = options.queryClient ?? createAppQueryClient();

  const rootRoute = createRootRoute({
    component: () => (
      <>
        {ui}
        <Outlet />
      </>
    ),
  });
  const routes = NAV_PATHS.map((path) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <p data-testid="route-marker">{path}</p>,
    }),
  );
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [options.initialPath ?? "/"] }),
    defaultPreload: false,
  });

  // Settle the first match before rendering, so the first paint is the route
  // rather than the router's pending state.
  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );

  return { ...result, router, queryClient };
}

/**
 * Mount the whole app — gate, rail, footer and page — at a path.
 *
 * Uses `createAppRouter`, not a copy of it, so a route that is missing from the
 * real tree is missing here too.
 */
export async function renderApp(options: HarnessOptions = {}): Promise<HarnessResult> {
  const queryClient = options.queryClient ?? createAppQueryClient();
  const router = createAppRouter(options.initialPath ?? "/");

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );

  return { ...result, router, queryClient };
}
