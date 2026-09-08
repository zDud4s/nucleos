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
import type { AgentRequest } from "../data/agents";
import type { Proposal } from "../data/system";
import type { TeamAction } from "../data/teams";
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

function teamAction(overrides: Partial<TeamAction> = {}): TeamAction {
  return {
    id: 900,
    team_run_id: "run-1",
    ordinal: 1,
    kind: "send_email",
    payload: JSON.stringify({ to: "cliente@example.com" }),
    why: "answer the open ticket",
    proposal_id: null,
    state: "pending",
    error: null,
    created_at: "2026-08-17T09:00:00Z",
    executed_at: null,
    ...overrides,
  };
}

function agentRequest(overrides: Partial<AgentRequest> = {}): AgentRequest {
  return {
    name: "Ana",
    speciality: "billing",
    prompt: "handle billing questions",
    engine: "claude-cli",
    model: null,
    tool_policy: "restricted",
    ...overrides,
  };
}

/* ------------------------------------------------------------ the daemon -- */

interface WaitingWorld {
  sessions: BrowserSession[];
  approvals: Proposal[];
  teamActions: Proposal[];
  recruits: Proposal[];
  openActions: TeamAction[];
  merges: MergeSuggestion[];
  exclusions: Proposal[];
  skipped: Proposal[];
  refused: Proposal[];
  vcs: VcsRequestSummary[];
  parked: AwaitingRun[];
  /** What `POST /proposals/{id}/approve` answers — thrown if it is an `Error`. */
  approveAnswer: unknown;
}

function waitingWorld(overrides: Partial<WaitingWorld> = {}): WaitingWorld {
  return {
    sessions: [],
    approvals: [],
    teamActions: [],
    recruits: [],
    openActions: [],
    merges: [],
    exclusions: [],
    skipped: [],
    refused: [],
    vcs: [],
    parked: [],
    approveAnswer: undefined,
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
    // Team actions and recruits are decided through the same door as every
    // other approval (`POST /proposals/{id}/approve`); a single override
    // here is what lets a test control that one answer without teaching the
    // whole suite a new route.
    if (init?.method === "POST" && /^\/proposals\/\d+\/approve$/.test(path)) {
      if (state.approveAnswer instanceof Error) throw state.approveAnswer;
      return state.approveAnswer;
    }
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    switch (path) {
      case "/browser/sessions":
        return state.sessions;
      case "/proposals":
        return state.approvals;
      case "/proposals/team-actions":
        return state.teamActions;
      case "/proposals/recruits":
        return state.recruits;
      case "/team-actions":
        return state.openActions;
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

    // The absence wears the page's own empty shape, so the paragraph is one
    // click away like every other one — the section itself is on screen from the
    // first paint, because it answers no route and waits for nothing.
    const section = await screen.findByRole("region", { name: "Calendar events" });
    fireEvent.click(within(section).getByRole("button", { name: "why?" }));

    const said = await screen.findByText(/mounts no route that lists the pending ones/);
    expect(said.textContent).toMatch(/what is missing is the door, not the record/);
    // Nothing was asked for on its behalf.
    const asked = daemon.apiFetch.mock.calls.map(([path]) => String(path));
    expect(asked.some((path) => path.includes("calendar"))).toBe(false);
  });
});

/* ------------------------------------------------------- the empty morning -- */

