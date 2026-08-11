import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Council from "./Council";
import type { CouncilSeatView, CouncilSummary, CouncilView } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function seat(overrides: Partial<CouncilSeatView> = {}): CouncilSeatView {
  return {
    seat_idx: 0,
    kind: "cloud",
    ref: "claude-opus-4-8",
    stage1_status: "ok",
    stage1_error: null,
    answer: "the first answer",
    stage2_status: "ok",
    stage2_error: null,
    rankings: [],
    ...overrides,
  };
}

function council(overrides: Partial<CouncilView> = {}): CouncilView {
  return {
    id: "c1",
    created_at: "2026-08-11T10:00:00Z",
    question: "why does it do that?",
    status: "running",
    stage: 1,
    error: null,
    chairman_kind: "cloud",
    chairman_ref: "the-chairman",
    synthesis: null,
    anon_map: {},
    leaderboard: [],
    seats: [seat()],
    ...overrides,
  };
}

function summary(overrides: Partial<CouncilSummary> = {}): CouncilSummary {
  return {
    id: "c1",
    created_at: "2026-08-11T10:00:00Z",
    question: "why does it do that?",
    status: "running",
    stage: 1,
    ...overrides,
  };
}

/** The daemon answering the list route and the detail route, and nothing else. */
function daemonHolding(list: CouncilSummary[], detail: CouncilView) {
  fetchMock.mockImplementation((url: string) => {
    if (/\/council\/[^/]+$/.test(String(url))) {
      return Promise.resolve({ ok: true, json: async () => detail });
    }
    return Promise.resolve({ ok: true, json: async () => list });
  });
}

