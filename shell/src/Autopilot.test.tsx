import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Autopilot, { ApprovalQueuePanel, JobsPanel } from "./Autopilot";
import type { ClassTally, Job, JobDetail, JobItem, ProjectSummary, Proposal } from "./api";

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

/**
 * The jobs panel is where a night of autonomous work becomes readable, and where the two
 * cancellation levels stop being a claim in a design document. These pin the parts that are easy
 * to get subtly, silently wrong.
 */
describe("JobsPanel", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  function job(over: Partial<Job> = {}): Job {
    return {
      id: 7,
      project_id: "alpha",
      rule_name: "nightly-backlog",
      status: "implementing",
      wait_reason: null,
      max_items: 5,
      created_at: "2026-07-30T03:00:00Z",
      completed_at: null,
      slot: 0,
      round: 0,
      max_rounds: 1,
      ...over,
    };
  }

  function jobItem(over: Partial<JobItem> = {}): JobItem {
    return { ordinal: 0, description: "an item", status: "pending", round: 0, run_id: null, gate_status: null, ...over };
  }

  /** Answers the detail route with a queue, and every other route with a bare ok. */
  function serveDetail(detail: Partial<JobDetail> & { items: JobItem[] }) {
    fetchMock.mockImplementation((url: string) => {
      if (typeof url === "string" && /\/jobs\/\d+$/.test(url)) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: async () => ({ ...job(), branch: "nucleos/job-7", ...detail }),
        });
      }
      return Promise.resolve({ ok: true, status: 204, json: async () => ({}) });
    });
  }

  function renderPanel(jobs: Job[], refresh = vi.fn(async () => {})) {
    return {
      ...render(
        <JobsPanel jobs={jobs} loading={false} selectedProject={null} token="test-token" refresh={refresh} isKill={false} />,
      ),
      refresh,
    };
  }

  it("offers to stop a job that is parked, which cancelling a run cannot reach", async () => {
    // The whole reason the two levels exist as separate buttons. A job waiting for the budget has
    // no node to cancel; without this its only ending is the four-hour ceiling.
    renderPanel([job({ status: "waiting", wait_reason: "budget" })]);

    expect(screen.getByText(/the budget is spent/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    advance(300);
    fireEvent.click(screen.getByRole("button", { name: "Stop the whole job?" }));
    await settle();

    const calls = fetchMock.mock.calls;
    const [url, init] = calls[calls.length - 1] ?? [];
    expect(url).toBe(`${DAEMON_URL}/jobs/7/cancel`);
    expect(init?.method).toBe("POST");
  });

  it("does not offer to stop a job that has already ended", () => {
    renderPanel([job({ status: "completed", completed_at: "2026-07-30T05:00:00Z" })]);
    expect(screen.queryByRole("button", { name: "Stop job" })).toBeNull();
  });

  it("reads a 409 as 'it already finished', not as a failure to stop it", async () => {
    // The job ended between the render and the click. Telling the user it "could not be cancelled"
    // would send them looking for a fault that is not there.
    fetchMock.mockResolvedValue({ ok: false, status: 409, json: async () => ({}) });
    renderPanel([job()]);

    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    advance(300);
    fireEvent.click(screen.getByRole("button", { name: "Stop the whole job?" }));
    await settle();

    expect(screen.getByText(/already finished/)).toBeTruthy();
  });

  it("never claims a verdict for an item nothing measured", async () => {
    // `passed` with no gate status is legitimate — no gate command, or an intermediate item under
    // gate_after_each_item: false — and it is not a verdict. One label for both would invent one.
    serveDetail({
      items: [
        jobItem({ ordinal: 0, description: "guard the cursor", status: "passed" }),
        jobItem({ ordinal: 1, description: "retry on lock", status: "passed", gate_status: "passed" }),
      ],
    });
    renderPanel([job()]);

    fireEvent.click(screen.getByRole("button", { name: "Show list" }));
    await settle();

    expect(screen.getByText("done, not measured")).toBeTruthy();
    expect(screen.getByText("passed the tests")).toBeTruthy();
    expect(screen.getByText("guard the cursor")).toBeTruthy();
  });

  it("reads an empty queue as a quiet night rather than as a failure", async () => {
    // A planner that looked and found no work had a successful night. Rendering that as an error
    // is what teaches somebody to stop reading the panel.
    serveDetail({ items: [], status: "completed" });
    renderPanel([job({ status: "completed" })]);

    fireEvent.click(screen.getByRole("button", { name: "Show list" }));
    await settle();

    expect(screen.getByText(/found no work/)).toBeTruthy();
  });

  it("keeps up with a live job's list and leaves a finished one alone", async () => {
    // A live job's items move while the reader is looking at them. A finished job never changes
    // again, so polling it is a request per tick for a row that has said all it has to say.
    serveDetail({ items: [jobItem({ status: "running" })] });
    const { unmount } = renderPanel([job({ status: "implementing" })]);
    fireEvent.click(screen.getByRole("button", { name: "Show list" }));
    await settle();
    const afterOpen = fetchMock.mock.calls.length;
    advance(3000);
    await settle();
    expect(fetchMock.mock.calls.length).toBeGreaterThan(afterOpen);
    unmount();

    fetchMock.mockClear();
    serveDetail({ items: [jobItem({ status: "passed" })], status: "completed" });
    renderPanel([job({ status: "completed" })]);
    fireEvent.click(screen.getByRole("button", { name: "Show list" }));
    await settle();
    const once = fetchMock.mock.calls.length;
    advance(9000);
    await settle();
    expect(fetchMock.mock.calls.length).toBe(once);
  });

  /**
   * Starting a job by hand.
   *
   * Until this landed a job could only be born from a `graph:` rule, so trying one meant writing a
   * schedule and waiting for it to fire. It goes through `POST /jobs` — the scheduler's own front
   * door — so every refusal the daemon raises at 3am is raised here too.
   */
  describe("starting one by hand", () => {
    function openForm() {
      renderPanel([]);
      fireEvent.click(screen.getByText("Start a job by hand"));
    }

    it("sends null for the two limits left blank rather than zero", async () => {
      fetchMock.mockImplementation(() =>
        Promise.resolve({ ok: true, status: 201, json: async () => ({ job_id: 42 }) }),
      );
      openForm();

      fireEvent.change(screen.getByPlaceholderText("which project"), {
        target: { value: "alpha" },
      });
      fireEvent.change(screen.getByPlaceholderText(/graph: rule would carry/), {
        target: { value: "tidy the logs" },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Start job" }));
      });

      const call = fetchMock.mock.calls.find(([url]) => String(url) === `${DAEMON_URL}/jobs`);
      expect(call).toBeTruthy();
      // Blank means "the house limit governs" and "one round". Zero would mean a job allowed to
      // spend nothing and run nothing.
      expect(JSON.parse((call![1] as RequestInit).body as string)).toEqual({
        project_id: "alpha", prompt: "tidy the logs", budget_usd: null, max_rounds: null,
      });
      expect(screen.getByText("Job 42 started.")).toBeTruthy();
    });

    /**
     * The daemon's sentence, not a translation of the status. `409` is both "the kill switch is
     * engaged" and "no room"; the two have different remedies, and only the body tells them apart.
     */
    it("shows the daemon's own words when it refuses", async () => {
      fetchMock.mockImplementation(() =>
        Promise.resolve({
          ok: false,
          status: 409,
          text: async () => "no free slot for alpha; something else is running",
        }),
      );
      openForm();

      fireEvent.change(screen.getByPlaceholderText("which project"), {
        target: { value: "alpha" },
      });
      fireEvent.change(screen.getByPlaceholderText(/graph: rule would carry/), {
        target: { value: "tidy the logs" },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Start job" }));
      });

      expect(screen.getByText("no free slot for alpha; something else is running")).toBeTruthy();
    });

    /** Caught here rather than at the daemon, which would answer 422 for a body it cannot parse. */
    it("refuses a budget that is not a number without asking the daemon", async () => {
      fetchMock.mockClear();
      openForm();

      fireEvent.change(screen.getByPlaceholderText("which project"), {
        target: { value: "alpha" },
      });
      fireEvent.change(screen.getByPlaceholderText(/graph: rule would carry/), {
        target: { value: "tidy the logs" },
      });
      fireEvent.change(screen.getByPlaceholderText("(the house limit)"), {
        target: { value: "as much as it takes" },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Start job" }));
      });

      expect(screen.getByText(/must be a number of dollars/)).toBeTruthy();
      expect(fetchMock.mock.calls.some(([url]) => String(url) === `${DAEMON_URL}/jobs`)).toBe(false);
    });
  });
});