describe("Waiting - a section with nothing in it", () => {
  /**
   * The shape of an empty queue, which is this page's *normal* state.
   *
   * Eleven panels each explaining an absence made the page longest on the
   * morning nothing was wrong — 1900px of scrolling to learn that there was
   * nothing to decide. An empty section is now one line under its own heading,
   * and the paragraph that used to sit above the list is behind "why?": kept,
   * because "nothing has asked for the wheel" alone reads as a list that failed
   * to load, and costing nothing to whoever does not ask.
   */
  it("an empty section is one line under its heading, not a panel", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    // Waited for by the sentence and not by the landmark, deliberately: a section
    // still *reading* is also one quiet line inside a region, so asserting the
    // shape first would pass on a page that had not answered yet.
    const line = await screen.findByText("nothing has asked for the wheel");
    const section = screen.getByRole("region", { name: "Wheel requests" });
    expect(line.closest("section")).toBe(section);
    expect(section.className).toContain("ui-section");

    // One quiet line, and no panel anywhere around it.
    expect(section.querySelectorAll(".ui-quiet")).toHaveLength(1);
    expect(section.querySelector(".ui-panel")).toBeNull();
    expect(section.closest(".ui-panel")).toBeNull();

    // The heading is still an `h2` carrying the section's own name, so the
    // page's outline does not depend on whether a queue happens to be busy.
    expect(within(section).getByRole("heading", { level: 2 }).textContent).toBe("Wheel requests");

    // The reasoning is kept rather than cut — one click away, no pixels until then.
    expect(within(section).queryByText(/asking for the window/)).toBeNull();
    fireEvent.click(within(section).getByRole("button", { name: "why?" }));
    expect(within(section).getByText(/asking for the window/)).toBeDefined();
  });

  /**
   * The control on the case above. A section is a panel again the moment it has
   * something in it, which is what makes the one line a statement about the data
   * rather than about the page.
   */
  it("is a panel again as soon as one row arrives", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 11 })] })),
    );

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Action approvals" });
    expect(list.closest(".ui-panel")).not.toBeNull();
    expect(screen.queryByRole("region", { name: "Action approvals" })).toBeNull();
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
  it("renders a team action's payload as readable fields, never as JSON", async () => {
    const world = waitingWorld({
      teamActions: [
        proposal({
          id: 201,
          tool_name: "send_email",
          reasoning: "the client asked about their invoice",
          tool_input: JSON.stringify({
            to: "ana@example.com",
            subject: "Monday",
            body: "the invoice is attached",
          }),
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Team actions" });
    expect(within(list).getByText("ana@example.com")).toBeDefined();
    expect(within(list).getByText("Monday")).toBeDefined();
    // No raw JSON dump anywhere in the section — braces and quotes are exactly
    // the punctuation this rendering exists to strip out.
    expect(within(list).queryByText(/[{}]/)).toBeNull();
  });

  it("says an approved action is queued rather than carried out", async () => {
    const world = waitingWorld({
      teamActions: [
        proposal({ id: 202, tool_name: "file_document", reasoning: "file the report" }),
      ],
    });
    world.approveAnswer = { queued: "the department's action will be carried out shortly" };
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Team actions" });
    fireEvent.click(within(list).getByRole("button", { name: "Approve #202" }));

    // The 300 ms dwell is real in this control; a fast `findBy*` would resolve
    // inside it and swallow the confirm click, so the wait has to be real too.
    // The confirm click is its own step — a `waitFor` must not both click the
    // confirm and assert the mutation, since disarming makes a retry throw.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(within(list).getByRole("button", { name: "Let this action happen" }));

    await waitFor(() => {
      expect(
        within(list).getByText("the department's action will be carried out shortly"),
      ).toBeDefined();
    });
    expect(within(list).queryByText(/\bdone\b/i)).toBeNull();
    expect(within(list).queryByText(/\bsent\b/i)).toBeNull();
  });

  it("shows the execution state apart from the owner's decision", async () => {
    const world = waitingWorld({
      teamActions: [
        proposal({ id: 210, tool_name: "send_email", reasoning: "notify the client" }),
        proposal({ id: 211, tool_name: "calendar_event", reasoning: "book the follow-up" }),
      ],
      openActions: [teamAction({ id: 900, proposal_id: 210, state: "working" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Team actions" });
    const row210 = within(list).getByText("team action #210").closest("li");
    if (row210 === null) throw new Error("no card for team action #210");
    // The pending decision and the execution state are two facts, not one.
    expect(within(row210 as HTMLElement).getByRole("button", { name: "Approve #210" })).toBeDefined();
    expect(within(row210 as HTMLElement).getByText("being carried out")).toBeDefined();

    const row211 = within(list).getByText("team action #211").closest("li");
    if (row211 === null) throw new Error("no card for team action #211");
    // `GET /team-actions` lists only pending and working, so a proposal with no
    // matching row must say the state is unknown rather than guess one.
    expect(within(row211 as HTMLElement).getByText(/execution state is not known yet/)).toBeDefined();
  });

  it("hires a recruit over the six fields as edited, not as proposed", async () => {
    const proposed = agentRequest();
    const world = waitingWorld({
      recruits: [
        proposal({
          id: 301,
          tool_name: null,
          reasoning: "the team needs a billing specialist",
          tool_input: JSON.stringify(proposed),
        }),
      ],
    });
    world.approveAnswer = { agent_id: "ag-9" };
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Recruitment" });
    fireEvent.change(within(list).getByLabelText("engine"), { target: { value: "codex-cli" } });

    fireEvent.click(within(list).getByRole("button", { name: "Hire #301" }));
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(
      within(list).getByRole("button", { name: "Write the agent and add them to the roster" }),
    );

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/301/approve", {
        method: "POST",
        body: JSON.stringify({ hire: { ...proposed, engine: "codex-cli" } }),
      });
    });
  });

  it("keeps the frozen section order with the two team sections in place", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    await screen.findByRole("heading", { level: 1, name: "Waiting" });
    const headings = await waitFor(() => {
      const found = screen.getAllByRole("heading", { level: 2 });
      // Every section reads before the assertion is trusted — a page still
      // loading would pass this on however many panels happened to be mounted.
      expect(found.length).toBe(11);
      return found.map((heading) => heading.textContent);
    });
    expect(headings).toEqual([
      "Wheel requests",
      "Action approvals",
      "Team actions",
      "Recruitment",
      "Contact merges",
      "Calendar events",
      "Exclusion requests",
      "Skipped items",
      "Refused actions",
      "Git queue",
      "Parked runs",
    ]);
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

  it("says which stranger the turn had read", async () => {
    const world = waitingWorld({
      refused: [
        proposal({
          id: 52,
          kind: "refused-action",
          tool_name: "send_email",
          tool_input: JSON.stringify({ to: "billing@example.com" }),
          read_from: JSON.stringify([
            {
              tool: "browser_open",
              arguments: JSON.stringify({ url: "https://fornecedor.example/fatura" }),
              at: "2026-08-21T09:00:00Z",
            },
          ]),
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();
    const list = await screen.findByRole("list", { name: "Refused actions" });

    // The tool that brought the words in, and the url — which is the half that
    // decides whether this was the owner's idea or the page's.
    expect(within(list).getByText("browser_open")).toBeDefined();
    expect(within(list).getByText("https://fornecedor.example/fatura")).toBeDefined();
  });

  it("says nothing at all when nothing was recorded", async () => {
    // The guard, and the direction it guards is the one that matters. `null`
    // covers a refusal that fired on whose work it is rather than on what was
    // read, AND a recording that failed — indistinguishable from here. A card
    // that filled the silence with "this turn read nothing" would be wrong in
    // the second case, in the direction that makes a contaminated action look
    // clean.
    const world = waitingWorld({
      refused: [
        proposal({ id: 53, kind: "refused-action", tool_name: "create_run", read_from: null }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();
    const list = await screen.findByRole("list", { name: "Refused actions" });

    expect(within(list).getByText("create_run")).toBeDefined();
    expect(within(list).queryByText(/read nothing|nothing was read/i)).toBeNull();
    expect(within(list).queryByText("this turn had read")).toBeNull();
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

/* ------------------------------------------------ a project in the location -- */

/**
 * The queue, narrowed by the location and saying so.
 *
 * A project page's loudest sentence links here with its own name in the search, so this page
 * has to be able to show one project's share of the queue. Two things are under test and the
 * second is the one that is easy to forget: rows belonging to another project are gone, AND
 * the page admits that it is withholding them. A silently filtered queue reads as an empty
 * one, which is the wrong claim to make to somebody deciding whether they are done.
 */
describe("Waiting - a project in the location", () => {
  it("a project in the location narrows the queue and says so", async () => {
    const world = waitingWorld({
      approvals: [
        proposal({ id: 11, project_id: "nucleos", tool_name: "Bash" }),
        proposal({ id: 12, project_id: "other", tool_name: "Bash" }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWithRouter(<Waiting />, { initialPath: "/waiting?project=nucleos" });

    expect(await screen.findByText("approval #11")).toBeDefined();
    expect(screen.queryByText("approval #12")).toBeNull();

    // The admission, and the way back out of it.
    const note = screen.getByRole("status");
    expect(note.textContent).toBe("Only nucleos. Show everything");
    expect(within(note).getByRole("link", { name: "Show everything" }).getAttribute("href")).toBe(
      "/waiting",
    );
  });
});
