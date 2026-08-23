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
import { ApiRefusal } from "../data/client";
import type { Concurrency, HeldSlot, ProjectConcurrency } from "../data/fleet";
import type { ClassTally } from "../data/autopilot";
import type { Changed, Worktree } from "../data/project-code";
import type { Claim } from "../data/project-config";
import type { Branches, Commit } from "../data/project-git";
import type { ProjectReadings } from "../data/project-readings";
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
  /**
   * Capacity, which the workspace reads as its occupancy panel.
   *
   * Defaults to an empty house rather than being absent, because absent is not
   * a thing this route does: `concurrency::readout` unions the roster with
   * everything holding a slot precisely so that capacity can never vanish
   * quietly, and a fake that could return nothing would let a test pass against
   * a shape the daemon cannot produce.
   */
  concurrency: Concurrency;
  /**
   * A project's four readings.
   *
   * One payload for every project rather than a map: no test so far needs two projects to read
   * differently, and a map would be scaffolding for a case nobody has.
   *
   * The default is a project with nothing behind it — every count zero, every median `null` — which
   * is both the honest empty answer and the state most worth having under a test by default, since
   * it is what a new project looks like for its first month.
   */
  readings: ProjectReadings;
  /** A project's local branches and the one they are measured against. */
  branches: Branches;
  /** A project's recent commits. */
  log: Commit[];
  /**
   * What one run changed, and where its checkout is.
   *
   * `null` means the route refuses — which is a state with three different meanings on the wire and
   * has to be reachable from a test, because the panel says a different sentence for each.
   */
  changed: Changed | null;
  worktree: Worktree | null;
  /**
   * The text routes, by path prefix.
   *
   * `diff` and `cat` return a body that is not JSON, and an **empty** body is a real answer to both
   * — a clean tree, an empty file. A responder that could not distinguish "empty" from "absent"
   * would make the most common answer untestable.
   */
  text: { diff: string; cat: string | null };
  /**
   * The write boundary this project's daemon declares.
   *
   * A list rather than a flag, because the page must not know the fence by heart: an editor that
   * appeared for a hard-coded path would keep appearing after the daemon stopped granting it. A
   * test can hand back an empty list, which is a real answer — a project the app authors nothing
   * in.
   */
  ownership: Claim[];
  /** What the classifier has been judged on, class by class. */
  scoreboard: ClassTally[];
  /**
   * Every write the shell has made, in order, so a test can assert what was SENT rather than what
   * the component thinks it sent.
   */
  writes: { path: string; contents: string }[];
  /**
   * What `POST /projects/{id}/write` refuses with, or `null` to accept.
   *
   * Refusals are the interesting half of this route — five of them, each a different sentence — and
   * they are reachable only if the fake can be told to give one.
   */
  writeRefusal: { status: number; code: string; detail: string } | null;
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
    concurrency: { house: { limit: 4, held: 0 }, projects: [] },
    readings: readings(),
    branches: { integration: "master", branches: [], omitted: 0 },
    log: [],
    changed: { paths: [], tracked: 0 },
    worktree: {
      path: "C:/Projects/nucleos-run-41",
      branch: "feat/x",
      base_sha: "a".repeat(40),
      created_at: "2026-08-23T09:00:00Z",
    },
    text: { diff: "", cat: "" },
    ownership: [
      {
        path: ".ai/autopilot.yaml",
        owner: "core",
        what: "what this project does on its own, and the gate command that decides what green means",
      },
    ],
    scoreboard: [],
    writes: [],
    writeRefusal: null,
    ...overrides,
  };
}

/**
 * A project holding some slots, as `/concurrency` reports it.
 *
 * Constructors rather than object literals in each test, because `HeldSlot` carries five fields no
 * test cares about — `ordinal`, `item_status`, and the collision pair — and a literal that omits
 * them typechecks nowhere while a literal that includes them is four lines of noise per slot.
 *
 * `not_measured` for both collision sources, which is the honest default: nothing computed an
 * overlap for a fixture, and `clean` is the one answer that must never be given in vain.
 */
export function heldSlots(projectId: string, limit: number, slots: HeldSlot[]): ProjectConcurrency {
  return {
    project_id: projectId,
    limit,
    slots,
    collision: {
      declared: { state: "not_measured", overlaps: [] },
      observed: { state: "not_measured", overlaps: [] },
    },
  };
}

