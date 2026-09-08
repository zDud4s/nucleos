import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Autopilot } from "./Autopilot";
import { ApiRefusal } from "../data/client";
import type { ClassTally, ScopedKill, ShadowDecision } from "../data/autopilot";
import type { FeedEntry } from "../data/feed";
import type { Job } from "../data/fleet";
import type { AutopilotMode, ProjectSummary, Proposal } from "../data/system";
import { daemonState, project, renderApp, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  // Up and authorising, for the one case below that mounts the whole app and so
  // has to get past the connection gate.
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/**
 * The page inside a real router, and nothing else.
 *
 * `renderApp` would mount the gate, the rail and its three live queries around
 * every one of these assertions — a cost per test that buys nothing here, and
 * one this machine cannot pay five times over without pushing other suites past
 * their timeouts. The single case that needs the whole app is the one that
 * proves the route is registered, and it says so where it does it.
 */
function renderCockpit() {
  return renderWithRouter(<Autopilot />, { initialPath: "/autopilot" });
}

/* ------------------------------------------------------------- fixtures -- */

function tally(overrides: Partial<ClassTally> = {}): ClassTally {
  return {
    mode: "shadow",
    action_class: "read-local",
    total: 12,
    would_allow: 12,
    would_pend: 0,
    would_deny: 0,
    reviewed: 11,
    agree: 11,
    disagree: 0,
    ...overrides,
  };
}

function decision(overrides: Partial<ShadowDecision> = {}): ShadowDecision {
  return {
    id: 301,
    run_id: 44,
    tool_name: "Bash",
    tool_input: JSON.stringify({ command: "git push origin main" }),
    decision: "pending_approval",
    reason: "pushing is outside what this run was asked to do",
    action_class: "push-merge-deploy",
    classifier_version: 7,
    human_verdict: null,
    reviewed_at: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/* ------------------------------------------------------------ the daemon -- */

interface CockpitWorld {
  projects: ProjectSummary[];
  proposals: Proposal[];
  scopedKills: ScopedKill[];
  decisions: ShadowDecision[];
  scoreboard: ClassTally[];
  jobs: Job[];
  feed: FeedEntry[];
  /** What `POST /autopilot/state` refuses with, when it refuses. */
  refuseMode: ApiRefusal | null;
}

function cockpitWorld(overrides: Partial<CockpitWorld> = {}): CockpitWorld {
  return {
    projects: [],
    proposals: [],
    scopedKills: [],
    decisions: [],
    scoreboard: [],
    jobs: [],
    feed: [],
    refuseMode: null,
    ...overrides,
  };
}

/**
 * The cockpit's routes, over mutable state.
 *
 * A local switch rather than an edit to `test/harness.tsx`, for the reason the
 * queue's suite gives: the harness is the shared floor, and a page that teaches
 * it nine routes of its own makes every other suite carry them.
 *
 * The POST arm is the interesting half. `POST /autopilot/state` answers **200
 * with a body** on success and a **bare 422 with an empty body** when a
 * prerequisite is missing, and both of those are what the page is built around
 * — so the responder models both rather than resolving `undefined` for every
 * write.
 */
function cockpitFetch(world: CockpitWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonState();
  return async (path, init) => {
    if (init?.method === "POST") {
      if (path === "/autopilot/state") {
        if (world.refuseMode !== null) throw world.refuseMode;
        const body = JSON.parse(String(init.body)) as { project_id: string; mode: AutopilotMode };
        world.projects = world.projects.map((row) =>
          row.project_id === body.project_id ? { ...row, mode: body.mode } : row,
        );
        return { project_id: body.project_id, mode: body.mode };
      }
      if (path === "/autopilot/kill/scoped") {
        const body = JSON.parse(String(init.body)) as ScopedKill;
        world.scopedKills = [
          ...world.scopedKills.filter(
            (row) => !(row.scope_type === body.scope_type && row.scope_id === body.scope_id),
          ),
          body,
        ];
        return undefined;
      }
      // 204 everywhere else on this page — the verdict door included.
      return undefined;
    }

    if (path === "/autopilot/kill") return shared.kill;
    if (path === "/autopilot/budget") return shared.budget;
    if (path === "/projects") return world.projects;
    if (path === "/proposals") return world.proposals;
    if (path === "/autopilot/kill/scoped") return world.scopedKills;
    if (path.startsWith("/scoreboard")) return world.scoreboard;
    if (path.startsWith("/shadow-decisions")) return world.decisions;
    if (path.startsWith("/jobs")) return world.jobs;
    if (path.startsWith("/feed")) return world.feed;
    return undefined;
  };
}

/** The list item a named switch belongs to. */
function switchFor(label: string): HTMLElement {
  const list = screen.getByRole("list", { name: "Trigger brakes" });
  const row = within(list).getByText(label).closest("li");
  if (row === null) throw new Error(`no switch row for ${label}`);
  return row;
}

/* ------------------------------------------- A17: the promotion 422 -- */

describe("Autopilot - a refused promotion asks for the one thing the shell can supply", () => {
  it("opens the project-root input on a bare 422 and does not invent a cause", async () => {
    const world = cockpitWorld({
      projects: [
        project({
          project_id: "alpha",
          mode: "shadow",
          project_root: null,
          promotable: true,
          classes_ready: 2,
          classes_total: 2,
          withheld_classes_ready: 1,
        }),
      ],
      // Exactly what `client.ts` builds out of a 422 with an empty body: the
      // status text, which is the code spelled with capital letters.
      refuseMode: new ApiRefusal(422, "unprocessable", "Unprocessable Entity"),
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    // Nothing asks for a folder until something says one is missing.
    expect(await screen.findByRole("button", { name: "Let it act" })).toBeDefined();
    expect(screen.queryByLabelText("Folder for alpha")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));
    // Clicks inside the 300 ms dwell are swallowed and leave the control armed,
    // so retrying until it disarms is safe. The mutation lands a tick later, so
    // it is a separate wait.
    await waitFor(() => {
      const armed = screen.queryByRole("button", { name: "It may act on its own" });
      if (armed !== null) fireEvent.click(armed);
      expect(screen.queryByRole("button", { name: "It may act on its own" })).toBeNull();
    });

    const input = await screen.findByLabelText("Folder for alpha");

    // The message names the whole set and admits which one is unknown. Four
    // different prerequisites map onto this one bare status in the núcleo, so
    // naming a single cause would be wrong three times out of four.
    const said = screen.getByText(/did not say which prerequisite is missing/);
    expect(said.textContent).toMatch(/\.ai\/workflow\/workflow\.md/);
    expect(said.textContent).toMatch(/PreToolUse hook/);
    // And it did not pass the daemon's non-sentence off as an explanation.
    expect(said.textContent).not.toMatch(/Unprocessable Entity/);

    // The retry carries the folder that was typed, on the same mode that was
    // refused — without that, the input would be a box that does nothing.
    world.refuseMode = null;
    fireEvent.change(input, { target: { value: "C:/repos/alpha" } });
    fireEvent.click(screen.getByRole("button", { name: "Try again with this folder" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/state", {
        method: "POST",
        body: JSON.stringify({
          project_id: "alpha",
          mode: "active",
          project_root: "C:/repos/alpha",
        }),
      });
    });
  });

  it("a project that cannot be promoted says why in the row", async () => {
    const world = cockpitWorld({
      projects: [
        project({
          project_id: "beta",
          mode: "shadow",
          project_root: "C:/repos/beta",
          promotable: false,
          classes_ready: 0,
          classes_total: 0,
          withheld_classes_ready: 0,
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    const promote = await screen.findByRole("button", { name: "Let it act" });
    expect((promote as HTMLButtonElement).disabled).toBe(true);
    expect(promote.getAttribute("title")).toBeNull();
    expect(
      screen.getByText(
        /nothing has been recorded in shadow yet.*no evidence is not the same as good evidence/,
      ),
    ).toBeDefined();
  });

  it("the budget period is a word and not a stem", async () => {
    const world = cockpitWorld();
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(await screen.findByText(/of \$5\.00 per day/)).toBeDefined();
    expect(screen.queryByText(/per dai/)).toBeNull();
  });
});

/* ----------------------------------------- A17: the brake nobody reads -- */

describe("Autopilot - the trigger brakes say which of them the núcleo reads", () => {
  it("offers the team trigger brake as a live switch", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha" })],
      scopedKills: [{ scope_type: "trigger", scope_id: "scheduled", engaged: false }],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    await screen.findByRole("list", { name: "Trigger brakes" });
    const team = switchFor("Team triggers");

    // Read now, so a real state rather than the fixed "not read" label — and a
    // button, where before there was none.
    expect(within(team).getByText("running")).toBeDefined();
    expect(within(team).getByRole("button", { name: "Hold team triggers" })).toBeDefined();
    // The hedge said engaging this brake would stop nothing. It is no longer
    // true and must no longer be on screen.
    expect(within(team).queryByText(/nothing in it reads the value/)).toBeNull();
  });

  /**
   * A job with a team and a job without one ran the same list line, and they are
   * not the same thing: one is a queue in a single checkout, the other is a
   * checkout per item with several moving at once. The ceiling travels with the
   * name because the name alone does not say what having a team buys.
   */
  it("says which jobs in flight a team is directing, and how wide they may go", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha" })],
      jobs: [
        {
          id: 41,
          project_id: "alpha",
          rule_name: null,
          status: "implementing",
          wait_reason: null,
          max_items: 4,
          created_at: "2026-08-21T09:00:00Z",
          completed_at: null,
          slot: 0,
          round: 0,
          max_rounds: 3,
          team_id: "infra",
          team_name: "Infra",
          team_max_parallel: 3,
        },
      ],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    const list = await screen.findByRole("list", { name: "Jobs in flight" });
    expect(within(list).getByText(/Infra, up to 3 at once/)).toBeDefined();
  });

  it("holds and releases the team trigger scope", async () => {
    const world = cockpitWorld({ projects: [project({ project_id: "alpha" })] });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    await screen.findByRole("list", { name: "Trigger brakes" });
    const team = switchFor("Team triggers");
    fireEvent.click(within(team).getByRole("button", { name: "Hold team triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "team", engaged: true }),
      });
    });

    // An absent row means *not engaged*, so the switch had to be able to add one
    // rather than only patch one — same as the other three scopes.
    expect(await within(team).findByRole("button", { name: "Release team triggers" })).toBeDefined();

    fireEvent.click(within(team).getByRole("button", { name: "Release team triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "team", engaged: false }),
      });
    });
  });

  it("engages a scope that is read, and sends the scope the núcleo checks", async () => {
    const world = cockpitWorld({ projects: [project({ project_id: "alpha" })] });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    fireEvent.click(await screen.findByRole("button", { name: "Hold repo triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "repo", engaged: true }),
      });
    });

    // An absent row means *not engaged*, so the switch had to be able to add one
    // rather than only patch one — the first engagement is the common case.
    expect(await screen.findByRole("button", { name: "Release repo triggers" })).toBeDefined();
  });
});

/* --------------------------------------------------- the scoreboard's honesty -- */

describe("Autopilot - the scoreboard is read-only and says what it is not", () => {
  it("separates shadow evidence from enforced decisions and disclaims its own counts", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha", mode: "shadow", classes_ready: 1, classes_total: 2 })],
      decisions: [decision()],
      scoreboard: [
        tally({ mode: "shadow", action_class: "read-local" }),
        tally({ mode: "worktree", action_class: "vcs-local", reviewed: 0, agree: 0 }),
      ],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(await screen.findByRole("table", { name: "Shadow evidence" })).toBeDefined();
    expect(screen.getByRole("table", { name: "Enforced decisions" })).toBeDefined();

    // The distinction that makes this panel honest rather than decorative: the
    // bar counts reviews distinct by tool and arguments, this table counts rows,
    // and the two are not the same number.
    expect(screen.getByText(/the count that decides it is not the count below/)).toBeDefined();
    // The authority stays the daemon's own figure.
    expect(screen.getByText("1/2 classes ready")).toBeDefined();
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Autopilot - the route", () => {
  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld()));

    // The whole app here, and only here: a stubbed destination would prove
    // nothing about whether `/autopilot` is in the real tree.
    const { router } = await renderApp({ initialPath: "/autopilot" });

    expect(await screen.findByRole("heading", { level: 1, name: "Autopilot" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/autopilot");
    expect(screen.queryByText("Autopilot is not built yet")).toBeNull();
  });
});
