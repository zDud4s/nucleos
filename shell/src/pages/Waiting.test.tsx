import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Waiting } from "./Waiting";
import { conflictedFiles, gitNextStep } from "./waiting/GitQueue";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import { seatNonce } from "../data/seat";
import type { AgentRequest } from "../data/agents";
import type { Proposal } from "../data/system";
import type { TeamAction } from "../data/teams";
import type {
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
    seat: null,
    shell_eligible: false,
    visible: false,
    owner_chat_id: null,
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
  vcs: VcsRequestSummary[];
  /**
   * What `GET /waiting/git` answers. Left out, it is the rows of `vcs` that want a person
   * (awaiting the owner, escalated, blocked) — the two lists agree unless a test is about the one
   * place they differ: the history listing is capped and permanent, the waiting listing is neither.
   */
  vcsWaiting?: VcsRequestSummary[];
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
    vcs: [],
    approveAnswer: undefined,
    ...overrides,
  };
}

/**
 * The queue's routes, over the foundation's responder.
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
const VCS_WANTS_A_PERSON = ["awaiting_owner", "escalated", "blocked"];

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
      case "/vcs/requests":
        return state.vcs;
      case "/waiting/git":
        return state.vcsWaiting ?? state.vcs.filter((row) => VCS_WANTS_A_PERSON.includes(row.status));
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

/**
 * The card a heading belongs to.
 *
 * Buttons carry no id any more, so several cards share the name "Approve" and a button is asked
 * for inside the card it sits on.
 */
function cardOf(heading: string): HTMLElement {
  const card = screen.getByText(heading).closest("li");
  if (card === null) throw new Error(`no card for ${heading}`);
  return card;
}

/** The bar over the approvals. Its Approve and Decline share their names with the cards' own. */
function bulkBar(): HTMLElement {
  const bar = document.querySelector<HTMLElement>(".waiting-bulk");
  if (bar === null) throw new Error("no bulk bar");
  return bar;
}

/** One of the empty sections' names in the absence line; the reasoning rides inside it as a tip. */
function quietName(title: string): HTMLElement {
  const found = Array.from(document.querySelectorAll<HTMLElement>(".waiting-quiet-name")).find(
    (name) => name.firstChild?.textContent === title,
  );
  if (found === undefined) throw new Error(`${title} is not in the absence line`);
  return found;
}

/* ------------------------------------------------------- A13: the sources -- */

