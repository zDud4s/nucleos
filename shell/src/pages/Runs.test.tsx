import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Runs } from "./Runs";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import type { RouteReport } from "../data/route";
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

/**
 * The header's New run, which is the control that owns the panel.
 *
 * Picked by `aria-expanded` rather than by position: an empty index offers a
 * second "New run" inside its teaching block, and that one is a shortcut to the
 * same panel rather than its owner.
 */
async function newRunButton(): Promise<HTMLElement> {
  const buttons = await screen.findAllByRole("button", { name: "New run" });
  const owner = buttons.find((button) => button.hasAttribute("aria-expanded"));
  if (owner === undefined) throw new Error("no New run button owns the panel");
  return owner;
}

async function openNewRun(): Promise<void> {
  fireEvent.click(await newRunButton());
  await screen.findByRole("heading", { name: "New run" });
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
    // The daemon sends both on every run response, so the fixture carries both.
    // Nothing on the index reads them — `RunDetail` is the detail route's shape
    // and this page fetches it for the launcher's sake — but a fixture that
    // omitted a field the type promises is a row that could not come back.
    authored_prompt_estimate: null,
    cli_own_estimate: null,
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
  /** What `/autopilot/kill` answers. */
  killEngaged: boolean;
  /** What `/route/report` answers; `undefined` is an older daemon's 404. */
  report?: RouteReport;
}