async function show(list: CouncilSummary[], detail: CouncilView) {
  daemonHolding(list, detail);
  await act(async () => {
    render(<Council token="t" connection="connected" />);
  });
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

describe("the council", () => {
  /**
   * A record read halfway through is the ordinary case: the deliberation takes minutes, and the
   * whole reason this polls is that a person watches it fill in.
   */
  it("renders seats as they arrive", async () => {
    await show(
      [summary()],
      council({
        seats: [
          seat({ seat_idx: 0, ref: "answered-already", answer: "the first answer" }),
          seat({
            seat_idx: 1,
            ref: "still-thinking",
            stage1_status: "pending",
            answer: null,
            stage2_status: "pending",
          }),
        ],
      }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText("the first answer")).toBeTruthy();
    // A seat still working says so. Rendering it as an absence would make a slow model and a broken
    // one look identical.
    expect(screen.getByText(/Still working/)).toBeTruthy();
  });

  /**
   * The three endings the daemon keeps apart must stay apart here. A refusal, a wall clock and a
   * cancellation say different things about a model, and only one of them is worth retrying.
   */
  it("shows a failed seat's error rather than an empty space", async () => {
    await show(
      [summary()],
      council({
        status: "done",
        stage: 3,
        seats: [
          seat({ seat_idx: 0, stage1_status: "error", stage1_error: "the model refused", answer: null }),
          seat({ seat_idx: 1, stage1_status: "timeout", stage1_error: null, answer: null }),
        ],
      }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText(/Did not answer/)).toBeTruthy();
    expect(screen.getByText(/the model refused/)).toBeTruthy();
    expect(screen.getByText(/Ran out of time/)).toBeTruthy();
  });

  /**
   * The state EVERY council reaches at a month old, now that the record outlives the transcripts it
   * reads out of: the seats and the leaderboard are kept ninety days, the answers thirty. A seat
   * that ended `ok` with nothing to show has to say the text expired — rendering nothing made it
   * look like a seat nobody asked, and "did not answer" would blame the model for the calendar.
   */
  it("says an answer expired rather than drawing a seat that answered as empty", async () => {
    await show(
      [summary({ status: "done" })],
      council({
        status: "done",
        stage: 3,
        synthesis: null,
        seats: [seat({ seat_idx: 0, ref: "answered-long-ago", stage1_status: "ok", answer: null })],
      }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText(/Answered\. The text has since expired\./)).toBeTruthy();
    expect(screen.queryByText(/Did not answer/)).toBeNull();
    // `done` is reached only when the chairman's own seat ended `ok`, so it DID write one. Saying
    // "produced nothing" here would be a false statement about a council that worked.
    expect(screen.getByText(/The chairman wrote one, and the text has since expired/)).toBeTruthy();
    expect(screen.queryByText(/produced nothing/)).toBeNull();
  });

  /** And a chairman that really did fail still says so — the inference above must not swallow it. */
  it("keeps a chairman that produced nothing apart from one that expired", async () => {
    await show(
      [summary({ status: "error" })],
      council({ status: "error", stage: 3, synthesis: null }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText(/The chairman produced nothing\./)).toBeTruthy();
    expect(screen.queryByText(/has since expired/)).toBeNull();
  });

  /**
   * The timer is the one thing in this tab that can go wrong quietly. A council that settled and
   * kept being fetched every two seconds would poll the daemon for as long as the tab stayed open,
   * and nothing on screen would say so.
   */
  it("stops polling when the council ends", async () => {
    await show([summary()], council({ status: "running" }));

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    // Now the daemon reports it finished, and one more poll picks that up.
    daemonHolding([summary({ status: "done" })], council({ status: "done", stage: 3 }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2100);
    });

    fetchMock.mockClear();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(
      fetchMock.mock.calls.filter((call) => /\/council\/[^/]+$/.test(String(call[0]))),
    ).toHaveLength(0);
  });

  /** An empty table would suggest the ranking ran and found nothing. Below two answers it does not run. */
  it("says why there is no ranking instead of showing an empty table", async () => {
    await show([summary()], council({ status: "done", stage: 3, leaderboard: [] }));

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText(/at least two seats to have answered/)).toBeTruthy();
  });

  /** The vote count is part of the claim: an average over one vote is not what five make. */
  it("shows how many peers ranked each seat", async () => {
    await show(
      [summary()],
      council({
        status: "done",
        stage: 3,
        synthesis: "they broadly agreed",
        leaderboard: [
          { seat_idx: 0, avg_rank: 1.5, n: 2 },
          { seat_idx: 1, avg_rank: 2, n: 1 },
        ],
        seats: [
          seat({ seat_idx: 0, ref: "first-model" }),
          seat({ seat_idx: 1, ref: "second-model" }),
        ],
      }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("why does it do that?"));
    });

    expect(screen.getByText(/average rank 1\.50 from 2 votes/)).toBeTruthy();
    expect(screen.getByText(/average rank 2\.00 from 1 vote/)).toBeTruthy();
    expect(screen.getByText("they broadly agreed")).toBeTruthy();
  });

  /**
   * Every refusal here is something a person can act on, and flattening them would present all
   * three as "it did not work" — the one answer that suggests nothing to do about it.
   */
  it("names what to do about each way convening can be refused", async () => {
    for (const [status, expected] of [
      [503, /\.ai\/council\.yaml/],
      [429, /budget/],
      [400, /would not take that question/],
    ] as const) {
      fetchMock.mockReset();
      fetchMock.mockImplementation((_url: string, init?: RequestInit) => {
        if (init?.method === "POST") {
          return Promise.resolve({ ok: false, status, text: async () => "" });
        }
        return Promise.resolve({ ok: true, json: async () => [] });
      });

      const view = render(<Council token="t" connection="connected" />);
      const box = screen.getByLabelText("Question for the council");
      fireEvent.change(box, { target: { value: "why?" } });
      await act(async () => {
        fireEvent.submit(box.closest("form")!);
      });

      // `getAllBy`, because the empty-list note names the same file: a daemon with no council both
      // refuses the POST and has nothing to list, and asserting on one element would be asserting
      // that the other message does not exist.
      expect(screen.getAllByText(expected).length).toBeGreaterThan(0);
      view.unmount();
    }
  });

  it("asks nothing when there is no token", async () => {
    await act(async () => {
      render(<Council token={null} connection="connected" />);
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
