import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Runs } from "./Runs";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import type { Preset } from "../data/presets";
import type { RunDetail, RunSearchResult } from "../data/runs";
import { money } from "../ui";
import { daemonFetch, daemonState, project, renderApp, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  // A daemon that is up and authorising, for the one test below that mounts the
  // whole app and therefore has to get past the handshake.
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/**
 * The page inside a real router, and nothing else.
 *
 * `renderApp` would mount the connection gate, the rail and its three live
 * queries around every one of these assertions — a cost per test that buys
 * nothing here, and one this machine cannot pay nine times over without pushing
 * other suites past their timeouts. The one test that genuinely needs the whole
 * app is the one that navigates to a run, and it says so where it does it.
 */
function renderRuns(path = "/runs") {
  return renderWithRouter(<Runs />, { initialPath: path });
}

/* ------------------------------------------------------------- fixtures -- */

function row(overrides: Partial<RunSearchResult> = {}): RunSearchResult {
  return {
    id: 7,
    project_id: "alpha",
    status: "completed",
    mode: "real",
    created_at: "2026-08-17T09:00:00Z",
    completed_at: "2026-08-17T09:02:00Z",
    cost_usd: 0.0123,
    prompt_excerpt: "tidy the imports",
    ...overrides,
  };
}

function detail(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: 77,
    project_id: "alpha",
    status: "completed",
    gate_status: "passed",
    gate_exit_code: 0,
    gate_output: null,
    exit_code: 0,
    stdout: "done",
    stderr: null,
    session_id: "s-1",
    cost_usd: 0.5,
    input_tokens: 1200,
    output_tokens: 300,
    cache_read_tokens: 8000,
    num_turns: 3,
    context_fill: 40_000,
    steerable: false,
    successor_run_id: null,
    ...overrides,
  };
}

function preset(overrides: Partial<Preset> = {}): Preset {
  return {
    id: 3,
    name: "nightly",
    prompt: "run the nightly tidy",
    project_id: "alpha",
    cwd: null,
    mode: "worktree",
    created_at: "2026-08-17T09:00:00Z",
    updated_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/**
 * The parsed search params off the router.
 *
 * The shared harness types its router down to `pathname` — everything built on
 * it so far only navigated — while the real object carries the validated search
 * beside it. Read through a widened view here rather than by widening the
 * harness, which is the shared floor and not this packet's to change.
 */
function searchOf(router: { state: { location: { pathname: string } } }): Record<string, unknown> {
  return (router.state.location as unknown as { search: Record<string, unknown> }).search;
}

interface RunsWorld {
  rows: RunSearchResult[];
  presets: Preset[];
  details: Record<number, RunDetail>;
}

function world(overrides: Partial<RunsWorld> = {}): RunsWorld {
  return { rows: [], presets: [], details: {}, ...overrides };
}

/**
 * The runs slice's routes, over the foundation's responder.
 *
 * A local switch rather than an edit to `test/harness.tsx`, for the reason the
 * fleet's suite gives: the harness is the shared floor, and a page that teaches
 * it six routes of its own makes every other suite carry them. Anything this
 * does not know falls through, so `/projects` and `/autopilot/budget` still
 * answer.
 */
function runsFetch(state: RunsWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(
    daemonState({ projects: [project({ project_id: "alpha" }), project({ project_id: "beta" })] }),
  );
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    if (path.startsWith("/runs?")) return state.rows;
    if (path === "/presets") return state.presets;
    const one = /^\/runs\/(\d+)$/.exec(path);
    if (one !== null) return state.details[Number(one[1])];
    // A 204 comes back through `apiFetch` as `undefined`, which is what "this
    // run has no live tail" looks like from the shell.
    if (/^\/runs\/\d+\/tail/.test(path)) return undefined;
    return await shared(path, init);
  };
}

/* -------------------------------------------------------------- filters -- */

describe("Runs - the filters live in the route", () => {
  it("writes a filter change into the search params, and the query key follows", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    const { router, queryClient } = await renderRuns();
    await screen.findByRole("heading", { level: 1, name: "Runs" });
    // Nothing is filtered to begin with, and the ceiling is asked for out loud
    // rather than left to the daemon's default.
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?limit=50");

    fireEvent.change(await screen.findByLabelText("Filter by status"), {
      target: { value: "failed" },
    });

    // In the *location*, which is what makes a filtered list linkable and what
    // lets it survive opening a run and coming back. Component state would lose
    // it on every remount.
    await waitFor(() => {
      expect(searchOf(router)).toEqual({ status: "failed" });
    });

    // And the cache followed, under a key built from the same object the route
    // validated. Two filter sets sharing one entry is exactly how a list ends up
    // showing the previous filter's rows.
    await waitFor(() => {
      const entry = queryClient
        .getQueryCache()
        .find({ queryKey: keys.runs.search({ status: "failed" }), exact: true });
      expect(entry).toBeDefined();
    });
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?status=failed&limit=50");
  });

  it("drops a filter that was cleared instead of asking for the empty string", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    const { router } = await renderRuns();
    fireEvent.change(await screen.findByLabelText("Filter by mode"), {
      target: { value: "worktree" },
    });
    await waitFor(() => expect(searchOf(router)).toEqual({ mode: "worktree" }));

    fireEvent.change(screen.getByLabelText("Filter by mode"), { target: { value: "" } });

    // `?mode=` would ask the daemon for runs whose mode is the empty string and
    // get an empty list, which reads on screen as "there are no runs".
    await waitFor(() => expect(searchOf(router)).toEqual({}));
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/runs?mode=&limit=50");
  });
});

