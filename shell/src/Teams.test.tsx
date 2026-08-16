import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Teams from "./Teams";
import type {
  Agent, Team, TeamAction, TeamRun, TeamRunDetail, TeamTrigger, TeamTriggerNext,
} from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "copywriter",
    name: "copywriter",
    speciality: "writes short copy",
    prompt: "You write short copy.",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: "2026-08-16T10:00:00Z",
    updated_at: "2026-08-16T10:00:00Z",
    ...overrides,
  };
}

function team(overrides: Partial<Team> = {}): Team {
  return {
    id: "marketing",
    name: "Marketing",
    mission: "sell the thing",
    director_agent_id: "director",
    max_rounds: 3,
    max_parallel: 2,
    budget_usd: null,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-16T10:00:00Z",
    updated_at: "2026-08-16T10:00:00Z",
    members: ["copywriter"],
    grants: [],
    ...overrides,
  };
}

function run(overrides: Partial<TeamRun> = {}): TeamRun {
  return {
    id: "run-1",
    team_id: "marketing",
    request: "write the launch post",
    workspace: "teams/marketing/run-1",
    state: "working",
    director_node: "none",
    director_run_id: null,
    round: 1,
    next_ordinal: 3,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "done",
    outcome: null,
    why: null,
    created_at: "2026-08-16T10:00:00Z",
    updated_at: "2026-08-16T10:05:00Z",
    finished_at: null,
    ...overrides,
  };
}

function detail(overrides: Partial<TeamRunDetail> = {}): TeamRunDetail {
  return {
    ...run(),
    cost_usd: 1.25,
    items: [
      {
        ordinal: 1, round: 0, agent_id: "researcher", description: "find the numbers",
        state: "done", run_id: 11, output_path: "1-researcher.md",
      },
      {
        ordinal: 2, round: 1, agent_id: "copywriter", description: "draft the post",
        state: "running", run_id: 12, output_path: null,
      },
    ],
    ...overrides,
  };
}

interface DaemonState {
  teams?: Team[];
  agents?: Agent[];
  runs?: TeamRun[];
  detail?: TeamRunDetail;
  actions?: TeamAction[];
  triggers?: TeamTrigger[];
  next?: TeamTriggerNext;
  write?: { ok: boolean; status: number };
}

function trigger(overrides: Partial<TeamTrigger> = {}): TeamTrigger {
  return {
    id: 1,
    team_id: "marketing",
    name: "morning summary",
    enabled: 1,
    source: "cron",
    cron: "0 7 * * *",
    timezone: "Europe/Lisbon",
    from_team: null,
    email_class: null,
    request: "prepare the summary",
    created_at: "2026-08-16T10:00:00Z",
    updated_at: "2026-08-16T10:00:00Z",
    ...overrides,
  };
}

/** A daemon holding this state, answering every write with the status given. */
function daemonHolding(state: DaemonState) {
  const write = state.write ?? { ok: true, status: 200 };
  fetchMock.mockImplementation((url: string, init?: { method?: string }) => {
    const method = init?.method ?? "GET";
    const path = String(url);
    if (method !== "GET") {
      return Promise.resolve({
        ...write,
        json: async () => (path.endsWith("/runs") ? { id: "run-new" } : (state.teams?.[0] ?? team())),
      });
    }
    // Before the detail branch: `/team-runs/{id}/actions` also contains `/team-runs/`, and a
    // detail object handed to a list would be a crash rather than an empty panel.
    if (path.endsWith("/actions")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.actions ?? [] });
    }
    if (path.includes("/team-runs/")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.detail ?? detail() });
    }
    if (path.endsWith("/team-runs")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.runs ?? [] });
    }
    if (path.includes("/team-triggers/") && path.endsWith("/next")) {
      return Promise.resolve({
        ok: true,
        status: 200,
        json: async () => state.next ?? { next: null, error: null },
      });
    }
    if (path.endsWith("/team-triggers")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.triggers ?? [] });
    }
    if (path.endsWith("/agents")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.agents ?? [] });
    }
    return Promise.resolve({ ok: true, status: 200, json: async () => state.teams ?? [] });
  });
}

async function show(state: DaemonState) {
  daemonHolding(state);
  await act(async () => {
    render(<Teams token="t" connection="connected" />);
  });
}

function writeCalls() {
  return fetchMock.mock.calls.filter(
    ([, init]) => (init as { method?: string } | undefined)?.method !== undefined,
  );
}

