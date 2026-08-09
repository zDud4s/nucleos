import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Runs from "./Runs";
import type { RunDetail, RunSearchResult } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function row(overrides: Partial<RunSearchResult> = {}): RunSearchResult {
  return {
    id: 7, project_id: "alpha", status: "running", mode: "real",
    created_at: "2026-07-30T10:00:00Z", completed_at: null,
    cost_usd: null, prompt_excerpt: "do the thing",
    ...overrides,
  };
}

function detail(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: 7, project_id: "alpha", status: "running",
    gate_status: null, gate_exit_code: null, gate_output: null,
    exit_code: null, stdout: null, stderr: null, session_id: null,
    cost_usd: null, input_tokens: null, output_tokens: null,
    cache_read_tokens: null, num_turns: null,
    context_fill: null, steerable: false,
    ...overrides,
  };
}

/** A daemon holding one run, answering the index and the detail with what the test describes. */
function daemonWith(one: RunDetail, presets: unknown[] = []) {
  fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
    const target = String(url);
    if (target.endsWith("/presets")) return { ok: true, status: 200, json: async () => presets };
    if (/\/runs\/\d+\/message$/.test(target)) {
      return { ok: true, status: init?.method === "DELETE" ? 204 : 202 };
    }
    if (/\/runs\/\d+$/.test(target)) return { ok: true, status: 200, json: async () => one };
    if (target.includes("/runs")) {
      return { ok: true, status: 200, json: async () => [row({ id: one.id, status: one.status })] };
    }
    return { ok: false, status: 404 };
  });
}

async function settle() {
  await act(async () => {});
}

/** Opens the single run row the index returned. */
async function openTheRun() {
  fireEvent.click(screen.getByText("do the thing"));
  await settle();
}

function steeringCalls() {
  return fetchMock.mock.calls.filter(([url]) => /\/runs\/\d+\/message$/.test(String(url)));
}