/* ----------------------------------------------------------------- list -- */

describe("Runs - the index", () => {
  it("shows a row per run and says when the list arrived at its ceiling", async () => {
    const many = Array.from({ length: 50 }, (_, index) => row({ id: index + 1 }));
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: many })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    expect(list.children.length).toBe(50);
    // A list that came back exactly at the ceiling may have been cut, and saying
    // so is the difference between "that is everything" and "that is the first
    // fifty".
    expect(screen.getByText(/showing the newest 50/)).toBeDefined();
  });

  it("keeps an empty filtered list apart from an empty machine", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [] })));

    await renderRuns("/runs?status=failed");

    expect(await screen.findByText(/Nothing matches those filters/)).toBeDefined();
    expect(screen.queryByText(/No runs yet/)).toBeNull();
  });

  it("the excerpt is the row's link and the run id is not one", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    await renderRuns();

    // What a run IS is the request it was given; the id is a handle you use
    // once you already know which run you want.
    const link = await screen.findByRole("link", { name: "tidy the imports" });
    expect(link.getAttribute("href")).toBe("/runs/7");
    expect(screen.queryByRole("link", { name: /run 7/ })).toBeNull();
    // The id is still on the row — demoted to metadata, not dropped.
    const list = screen.getByRole("list", { name: "Runs" });
    expect(list.textContent).toContain("run 7");
  });

  it("every row states id, mode, project, time and cost on one metadata line", async () => {
    const rows = [row({ id: 7 }), row({ id: 8, status: "failed" })];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    expect(list.children.length).toBe(2);
    for (const item of Array.from(list.children)) {
      const metas = item.querySelectorAll(".runs-row-meta");
      // Exactly one: two metadata lines on one row is how a column stops
      // lining up with the row above it.
      expect(metas.length).toBe(1);
      const cells = Array.from(metas[0].children).map((cell) => cell.className);
      expect(cells).toEqual([
        "runs-row-id",
        "runs-row-mode",
        "runs-row-project",
        "ui-reading-time",
        "runs-row-cost",
      ]);
    }
  });

  it("a run the ledger never priced says so, and a priced one is money", async () => {
    const rows = [row({ id: 7, cost_usd: null }), row({ id: 8, cost_usd: 0.0123 })];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    const costs = Array.from(list.querySelectorAll(".runs-row-cost")).map(
      (cell) => cell.textContent,
    );
    // Absent is not zero: "$0.00" would claim the run was free, which is a
    // different fact from nobody having priced it.
    expect(costs).toEqual(["cost not recorded", money(0.0123)]);
  });

  it("the header names the run waiting on a person, and links to it", async () => {
    const rows = [
      row({ id: 7, status: "running" }),
      row({ id: 8, status: "pending" }),
      row({ id: 9, status: "awaiting_approval" }),
    ];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    // Read through the link: `getByText` matches direct text-node children
    // only, and the clause the header must not hide is inside an anchor.
    const link = await screen.findByRole("link", { name: "1 waiting on you" });
    expect(link.getAttribute("href")).toBe("/waiting");
    expect(link.closest("p")?.textContent).toBe(
      "3 in the index; 2 still moving; 1 waiting on you",
    );
  });
});

