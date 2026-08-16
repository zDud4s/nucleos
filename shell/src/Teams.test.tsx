import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Teams from "./Teams";
import type { Agent, Team, TeamRun, TeamRunDetail } from "./api";

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
    created_at: "2026-08-16T10:00:00Z",
    updated_at: "2026-08-16T10:00:00Z",
    members: ["copywriter"],
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
  write?: { ok: boolean; status: number };
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
    if (path.includes("/team-runs/")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.detail ?? detail() });
    }
    if (path.endsWith("/team-runs")) {
      return Promise.resolve({ ok: true, status: 200, json: async () => state.runs ?? [] });
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
