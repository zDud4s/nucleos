import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Autopilot, { ApprovalQueuePanel } from "./Autopilot";
import type { ClassTally, ProjectSummary, Proposal } from "./api";

const DAEMON_URL = "http://127.0.0.1:8791";
const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function proposal(over: Partial<Proposal>): Proposal {
  return {
    id: 1,
    kind: "tool_use",
    status: "pending",
    run_id: 5,
    session_id: null,
    project_id: "alpha",
    tool_name: "Bash(rm -rf build)",
    reasoning: "wants to clear the build directory",
    tool_input: null,
    created_at: "2026-07-28T10:00:00Z",
    decided_at: null,
    ...over,
  };
}

/** Timer callbacks flip React state, so they belong inside an act() batch. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

/**
 * Lets the awaited fetch chains settle. Several rounds, because a refresh
 * awaits one batch of requests and then a second, scoped one.
 */
async function settle(rounds = 4) {
  for (let round = 0; round < rounds; round += 1) {
    await act(async () => {});
  }
}

/** The ids on screen, top to bottom — the thing a click lands on. */
function renderedIds(container: HTMLElement): number[] {
  return Array.from(container.querySelectorAll("article.approval-card .a-meta span:first-child"))
    .map((node) => Number(/#(\d+)/.exec(node.textContent ?? "")?.[1]));
}

/**
 * The approval queue is the one screen where a mis-aimed click costs
 * something irreversible: approve resumes an agent run that was stopped for
 * asking to leave its allowlist. It also refreshes itself every 3 seconds
 * from a list the daemon sorts, which is the hazard these tests pin.
 */
describe("ApprovalQueuePanel", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({ resume_run_id: 9 }) });
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  function renderPanel(proposals: Proposal[], refresh = vi.fn(async () => {})) {
    const view = render(
      <ApprovalQueuePanel
        proposals={proposals}
        loading={false}
        token="test-token"
        refresh={refresh}
        isKill={false}
        isSwamped={false}
      />,
    );
    return { ...view, refresh };
  }

  it("makes approving a two-step decision, like rejecting already was", async () => {
    renderPanel([proposal({ id: 11 })]);

    fireEvent.click(screen.getByRole("button", { name: "Approve & resume" }));
    expect(fetchMock).not.toHaveBeenCalled();

    advance(300);
    fireEvent.click(screen.getByRole("button", { name: "Approve & resume?" }));
    await settle();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe(`${DAEMON_URL}/proposals/11/approve`);
  });

  it("holds the queue still while a decision is open, then confirms the proposal that was aimed at", async () => {
    const first = proposal({ id: 11 });
    const second = proposal({ id: 12, run_id: 6 });
    const { container, rerender } = renderPanel([first, second]);

    // Two rows, so the aim matters: this is the FIRST card's button.
    fireEvent.click(screen.getAllByRole("button", { name: "Approve & resume" })[0]);

    // The 3s poll lands mid-decision with the daemon's new order. Re-sorting
    // now would slide proposal 12 under a cursor aimed at proposal 11.
    rerender(
      <ApprovalQueuePanel
        proposals={[second, first]}
        loading={false}
        token="test-token"
        refresh={vi.fn(async () => {})}
        isKill={false}
        isSwamped={false}
      />,
    );
    expect(renderedIds(container)).toEqual([11, 12]);

    advance(300);
    fireEvent.click(screen.getByRole("button", { name: "Approve & resume?" }));
    await settle();

    expect(fetchMock.mock.calls[0][0]).toBe(`${DAEMON_URL}/proposals/11/approve`);
    // And the hold is only for the duration of the decision: once it is made,
    // the queue takes the order the daemon last sent.
    expect(renderedIds(container)).toEqual([12, 11]);
  });

  it("holds the queue still for a reject in progress too", () => {
    const first = proposal({ id: 11 });
    const second = proposal({ id: 12, run_id: 6 });
    const { container, rerender } = renderPanel([first, second]);

    fireEvent.click(screen.getAllByRole("button", { name: "Reject and discard worktree" })[0]);
    rerender(
      <ApprovalQueuePanel
        proposals={[second, first]}
        loading={false}
        token="test-token"
        refresh={vi.fn(async () => {})}
        isKill={false}
        isSwamped={false}
      />,
    );

    expect(renderedIds(container)).toEqual([11, 12]);
  });

  it("takes a fresh order when nothing is being decided", () => {
    const first = proposal({ id: 11 });
    const second = proposal({ id: 12, run_id: 6 });
    const { container, rerender } = renderPanel([first, second]);

    rerender(
      <ApprovalQueuePanel
        proposals={[second, first]}
        loading={false}
        token="test-token"
        refresh={vi.fn(async () => {})}
        isKill={false}
        isSwamped={false}
      />,
    );

    expect(renderedIds(container)).toEqual([12, 11]);
  });

  it("reports a failed approval against the proposal it was aimed at", async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 409, json: async () => ({}) });
    renderPanel([proposal({ id: 11 })]);

    fireEvent.click(screen.getByRole("button", { name: "Approve & resume" }));
    advance(300);
    fireEvent.click(screen.getByRole("button", { name: "Approve & resume?" }));
    await settle();

    expect(screen.getByText("Could not approve this proposal.")).toBeTruthy();
  });
});

function projectSummary(over: Partial<ProjectSummary>): ProjectSummary {
  return {
    project_id: "alpha",
    mode: "shadow",
    project_root: "C:/Projects/alpha",
    pending: 0,
    classes_ready: 0,
    classes_total: 0,
    promotable: false,
    open_proposals: 0,
    wip_limit: 3,
    queue_full: false,
    ...over,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

describe("Autopilot project scope", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  it("drops a scoped refresh belonging to the project the user just left", async () => {
    const alphaScoreboard = deferred<unknown>();
    const okJson = (payload: unknown) => ({ ok: true, status: 200, json: async () => payload });
    const budget = {
      limit_usd: null, period: "daily", hourly_limit_usd: null,
      per_run_reserve_usd: 0.5, time_cost_per_hour_usd: 0,
      window_spend_usd: 0, hourly_spend_usd: 0, paused: false, reason: null,
    };
    const alphaTallies = [
      {
        mode: "shadow", action_class: "filesystem.write", total: 12, would_allow: 12,
        would_pend: 0, would_deny: 0, reviewed: 12, agree: 12, disagree: 0,
      },
    ] satisfies ClassTally[];

    fetchMock.mockImplementation(async (url: string) => {
      if (url.includes("/scoreboard?project_id=alpha")) return alphaScoreboard.promise;
      if (url.endsWith("/projects")) {
        return okJson([projectSummary({}), projectSummary({ project_id: "beta" })]);
      }
      if (url.endsWith("/autopilot/budget")) return okJson(budget);
      return okJson([]);
    });

    render(
      <Autopilot
        token="test-token"
        connection="connected"
        killEngaged={false}
        killBusy={false}
        toggleKill={vi.fn(async () => {})}
      />,
    );
    await settle();

    fireEvent.click(screen.getAllByRole("button", { name: "View" })[0]);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "View" }));
    await settle();

    // alpha's scoreboard finally answers, long after the user moved on. It is
    // the previous project's readiness — putting it on beta's panel would tell
    // the user beta had earned a promotion it never asked for.
    await act(async () => {
      alphaScoreboard.resolve(okJson(alphaTallies));
    });
    await settle();

    expect(screen.getByText("Viewing beta")).toBeTruthy();
    expect(screen.queryByText("filesystem.write")).toBeNull();
  });
});