/* --------------------------------------------------------- new run: no -- */

describe("Runs - when the nucleo refuses to start one", () => {
  async function refuseStartWith(refusal: ApiRefusal): Promise<void> {
    const answer = runsFetch(world());
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/runs" && init?.method === "POST") throw refusal;
      return await answer(path, init);
    });

    await renderRuns();
    fireEvent.change(await screen.findByLabelText("What the run should do"), {
      target: { value: "tidy the imports" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start run" }));
  }

  it("reads a 409 as the kill switch, which is what the daemon actually sends", async () => {
    // Verified in `core/src/runs.rs`: `kill_switch_engaged` -> `Ok(true)` returns
    // CONFLICT. The old shell's comment said 503 for this and was wrong.
    await refuseStartWith(new ApiRefusal(409, "conflict", ""));

    const said = await screen.findByText(/kill switch is engaged/);
    // Not the shared floor's sentence for a 409 ("something about this has
    // already changed"), which explains nothing and offers no remedy.
    expect(said.textContent).not.toMatch(/already changed/);
  });

  it("reads a 503 as a switch that could not be read, not as an outage", async () => {
    // The same route's `Err(_)` arm: it fails CLOSED, so an unreadable switch
    // stops a run rather than starting one. "The part of the núcleo this needs
    // is not available" would send somebody to restart a daemon that is
    // answering perfectly well.
    await refuseStartWith(new ApiRefusal(503, "unavailable", ""));

    const said = await screen.findByText(/could not be read/);
    expect(said.textContent).not.toMatch(/is not available/);
  });

  it("has no sentence of its own for a 429, because this route never sends one", async () => {
    // `create_run` brakes on the kill switch and on a full project. It does not
    // brake on the budget: the ceilings pace proactive autonomy, and a person
    // sitting at the window asking for a run is not that. A branch here would
    // send people to raise a ceiling that is holding nothing.
    await refuseStartWith(new ApiRefusal(429, "too_many_requests", ""));

    const said = await screen.findByText(/a ceiling is holding this back/);
    // The shared floor's own words, verbatim - the page contributed nothing.
    expect(said.textContent).toBe("a ceiling is holding this back");
  });
});

/* -------------------------------------------------------------- presets -- */

describe("Runs - presets", () => {
  it("says the name is taken when a preset already has it", async () => {
    const answer = runsFetch(world());
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/presets" && init?.method === "POST") {
        throw new ApiRefusal(409, "conflict", "");
      }
      return await answer(path, init);
    });

    await renderRuns();
    fireEvent.change(await screen.findByLabelText("What the run should do"), {
      target: { value: "tidy the imports" },
    });
    fireEvent.change(screen.getByLabelText("Name for the preset"), {
      target: { value: "nightly" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save as preset" }));

    // The remedy is to type a different name, which is not something anybody
    // works out from the word "conflict".
    const said = await screen.findByText(/that name is taken/);
    expect(said.textContent).not.toMatch(/already changed/);
  });

  it("goes to the run a preset started", async () => {
    const answer = runsFetch(
      world({ presets: [preset({ id: 3, name: "nightly" })], details: { 77: detail({ id: 77 }) } }),
    );
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/presets/3/run" && init?.method === "POST") return { id: 77 };
      return await answer(path, init);
    });

    // The whole app here, and only here: this is the assertion that the detail
    // route is really registered, so a stubbed destination would prove nothing.
    const { router } = await renderApp({ initialPath: "/runs" });
    fireEvent.click(await screen.findByRole("button", { name: "Run nightly" }));

    // A preset that started something and said nothing about where it went is a
    // button with no visible effect. `findBy` and not `getBy`: the navigation
    // lands through react-query's zero-delay notification, one tick after the
    // mutation settles.
    expect(await screen.findByRole("heading", { level: 1, name: "Run 77" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/runs/77");
  });
});
