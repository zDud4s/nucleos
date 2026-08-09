import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Approvals from "./Approvals";
import type { AwaitingRun, Proposal, VcsRequestSummary } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function request(overrides: Partial<VcsRequestSummary> = {}): VcsRequestSummary {
  return {
    id: 12,
    op: "merge",
    project_id: "alpha",
    repo_key: "c:/projects/alpha",
    origin: "run",
    status: "queued",
    created_at: "2026-08-08T10:00:00Z",
    ...overrides,
  };
}

function parked(overrides: Partial<AwaitingRun> = {}): AwaitingRun {
  return {
    id: 88,
    project_id: "alpha",
    prompt: "rename the config key",
    // Distinct from the request's `repo_key` on purpose: a parked run works in its own tree, and a
    // test where the two strings match cannot tell which panel rendered which.
    cwd: "c:/projects/alpha/.worktrees/run-88",
    created_at: "2026-08-08T09:00:00Z",
    ...overrides,
  };
}

function skipped(overrides: Partial<Proposal> = {}): Proposal {
  return {
    id: 4,
    kind: "skipped-item",
    status: "pending",
    run_id: 71,
    session_id: null,
    project_id: "alpha",
    tool_name: null,
    reasoning: "the gate stayed red after three attempts",
    tool_input: null,
    created_at: "2026-08-07T22:00:00Z",
    decided_at: null,
    ...overrides,
  };
}

/** A daemon holding one of each, answering every route this page reads. */
function daemonWith({
  requests = [request()],
  runs = [parked()],
  items = [skipped()],
}: {
  requests?: VcsRequestSummary[];
  runs?: AwaitingRun[];
  items?: Proposal[];
} = {}) {
  fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
    const target = String(url);
    if (target.includes("/proposals/skipped-items")) {
      return { ok: true, status: 200, json: async () => items };
    }
    if (/\/proposals\/\d+\/dismiss$/.test(target)) {
      return { ok: init?.method === "POST", status: 204 };
    }
    if (/\/vcs\/requests\/\d+$/.test(target)) {
      return {
        ok: true,
        status: 200,
        json: async () => ({ id: 12, status: "failed", result_sha: null, failure_reason: "conflict in src/main.rs" }),
      };
    }
    if (target.includes("/vcs/requests")) {
      return { ok: true, status: 200, json: async () => requests };
    }
    if (target.includes("/runs/awaiting-approval")) {
      return { ok: true, status: 200, json: async () => runs };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  });
}

async function show(state?: Parameters<typeof daemonWith>[0]) {
  daemonWith(state);
  await act(async () => {
    render(<Approvals token="t" connection="connected" />);
  });
}

/** Timer callbacks flip React state, so they belong inside an act() batch. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

beforeEach(() => {
  // Fake timers for the arm dwell as much as for the poll: `ConfirmButton` discards a second click
  // inside 300ms, so a test that clicks twice in a row confirms nothing without them.
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

describe("the waiting page", () => {
  it("reads all three lists the daemon has", async () => {
    await show();
    const asked = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(asked.some((url) => url.endsWith("/vcs/requests"))).toBe(true);
    expect(asked.some((url) => url.includes("/runs/awaiting-approval"))).toBe(true);
    expect(asked.some((url) => url.includes("/proposals/skipped-items"))).toBe(true);
  });

  it("shows a git request with who asked for it and which repository it locked", async () => {
    await show();
    expect(screen.getByText(/#12 · merge/)).toBeTruthy();
    // The origin is translated, because `run` alone does not say that the request was autonomous.
    expect(screen.getByText(/asked by a run/)).toBeTruthy();
    // The key travels beside the project label, which is the whole reason `RequestSummary` carries
    // both — two rows with different labels and one key were queued behind each other.
    expect(screen.getByText("c:/projects/alpha")).toBeTruthy();
  });

  /**
   * The count is of what is still moving, never of the list's length. The listing is the permanent
   * history of every operation the daemon ever queued, so "3 requests" would be a statement about
   * uptime rather than about the queue.
   */
  it("counts only the requests still in flight", async () => {
    await show({
      requests: [
        request({ id: 1, status: "succeeded" }),
        request({ id: 2, status: "queued" }),
        request({ id: 3, status: "running" }),
        request({ id: 4, status: "failed" }),
      ],
    });
    expect(screen.getByText("2 in flight")).toBeTruthy();
  });

  it("says nothing is in flight when every request has settled", async () => {
    await show({ requests: [request({ status: "succeeded" }), request({ id: 2, status: "rejected" })] });
    expect(screen.getByText("nothing in flight")).toBeTruthy();
  });

  /** A settled row is the only one worth asking about — the others have not ended yet. */
  it("offers the ending only for a request that has one", async () => {
    await show({ requests: [request({ status: "queued" })] });
    expect(screen.queryByText("How it ended")).toBeNull();

    await show({ requests: [request({ status: "failed" })] });
    const ending = screen.getByText("How it ended");
    await act(async () => {
      fireEvent.click(ending);
    });
    expect(screen.getByText("conflict in src/main.rs")).toBeTruthy();
  });

  /**
   * The queue has no approve and no reject route — a merge that needs a person is answered through
   * the proposal it raised. A button here would be a promise the daemon cannot keep.
   */
  it("offers no verdict on a git request", async () => {
    await show({ requests: [request({ status: "awaiting_approval" })] });
    expect(screen.queryByText(/^Approve/)).toBeNull();
    expect(screen.queryByText(/^Reject/)).toBeNull();
  });

  it("shows a parked run and points at where it is answered", async () => {
    await show();
    expect(screen.getByText("rename the config key")).toBeTruthy();
    expect(screen.getByText(/Autopilot tab/)).toBeTruthy();
  });

  /** Its own door, not `/reject` — `reject_proposal` guards on `action-approval` and would 409. */
  it("dismisses a skipped item through the dismiss route", async () => {
    await show();
    expect(screen.getByText("the gate stayed red after three attempts")).toBeTruthy();

    const dismiss = screen.getByText("Dismiss");
    await act(async () => {
      fireEvent.click(dismiss);
    });
    // Armed, not fired: the second click is the confirmation.
    expect(fetchMock.mock.calls.some((call) => String(call[0]).includes("/dismiss"))).toBe(false);

    // Past the dwell, or the second click is discarded as an accidental double-click.
    advance(300);
    await act(async () => {
      fireEvent.click(screen.getByText("Put it away?"));
    });
    const call = fetchMock.mock.calls.find((entry) => String(entry[0]).includes("/dismiss"));
    expect(call).toBeTruthy();
    expect(String(call![0])).toContain("/proposals/4/dismiss");
    expect(call![1].method).toBe("POST");
  });

  it("teaches each empty list instead of showing nothing", async () => {
    await show({ requests: [], runs: [], items: [] });
    expect(screen.getByText("No git operation has been queued.")).toBeTruthy();
    expect(screen.getByText("No run is parked.")).toBeTruthy();
    expect(screen.getByText("Nothing was put down.")).toBeTruthy();
  });

  it("reads nothing when there is no token", async () => {
    await act(async () => {
      render(<Approvals token={null} connection="connected" />);
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
