import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Waiting } from "./Waiting";
import { keys } from "../data/keys";
import type { Proposal } from "../data/system";
import type {
  AwaitingRun,
  BrowserSession,
  MergeSuggestion,
  VcsRequestSummary,
} from "../data/waiting";
import { daemonFetch, daemonState, proposal, renderApp, renderWithRouter } from "../test/harness";

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
 * one this machine cannot pay six times over without pushing other suites past
 * their timeouts. The single case that genuinely needs the whole app is the one
 * that proves the route is registered, and it says so where it does it.
 */
function renderWaiting() {
  return renderWithRouter(<Waiting />, { initialPath: "/waiting" });
}

/* ------------------------------------------------------------- fixtures -- */

function session(overrides: Partial<BrowserSession> = {}): BrowserSession {
  return {
    id: 5,
    sidecar_id: "sc-1",
    run_id: null,
    project_id: "alpha",
    profile_kind: "project",
    profile_id: "alpha",
    requested_url: "https://example.com/login",
    final_url: "https://example.com/login",
    rule: "ask",
    mode: "wheel-requested",
    refusal: null,
    proposal_id: 91,
    chain: null,
    chain_decided_at: null,
    opened_at: "2026-08-17T09:00:00Z",
    closed_at: null,
    ...overrides,
  };
}

function merge(overrides: Partial<MergeSuggestion> = {}): MergeSuggestion {
  return {
    proposal_id: 21,
    reasoning: "the same display name against two addresses",
    created_at: "2026-08-17T09:00:00Z",
    keep: {
      contact_id: 1,
      addresses: ["ana@example.com"],
      display_name: "Ana",
      messages_in: 12,
      verdict: null,
    },
    absorb: {
      contact_id: 2,
      addresses: ["ana.silva@example.com"],
      display_name: "Ana Silva",
      messages_in: 3,
      verdict: null,
    },
    ...overrides,
  };
}