describe("talking to a run that is still working", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("offers no composer for a run that was never created to listen", async () => {
    daemonWith(detail({ steerable: false, status: "running" }));

    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    // Absent rather than disabled, and deliberately: whether a run listens is decided when it
    // starts and never revisited, so there is nothing the person could do here to change it. A
    // greyed-out box would suggest otherwise.
    expect(screen.queryByPlaceholderText("Say something to this run…")).toBeNull();
    expect(screen.queryByText("End the conversation")).toBeNull();
  });

  it("offers no composer once a listening run has finished", async () => {
    daemonWith(detail({ steerable: true, status: "completed" }));

    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    // The flag stays true on a finished run — it records how the run was started. What ended is the
    // chance to speak to it, so the composer goes and the "listening" badge with it.
    expect(screen.queryByPlaceholderText("Say something to this run…")).toBeNull();
  });

  it("sends a turn to a listening run and says it was queued, not answered", async () => {
    daemonWith(detail({ steerable: true, status: "running" }));

    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    fireEvent.change(screen.getByPlaceholderText("Say something to this run…"), {
      target: { value: "check the other branch first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await settle();

    const [url, init] = steeringCalls()[0] as [string, RequestInit];
    expect(url).toContain("/runs/7/message");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify({ message: "check the other branch first" }));
    // The daemon answers 202: the text reached the run's channel. It has not been read yet, and
    // saying "sent" would claim the run has seen it.
    expect(screen.getByText(/Queued/)).toBeTruthy();
  });

  it("names a refusal that no retry can fix differently from one that a moment changed", async () => {
    daemonWith(detail({ steerable: true, status: "running" }));
    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    const say = async (status: number) => {
      fetchMock.mockImplementationOnce(async () => ({ ok: false, status }));
      fireEvent.change(screen.getByPlaceholderText("Say something to this run…"), {
        target: { value: "hello" },
      });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      await settle();
    };

    await say(409);
    expect(screen.getByText(/not listening any more/)).toBeTruthy();

    await say(403);
    // A different sentence, because the answer is different: this run may never be spoken to, so
    // "try again" is advice that cannot work.
    expect(screen.getByText(/may never be spoken to/)).toBeTruthy();
  });

  it("closes the conversation with a DELETE, behind a confirmation", async () => {
    // Fake timers only here: `ConfirmButton` swallows a second click inside 400ms as a double-click
    // accident, so the deliberate second click has to happen after that dwell rather than instantly.
    vi.useFakeTimers();
    try {
      daemonWith(detail({ steerable: true, status: "running" }));
      render(<Runs token="t" connection="connected" />);
      await settle();
      await openTheRun();

      fireEvent.click(screen.getByRole("button", { name: "End the conversation" }));
      // Confirmed because there is no reopening it — the daemon lets go of the run's input for good.
      expect(steeringCalls()).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(400);
      });
      fireEvent.click(screen.getByRole("button", { name: "Confirm end?" }));
      await settle();

      const [url, init] = steeringCalls()[0] as [string, RequestInit];
      expect(url).toContain("/runs/7/message");
      expect(init.method).toBe("DELETE");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("how close a run is to handing off", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("draws nothing when the run has not reported a context fill", async () => {
    daemonWith(detail({ context_fill: null }));
    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    expect(screen.queryByText(/context/)).toBeNull();
  });

  it("says a long run is about to continue in a successor", async () => {
    daemonWith(detail({ context_fill: 170_000 }));
    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();

    // Past four fifths of the window, the daemon stops this run and starts a fresh one. That is a
    // visible change in how the work proceeds, so it is stated rather than left to the bar.
    expect(screen.getByText(/past the handoff line/)).toBeTruthy();
  });
});

/**
 * The other door out of `awaiting_approval`.
 *
 * `worktree::release` claims the run with `status = 'awaiting_approval'` inside its own write and
 * answers 409 for anything else, so the button belongs to exactly one state. Until it landed here
 * nothing in this window could let a parked run's tree go: the daemon's GC will not touch a
 * worktree an `awaiting_approval` run still owns.
 */
describe("abandoning a run parked for approval", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  async function openParked(status = "awaiting_approval") {
    daemonWith(detail({ status }));
    render(<Runs token="t" connection="connected" />);
    await settle();
    await openTheRun();
  }

  it("offers no release for a run that is still working", async () => {
    await openParked("running");
    expect(screen.queryByText("Abandon and release worktree")).toBeNull();
  });

  it("offers no release for a run that has already finished", async () => {
    await openParked("completed");
    expect(screen.queryByText("Abandon and release worktree")).toBeNull();
  });

  it("releases the worktree through the run's own route", async () => {
    await openParked();

    fireEvent.click(screen.getByText("Abandon and release worktree"));
    // Armed, not fired. This deletes a tree and everything written in it.
    expect(fetchMock.mock.calls.some(([url]) => String(url).includes("/release"))).toBe(false);

    act(() => {
      vi.advanceTimersByTime(300);
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Discard run and worktree?"));
    });

    const call = fetchMock.mock.calls.find(([url]) => String(url).includes("/release"));
    expect(call).toBeTruthy();
    expect(String(call![0])).toContain("/worktrees/7/release");
    expect((call![1] as RequestInit).method).toBe("POST");
  });

  /** 409 means the proposal was answered elsewhere — the good ending, not a fault to chase. */
  it("does not call a race with the proposal queue a failure", async () => {
    await openParked();

    fetchMock.mockImplementation(async (url: string) => {
      const target = String(url);
      if (target.includes("/release")) return { ok: false, status: 409 };
      if (/\/runs\/\d+$/.test(target)) {
        return { ok: true, status: 200, json: async () => detail({ status: "awaiting_approval" }) };
      }
      // The row stays in the index the refresh reads, or the detail view unmounts under the note it
      // was about to show and the test would be measuring its own mock.
      return { ok: true, status: 200, json: async () => [row({ status: "awaiting_approval" })] };
    });

    fireEvent.click(screen.getByText("Abandon and release worktree"));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Discard run and worktree?"));
    });

    expect(screen.getByText(/no longer waiting for approval/)).toBeTruthy();
  });
});
