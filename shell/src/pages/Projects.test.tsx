import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Projects } from "./Projects";
import { createAppQueryClient } from "../app/queryClient";
import type { InspectEntry, InspectMatch, ProjectRules } from "../data/projects";
import type { ProjectSummary } from "../data/system";
import { daemonState, project, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function rules(overrides: Partial<ProjectRules> = {}): ProjectRules {
  return {
    project_id: "alpha",
    project_root: "C:/repos/alpha",
    rules_file: "present",
    rules_error: null,
    gate_command: "cargo test -p nucleos-core",
    gate_before_publish: false,
    schedules: [],
    repo_triggers: [],
    wip_limit: null,
    open_proposals: 0,
    queue_full: false,
    ...overrides,
  };
}

/* ----------------------------------------------------------- the daemon -- */

interface ProjectsWorld {
  projects: ProjectSummary[];
  rules: ProjectRules;
  entries: InspectEntry[];
  matches: InspectMatch[];
  file: string;
  diff: string;
}

function projectsWorld(overrides: Partial<ProjectsWorld> = {}): ProjectsWorld {
  return {
    projects: [project({ project_id: "alpha", mode: "shadow", project_root: "C:/repos/alpha" })],
    rules: rules(),
    entries: [
      { name: "src", is_dir: true },
      { name: "Cargo.toml", is_dir: false },
    ],
    matches: [],
    file: "",
    diff: "",
    ...overrides,
  };
}

/**
 * The inspector's routes, over mutable state.
 *
 * Two mocks and not one, because the page really does use two clients: `ls`,
 * `grep`, `rules` and the WIP write are JSON, while `cat` and `diff` come back
 * as a bare `String` from the núcleo and go through `apiText`. A test that
 * answered all six through `apiFetch` would pass while the page shipped a JSON
 * parse error over every file it opened.
 */
function projectsFetch(world: ProjectsWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonState();
  return async (path, init) => {
    if (init?.method === "POST") {
      if (path === "/projects/alpha/wip-limit") {
        const body = JSON.parse(String(init.body)) as { limit: number | null };
        world.rules = { ...world.rules, wip_limit: body.limit };
        world.projects = world.projects.map((row) => ({ ...row, wip_limit: body.limit }));
        // 204: the route answers a status and nothing else.
        return undefined;
      }
      return undefined;
    }

    if (path === "/projects") return world.projects;
    if (path === "/autopilot/kill") return shared.kill;
    if (path === "/autopilot/budget") return shared.budget;
    if (path === "/proposals") return shared.proposals;
    if (path.endsWith("/rules")) return world.rules;
    if (path.includes("/ls?")) return world.entries;
    if (path.includes("/grep?")) return world.matches;
    return undefined;
  };
}

function projectsText(world: ProjectsWorld): (path: string) => Promise<string> {
  return async (path) => {
    if (path.includes("/cat?")) return world.file;
    if (path.endsWith("/diff")) return world.diff;
    // The connection gate's own read, for the one case that mounts the app.
    return "daemon running";
  };
}

function answerWith(world: ProjectsWorld): void {
  daemon.apiFetch.mockImplementation(projectsFetch(world));
  daemon.apiText.mockImplementation(projectsText(world));
}

/**
 * The page under a router that knows its two routes, and nothing else.
 *
 * `renderWithRouter` builds its tree from `NAV_PATHS`, which has `/projects` and
 * *not* `/projects/$projectId/$view` — so a page whose whole subject comes out
 * of a route parameter would render with no parameter at all under it. The
 * alternative is `renderApp` per test, and a full-app mount costs the gate, the
 * rail and three live queries around every assertion; this machine cannot pay
 * that five times in one file. So the two routes are built here, from the real
 * component, and the case that proves the app's own tree registers them mounts
 * the app once at the bottom of this file.
 */
async function renderProjects(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/projects", component: Projects }),
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/projects/$projectId/$view",
      component: Projects,
    }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/* ------------------------------------------------- A18: the WIP ceiling -- */

describe("Projects - the work-in-progress ceiling", () => {
  it("reads a null ceiling as the brake being off, never as zero", async () => {
    answerWith(projectsWorld({ rules: rules({ wip_limit: null }) }));

    await renderProjects("/projects/alpha/rules");

    // The sentence says off, and says what off is not.
    const state = await screen.findByText(/no ceiling at all/);
    expect(state.textContent).toMatch(/not the same as a ceiling of zero/);

    // And the field is empty rather than seeded with 0 — the one keystroke
    // between "no brake" and "never start anything again".
    const field = (await screen.findByLabelText("Ceiling")) as HTMLInputElement;
    expect(field.value).toBe("");
  });

  it("sets a ceiling and clears it, sending null rather than zero to clear", async () => {
    const world = projectsWorld({ rules: rules({ wip_limit: null }) });
    answerWith(world);

    await renderProjects("/projects/alpha/rules");

    const field = (await screen.findByLabelText("Ceiling")) as HTMLInputElement;
    fireEvent.change(field, { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Set the ceiling" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/wip-limit", {
        method: "POST",
        body: JSON.stringify({ limit: 3 }),
      });
    });
    // The write is read back, not assumed: the panel refetches and says so.
    expect(await screen.findByText(/with 0 open/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Remove the ceiling" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/wip-limit", {
        method: "POST",
        body: JSON.stringify({ limit: null }),
      });
    });
    // `null` and never `0`: the daemon compares `open >= limit`, so a zero would
    // read like a number somebody chose and mean the opposite of removing it.
    const bodies = daemon.apiFetch.mock.calls
      .filter(([path]) => String(path) === "/projects/alpha/wip-limit")
      .map(([, init]) => String((init as RequestInit).body));
    expect(bodies).not.toContain(JSON.stringify({ limit: 0 }));
    expect(await screen.findByText(/no ceiling at all/)).toBeDefined();
  });
});