function vcsRow(overrides: Partial<VcsRequestSummary> = {}): VcsRequestSummary {
  return {
    id: 61,
    op: "push",
    project_id: "alpha",
    repo_key: "alpha:main",
    origin: "job",
    status: "escalated",
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

function parkedRun(overrides: Partial<AwaitingRun> = {}): AwaitingRun {
  return {
    id: 71,
    project_id: "alpha",
    prompt: "tidy the imports",
    cwd: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/* ------------------------------------------------------------ the daemon -- */

interface WaitingWorld {
  sessions: BrowserSession[];
  approvals: Proposal[];
  merges: MergeSuggestion[];
  exclusions: Proposal[];
  skipped: Proposal[];
  refused: Proposal[];
  vcs: VcsRequestSummary[];
  parked: AwaitingRun[];
}

function waitingWorld(overrides: Partial<WaitingWorld> = {}): WaitingWorld {
  return {
    sessions: [],
    approvals: [],
    merges: [],
    exclusions: [],
    skipped: [],
    refused: [],
    vcs: [],
    parked: [],
    ...overrides,
  };
}

/**
 * The queue's eight routes, over the foundation's responder.
 *
 * A local switch rather than an edit to `test/harness.tsx`, for the reason the
 * fleet's and the runs' suites give: the harness is the shared floor, and a page
 * that teaches it eight routes of its own makes every other suite carry them.
 * Anything this does not know falls through, so `/projects` and
 * `/autopilot/budget` still answer for the whole-app case.
 *
 * Note the shape of the table. **Every section has a route of its own** — that
 * is the fact this suite is here to hold: `GET /proposals` answers
 * `action-approval` and nothing else (`proposals::list_pending`), so a page that
 * read it once and sorted by `kind` would show an empty queue for six of the
 * nine sections that exist.
 */
function waitingFetch(state: WaitingWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState());
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    switch (path) {
      case "/browser/sessions":
        return state.sessions;
      case "/proposals":
        return state.approvals;
      case "/contacts/merges":
        return state.merges;
      case "/fleet/exclusions/requests":
        return state.exclusions;
      case "/proposals/skipped-items":
        return state.skipped;
      case "/proposals/refused-actions":
        return state.refused;
      case "/vcs/requests":
        return state.vcs;
      case "/runs/awaiting-approval":
        return state.parked;
      default:
        return await shared(path, init);
    }
  };
}

/** The card ids of a named list, in the order they are on screen. */
function orderIn(listName: string, pattern: RegExp): string[] {
  const list = screen.getByRole("list", { name: listName });
  return within(list)
    .getAllByRole("listitem")
    .map((item) => within(item).getByText(pattern).textContent ?? "");
}

/* ------------------------------------------------------- A13: the sources -- */

describe("Waiting - each section reads the route that serves it", () => {
  it("asks eight routes and shows a card from each", async () => {
    const world = waitingWorld({
      sessions: [
        session({ id: 5, proposal_id: 91 }),
        // In `wheel-requested` with no proposal yet: the window between the mode
        // flipping and the record landing. Nothing to answer, so nothing shown.
        session({ id: 6, proposal_id: null }),
        // Driving itself. Not a decision at all.
        session({ id: 7, mode: "agent", proposal_id: null }),
      ],
      approvals: [proposal({ id: 11, tool_name: "Bash" })],
      merges: [merge({ proposal_id: 21 })],
      exclusions: [
        proposal({
          id: 31,
          kind: "fleet-exclusion",
          project_id: "alpha",
          tool_input: JSON.stringify({ pair: "4:9", job_low: 4, job_high: 9, paths: ["src/a.rs"] }),
        }),
      ],
      skipped: [proposal({ id: 41, kind: "skipped-item", tool_name: "Write" })],
      refused: [
        proposal({ id: 51, kind: "refused-action", tool_name: "send_email", errand_name: "invoices" }),
      ],
      vcs: [vcsRow({ id: 61 })],
      parked: [parkedRun({ id: 71 })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    // One card per section, each carrying the id its own route sent.
    expect(await screen.findByText("wheel #91")).toBeDefined();
    expect(await screen.findByText("approval #11")).toBeDefined();
    expect(await screen.findByText("merge #21")).toBeDefined();
    expect(await screen.findByText("request #31")).toBeDefined();
    expect(await screen.findByText("item #41")).toBeDefined();
    expect(await screen.findByText("refusal #51")).toBeDefined();
    expect(await screen.findByText("push #61")).toBeDefined();
    expect(await screen.findByText("run 71")).toBeDefined();

    // And every one of those came from its own door, by name.
    for (const path of [
      "/browser/sessions",
      "/proposals",
      "/contacts/merges",
      "/fleet/exclusions/requests",
      "/proposals/skipped-items",
      "/proposals/refused-actions",
      "/vcs/requests",
      "/runs/awaiting-approval",
    ]) {
      expect(daemon.apiFetch).toHaveBeenCalledWith(path);
    }

    // The distinction that makes this more than a count: `/proposals` is
    // action-approval only, so the skipped item is NOT in the approval queue —
    // a page that filtered one list by `kind` would have put it there.
    const approvals = screen.getByRole("list", { name: "Action approvals" });
    expect(within(approvals).queryByText("item #41")).toBeNull();

    // A session with no proposal has nothing to answer, so it is not a request.
    const wheels = screen.getByRole("list", { name: "Wheel requests" });
    expect(within(wheels).getAllByRole("listitem").length).toBe(1);
  });

  it("says the calendar section has no route rather than inventing one", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    const said = await screen.findByText(/mounts no route that lists the pending ones/);
    expect(said.textContent).toMatch(/what is missing is the door, not the record/);
    // Nothing was asked for on its behalf.
    const asked = daemon.apiFetch.mock.calls.map(([path]) => String(path));
    expect(asked.some((path) => path.includes("calendar"))).toBe(false);
  });
});

/* -------------------------------------------------------- A13: the freeze -- */

describe("Waiting - the ordering freeze", () => {
  it("holds a section's order still while one of its cards is armed", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    await screen.findByText("approval #1");
    expect(orderIn("Action approvals", /^approval #\d+$/)).toEqual(["approval #1", "approval #2"]);

    // One click arms; it does not decide.
    fireEvent.click(screen.getByRole("button", { name: "Approve #1" }));
    expect(screen.getByRole("button", { name: "Let this action happen" })).toBeDefined();

    // Now the daemon answers in a different order, with one more row.
    world.approvals = [proposal({ id: 2 }), proposal({ id: 1 }), proposal({ id: 3 })];
    await act(async () => {
      await queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    });

    // The refetch really landed — the new row is on screen. Without this the
    // assertion below would pass on a page that never refetched at all.
    expect(await screen.findByText("approval #3")).toBeDefined();

    // And the two rows that were already there did not move under the armed
    // finger. The newcomer goes to the end: it is the only honest place for a
    // row nobody has looked at yet.
    expect(orderIn("Action approvals", /^approval #\d+$/)).toEqual([
      "approval #1",
      "approval #2",
      "approval #3",
    ]);
  });

  it("follows the daemon's order when nothing is armed", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    await screen.findByText("approval #1");

    world.approvals = [proposal({ id: 2 }), proposal({ id: 1 }), proposal({ id: 3 })];
    await act(async () => {
      await queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    });
    expect(await screen.findByText("approval #3")).toBeDefined();

    // The control on the test above: the freeze is a freeze and not the page
    // being unable to reorder in the first place.
    expect(orderIn("Action approvals", /^approval #\d+$/)).toEqual([
      "approval #2",
      "approval #1",
      "approval #3",
    ]);
  });
});

/* ------------------------------------------- A14: what is deliberately absent -- */

describe("Waiting - the sections that are not there", () => {
  it("renders no team or recruitment section, only a note naming the Teams slice", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    const said = await screen.findByText(/recruitment requests/);
    expect(said.textContent).toMatch(/Teams slice/);

    // A sentence, not a section: nothing to list and nothing to press. A team
    // decision cannot be shown because the núcleo mounts no team routes at all.
    const panel = said.closest("section");
    expect(panel).not.toBeNull();
    expect(within(panel as HTMLElement).queryAllByRole("button")).toEqual([]);
    expect(within(panel as HTMLElement).queryAllByRole("list")).toEqual([]);

    const asked = daemon.apiFetch.mock.calls.map(([path]) => String(path));
    expect(asked.some((path) => path.includes("team"))).toBe(false);
  });

  it("gives the refused actions no buttons at all", async () => {
    const world = waitingWorld({
      refused: [
        proposal({
          id: 51,
          kind: "refused-action",
          tool_name: "send_email",
          errand_name: "invoices",
          reasoning: "the message asked for it, and the message is not the operator",
          tool_input: JSON.stringify({ to: "billing@example.com", subject: "March" }),
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Refused actions" });
    const panel = list.closest("section");
    expect(panel).not.toBeNull();

    // Nothing to allow: the turn that reached for this ended long ago, so a
    // control here would read as "handled" for something nobody handled.
    expect(within(panel as HTMLElement).queryAllByRole("button")).toEqual([]);

    // It is still a record worth reading — the verb, the subject it belonged to,
    // and what it was going to do, as fields rather than as a JSON dump.
    expect(within(list).getByText("send_email")).toBeDefined();
    expect(within(list).getByText("invoices")).toBeDefined();
    expect(within(list).getByText("billing@example.com")).toBeDefined();
    expect(within(list).queryByText(/[{}]/)).toBeNull();
  });
});

/* ----------------------------------------------------------- the two doors -- */

describe("Waiting - a skipped item is put away, not refused", () => {
  it("posts to /dismiss and never to /reject", async () => {
    const world = waitingWorld({ skipped: [proposal({ id: 41, kind: "skipped-item" })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    fireEvent.click(await screen.findByRole("button", { name: "Put item #41 away" }));

    // Clicks inside the 300 ms dwell are swallowed by design and leave the
    // control armed, so retrying the click until the control disarms is safe —
    // and it is cheaper than either sleeping for the dwell or bringing fake
    // timers into a suite that waits on react-query. The confirm is a *separate*
    // wait, because `mutate` reaches the client a tick after the click.
    await waitFor(() => {
      const armed = screen.queryByRole("button", { name: "I have read it" });
      if (armed !== null) fireEvent.click(armed);
      expect(screen.queryByRole("button", { name: "I have read it" })).toBeNull();
    });
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/41/dismiss", { method: "POST" });
    });

    // `reject_proposal` guards on `kind = 'action-approval'` and answers 409 for
    // anything else, so a dismiss sent to that door would fail every time.
    const asked = daemon.apiFetch.mock.calls.map(([path]) => String(path));
    expect(asked.some((path) => path.endsWith("/reject"))).toBe(false);
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Waiting - the route", () => {
  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    // The whole app here, and only here: a stubbed destination would prove
    // nothing about whether `/waiting` is in the real tree.
    const { router } = await renderApp({ initialPath: "/waiting" });

    expect(await screen.findByRole("heading", { level: 1, name: "Waiting" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/waiting");
    expect(screen.queryByText("Waiting is not built yet")).toBeNull();
  });
});