/** `ConfirmButton` discards a second click inside its 300ms dwell, so the clock has to move. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }));
afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

describe("the departments tab", () => {
  it("says what a team is when there are none, rather than showing an empty box", async () => {
    await show({});
    expect(screen.getByText(/No teams yet/)).toBeTruthy();
  });

  it("shows a team with its director, its roster size and both ceilings", async () => {
    await show({ teams: [team()], agents: [agent()] });

    expect(screen.getByText("Marketing")).toBeTruthy();
    expect(screen.getByText("director")).toBeTruthy();
    expect(screen.getByText("1 specialists")).toBeTruthy();
    expect(screen.getByText("3 rounds, 2 at once")).toBeTruthy();
    // Said in words rather than left blank: "no ceiling of its own" and "a ceiling of nothing" are
    // opposite facts, and a blank reads as the second.
    expect(screen.getByText("no ceiling of its own")).toBeTruthy();
  });

  it("asks a team for something and opens the run it started", async () => {
    await show({ teams: [team()], agents: [agent()] });

    fireEvent.change(screen.getByLabelText("ask Marketing for something"), {
      target: { value: "write the launch post" },
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Start"));
    });

    const started = writeCalls().find(([url]) => String(url).endsWith("/teams/marketing/runs"));
    expect(started).toBeTruthy();
    expect(JSON.parse(String((started?.[1] as { body?: string }).body))).toEqual({
      request: "write the launch post",
    });
  });

  /** The daemon's refusals are the useful half of this screen; a generic failure is not. */
  it("explains a refused start in terms the owner can act on", async () => {
    await show({ teams: [team()], agents: [agent()], write: { ok: false, status: 400 } });

    fireEvent.change(screen.getByLabelText("ask Marketing for something"), {
      target: { value: "do the thing" },
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Start"));
    });

    expect(screen.getByText(/no specialists/)).toBeTruthy();
  });

  it("says the budget reopens rather than calling a 429 a failure", async () => {
    await show({ teams: [team()], agents: [agent()], write: { ok: false, status: 429 } });

    fireEvent.change(screen.getByLabelText("ask Marketing for something"), {
      target: { value: "do the thing" },
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Start"));
    });

    expect(screen.getByText(/reopens/)).toBeTruthy();
  });

  it("sends the roster as the whole membership, because the daemon replaces it", async () => {
    await show({ teams: [], agents: [agent(), agent({ id: "director", name: "director" })] });

    fireEvent.click(screen.getByText("New team"));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Marketing" } });
    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "sell the thing" } });
    fireEvent.change(screen.getByLabelText("Director"), { target: { value: "director" } });
    fireEvent.click(screen.getByLabelText(/copywriter/));
    await act(async () => {
      fireEvent.click(screen.getByText("Save team"));
    });

    const saved = writeCalls().find(([url]) => String(url).endsWith("/teams"));
    expect(JSON.parse(String((saved?.[1] as { body?: string }).body))).toMatchObject({
      name: "Marketing",
      director_agent_id: "director",
      members: ["copywriter"],
      budget_usd: null,
    });
  });

  it("will not save a team with no director, because the daemon would refuse it", async () => {
    await show({ teams: [], agents: [agent()] });

    fireEvent.click(screen.getByText("New team"));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Marketing" } });
    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "sell the thing" } });

    expect((screen.getByText("Save team") as HTMLButtonElement).disabled).toBe(true);
    expect(writeCalls()).toHaveLength(0);
  });

  /**
   * A department may only write documents into its own folder until somebody says otherwise, so
   * the default the form sends is an EMPTY grants list. The absence of a row is the denial: there
   * is no `deny` mode for a second opinion to disagree with.
   */
  it("grants nothing unless the owner chooses it, and sends only what was chosen", async () => {
    await show({ teams: [], agents: [agent({ id: "director", name: "director" })] });

    fireEvent.click(screen.getByText("New team"));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Marketing" } });
    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "sell" } });
    fireEvent.change(screen.getByLabelText("Director"), { target: { value: "director" } });
    fireEvent.change(screen.getByLabelText(/Send email/), { target: { value: "propose" } });
    await act(async () => {
      fireEvent.click(screen.getByText("Save team"));
    });

    const saved = writeCalls().find(([url]) => String(url).endsWith("/teams"));
    const body = JSON.parse(String((saved?.[1] as { body?: string }).body));
    expect(body.grants).toEqual([{ kind: "send_email", mode: "propose" }]);
    expect(body.max_open_actions).toBe(5);
  });

  it("says on the card what a department may do, and says nothing when it may do nothing", async () => {
    await show({
      teams: [
        team(),
        team({
          id: "support",
          name: "Support",
          grants: [{ kind: "file_document", mode: "allow" }],
        }),
      ],
      agents: [agent()],
    });

    expect(screen.getByText(/may file_document freely/)).toBeTruthy();
    expect(screen.queryByText(/^may send_email/)).toBeNull();
  });

  /**
   * A rule sits beside the department it starts, and says whether it is armed — because writing a
   * rule and arming it are two acts, and a screen that showed only the text would make the two
   * look like one.
   */
  it("shows what starts a department, and whether it is armed", async () => {
    await show({
      teams: [team()],
      agents: [agent()],
      triggers: [
        trigger(),
        trigger({
          id: 2,
          name: "write it up",
          enabled: 0,
          source: "team_finished",
          cron: null,
          timezone: null,
          from_team: "research",
        }),
      ],
      next: { next: "2026-08-17T07:00:00Z", error: null },
    });

    expect(screen.getByText("morning summary")).toBeTruthy();
    expect(screen.getByText("0 7 * * * Europe/Lisbon")).toBeTruthy();
    expect(screen.getByText("armed")).toBeTruthy();
    // The name and what fires it are two facts, and the card shows both.
    expect(screen.getByText("write it up")).toBeTruthy();
    expect(screen.getByText("after research")).toBeTruthy();
    expect(screen.getByText("not armed")).toBeTruthy();
  });

  /**
   * An invalid cron makes a rule that is skipped every tick and logged at debug — 2,880 times a
   * day, which is to say invisibly. This is where it becomes a sentence somebody reads.
   */
  it("says why a rule will never fire instead of showing a time that never comes", async () => {
    await show({
      teams: [team()],
      agents: [agent()],
      triggers: [trigger({ cron: "every morning please" })],
      next: { next: null, error: "'every morning please' is not a cron expression" },
    });

    expect(screen.getByText(/is not a cron expression/)).toBeTruthy();
  });

  it("arms a rule through the daemon rather than by hiding the button", async () => {
    // A team with a ceiling of its own, which is the path where arming is one click. The
    // ceiling-less team is the other path, and has its own tests below.
    await show({
      teams: [team({ budget_usd: 5 })],
      agents: [agent()],
      triggers: [trigger({ enabled: 0 })],
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Arm"));
    });
    const armed = writeCalls().find(([url]) => String(url).endsWith("/team-triggers/1/enable"));
    expect(JSON.parse(String((armed?.[1] as { body?: string }).body))).toEqual({ enabled: true });
  });

  /**
   * The design's second risk, made visible. A clock rule on a team with no `budget_usd` starts a
   * run that is its own root, and the tree ceiling read is the root's — so nothing the owner set
   * for this department caps what the chain spends. A question and not a refusal: the house budget
   * is still there, and the owner may well mean it.
   */
  it("asks before arming a rule whose tree no ceiling of this team's bounds", async () => {
    await show({
      teams: [team({ budget_usd: null })],
      agents: [agent()],
      triggers: [trigger({ enabled: 0 })],
    });

    expect(screen.getByText(/no ceiling on what this starts/)).toBeTruthy();

    // The first click asks. Nothing has been armed at the daemon by then.
    fireEvent.click(screen.getByText("Arm"));
    expect(screen.getByText("Arm with no ceiling?")).toBeTruthy();
    expect(writeCalls().filter(([url]) => String(url).endsWith("/enable"))).toHaveLength(0);

    advance(400);
    await act(async () => {
      fireEvent.click(screen.getByText("Arm with no ceiling?"));
    });
    const armed = writeCalls().find(([url]) => String(url).endsWith("/team-triggers/1/enable"));
    expect(JSON.parse(String((armed?.[1] as { body?: string }).body))).toEqual({ enabled: true });
  });

  /**
   * The distinction that makes the warning true rather than decorative. `team_finished` fires with
   * the lineage of the run that ended, so the tree ceiling is the ROOT team's — this team's null
   * says nothing about it, and a warning here would name the wrong number.
   */
  it("does not warn about a rule that joins someone else's tree instead of starting one", async () => {
    await show({
      teams: [team({ budget_usd: null })],
      agents: [agent()],
      triggers: [
        trigger({
          enabled: 0,
          source: "team_finished",
          cron: null,
          timezone: null,
          from_team: "research",
        }),
      ],
    });

    expect(screen.queryByText(/no ceiling on what this starts/)).toBeNull();
    expect(screen.getByText("Arm")).toBeTruthy();
  });

  /** Disarming is the safe direction, and a question in front of it is one people learn to skip. */
  it("does not ask before disarming a rule that has no ceiling", async () => {
    await show({
      teams: [team({ budget_usd: null })],
      agents: [agent()],
      triggers: [trigger({ enabled: 1 })],
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Disarm"));
    });
    const off = writeCalls().find(([url]) => String(url).endsWith("/team-triggers/1/enable"));
    expect(JSON.parse(String((off?.[1] as { body?: string }).body))).toEqual({ enabled: false });
  });

  /** The ceilings belong to the daemon; the form only refuses one round trip earlier. */
  it("will not save a team past the daemon's ceilings", async () => {
    await show({ teams: [], agents: [agent({ id: "director", name: "director" })] });

    fireEvent.click(screen.getByText("New team"));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Marketing" } });
    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "sell" } });
    fireEvent.change(screen.getByLabelText("Director"), { target: { value: "director" } });
    fireEvent.change(screen.getByLabelText("Rounds"), { target: { value: "99" } });

    expect((screen.getByText("Save team") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("one run", () => {
  async function open() {
    await show({ teams: [team()], agents: [agent()], runs: [run()] });
    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });
  }

  it("shows every round's items with who did them and where the answer landed", async () => {
    await open();

    expect(screen.getByText("Round 0")).toBeTruthy();
    expect(screen.getByText("Round 1")).toBeTruthy();
    expect(screen.getByText("researcher")).toBeTruthy();
    expect(screen.getByText("1-researcher.md")).toBeTruthy();
    expect(screen.getByText("find the numbers")).toBeTruthy();
    // What it has cost so far, the director's own nodes included.
    expect(screen.getByText("$1.25")).toBeTruthy();
  });

  it("points at the folder rather than growing a second file browser", async () => {
    await open();
    expect(screen.getByText("Delivery: teams/marketing/run-1/")).toBeTruthy();
  });

  /**
   * An email is shown as a recipient, a subject and a body — not as the JSON it is stored as. A
   * person who cannot read what a department asked for cannot judge whether to allow it.
   */
  it("shows what a department asked for as the thing it would do", async () => {
    await show({
      teams: [team()],
      agents: [agent()],
      runs: [run()],
      actions: [
        {
          id: 4,
          team_run_id: "run-1",
          kind: "send_email",
          payload: JSON.stringify({
            to: "list@example.com",
            subject: "we launch tomorrow",
            body: "Details inside.",
          }),
          why: "the list asked to be told",
          proposal_id: 41,
          state: "pending",
          error: null,
          created_at: "2026-08-16T11:00:00Z",
          executed_at: null,
        },
      ],
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });

    expect(screen.getByText("list@example.com")).toBeTruthy();
    expect(screen.getByText("we launch tomorrow")).toBeTruthy();
    expect(screen.getByText("Details inside.")).toBeTruthy();
    expect(screen.getByText("the list asked to be told")).toBeTruthy();
    expect(screen.getByText("proposal #41")).toBeTruthy();
  });

  it("offers cancel while the run is live and delete once it is not", async () => {
    await open();
    expect(screen.getByText("Cancel run")).toBeTruthy();
    expect(screen.queryByText("Delete")).toBeNull();
  });

  it("offers delete once the run has ended, and never cancel", async () => {
    await show({
      teams: [team()],
      agents: [agent()],
      runs: [run({ state: "done", finished_at: "2026-08-16T11:00:00Z" })],
      detail: detail({ state: "done", finished_at: "2026-08-16T11:00:00Z" }),
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });

    expect(screen.getByText("Delete")).toBeTruthy();
    expect(screen.queryByText("Cancel run")).toBeNull();
  });

  it("cancels through the daemon rather than just hiding the run", async () => {
    await open();

    fireEvent.click(screen.getByText("Cancel run"));
    advance(400);
    await act(async () => {
      fireEvent.click(screen.getByText("Confirm cancel?"));
    });

    expect(
      writeCalls().some(([url]) => String(url).endsWith("/team-runs/run-1/cancel")),
    ).toBe(true);
  });

  /** A ceiling is not a failure, and the badge is where an owner reads which one it was. */
  it("shows a stopped run as stopped and says why", async () => {
    await show({
      teams: [team()],
      agents: [agent()],
      runs: [run({ state: "stopped" })],
      detail: detail({
        state: "stopped",
        why: "the team's ceiling of $5.00 is spent ($5.40)",
        finished_at: "2026-08-16T11:00:00Z",
      }),
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });

    expect(screen.getByText("stopped")).toBeTruthy();
    expect(screen.getByText(/ceiling of \$5\.00 is spent/)).toBeTruthy();
  });
});