describe("Waiting - each section reads the route that serves it", () => {
  it("the headline is the number the rail shows", async () => {
    const world = waitingWorld({
      approvals: [proposal({ id: 1 })],
      teamActions: [proposal({ id: 2, kind: "team-action" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    expect(await screen.findByText("2 waiting on a decision")).toBeDefined();
  });

  it("asks the routes that serve a decision and shows a card from each", async () => {
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
      vcs: [vcsRow({ id: 61, status: "awaiting_owner" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    // One card per section, each carrying the id its own route sent.
    expect(await screen.findByText("wheel #91")).toBeDefined();
    expect(await screen.findByText("approval #11")).toBeDefined();
    expect(await screen.findByText("merge #21")).toBeDefined();
    expect(await screen.findByText("request #31")).toBeDefined();
    expect(await screen.findByText("push #61")).toBeDefined();

    // And every one of those came from its own door, by name.
    for (const path of [
      "/browser/sessions",
      "/proposals",
      "/contacts/merges",
      "/fleet/exclusions/requests",
      "/vcs/requests",
      "/waiting/git",
    ]) {
      expect(daemon.apiFetch).toHaveBeenCalledWith(path);
    }

    // The page's one job is deciding, so the three routes that only carry records are not read
    // here: skipped items and refused actions are on the Autopilot page, and a parked run is
    // the run side of an approval that is already on this one.
    for (const path of [
      "/proposals/skipped-items",
      "/proposals/refused-actions",
      "/runs/awaiting-approval",
    ]) {
      expect(daemon.apiFetch).not.toHaveBeenCalledWith(path);
    }

    // A session with no proposal has nothing to answer, so it is not a request.
    const wheels = screen.getByRole("list", { name: "Browser access requests" });
    expect(within(wheels).getAllByRole("listitem").length).toBe(1);
  });

  it("has no section that asks for no decision", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    await screen.findByText(/^Nothing waiting in:/);
    for (const name of ["Calendar events", "Skipped items", "Refused actions", "Parked runs"]) {
      expect(screen.queryByRole("region", { name })).toBeNull();
      expect(screen.queryByRole("heading", { name })).toBeNull();
    }
    // Nothing was asked for on behalf of the calendar, which has no route to ask.
    const asked = daemon.apiFetch.mock.calls.map(([path]) => String(path));
    expect(asked.some((path) => path.includes("calendar"))).toBe(false);
  });
});

describe("Waiting - git requests", () => {
  it("a blocked git request wears one word and the row says what to do", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld({ vcs: [vcsRow({ status: "blocked" })] })));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Git requests to put away" });
    const row = within(list).getByText("push #61").closest("li");
    if (row === null) throw new Error("no blocked git row");
    expect(within(row).getByText("blocked").textContent).toBe("blocked");

    // The instruction is its own line now. It rode at the end of the faint mono path line —
    // `--text-faint` at 11px, third clause in an em-dash chain — which is the quietest rank on
    // the card, for the one sentence on it that is asking somebody to do something.
    expect(within(row).getByText(/alpha:main/).textContent).not.toContain("submit it again");
    expect(within(row).getByText("submit it again").classList.contains("waiting-hint")).toBe(true);
  });

  it("the git section waits on /waiting/git, not on the history's escalations", async () => {
    // The history listing still holds an old escalation that something else has since settled, and
    // the one row that really wants a person is older than that listing's window — so it is not in
    // it at all. Only a page reading `/waiting/git` can get both of those right.
    const world = waitingWorld({
      vcs: [vcsRow({ id: 61, status: "escalated" })],
      vcsWaiting: [vcsRow({ id: 62, status: "escalated" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const list = await screen.findByRole("list", { name: "Git requests to put away" });
    expect(await within(list).findByText("push #62")).toBeDefined();
    expect(within(list).queryByText("push #61")).toBeNull();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/waiting/git");
  });

  it("a git row waiting on you can be dismissed", async () => {
    const world = waitingWorld({ vcsWaiting: [vcsRow({ id: 62, status: "escalated" })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    fireEvent.click(await screen.findByRole("button", { name: "Put push #62 away" }));

    // Two clicks, and the second may land inside the dwell and be swallowed, so it is retried until
    // the control disarms.
    await waitFor(() => {
      const armed = screen.queryByRole("button", { name: "Nobody needs to act · #62" });
      if (armed !== null) fireEvent.click(armed);
      expect(screen.queryByRole("button", { name: "Nobody needs to act · #62" })).toBeNull();
    });
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/vcs/requests/62/dismiss", { method: "POST" });
    });
  });

  async function decideHeld(button: string, confirm: string, path: string) {
    const world = waitingWorld({ vcsWaiting: [vcsRow({ id: 63, status: "awaiting_owner" })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    fireEvent.click(await screen.findByRole("button", { name: button }));
    await waitFor(() => {
      const armed = screen.queryByRole("button", { name: confirm });
      if (armed !== null) fireEvent.click(armed);
      expect(screen.queryByRole("button", { name: confirm })).toBeNull();
    });
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith(path, { method: "POST" });
    });
  }

  it("a merge held for the test map can be approved", async () => {
    await decideHeld("Approve", "Approve the map change", "/vcs/requests/63/approve");
  });

  it("a merge held for the test map can be declined", async () => {
    await decideHeld("Decline", "Decline the map change", "/vcs/requests/63/refuse");
  });

  it("a held merge is decided, not dismissed", async () => {
    const world = waitingWorld({ vcsWaiting: [vcsRow({ id: 63, status: "awaiting_owner" })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    expect(await screen.findByText(/changes the test map/)).toBeDefined();
    expect(screen.queryByRole("button", { name: /away/ })).toBeNull();
  });

  it("only a request that awaits the owner sits in the decision list; the rest is read inline under its own headings", async () => {
    const world = waitingWorld({
      vcsWaiting: [
        vcsRow({ id: 63, status: "awaiting_owner" }),
        vcsRow({ id: 64, status: "escalated" }),
        vcsRow({ id: 65, status: "blocked" }),
      ],
      vcs: [vcsRow({ id: 66, status: "merged" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const waiting = await screen.findByRole("list", { name: "Git requests waiting on you" });
    expect(within(waiting).getByText("push #63")).toBeDefined();
    expect(within(waiting).queryByText("push #64")).toBeNull();
    expect(within(waiting).queryByText("push #65")).toBeNull();

    // Nothing is behind a disclosure: the rest has headings of its own, on the page.
    expect(document.querySelector("details")).toBeNull();
    expect(screen.getByRole("heading", { level: 3, name: "escalated or blocked" })).toBeDefined();
    expect(screen.getByRole("heading", { level: 3, name: "recently through the queue" })).toBeDefined();

    const away = screen.getByRole("list", { name: "Git requests to put away" });
    expect(within(away).getByText("push #64")).toBeDefined();
    expect(within(away).getByText("push #65")).toBeDefined();
    const recent = screen.getByRole("list", { name: "Recent git requests" });
    expect(within(recent).getByText("push #66")).toBeDefined();
  });

  it("an escalated merge says which branches, which files, and who is resolving it", async () => {
    const world = waitingWorld({
      vcsWaiting: [
        vcsRow({
          id: 573,
          op: "merge",
          repo_key: "\\\\?\\C:\\Projects\\nucleos\\.git",
          origin: "shell",
          source: "master",
          target: "feat/agent-loadout-preview",
          output_tail:
            "Auto-merging shell/src/preview/daemon.ts\nCONFLICT (content): Merge conflict in shell/src/preview/daemon.ts\n",
          failure_reason: "merging master into feat/agent-loadout-preview conflicts, so nothing was published",
          resolution_run_id: 900623,
          resolution_status: "running",
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    await screen.findByText("merge #573");
    const card = cardOf("merge #573");
    expect(within(card).getByText("master")).toBeDefined();
    expect(within(card).getByText("feat/agent-loadout-preview")).toBeDefined();
    const files = within(card).getByRole("list", { name: "Conflicts in merge #573" });
    expect(within(files).getByText("shell/src/preview/daemon.ts")).toBeDefined();
    expect(within(card).getByRole("link", { name: "run #900623" })).toBeDefined();
    expect(within(card).getByText(/is resolving it — nothing to do yet/)).toBeDefined();
    // The badge agrees with the hint: the next move is the agent's, not the reader's.
    expect(within(card).getByText("with an agent")).toBeDefined();
    expect(within(card).queryByText("escalated to you")).toBeNull();
    // The files say it better than the reason written for the agent that asked.
    expect(within(card).queryByText(/nothing was published/)).toBeNull();
    // Git's verbatim prefix is not something a person needs to read.
    expect(within(card).getByText(/^C:\\Projects\\nucleos\\\.git — asked from shell$/)).toBeDefined();
  });

  it("arming a git control holds the order of the requests still", async () => {
    const world = waitingWorld({
      vcsWaiting: [vcsRow({ id: 63, status: "awaiting_owner" }), vcsRow({ id: 64, status: "awaiting_owner" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    await screen.findByText("push #63");

    fireEvent.click(within(cardOf("push #63")).getByRole("button", { name: "Approve" }));
    world.vcsWaiting = [
      vcsRow({ id: 64, status: "awaiting_owner" }),
      vcsRow({ id: 63, status: "awaiting_owner" }),
      vcsRow({ id: 65, status: "awaiting_owner" }),
    ];
    await act(async () => {
      await queryClient.invalidateQueries();
    });
    expect(await screen.findByText("push #65")).toBeDefined();
    expect(orderIn("Git requests waiting on you", /^push #\d+$/)).toEqual(["push #63", "push #64", "push #65"]);
  });
});

/* ------------------------------------------------------- the empty morning -- */

describe("Waiting - a section with nothing in it", () => {
  /**
   * The shape of an empty queue, which is this page's *normal* state.
   *
   * Eleven panels each explaining an absence made the page longest on the
   * morning nothing was wrong — 1900px of scrolling to learn that there was
   * nothing to decide. An empty section is now one line under the page header, and the
   * paragraph that used to sit above the list rides on its name as a tip: kept,
   * because "nothing has asked for the wheel" alone reads as a list that failed
   * to load, and costing nothing to whoever does not ask.
   */
  it("the empty sections are one line, and their reasoning is behind it", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    // Waited for by every name and not by the line's start: the line grows as each list
    // answers, so asserting on a prefix would pass on a page that had not finished reading.
    const everything = [
      "Browser access requests",
      "Action approvals",
      "Team actions",
      "Recruitment",
      "Contact merges",
      "Keep jobs apart?",
      "Git queue",
    ];
    const line = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(".waiting-quiet-line");
      if (found === null) throw new Error("no absence line yet");
      const names = Array.from(
        found.querySelectorAll(".waiting-quiet-name"),
        (item) => item.firstChild?.textContent,
      );
      expect(names).toEqual(everything);
      return found;
    });
    expect(line.tagName).toBe("P");
    expect(line.firstChild?.textContent).toMatch(/^Nothing waiting in:/);

    // No heading, no panel and no region for any of them: an absence is the normal state of
    // this page and does not get a block each.
    expect(screen.queryAllByRole("heading", { level: 2 })).toHaveLength(0);
    expect(screen.queryByRole("region", { name: "Browser access requests" })).toBeNull();

    // The reasoning is kept rather than cut: a tip inside the name, which describes it. No
    // disclosure and no `why?` button.
    const name = quietName("Browser access requests");
    const trigger = name.querySelector<HTMLElement>(".ui-tip-text");
    if (trigger === null) throw new Error("no tip trigger inside the name");
    expect(trigger.getAttribute("tabindex")).toBe("0");
    const tip = name.querySelector<HTMLElement>(".ui-tip-pop");
    if (tip === null) throw new Error("no tip inside the name");
    expect(tip.getAttribute("role")).toBe("tooltip");
    expect(trigger.getAttribute("aria-describedby")).toBe(tip.id);
    expect(tip.textContent).toMatch(/asking for the window/);
    expect(line.querySelector("details")).toBeNull();
    expect(within(line).queryByRole("button")).toBeNull();
  });

  it("a queue's reasoning opens on a press and Escape closes it", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));
    await renderWaiting();
    await waitFor(() => quietName("Git queue"));
    const name = quietName("Git queue");
    const trigger = name.querySelector<HTMLElement>(".ui-tip-text");
    const tip = name.querySelector<HTMLElement>(".ui-tip-pop");
    if (trigger === null || tip === null) throw new Error("no tip");
    expect(tip.hidden).toBe(true);
    fireEvent.click(trigger);
    expect(tip.hidden).toBe(false);
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(tip.hidden).toBe(true);
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
    // And it leaves the line: it is not both filled and absent.
    await waitFor(() => {
      const line = screen.getByText(/^Nothing waiting in:/).textContent ?? "";
      expect(line).toContain("Team actions");
      expect(line).not.toContain("Action approvals");
    });
  });
});

/* ----------------------------------------------------- A13: batch approvals -- */

/** Tick the named approvals' boxes by hand, which is how a selection is made below four rows. */
async function selectApprovals(...ids: number[]) {
  fireEvent.click(await screen.findByRole("checkbox", { name: `Select approval #${ids[0]}` }));
  for (const id of ids.slice(1)) {
    fireEvent.click(screen.getByRole("checkbox", { name: `Select approval #${id}` }));
  }
}

describe("Waiting - batch action approvals", () => {
  it("each approval card carries a checkbox named for its row", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] })),
    );

    await renderWaiting();

    expect(await screen.findByRole("checkbox", { name: "Select approval #1" })).toBeDefined();
    expect(screen.getByRole("checkbox", { name: "Select approval #2" })).toBeDefined();
  });

  it("Select all takes every row and Clear empties the selection", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [1, 2, 3, 4].map((id) => proposal({ id })) })),
    );

    await renderWaiting();
    await screen.findByRole("checkbox", { name: "Select approval #1" });
    // Select all is a box in the bar, in the column of the row boxes.
    const selectAll = within(bulkBar()).getByRole("checkbox", { name: "Select all" }) as HTMLInputElement;
    // Nothing is selected, so there is nothing to clear: the button is not drawn yet.
    expect(within(bulkBar()).queryByRole("button", { name: "Clear" })).toBeNull();
    expect(selectAll.checked).toBe(false);
    expect(selectAll.disabled).toBe(false);

    fireEvent.click(selectAll);
    expect((screen.getByRole("checkbox", { name: "Select approval #1" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "Select approval #4" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByText("4 of 4 selected")).toBeDefined();

    fireEvent.click(within(bulkBar()).getByRole("button", { name: "Clear" }));
    expect((screen.getByRole("checkbox", { name: "Select approval #1" }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("checkbox", { name: "Select approval #4" }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText("0 of 4 selected")).toBeDefined();
    expect(within(bulkBar()).queryByRole("button", { name: "Clear" })).toBeNull();
  });

  it("Select all is partial while some rows are ticked, and unticking it clears the rest", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [1, 2, 3].map((id) => proposal({ id })) })),
    );

    await renderWaiting();
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select approval #2" }));
    const selectAll = within(bulkBar()).getByRole("checkbox", { name: "Select all" }) as HTMLInputElement;
    expect(selectAll.checked).toBe(false);
    expect(selectAll.indeterminate).toBe(true);

    // A click on a partial box completes the set; a click on the full one empties it.
    fireEvent.click(selectAll);
    expect(screen.getByText("3 of 3 selected")).toBeDefined();
    expect(selectAll.checked).toBe(true);
    expect(selectAll.indeterminate).toBe(false);
    fireEvent.click(selectAll);
    expect(screen.getByText("0 of 3 selected")).toBeDefined();
  });

  it("draws the bar only when there is more than one row to pick from", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 })] })),
    );

    await renderWaiting();
    await screen.findByRole("checkbox", { name: "Select approval #1" });

    expect(document.querySelector(".waiting-bulk")).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "Select all" })).toBeNull();
  });

  it("offers the batch decisions only once something is selected", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [1, 2, 3].map((id) => proposal({ id })) })),
    );

    await renderWaiting();
    await screen.findByRole("checkbox", { name: "Select approval #1" });
    // Two disabled buttons under "0 of 3 selected" were chrome for something that cannot be done
    // yet, so the bar's decisions appear with the first tick and go with the last.
    expect(within(bulkBar()).queryByRole("button", { name: "Approve" })).toBeNull();
    expect(within(bulkBar()).queryByRole("button", { name: "Decline" })).toBeNull();

    fireEvent.click(screen.getByRole("checkbox", { name: "Select approval #2" }));
    const approveAll = within(bulkBar()).getByRole("button", { name: "Approve" }) as HTMLButtonElement;
    const declineAll = within(bulkBar()).getByRole("button", { name: "Decline" }) as HTMLButtonElement;
    expect(approveAll.disabled).toBe(false);
    expect(declineAll.disabled).toBe(false);

    fireEvent.click(screen.getByRole("checkbox", { name: "Select approval #2" }));
    expect(within(bulkBar()).queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("offers Select all from two rows up, and shows it ticked once every row is selected", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] })),
    );

    await renderWaiting();
    await screen.findByRole("checkbox", { name: "Select approval #1" });
    const selectAll = within(bulkBar()).getByRole("checkbox", { name: "Select all" }) as HTMLInputElement;
    expect(selectAll.disabled).toBe(false);
    expect(selectAll.checked).toBe(false);

    fireEvent.click(selectAll);
    expect(selectAll.checked).toBe(true);
    expect(screen.getByText("2 of 2 selected")).toBeDefined();
  });

  it("the checkbox sits in a label that is a 28px target", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 })] })),
    );

    await renderWaiting();
    const box = await screen.findByRole("checkbox", { name: "Select approval #1" });
    // The label is what a click lands on; its size is `.waiting-select-hit` in `waiting.css`
    // (jsdom lays nothing out), so the contract held here is that the box is inside it.
    const hit = box.closest("label");
    expect(hit).not.toBeNull();
    expect(hit?.className).toBe("waiting-select-hit");
    fireEvent.click(hit as HTMLElement);
    expect((box as HTMLInputElement).checked).toBe(true);
  });

  it("the bar says how many are selected, and no longer which", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] })),
    );

    await renderWaiting();
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select approval #2" }));

    // The ticked boxes on the cards already say which; the line is a count and stays one line.
    const said = screen.getByText("1 of 2 selected");
    expect(said.getAttribute("role")).toBe("status");
    expect(said.textContent).not.toContain("#");
  });

  it("approves the selected rows one at a time, in the order they are on screen", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 2 }), proposal({ id: 1 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select approval #2" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select approval #1" }));
    const approveAll = within(bulkBar()).getByRole("button", { name: "Approve" });
    fireEvent.click(approveAll);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    fireEvent.click(approveAll);

    await waitFor(() => {
      const approveCalls = daemon.apiFetch.mock.calls
        .filter(([path, init]) => init?.method === "POST" && String(path).endsWith("/approve"))
        .map(([path]) => String(path));
      expect(approveCalls).toEqual(["/proposals/2/approve", "/proposals/1/approve"]);
    });
  });

  it("a refusal on one row does not stop the others and is shown on that row", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    const base = waitingFetch(world);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && path === "/proposals/1/approve") {
        throw new ApiRefusal(409, "conflict", "");
      }
      return await base(path, init);
    });

    await renderWaiting();
    await selectApprovals(1, 2);
    const approveAll = within(bulkBar()).getByRole("button", { name: "Approve" });
    fireEvent.click(approveAll);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    fireEvent.click(approveAll);

    const failedRow = screen.getByText("approval #1").closest("li");
    expect(failedRow).not.toBeNull();
    expect(
      await within(failedRow as HTMLElement).findByText(
        "this one was already answered — the list clears it on the next read, and nothing further is needed",
      ),
    ).toBeDefined();
    const approveCalls = daemon.apiFetch.mock.calls
      .filter(([path, init]) => init?.method === "POST" && String(path).endsWith("/approve"))
      .map(([path]) => String(path));
    expect(approveCalls).toEqual(["/proposals/1/approve", "/proposals/2/approve"]);
  });

  it("keeps the rows that failed selected and drops the ones that went through", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    const base = waitingFetch(world);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && path === "/proposals/1/approve") {
        throw new ApiRefusal(409, "conflict", "");
      }
      return await base(path, init);
    });

    await renderWaiting();
    await selectApprovals(1, 2);
    const approveAll = within(bulkBar()).getByRole("button", { name: "Approve" });
    fireEvent.click(approveAll);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    fireEvent.click(approveAll);

    await screen.findByText(
      "this one was already answered — the list clears it on the next read, and nothing further is needed",
    );
    expect((screen.getByRole("checkbox", { name: "Select approval #1" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "Select approval #2" }) as HTMLInputElement).checked).toBe(false);
  });

  it("the armed batch says what it will do, and names no rows", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] })),
    );

    await renderWaiting();
    await selectApprovals(1, 2);
    fireEvent.click(within(bulkBar()).getByRole("button", { name: "Approve" }));

    // The rows are the ticked boxes on the cards; the button would only have to grow to list them.
    expect(within(bulkBar()).getByRole("button", { name: "Approve the selected" })).toBeDefined();
    const said = screen.getByText("armed: Approve the selected — press again to confirm");
    expect(said.getAttribute("role")).toBe("status");
  });

  it("holds the order still while the batch control is armed", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    await selectApprovals(1, 2);
    fireEvent.click(within(bulkBar()).getByRole("button", { name: "Approve" }));

    world.approvals = [proposal({ id: 2 }), proposal({ id: 1 }), proposal({ id: 3 })];
    await act(async () => {
      await queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    });
    expect(await screen.findByText("approval #3")).toBeDefined();
    expect(orderIn("Action approvals", /^approval #\d+$/)).toEqual([
      "approval #1",
      "approval #2",
      "approval #3",
    ]);
  });
});