function world(overrides: Partial<RunsWorld> = {}): RunsWorld {
  return { rows: [], presets: [], details: {}, killEngaged: false, ...overrides };
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
    daemonState({
      projects: [project({ project_id: "alpha" }), project({ project_id: "beta" })],
      kill: { engaged: state.killEngaged },
    }),
  );
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    if (path.startsWith("/runs?")) return state.rows;
    if (path.startsWith("/route/report")) {
      if (state.report === undefined) throw new ApiRefusal(404, "not_found", "no such route");
      return state.report;
    }
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
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?group=chat&limit=50");

    fireEvent.click(screen.getByRole("button", { name: "More filters" }));
    fireEvent.change(await screen.findByLabelText("Filter by status"), {
      target: { value: "timed_out" },
    });

    // In the *location*, which is what makes a filtered list linkable and what
    // lets it survive opening a run and coming back. Component state would lose
    // it on every remount.
    await waitFor(() => {
      expect(searchOf(router)).toEqual({ status: "timed_out" });
    });

    // And the cache followed, under a key built from the same object the route
    // validated. Two filter sets sharing one entry is exactly how a list ends up
    // showing the previous filter's rows.
    await waitFor(() => {
      const entry = queryClient
        .getQueryCache()
        .find({ queryKey: keys.runs.search({ status: "timed_out", group: "chat" }), exact: true });
      expect(entry).toBeDefined();
    });
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?status=timed_out&group=chat&limit=50");
  });

  it("drops a filter that was cleared instead of asking for the empty string", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    const { router } = await renderRuns();
    fireEvent.click(await screen.findByRole("button", { name: "More filters" }));
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

  it("a scope writes its status into the route and says which one is in force", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    const { router } = await renderRuns();
    const scopes = await screen.findByRole("group", { name: "Show" });
    const all = within(scopes).getByRole("button", { name: "All" });
    const failed = within(scopes).getByRole("button", { name: "Failed" });
    expect(all.getAttribute("aria-pressed")).toBe("true");
    expect(failed.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(failed);

    // The same route model as every other filter — a scope is a status, not a
    // second piece of state beside it.
    await waitFor(() => expect(searchOf(router)).toEqual({ status: "failed" }));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?status=failed&group=chat&limit=50");
    await waitFor(() => {
      expect(within(scopes).getByRole("button", { name: "Failed" }).getAttribute("aria-pressed")).toBe(
        "true",
      );
    });
    expect(within(scopes).getByRole("button", { name: "All" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("an exact status the scopes do not cover opens More filters on arrival", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    await renderRuns("/runs?status=cancelled");

    // Otherwise the list would be narrowed by a control nobody can see.
    const more = await screen.findByRole("button", { name: "More filters" });
    expect(more.getAttribute("aria-expanded")).toBe("true");
    // In the badge's words, not the daemon's literal.
    const status = screen.getByLabelText("Filter by status") as HTMLSelectElement;
    expect(status.selectedOptions[0]?.textContent).toBe("cancelled");
    expect(
      Array.from(status.options).some((option) => option.textContent === "awaiting approval"),
    ).toBe(true);
  });
});

/* -------------------------------------------------------- conversations -- */

/** A folded conversation row, as `GET /runs?group=chat` sends it: the latest turn's columns plus the aggregates. */
function conversation(overrides: Partial<RunSearchResult> = {}): RunSearchResult {
  return row({
    id: 90,
    mode: "assistant",
    chat_id: "c-1",
    chat_title: "Refactor the gate",
    turns: 40,
    running_turns: 1,
    status: "running",
    completed_at: null,
    cost_usd: 3.2,
    prompt_excerpt: "latest turn",
    ...overrides,
  });
}

describe("Runs - conversations", () => {
  it("folds a conversation into one row that opens the chat", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [conversation(), row({ id: 7 })] })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    expect(list.children.length).toBe(2);
    const link = screen.getByRole("link", { name: "Refactor the gate" });
    expect(link.getAttribute("href")).toBe("/chats/c-1");
    expect(screen.getByText("40 turns")).toBeDefined();
    expect(screen.getByText("1 running")).toBeDefined();
    expect(screen.getByText(money(3.2))).toBeDefined();
  });

  it("a run outside any conversation keeps its own row", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [conversation(), row({ id: 7 })] })));

    await renderRuns();

    const link = await screen.findByRole("link", { name: "tidy the imports" });
    expect(link.getAttribute("href")).toBe("/runs/7");
    expect(screen.getByText("run 7")).toBeDefined();
    expect(
      within(link.closest(".runs-row") as HTMLElement).queryByRole("button", { name: "Show turns" }),
    ).toBeNull();
  });

  it("expanding a conversation lists its turns, one click away", async () => {
    const base = runsFetch(world({ rows: [conversation()] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path.startsWith("/runs?chat_id=c-1")
        ? [row({ id: 88, mode: "assistant", chat_id: "c-1", prompt_excerpt: "first turn" })]
        : await base(path, init),
    );

    await renderRuns();

    const show = await screen.findByRole("button", { name: "Show turns" });
    // Nothing is fetched for a conversation nobody opened.
    expect(daemon.apiFetch.mock.calls.some(([path]) => String(path).includes("chat_id="))).toBe(false);

    fireEvent.click(show);

    await screen.findByRole("list", { name: "Turns of Refactor the gate" });
    expect(screen.getByRole("link", { name: "first turn" }).getAttribute("href")).toBe("/runs/88");
    expect(daemon.apiFetch).toHaveBeenCalledWith("/runs?chat_id=c-1&limit=50");
    expect(screen.getByRole("button", { name: "Hide turns" }).getAttribute("aria-expanded")).toBe("true");
  });
  it("says so when a conversation has more turns than the list shows", async () => {
    const base = runsFetch(world({ rows: [conversation({ turns: 60 })] }));
    const listed = Array.from({ length: 50 }, (_, i) =>
      row({ id: 100 + i, mode: "assistant", chat_id: "c-1", prompt_excerpt: `turn ${i}` }),
    );
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path.startsWith("/runs?chat_id=c-1") ? listed : await base(path, init),
    );

    await renderRuns();
    fireEvent.click(await screen.findByRole("button", { name: "Show turns" }));

    await screen.findByRole("list", { name: "Turns of Refactor the gate" });
    expect(screen.getByText("showing the newest 50 of 60 turns")).toBeDefined();
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
    // And the headline counts the page, not the index.
    expect(screen.getByRole("status").textContent).toBe("the newest 50; none of them still moving");
  });

  it("keeps an empty filtered list apart from an empty machine", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [] })));

    await renderRuns("/runs?status=failed");

    expect(await screen.findByText(/Nothing matches those filters/)).toBeDefined();
    expect(screen.queryByText(/No runs yet/)).toBeNull();
  });

  it("an empty machine points at New run, and its own button opens the panel", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [] })));

    await renderRuns();

    const teach = (await screen.findByText(/No runs yet/)).parentElement as HTMLElement;
    expect(teach.textContent).toMatch(/New run at the top of the page/);
    expect(teach.textContent).not.toMatch(/beside this list/);
    fireEvent.click(within(teach).getByRole("button", { name: "New run" }));
    expect((await newRunButton()).getAttribute("aria-expanded")).toBe("true");
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

  it("a completed run shows its word and no badge, and every other state keeps one", async () => {
    const rows = [row({ id: 7, status: "completed" }), row({ id: 8, status: "failed" })];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    const [done, broke] = Array.from(list.children).map(
      (item) => item.querySelector(".runs-row-state") as HTMLElement,
    );
    // The normal recedes: the column is coloured only where something happened.
    expect(done.querySelector(".ui-badge")).toBeNull();
    expect(done.textContent).toBe("completed");
    expect(broke.querySelector(".ui-badge")).not.toBeNull();
    expect(broke.textContent).toBe("failed");
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

  it("a run that has not settled has an empty cost cell, not a missing price", async () => {
    const rows = [
      row({ id: 7, status: "running", completed_at: null, cost_usd: null }),
      row({ id: 8, status: "awaiting_approval", completed_at: null, cost_usd: null }),
    ];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    const list = await screen.findByRole("list", { name: "Runs" });
    const costs = Array.from(list.querySelectorAll(".runs-row-cost")).map(
      (cell) => cell.textContent,
    );
    // Nothing has been priced yet, which is not the ledger failing to record it.
    expect(costs).toEqual(["", ""]);
  });

  it("the third clause names runs and links to waiting", async () => {
    const rows = [
      row({ id: 7, status: "running" }),
      row({ id: 8, status: "completed" }),
      row({ id: 9, status: "awaiting_approval" }),
    ];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    // Read through the link: `getByText` matches direct text-node children
    // only, and the clause the header must not hide is inside an anchor.
    const link = await screen.findByRole("link", {
      name: "1 run awaiting approval",
    });
    expect(link.getAttribute("href")).toBe("/waiting");
    expect(link.closest("p")?.textContent).toBe(
      "3 in the index; 1 still moving; 1 run awaiting approval",
    );
    // A status region, so a change is said and not only drawn.
    expect(link.closest("[role='status']")).not.toBeNull();
  });

  it("only a running run is still moving", async () => {
    const rows = [
      row({ id: 7, status: "running" }),
      row({ id: 8, status: "completed" }),
      row({ id: 9, status: "superseded" }),
      row({ id: 10, status: "awaiting_approval" }),
    ];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    const link = await screen.findByRole("link", {
      name: "1 run awaiting approval",
    });
    expect(link.closest("p")?.textContent).toBe(
      "4 in the index; 1 still moving; 1 run awaiting approval",
    );
  });

  it("the headline counts failures, because that is what a glance is for", async () => {
    const rows = [
      row({ id: 7, status: "completed" }),
      row({ id: 8, status: "failed" }),
      row({ id: 9, status: "failed" }),
    ];
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows })));

    await renderRuns();

    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toBe(
        "3 in the index; none of them still moving; 2 failed",
      ),
    );
  });
});