/* -------------------------------------- A19: a rules file that would not parse -- */

describe("Projects - a rules file that cannot be read", () => {
  it("surfaces the núcleo's own parse error as first-class information", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          rules_file: "unreadable",
          rules_error:
            "unknown field `schedule`, expected one of `schedules`, `repo_triggers`, `gate` at line 3 column 1",
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    // Loud, because the consequence is silent: a strict parse failure stops all
    // autonomy for the project and used to reach only a log line.
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/doing nothing on its own/)).toBeDefined();

    // The daemon's message verbatim — it names the key and the line, and any
    // paraphrase would lose exactly that.
    expect(within(alert).getByText(/unknown field `schedule`/)).toBeDefined();
    expect(within(alert).getByText(/line 3 column 1/)).toBeDefined();

    // And the empty rule lists below are explained rather than read as "there
    // are none": nothing could be loaded, which is a different fact.
    expect(within(alert).getByText(/none could be loaded/)).toBeDefined();
    expect(screen.getByText("unreadable")).toBeDefined();
  });

  it("does not call an absent rules file a fault", async () => {
    answerWith(projectsWorld({ rules: rules({ rules_file: "absent" }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/ordinary state and not a fault/)).toBeDefined();
    // The gitignored file is absent in every worktree and every fresh clone, so
    // it must not raise an alarm.
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("Projects - whether a merge waits for the gate", () => {
  it("says a merge does not wait, which is the default and the quiet one", async () => {
    answerWith(projectsWorld({ rules: rules({ gate_before_publish: false }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/Merges do not wait for it/)).toBeDefined();
  });

  it("says a merge waits, because a landing that takes twenty minutes needs a reason", async () => {
    answerWith(projectsWorld({ rules: rules({ gate_before_publish: true }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/Merges wait for it/)).toBeDefined();
  });

  // The state the daemon refuses every merge in. Drawn rather than left to be
  // inferred from two lines that each look fine on their own: a queue that turns
  // every landing away, over a key nobody can see, is the worst of the three.
  it("names the contradiction when a gate is asked for and none is configured", async () => {
    answerWith(
      projectsWorld({ rules: rules({ gate_before_publish: true, gate_command: null }) }),
    );

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/none is configured, so the queue refuses them/)).toBeDefined();
  });
});

/* ------------------------------------------- the rules a person cannot see -- */

describe("Projects - rules that are armed and inert", () => {
  it("says a rule never fires, why, and what its silence would otherwise look like", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          schedules: [
            {
              name: "nightly",
              cron: "0 3 * * *",
              prompt: "tidy the imports",
              cwd: null,
              // `null` is UTC, the scheduler's own default — not an unset field.
              timezone: null,
              next_fire_at: null,
              problem: "unknown timezone: Europe/Lisboa",
              last_fired_at: null,
              fires_today: 0,
              daily_cap: 4,
            },
          ],
          repo_triggers: [
            { name: "on-main", branch: "main", prompt: "run the gate", last_sha: null },
          ],
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    const schedules = await screen.findByRole("list", { name: "Scheduled rules" });
    expect(within(schedules).getByText("never fires")).toBeDefined();
    // The daemon logs this at debug 2,880 times a day and the rule silently does
    // nothing; the page is where that stops being invisible.
    expect(within(schedules).getByText("unknown timezone: Europe/Lisboa")).toBeDefined();
    expect(within(schedules).getByText("UTC")).toBeDefined();

    // Armed with no first commit to compare against fires nothing, by design —
    // and "armed" on its own would read as "will run".
    const triggers = screen.getByRole("list", { name: "Repo triggers" });
    expect(within(triggers).getByText("no commit seen yet")).toBeDefined();
    expect(within(triggers).getByText(/no commit to compare a new one against/)).toBeDefined();
  });
});

/* ---------------------------------------------------------------- the route -- */

describe("Projects - the route", () => {
  it("falls back to browse for a view nobody registered, rather than a dead end", async () => {
    answerWith(projectsWorld());

    await renderProjects("/projects/alpha/brwose");

    // A route parameter is a string and anybody can type one. A typo in a path
    // is not a missing page, so it lands on the view that needs no input.
    const browse = await screen.findByRole("link", { name: "Browse" });
    expect(browse.getAttribute("aria-current")).toBe("page");
    expect(await screen.findByRole("list", { name: "Folder contents" })).toBeDefined();
    expect(screen.queryByRole("list", { name: "Scheduled rules" })).toBeNull();
  });

  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    answerWith(projectsWorld());

    // The whole app here, and only here: a locally built router would prove
    // nothing about whether `/projects` is in the real tree.
    const { router } = await renderApp({ initialPath: "/projects" });

    expect(await screen.findByRole("heading", { level: 1, name: "Projects" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/projects");
    expect(screen.queryByText("Projects is not built yet")).toBeNull();
  });
});