/* -------------------------------------------------------- A13: the freeze -- */

describe("Waiting - the ordering freeze", () => {
  it("the armed approval says what it will do, on the card it was pressed on", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();
    await screen.findByText("approval #1");

    const first = cardOf("approval #1");
    fireEvent.click(within(first).getByRole("button", { name: "Approve" }));

    // Two cards share the name "Approve", so what shows which one is armed is the card itself:
    // its button changed and the other card's did not.
    const armed = within(first).getByRole("button", { name: "Approve and let this action happen" });
    expect(within(cardOf("approval #2")).getByRole("button", { name: "Approve" })).toBeDefined();
    const said = screen.getByText("armed: Approve and let this action happen — press again to confirm");
    expect(said.getAttribute("role")).toBe("status");

    // The rest label is still here, holding the width it had a click ago — hidden from the eye
    // by `visibility` and from the ear by `aria-hidden`, which is why the button's accessible
    // name is only the label that is showing — and the third cell is the always-hidden
    // "Keep holding…".
    const labels = armed.querySelectorAll(".ui-confirm-stack > *");
    expect(labels).toHaveLength(3);
    expect(labels[0].textContent).toBe("Approve");
    expect(labels[0].getAttribute("aria-hidden")).toBe("true");
    expect(labels[1].textContent).toBe("Approve and let this action happen");
    expect(labels[1].getAttribute("aria-hidden")).toBeNull();
    expect(labels[2].getAttribute("aria-hidden")).toBe("true");
  });

  it("holds a section's order still while one of its cards is armed", async () => {
    const world = waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    await screen.findByText("approval #1");
    expect(orderIn("Action approvals", /^approval #\d+$/)).toEqual(["approval #1", "approval #2"]);

    // One click arms; it does not decide.
    const first = cardOf("approval #1");
    fireEvent.click(within(first).getByRole("button", { name: "Approve" }));
    expect(within(first).getByRole("button", { name: "Approve and let this action happen" })).toBeDefined();

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
  it("the queue calls a team a team", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    // The absence is one line; what the two team sections are for rides on their names as tips.
    await waitFor(() => {
      quietName("Team actions");
      quietName("Recruitment");
    });
    const tipOf = (title: string) =>
      quietName(title).querySelector('[role="tooltip"]')?.textContent ?? "";

    expect(tipOf("Team actions")).toMatch(/An action a team asked the daemon to carry out/);
    expect(tipOf("Recruitment")).toMatch(/A director asked for a specialist by name/);
    expect(screen.queryByText(/no department/)).toBeNull();
  });

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
    fireEvent.click(within(list).getByRole("button", { name: "Approve" }));

    // The 300 ms dwell is real in this control; a fast `findBy*` would resolve
    // inside it and swallow the confirm click, so the wait has to be real too.
    // The confirm click is its own step — a `waitFor` must not both click the
    // confirm and assert the mutation, since disarming makes a retry throw.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(within(list).getByRole("button", { name: "Approve and let this action happen" }));

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
    expect(within(row210 as HTMLElement).getByRole("button", { name: "Approve" })).toBeDefined();
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
    // The proposed engine ("claude-cli") is not one the catalogue accepts, so it rides as an
    // extra option; editing picks one of the accepted ones.
    fireEvent.change(within(list).getByLabelText("Engine"), { target: { value: "codex" } });

    fireEvent.click(within(list).getByRole("button", { name: "Hire" }));
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(
      within(list).getByRole("button", { name: "Hire and add them to the roster" }),
    );

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/301/approve", {
        method: "POST",
        body: JSON.stringify({ hire: { ...proposed, engine: "codex" } }),
      });
    });
  });

  it("keeps the frozen section order with the two team sections in place", async () => {
    const world = waitingWorld({
      sessions: [session({ id: 5, proposal_id: 91 })],
      approvals: [proposal({ id: 11 })],
      teamActions: [proposal({ id: 12, kind: "team-action", tool_name: "send_email" })],
      recruits: [proposal({ id: 13, tool_input: JSON.stringify(agentRequest()) })],
      merges: [merge({ proposal_id: 21 })],
      exclusions: [proposal({ id: 14, kind: "fleet-exclusion" })],
      vcs: [vcsRow({ id: 61, status: "awaiting_owner" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    await screen.findByRole("heading", { level: 1, name: "Waiting" });
    const headings = await waitFor(() => {
      const found = screen.getAllByRole("heading", { level: 2 });
      // Every section reads before the assertion is trusted — a page still
      // loading would pass this on however many panels happened to be mounted.
      expect(found.length).toBe(7);
      return found.map((heading) => heading.textContent);
    });
    expect(headings).toEqual([
      "Browser access requests",
      "Action approvals",
      "Team actions",
      "Recruitment",
      "Contact merges",
      "Keep jobs apart?",
      "Git queue",
    ]);
    // Nothing is empty, so there is no absence line.
    expect(screen.queryByText(/^Nothing waiting in:/)).toBeNull();
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Waiting - the header is an index", () => {
  it("names each queue with something in it, with its count, and links to its section", async () => {
    const world = waitingWorld({
      approvals: [1, 2, 3, 4].map((id) => proposal({ id })),
      teamActions: [proposal({ id: 210, kind: "team-action" }), proposal({ id: 211, kind: "team-action" })],
      recruits: [proposal({ id: 301, tool_input: JSON.stringify(agentRequest()) })],
      merges: [merge({ proposal_id: 21 }), merge({ proposal_id: 22 })],
      vcs: [vcsRow({ id: 63, status: "awaiting_owner" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const index = await screen.findByRole("navigation", { name: "Queues with something waiting" });
    const links = within(index).getAllByRole("link");
    expect(links.map((link) => link.textContent)).toEqual([
      "4 action approvals",
      "2 team actions",
      "1 recruitment",
      "2 contact merges",
      "1 in the git queue",
    ]);
    // Each link points at a section that is really there, and the section holds that queue.
    const targets = links.map((link) => (link.getAttribute("href") ?? "").replace(/^#/, ""));
    expect(targets).toEqual([
      "waiting-approvals",
      "waiting-team-actions",
      "waiting-recruits",
      "waiting-merges",
      "waiting-git",
    ]);
    for (const [at, title] of ["Action approvals", "Team actions", "Recruitment", "Contact merges", "Git queue"].entries()) {
      const section = document.getElementById(targets[at]);
      expect(section, targets[at]).not.toBeNull();
      expect(within(section as HTMLElement).getByRole("heading", { level: 2, name: title })).toBeDefined();
    }
    // The queues with nothing in them are the sentence below, not links.
    expect(screen.getByText(/^Nothing waiting in:/).textContent).toContain("Browser access requests");
    expect(within(index).queryByText(/browser access/)).toBeNull();
  });

  it("is not drawn when nothing is waiting", async () => {
    daemon.apiFetch.mockImplementation(waitingFetch(waitingWorld()));

    await renderWaiting();

    await screen.findByText(/^Nothing waiting in:/);
    expect(screen.queryByRole("navigation", { name: "Queues with something waiting" })).toBeNull();
  });
});

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
    //
    // Found by what it says rather than by being the page's only live region: every
    // `ConfirmButton` now renders its own polite `role="status"` — empty at rest — so that
    // arming and expiring are announced, and this page holds several of them.
    const note = screen.getByText(/^Only nucleos\./);
    expect(note.getAttribute("role")).toBe("status");
    expect(note.textContent).toBe("Only nucleos. Show everything");
    expect(within(note).getByRole("link", { name: "Show everything" }).getAttribute("href")).toBe(
      "/waiting",
    );
  });
});

describe("Waiting - declining only the action", () => {
  it("offers a third door that refuses the action and keeps the run going", async () => {
    const approval = proposal({ id: 41, kind: "action-approval", run_id: 7, tool_name: "Bash" });
    const base = waitingFetch(waitingWorld({ approvals: [approval] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/proposals/41/decline-action" && init?.method === "POST"
        ? { resume_run_id: 8 }
        : base(path, init),
    );
    await renderWaiting();

    const decline = await screen.findByRole("button", { name: "Decline, run continues" });
    fireEvent.click(decline);
    // The control has a real 300 ms dwell; a confirm click inside it is swallowed.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(await screen.findByRole("button", { name: /keep the run going/i }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/41/decline-action", { method: "POST" }),
    );
  });
});

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

describe("Waiting - wheel requests, two seats", () => {
  const approvals = () =>
    daemon.apiFetch.mock.calls
      .filter(
        ([path, init]) =>
          /^\/proposals\/\d+\/approve$/.test(String(path)) && (init as RequestInit | undefined)?.method === "POST",
      )
      .map(([path, init]) => ({
        path: String(path),
        body: JSON.parse(String((init as RequestInit).body ?? "{}")),
      }));

  it("an eligible wheel request offers Approve and Open real window and sends the seat", async () => {
    const world = waitingWorld({
      sessions: [session({ id: 5, proposal_id: 91, shell_eligible: true })],
      approveAnswer: { session: session({ id: 5, mode: "human", seat: "shell" }), seat_nonce: "n1" },
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));
    await renderWaiting();

    const approve = await screen.findByRole("button", { name: "Approve wheel #91" });
    expect(screen.getByRole("button", { name: "Open real window for wheel #91" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Decline wheel #91" })).toBeDefined();

    fireEvent.click(approve);
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Approve and drive it here in the shell" }));
    await waitFor(() => expect(approvals()).toHaveLength(1));
    expect(approvals()[0]).toEqual({ path: "/proposals/91/approve", body: { seat: "shell" } });
    // After a shell approval the way to the session is a link, not a guess.
    expect(await screen.findByRole("link", { name: "drive it on Web" })).toBeDefined();
    // The returned nonce is what lets the shell drive: it must have been stored.
    expect(seatNonce(5)).toBe("n1");

    fireEvent.click(screen.getByRole("button", { name: "Open real window for wheel #91" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Open a real browser here" }));
    await waitFor(() => expect(approvals()).toHaveLength(2));
    expect(approvals()[1].body).toEqual({ seat: "window" });
  });

  it("an ineligible wheel request offers only Open real window", async () => {
    const world = waitingWorld({
      sessions: [session({ id: 5, proposal_id: 91, shell_eligible: false })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));
    await renderWaiting();

    expect(await screen.findByRole("button", { name: "Open real window for wheel #91" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Approve wheel #91" })).toBeNull();
    expect(screen.getByRole("button", { name: "Decline wheel #91" })).toBeDefined();
  });
});

/* ------------------------------------------------- the design review's fixes -- */

describe("Waiting - clamped reasoning has a way out", () => {
  const long = "the agent explains at length why it wants this ".repeat(20);

  it("offers Show all only when the text overflows, and toggles it", async () => {
    // The DOM lays nothing out, so the measure is stubbed: scrollHeight above clientHeight is what
    // a clamped paragraph reports. happy-dom defines `scrollHeight` on Element and `clientHeight`
    // on HTMLElement, so each is spied where it really lives — a spy on a property that is not
    // there throws, and one that threw after the first was set left it set for the next test.
    const restores: Array<() => void> = [];
    try {
      const scroll = vi.spyOn(Element.prototype, "scrollHeight", "get").mockReturnValue(120);
      restores.push(() => scroll.mockRestore());
      const client = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(60);
      restores.push(() => client.mockRestore());

      daemon.apiFetch.mockImplementation(
        waitingFetch(
          waitingWorld({
            approvals: [1, 2, 3, 4].map((id) => proposal({ id, reasoning: long })),
          }),
        ),
      );

      await renderWaiting();

      const toggles = await screen.findAllByRole("button", { name: "Show all" });
      expect(toggles).toHaveLength(4);
      expect(toggles[0].getAttribute("aria-expanded")).toBe("false");
      expect(toggles[0].getAttribute("aria-controls")).not.toBeNull();

      fireEvent.click(toggles[0]);
      const opened = screen.getAllByRole("button", { name: "Show less" });
      expect(opened).toHaveLength(1);
      expect(opened[0].getAttribute("aria-expanded")).toBe("true");
      const paragraph = document.getElementById(opened[0].getAttribute("aria-controls") ?? "");
      expect(paragraph?.getAttribute("data-expanded")).toBe("true");
    } finally {
      for (const restore of restores) restore();
    }
  });

  it("offers nothing when the text fits", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1, reasoning: "a short reason" })] })),
    );

    await renderWaiting();

    await screen.findByText("a short reason");
    expect(screen.queryByRole("button", { name: "Show all" })).toBeNull();
  });
});

describe("Waiting - one verb pair", () => {
  it("an action approval says Approve, Decline (run continues) and Decline (stop run), and keeps each name across the arm", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 7, run_id: 3 })] })),
    );

    await renderWaiting();

    expect(await screen.findByRole("button", { name: "Approve" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Decline, run continues" })).toBeDefined();
    const stop = screen.getByRole("button", { name: "Decline, stop run" });
    // The three sit together in the card's one cluster of actions, and the one that ends the run
    // is the outline that says so.
    expect(stop.closest(".waiting-cluster")).not.toBeNull();
    expect(stop.className).toContain("ui-button-danger");
    expect(screen.getByRole("button", { name: "Approve" }).className).toContain("ui-button-approve");
    expect(screen.getByRole("button", { name: "Decline, run continues" }).className).toContain("ui-button-ghost");
    expect(screen.queryByRole("button", { name: /Reject|Refuse/ })).toBeNull();

    // Armed, every one still begins with the verb it had at rest, and the consequence follows.
    fireEvent.click(stop);
    expect(screen.getByRole("button", { name: "Decline and end the run" })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Decline, run continues" }));
    expect(screen.getByRole("button", { name: "Decline action and keep the run going" })).toBeDefined();
  });

  it("the recruit's caution is visible, and says so when engine or tool policy left the proposal", async () => {
    const world = waitingWorld({
      recruits: [proposal({ id: 301, tool_input: JSON.stringify(agentRequest({ engine: "claude" })) })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const engine = await screen.findByLabelText("Engine");
    const caution = document.getElementById(engine.getAttribute("aria-describedby") ?? "");
    if (caution === null) throw new Error("no caution line");
    expect(caution.hidden).toBe(false);
    expect(caution.className).not.toContain("sr-only");
    expect(caution.textContent).not.toMatch(/Changed from what the director proposed/);

    fireEvent.change(engine, { target: { value: "codex" } });
    expect(caution.textContent).toMatch(/Changed from what the director proposed/);
    fireEvent.change(engine, { target: { value: "claude" } });
    expect(caution.textContent).not.toMatch(/Changed from what the director proposed/);
  });

  it("the recruit form says the proposal changed under it, and loads the new one on request", async () => {
    const world = waitingWorld({
      recruits: [proposal({ id: 301, tool_input: JSON.stringify(agentRequest()) })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    const { queryClient } = await renderWaiting();
    const name = await screen.findByLabelText("Name");
    fireEvent.change(name, { target: { value: "Typed by hand" } });
    expect(screen.queryByText(/The director changed this proposal/)).toBeNull();

    world.recruits = [
      proposal({ id: 301, tool_input: JSON.stringify(agentRequest({ name: "Bea" })) }),
    ];
    await act(async () => {
      await queryClient.invalidateQueries();
    });

    // The form is left as typed, and a status line says it is behind.
    const said = await screen.findByText(/The director changed this proposal/);
    expect(said.getAttribute("role")).toBe("status");
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Typed by hand");

    fireEvent.click(screen.getByRole("button", { name: "Load the new proposal" }));
    await waitFor(() =>
      expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Bea"),
    );
    expect(screen.queryByText(/The director changed this proposal/)).toBeNull();
  });

  it("the recruit's Not now is an interlock that says Decline", async () => {
    const world = waitingWorld({
      recruits: [proposal({ id: 301, tool_input: JSON.stringify(agentRequest()) })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    expect(screen.queryByRole("button", { name: "Not now" })).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Decline" }));
    // One click arms and does not decide.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/proposals/301/reject", { method: "POST" });

    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Decline — the team may ask again" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/301/reject", { method: "POST" }),
    );
  });

  it("a contact merge and an exclusion say Decline too", async () => {
    const world = waitingWorld({
      merges: [merge({ proposal_id: 21 })],
      exclusions: [proposal({ id: 31, kind: "fleet-exclusion" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    expect(await screen.findByRole("button", { name: "Decline merge #21" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Decline request #31" })).toBeDefined();
    expect(screen.queryByRole("button", { name: /apart$/ })).toBeNull();
    // The sections wear plain names.
    expect(screen.getByRole("list", { name: "Keep jobs apart?" })).toBeDefined();
  });

  it("an error sentence says the daemon, not the project's own word for it", async () => {
    const base = waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 })] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/contacts/merges") throw new Error("network down");
      return await base(path, init);
    });

    await renderWaiting();

    const said = await screen.findByText(
      /the background service is not responding — nothing is known about the suggested merges/,
    );
    expect(said.textContent).not.toContain("núcleo");
  });
});

describe("Waiting - reading states and small guards", () => {
  it("a failed read offers Retry beside the sentence", async () => {
    const base = waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 })] }));
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/contacts/merges") throw new Error("network down");
      return await base(path, init);
    });

    await renderWaiting();

    expect(await screen.findByText(/nothing is known about the suggested merges/)).toBeDefined();
    expect(screen.getAllByRole("button", { name: "Retry" }).length).toBeGreaterThan(0);
  });

  it("the count of decisions wears the awaiting tone", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1 })] })),
    );

    await renderWaiting();

    const headline = await screen.findByText("1 waiting on a decision");
    expect(headline.className).toContain("ui-awaiting");
  });

  it("an exclusion request that names no pair cannot be approved", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(
        waitingWorld({
          exclusions: [
            proposal({ id: 31, kind: "fleet-exclusion", tool_input: JSON.stringify({ paths: [] }) }),
          ],
        }),
      ),
    );

    await renderWaiting();

    const approve = await screen.findByRole("button", { name: "Approve request #31" });
    expect((approve as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("Waiting - the facts a card needs", () => {
  it("an approval's run and project sit in its head with no uppercase labels", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(waitingWorld({ approvals: [proposal({ id: 1, run_id: 9, project_id: "alpha" })] })),
    );

    await renderWaiting();

    const card = (await screen.findByText("approval #1")).closest("li");
    if (card === null) throw new Error("no approval card");
    expect(within(card).getByRole("link", { name: "run 9" }).getAttribute("href")).toContain("/runs/9");
    expect(within(card).getByText("alpha")).toBeDefined();
    expect(card.querySelector("dt")).toBeNull();
  });

  it("the recruit's caution describes the two fields it is about, and is not a paragraph", async () => {
    const world = waitingWorld({
      recruits: [proposal({ id: 301, tool_input: JSON.stringify(agentRequest()) })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const engine = await screen.findByLabelText("Engine");
    const policy = screen.getByLabelText("Tool policy");
    for (const field of [engine, policy]) {
      const described = field.getAttribute("aria-describedby");
      expect(described).not.toBeNull();
      expect(document.getElementById(described ?? "")?.textContent).toMatch(/two fields a director gets wrong most often/);
      expect(field.getAttribute("title")).toBeNull();
      expect(field.className).toContain("waiting-recruit-input-careful");
    }
    expect(screen.getByLabelText("Speciality").getAttribute("aria-describedby")).toBeNull();
    expect(document.querySelector(".waiting-recruit-hint")).toBeNull();
  });
});

describe("Waiting - focus after a decision", () => {
  /** Approve the first card for real: arm, wait out the dwell, confirm — and the daemon drops the row. */
  async function decideFirst(world: ReturnType<typeof waitingWorld>) {
    const base = waitingFetch(world);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && path === "/proposals/1/approve") {
        world.approvals = world.approvals.filter((row) => row.id !== 1);
        return { resume_run_id: 5 };
      }
      return await base(path, init);
    });
    await renderWaiting();
    await screen.findByText("approval #1");
    const card = cardOf("approval #1");
    const approve = within(card).getByRole("button", { name: "Approve" });
    act(() => approve.focus());
    fireEvent.click(approve);
    await afterDwell();
    fireEvent.click(within(card).getByRole("button", { name: "Approve and let this action happen" }));
  }

  it("moves to the next card's heading when one is left", async () => {
    await decideFirst(waitingWorld({ approvals: [proposal({ id: 1 }), proposal({ id: 2 })] }));

    await waitFor(() => expect(screen.queryByText("approval #1")).toBeNull());
    const next = screen.getByText("approval #2");
    expect(next.getAttribute("tabindex")).toBe("-1");
    await waitFor(() => expect(document.activeElement).toBe(next));
  });

  it("moves to the section title when none is left, and keeps the receipt there", async () => {
    await decideFirst(waitingWorld({ approvals: [proposal({ id: 1 })] }));

    // Queried inside the wait: the panel's own title is still on screen for a tick before the
    // section collapses to its quiet line, and that is a different element.
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("heading", { level: 2, name: "Action approvals" })),
    );
    // The section stays for the visit, with its one line and the receipt of what was decided.
    expect(screen.getByText("nothing is waiting to be let through")).toBeDefined();
    expect(screen.getByText("run 5 is going again")).toBeDefined();
  });
});

describe("Waiting - an approval says what the run was doing", () => {
  it("shows why it was held, where it runs, what the run is for and what it did just before", async () => {
    const world = waitingWorld({
      approvals: [
        proposal({
          id: 7,
          run_id: 70,
          tool_name: "Bash",
          reasoning: "unrecognized shell commands require approval",
          tool_input: JSON.stringify({ command: "wc -l /tmp/ours.rs" }),
          run_status: "awaiting_approval",
          run_cwd: "C:/w/run-70",
          run_prompt: "Resolve the conflict in main.rs." + String.fromCharCode(10, 10) + "Rules follow.",
          run_recent: JSON.stringify([
            { name: "Bash", detail: "git status" },
            { name: "Bash", detail: "git show :2:main.rs" },
          ]),
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    expect(await screen.findByText("unrecognized shell commands require approval")).toBeDefined();
    expect(screen.getByText("C:/w/run-70")).toBeDefined();
    expect(screen.getByText("Resolve the conflict in main.rs.")).toBeDefined();
    expect(screen.queryByText(/Rules follow/)).toBeNull();
    const steps = screen.getByRole("list", { name: "Steps before approval #7" });
    expect(steps.textContent).toContain("git status");
    expect(steps.textContent).toContain("git show :2:main.rs");
    expect(screen.queryByText(/approving resumes nothing/)).toBeNull();
  });

  it("says so when the run is no longer waiting on it", async () => {
    const world = waitingWorld({
      approvals: [proposal({ id: 8, run_id: 80, run_status: "cancelled" })],
    });
    daemon.apiFetch.mockImplementation(waitingFetch(world));

    await renderWaiting();

    const notice = await screen.findByText(/The run is cancelled, not waiting on this/);
    expect(notice.getAttribute("role")).toBe("status");
    expect(notice.className).toBe("waiting-notice");
  });
});

describe("Waiting - a department's request says who asked and what for", () => {
  const department = {
    team_run_id: "tr-1",
    team_name: "Finance",
    team_request: "Reconcile October." + String.fromCharCode(10, 10) + "Use the ledger.",
  };

  it("a team action names the asker, the item, and the department's job, with a link", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(
        waitingWorld({
          teamActions: [
            proposal({
              id: 5,
              kind: "team-action",
              tool_name: "send_email",
              ...department,
              asked_by: "Rui",
              asked_for: "match the statement",
            }),
          ],
        }),
      ),
    );

    await renderWaiting();

    const link = await screen.findByRole("link", { name: "Finance" });
    expect(link.getAttribute("href")).toContain("/team-runs/tr-1");
    expect(link.parentElement?.textContent).toContain("asked by Rui");
    expect(link.parentElement?.textContent).toContain("match the statement");
    expect(screen.getByText("Reconcile October.")).toBeDefined();
    expect(screen.queryByText(/Use the ledger/)).toBeNull();
  });

  it("a recruit names the director who asked", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(
        waitingWorld({
          recruits: [proposal({ id: 6, kind: "agent-recruit", ...department, asked_by: "Ana" })],
        }),
      ),
    );

    await renderWaiting();

    const link = await screen.findByRole("link", { name: "Finance" });
    expect(link.parentElement?.textContent).toContain("asked by Ana");
  });
});

describe("Waiting - an exclusion request says what each job is doing", () => {
  it("names both jobs by their task", async () => {
    daemon.apiFetch.mockImplementation(
      waitingFetch(
        waitingWorld({
          exclusions: [
            proposal({
              id: 31,
              kind: "fleet-exclusion",
              tool_input: JSON.stringify({ pair: "4:9", job_low: 4, job_high: 9, paths: ["DOGFOOD.md"] }),
              job_low_task: "Rewrite the docs",
              job_high_task: "nightly-deps",
            }),
          ],
        }),
      ),
    );

    await renderWaiting();

    const jobs = await screen.findByLabelText("Jobs in request #31");
    expect(jobs.textContent).toContain("job 4");
    expect(jobs.textContent).toContain("Rewrite the docs");
    expect(jobs.textContent).toContain("job 9");
    expect(jobs.textContent).toContain("nightly-deps");
  });
});

describe("what a git request asks of the reader", () => {
  const merge = (overrides: Partial<VcsRequestSummary>): VcsRequestSummary => ({
    id: 1,
    op: "merge",
    project_id: "alpha",
    repo_key: "alpha",
    origin: "shell",
    status: "escalated",
    created_at: "2026-10-09T20:55:15Z",
    ...overrides,
  });

  it("reads every conflict git named, once, and keeps the shapes it cannot parse whole", () => {
    expect(
      conflictedFiles(
        [
          "Auto-merging a.rs",
          "CONFLICT (content): Merge conflict in a.rs",
          "CONFLICT (content): Merge conflict in a.rs",
          "CONFLICT (modify/delete): b.rs deleted in HEAD and modified in feat/x.",
          "",
        ].join("\n"),
      ),
    ).toEqual(["a.rs", "b.rs deleted in HEAD and modified in feat/x."]);
    expect(conflictedFiles(null)).toEqual([]);
  });

  it("says who acts next, and asks the reader only when it is theirs", () => {
    expect(gitNextStep(merge({ resolution_run_id: null }))?.yours).toBe(false);
    expect(gitNextStep(merge({ resolution_run_id: 7, resolution_status: "running" }))).toMatchObject({
      run: 7,
      yours: false,
    });
    expect(gitNextStep(merge({ resolution_run_id: 7, resolution_status: "awaiting_approval" }))?.yours).toBe(true);
    expect(gitNextStep(merge({ resolution_run_id: 7, resolution_status: "failed" }))).toMatchObject({
      run: 7,
      yours: true,
    });
    expect(gitNextStep(merge({ from_resolution: true }))?.yours).toBe(true);
    expect(gitNextStep(merge({ status: "blocked" }))?.before).toBe("submit it again");
  });

  // Requests 688/689/699: the run `completed` but the resolver refused it, so "the queue lands it
  // and this clears on its own" was a promise nobody would keep.
  it("does not promise a landing the resolver will refuse", () => {
    const done = { resolution_run_id: 7, resolution_status: "completed" } as const;
    expect(gitNextStep(merge({ ...done, resolution_gate_status: "passed" }))).toMatchObject({
      run: 7,
      yours: false,
    });
    expect(gitNextStep(merge({ ...done, resolution_gate_status: null }))?.yours).toBe(false);
    const red = gitNextStep(merge({ ...done, resolution_gate_status: "errored" }));
    expect(red).toMatchObject({ run: 7, yours: true });
    expect(red?.after).toContain("gate errored");
    expect(red?.after).not.toContain("clears on its own");
    const gone = gitNextStep(merge({ ...done, resolution_gate_status: "passed", resolution_worktree_gone: true }));
    expect(gone).toMatchObject({ run: 7, yours: true });
    expect(gone?.after).toContain("worktree is gone");
  });

  it("names no next step for a request that is not an escalated merge", () => {
    expect(gitNextStep(merge({ op: "push" }))).toBeNull();
  });
});