/* -------------------------------------------------------------- new run -- */

describe("Runs - New run", () => {
  it("opens from the header, takes focus to the prompt, and Escape hands it back", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    await renderRuns();
    const opener = await newRunButton();
    expect(opener.getAttribute("aria-expanded")).toBe("false");
    // Closed is not absent: the panel is there for `aria-controls` to name.
    const panel = document.getElementById(opener.getAttribute("aria-controls") ?? "");
    expect(panel?.hidden).toBe(true);

    fireEvent.click(opener);

    expect(opener.getAttribute("aria-expanded")).toBe("true");
    expect(panel?.hidden).toBe(false);
    const prompt = screen.getByLabelText("Prompt");
    await waitFor(() => expect(document.activeElement).toBe(prompt));

    fireEvent.keyDown(prompt, { key: "Escape" });

    expect(opener.getAttribute("aria-expanded")).toBe("false");
    expect(panel?.hidden).toBe(true);
    expect(document.activeElement).toBe(opener);
  });

  it("Close does what Escape does", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));

    await renderRuns();
    await openNewRun();
    const opener = await newRunButton();

    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(opener.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(opener);
  });

  it("Start run waits for a prompt, and says why", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world()));

    await renderRuns();
    await openNewRun();

    const start = screen.getByRole("button", { name: "Start run" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    // The reason is attached to the control, not only printed near it.
    const why = document.getElementById(start.getAttribute("aria-describedby") ?? "");
    expect(why?.textContent).toMatch(/what the run should do/);

    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    expect(start.disabled).toBe(false);

    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "   " } });
    expect(start.disabled).toBe(true);
  });

  it("starts in shadow, the mode that cannot act, unless somebody chooses otherwise", async () => {
    const answer = runsFetch(world());
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/runs" && init?.method === "POST") return { id: 91 };
      return await answer(path, init);
    });

    await renderRuns();
    await openNewRun();

    const modes = screen.getByRole("group", { name: "Mode" });
    expect((within(modes).getByRole("radio", { name: "shadow" }) as HTMLInputElement).checked).toBe(true);
    // Every choice carries its consequence where a screen reader will read it.
    const real = within(modes).getByRole("radio", { name: "real" });
    expect(document.getElementById(real.getAttribute("aria-describedby") ?? "")?.textContent).toMatch(
      /Nothing isolates it/,
    );

    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "Start run" }));

    await waitFor(() => {
      const posted = daemon.apiFetch.mock.calls.find(
        ([path, init]) => path === "/runs" && (init as RequestInit | undefined)?.method === "POST",
      );
      expect(JSON.parse((posted?.[1] as RequestInit).body as string)).toMatchObject({
        prompt: "tidy the imports",
        mode: "shadow",
      });
    });
  });

  it("says the kill switch will refuse the run, and leaves the button to the daemon", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ killEngaged: true })));

    await renderRuns();
    await openNewRun();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });

    const note = await screen.findByText(
      "The kill switch is engaged — the núcleo will refuse this run until it is released.",
    );
    const start = screen.getByRole("button", { name: "Start run" }) as HTMLButtonElement;
    // Still pressable: the reading here can be a poll behind the switch.
    expect(start.disabled).toBe(false);
    expect(start.getAttribute("aria-describedby")?.split(" ")).toContain(note.id);
  });

  it("says nothing about the kill switch while it is released", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ killEngaged: false })));

    await renderRuns();
    await openNewRun();

    // Wait for the switch to have been read, then assert the silence.
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill"));
    expect(screen.queryByText(/kill switch is engaged/)).toBeNull();
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
    await openNewRun();
    fireEvent.change(screen.getByLabelText("Prompt"), {
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
    // Two things are read before a start and either can be the one that failed, so a body that
    // came back empty is answered with both rather than with a guess.
    expect(said.textContent).toMatch(/kill switch/);
    expect(said.textContent).toMatch(/autopilot mode/);
  });

  it("says which of the two could not be read when the daemon says so", async () => {
    // `create_run`'s second 503: a shadow or worktree run in a project whose autopilot mode it
    // could not read. It fails closed there too, and names the project in its own sentence.
    await refuseStartWith(
      new ApiRefusal(503, "unavailable", "`alpha`'s autopilot mode could not be read, so nothing was started"),
    );

    const said = await screen.findByText(/autopilot mode could not be read/);
    expect(said.textContent).toBe("`alpha`'s autopilot mode could not be read, so nothing was started");
  });

  it("tells a full project from the kill switch by the daemon's own sentence, though both are 409", async () => {
    // `create_run_reason` in `core/src/http.rs` writes the full project's sentence; the switch
    // writes its own. Same status, different prose, and the prose is what is shown.
    await refuseStartWith(
      new ApiRefusal(409, "conflict", "this project has no free slot right now, so nothing was started"),
    );

    const said = await screen.findByText(/no free slot right now/);
    expect(said.textContent).not.toMatch(/kill switch/);
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
    await openNewRun();
    fireEvent.change(screen.getByLabelText("Prompt"), {
      target: { value: "tidy the imports" },
    });
    fireEvent.change(screen.getByLabelText("Preset name"), {
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
    await openNewRun();
    fireEvent.click(await screen.findByRole("button", { name: "Run nightly" }));

    // A preset that started something and said nothing about where it went is a
    // button with no visible effect. `findBy` and not `getBy`: the navigation
    // lands through react-query's zero-delay notification, one tick after the
    // mutation settles.
    expect(await screen.findByRole("heading", { level: 1, name: "Run 77" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/runs/77");
  });

  it("a real-mode preset asks twice, and a worktree preset starts at once", async () => {
    const answer = runsFetch(
      world({
        presets: [
          preset({ id: 3, name: "nightly", mode: "worktree" }),
          preset({ id: 4, name: "tidy", mode: "real", cwd: "C:/Projects/alpha" }),
        ],
      }),
    );
    const started: string[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      const run = /^\/presets\/(\d+)\/run$/.exec(path);
      if (run !== null && init?.method === "POST") {
        started.push(path);
        return { id: 100 + Number(run[1]) };
      }
      return await answer(path, init);
    });

    await renderRuns();
    await openNewRun();

    // The row says where a real run will act before anybody presses anything.
    const presets = await screen.findByRole("list", { name: "Presets" });
    expect(presets.textContent).toContain("C:/Projects/alpha");

    fireEvent.click(within(presets).getByRole("button", { name: "Run tidy" }));
    // Armed, and nothing has run: the label now says what the second press does.
    // No subject appended: the name is on the row already, and appending it made the armed
    // label — and so the reserved width — the widest thing in the list.
    expect(await within(presets).findByRole("button", { name: "Run in real mode" })).toBeDefined();
    expect(started).toEqual([]);

    // Past the interlock's double-click dwell, the second press is a decision.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(within(presets).getByRole("button", { name: "Run in real mode" }));
    await waitFor(() => expect(started).toEqual(["/presets/4/run"]));

    fireEvent.click(within(presets).getByRole("button", { name: "Run nightly" }));
    await waitFor(() => expect(started).toEqual(["/presets/4/run", "/presets/3/run"]));
  });

  it("deleting a preset is a quiet interlock, not red at rest", async () => {
    daemon.apiFetch.mockImplementation(runsFetch(world({ presets: [preset({ name: "nightly" })] })));

    await renderRuns();
    await openNewRun();

    const remove = await screen.findByRole("button", { name: "Delete nightly" });
    expect(remove.className).toContain("ui-button-quiet");
    expect(remove.className).not.toContain("ui-button-danger");
  });

  describe("router trail", () => {
    const report: RouteReport = {
      days: 30,
      runs: 12,
      shadow: 10,
      apply: 2,
      advised: 10,
      shadow_advised: 8,
      matched: 6,
      pairs: [
        {
          runner: "claude", model: "sonnet", effort: "high",
          advised_runner: "claude", advised_model: "haiku", advised_effort: "low",
          runs: 4, passed: 3, failed: 1,
        },
      ],
    };

    it("shows what ran and, in shadow, the advice that differed", async () => {
      daemon.apiFetch.mockImplementation(
        runsFetch(
          world({
            rows: [
              row({
                id: 1, runner: "claude", model: "sonnet", effort: "high", route_mode: "shadow",
                advised_runner: "claude", advised_model: "haiku", advised_effort: "low",
              }),
              row({
                id: 2, prompt_excerpt: "same", runner: "claude", model: "opus", effort: "low", route_mode: "shadow",
                advised_runner: "claude", advised_model: "opus", advised_effort: "low",
              }),
              row({ id: 3, prompt_excerpt: "old run" }),
            ],
          }),
        ),
      );
      await renderRuns();
      expect(await screen.findByText("sonnet · high")).toBeTruthy();
      expect(screen.getByText("router: haiku · low")).toBeTruthy();
      expect(screen.getByText("opus · low")).toBeTruthy();
      expect(screen.getAllByText(/^router:/)).toHaveLength(1);
    });

    it("renders the report's totals and pairs", async () => {
      daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()], report })));
      await renderRuns();
      expect(await screen.findByRole("heading", { name: "Router (last 30 days)" })).toBeTruthy();
      expect(screen.getByText("12 routed runs")).toBeTruthy();
      expect(screen.getByText("match rate 75%")).toBeTruthy();
      expect(screen.getByText("claude · sonnet · high → claude · haiku · low")).toBeTruthy();
    });

    it("renders nothing when no run was routed or the daemon has no report", async () => {
      daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()], report: { ...report, runs: 0, pairs: [] } })));
      const first = await renderRuns();
      await screen.findByText("tidy the imports");
      await waitFor(() =>
        expect(daemon.apiFetch).toHaveBeenCalledWith("/route/report?days=30"),
      );
      expect(screen.queryByText(/Router \(last/)).toBeNull();
      first.unmount?.();
    });

    it("an older daemon's 404 shows no panel and no error", async () => {
      daemon.apiFetch.mockImplementation(runsFetch(world({ rows: [row()] })));
      await renderRuns();
      await screen.findByText("tidy the imports");
      await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/route/report?days=30"));
      expect(screen.queryByText(/Router \(last/)).toBeNull();
      expect(screen.queryByRole("alert")).toBeNull();
    });
  });
});