/** One taken slot. `run` by default, which is the only kind the Code mode can open. */
export function slot(overrides: Partial<HeldSlot> = {}): HeldSlot {
  return {
    project_id: "alpha",
    slot: 1,
    owner_kind: "run",
    owner_id: 1,
    claimed_at: "2026-08-23T09:00:00Z",
    job_id: null,
    ordinal: null,
    item_status: null,
    ...overrides,
  };
}

/** A project's readings, empty unless a test says otherwise. */
export function readings(overrides: Partial<ProjectReadings> = {}): ProjectReadings {
  return {
    window_days: 30,
    efficiency: {
      measured_runs: 0,
      unmeasured_runs: 0,
      median_total_tokens: null,
      previous_median_total_tokens: null,
    },
    cost: { usd: 0, runs: 0 },
    gate: { passed: 0, failed: 0, errored: 0, no_gate: 0 },
    delivered: { landed: 0, timed: 0, median_minutes: null },
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
    read_from: null,
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
      if (path.includes("/write") && typeof init.body === "string") {
        if (state.writeRefusal !== null) {
          const { status, code, detail } = state.writeRefusal;
          throw new ApiRefusal(status, code, detail);
        }
        const body = JSON.parse(init.body) as { path: string; contents: string };
        state.writes.push(body);
        // Applied to the text the read route serves, so the refetch after a save reads back what
        // was written — the same thing the daemon does, and the only way a test can tell a save
        // that landed from one that only looked like it did.
        state.text.cat = body.contents;
      }
      if (path.includes("/wip-limit") && typeof init.body === "string") {
        const { limit } = JSON.parse(init.body) as { limit: number | null };
        state.projects = state.projects.map((row) =>
          row.project_id === path.split("/")[2] ? { ...row, wip_limit: limit } : row,
        );
      }
      if (path === "/autopilot/state" && typeof init.body === "string") {
        const change = JSON.parse(init.body) as { project_id: string; mode: ProjectSummary["mode"] };
        state.projects = state.projects.map((row) =>
          row.project_id === change.project_id ? { ...row, mode: change.mode } : row,
        );
      }
      return undefined;
    }

    // Parameterised before the exact matches: the readings route carries a project id, which a
    // `switch` over literals cannot express.
    if (path.startsWith("/projects/") && path.endsWith("/readings")) return state.readings;
    if (path.startsWith("/projects/") && path.endsWith("/branches")) return state.branches;
    if (path.startsWith("/projects/") && path.endsWith("/ownership")) return state.ownership;
    if (path.startsWith("/scoreboard")) return state.scoreboard;
    if (path.startsWith("/projects/") && path.includes("/changed")) {
      if (state.changed === null) throw new ApiRefusal(422, "unprocessable", "no branch point");
      return state.changed;
    }
    if (path.startsWith("/projects/") && path.includes("/worktree")) {
      if (state.worktree === null) throw new ApiRefusal(404, "not_found", "gone");
      return state.worktree;
    }
    // The log route carries a query string, so it is matched on its segment rather than its end.
    if (path.startsWith("/projects/") && path.includes("/log")) return state.log;

    switch (path) {
      case "/autopilot/kill":
        return state.kill;
      case "/autopilot/budget":
        return state.budget;
      case "/projects":
        return state.projects;
      case "/proposals":
        return state.proposals;
      case "/concurrency":
        return state.concurrency;
      default:
        return undefined;
    }
  };
}

export interface HarnessOptions {
  initialPath?: string;
  queryClient?: QueryClient;
}

/**
 * The núcleo's text routes, over the same mutable state.
 *
 * A sibling of {@link daemonFetch} rather than part of it, because `apiText` and `apiFetch` are two
 * different functions on the seam and a test replaces them separately.
 */
export function daemonText(state: DaemonState): (path: string) => Promise<string> {
  return async (path) => {
    if (path.includes("/diff")) return state.text.diff;
    if (path.includes("/cat")) {
      // `null` is a file that is not there — a 404, and a different fact from an empty file. A
      // project with no rules file yet is the ordinary case, and the editor says a different
      // sentence for it.
      if (state.text.cat === null) throw new ApiRefusal(404, "not_found", "no such file");
      return state.text.cat;
    }
    return "";
  };
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
