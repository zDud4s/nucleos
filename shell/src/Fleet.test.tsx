import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import Fleet from "./Fleet";
import {
  CONCURRENCY,
  JOBS,
  RUNS,
  advance,
  column,
  detail,
  fetchMock,
  job,
  readout,
  respondWith,
  run,
  settle,
} from "./test-fleet";

/** The spy leaves with the view: without that there is no way to assert it was called. */
function renderFleet() {
  const onOpenRuns = vi.fn();
  const view = render(
    <Fleet token="test-token" connection="connected" killEngaged={false} onOpenRuns={onOpenRuns} />,
  );
  return { ...view, onOpenRuns };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

/**
 * A second poll never stacks on one still in flight — the same discipline Autopilot and Home keep.
 * A slow daemon would otherwise start a fresh batch every 3 seconds and let the answers land out of
 * order.
 *
 * The promise is left unresolved ACROSS two ticks on purpose: it is the only way to tell "did not
 * stack" from "was fast".
 */
it("never stacks a poll on one still in flight", async () => {
  // NOT `let release: (() => void) | null = null` — TS narrows to `null` at the declaration, does
  // not see the assignment inside the executor, and `release?.()` reports TS2349. A callable
  // initial value avoids the narrowing.
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  // Shaped per route even though this test only counts calls: an object where the component
  // expects a list makes the render throw, and a thrown render is not the failure being tested.
  fetchMock.mockImplementation(async (url: string) => {
    await held;
    const body = url.includes(CONCURRENCY) ? readout([]) : [];
    return { ok: true, status: 200, json: async () => body };
  });

  renderFleet();
  await settle();
  const afterFirst = fetchMock.mock.calls.length;

  advance(3000);
  await settle();
  advance(3000);
  await settle();

  expect(fetchMock.mock.calls.length).toBe(afterFirst);

  // Released BEFORE the end, and allowed to land: a promise resolving after cleanup and
  // `useRealTimers` touches the state of an already-unmounted component.
  release();
  await settle();
});

/**
 * Capacity is the authority. When it fails, the cards from the last good tick stay and the view
 * calls itself stale, with the time — because a blank capacity reads as "there is room".
 */
it("keeps the last good cards and says when they were read", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: [job()] });
  renderFleet();
  await settle();
  expect(screen.getByText(/job 41/)).toBeTruthy();

  respondWith({ [CONCURRENCY]: null, [JOBS]: [job()] });
  advance(3000);
  await settle();

  expect(screen.getByText(/job 41/)).toBeTruthy();
  const banner = screen.getByRole("status");
  expect(banner.textContent).toMatch(/stale/i);
  // The time lives in a `<time>` attribute, not in the text: the text is relative and moves.
  expect(banner.querySelector("time")?.getAttribute("datetime")).toBeTruthy();
});

/** And the *New job* action leaves rather than failing after it is clicked. */
it("takes the new-job action away while the view is stale", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: [job()] });
  renderFleet();
  await settle();
  expect(screen.getByRole("button", { name: /new job/i })).toBeTruthy();

  respondWith({ [CONCURRENCY]: null, [JOBS]: [job()] });
  advance(3000);
  await settle();

  expect(screen.queryByRole("button", { name: /new job/i })).toBeNull();
});

/**
 * The card is drawn all the same when the description does not arrive. Capacity never lies, even
 * when the detail fails.
 */
it("draws a card for a slot whose owner no listing described", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: null });
  renderFleet();
  await settle();

  expect(screen.getByText(/job 41/)).toBeTruthy();
  expect(screen.getByText(/detail unavailable/i)).toBeTruthy();
});

/** A project on the roster with no slots gets a 0/N column, with the action to start one. */
it("gives an idle project a column of its own", async () => {
  respondWith({ [CONCURRENCY]: readout([column({ slots: [] })]) });
  renderFleet();
  await settle();

  expect(screen.getByText("0/2")).toBeTruthy();
  expect(screen.getByRole("button", { name: /new job/i })).toBeTruthy();
});

/** The two badges render apart, and `not_measured` is never read as clean. */
it("renders the two collision sources apart, and never as clean", async () => {
  const owner = { kind: "job", id: 41 };
  const other = { kind: "job", id: 42 };
  const twoSlots = [
    { project_id: "alpha", slot: 0, owner_kind: "job" as const, owner_id: 41, claimed_at: "t" },
    { project_id: "alpha", slot: 1, owner_kind: "job" as const, owner_id: 42, claimed_at: "t" },
  ];

  respondWith({
    [CONCURRENCY]: readout([
      column({
        slots: twoSlots,
        collision: {
          declared: { state: "collide", overlaps: [{ a: owner, b: other, paths: ["planned.rs"] }] },
          observed: { state: "collide", overlaps: [{ a: owner, b: other, paths: ["written.rs"] }] },
        },
      }),
    ]),
    [JOBS]: [job(), job({ id: 42, slot: 1 })],
  });
  renderFleet();
  await settle();

  // Two cards, each naming both sources: four badges, and the words are distinct without colour.
  expect(screen.getAllByText(/observed/i).length).toBe(2);
  expect(screen.getAllByText(/predicted/i).length).toBe(2);
  expect(screen.getAllByText(/written\.rs/).length).toBe(2);
  expect(screen.getAllByText(/planned\.rs/).length).toBe(2);

  respondWith({
    [CONCURRENCY]: readout([
      column({
        slots: twoSlots,
        collision: {
          declared: { state: "not_measured", overlaps: [] },
          observed: { state: "not_measured", overlaps: [] },
        },
      }),
    ]),
    [JOBS]: [job(), job({ id: 42, slot: 1 })],
  });
  advance(3000);
  await settle();

  expect(screen.getAllByText(/not measured/i).length).toBe(4);
  expect(screen.queryByText(/\bclean\b/i)).toBeNull();
});

/**
 * A tick landing mid-cancel does not resurrect the card.
 *
 * The `batchSeq` guard alone is not enough: the batch that lands is not stale, it is simply older
 * than the user — the daemon has not run its 30-second tick yet and still reports the slot.
 */
it("does not put a cancelled card back when a tick lands mid-cancel", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: [job()] });
  renderFleet();
  await settle();
  expect(screen.getByText(/job 41/)).toBeTruthy();

  // Two clicks with a dwell between them: `ConfirmButton` discards a second click inside 300ms, so
  // that a double-click cannot stop a night's work by accident.
  fireEvent.click(screen.getByRole("button", { name: /^cancel$/i }));
  advance(400);
  fireEvent.click(screen.getByRole("button", { name: /confirm cancel/i }));
  await settle();

  // The daemon still reports the slot: the job tick has not swept it yet.
  advance(3000);
  await settle();

  expect(screen.queryByText(/job 41/)).toBeNull();
});

/** A job's card opens into the chain of items it is running, and closes again. */
it("opens a job's card into its items", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: [job()], "/jobs/41": detail() });
  renderFleet();
  await settle();

  fireEvent.click(screen.getByRole("button", { name: /show items/i }));
  await settle();
  expect(screen.getAllByText("an item").length).toBeGreaterThan(0);

  fireEvent.click(screen.getByRole("button", { name: /hide items/i }));
  expect(screen.queryByText("an item")).toBeNull();
});

/** Two job cards in one column, which is what pairing needs. */
const TWO_JOBS = {
  [CONCURRENCY]: readout([
    column({
      slots: [
        { project_id: "alpha", slot: 0, owner_kind: "job" as const, owner_id: 41, claimed_at: "t" },
        { project_id: "alpha", slot: 1, owner_kind: "job" as const, owner_id: 42, claimed_at: "t" },
      ],
    }),
  ]),
  [JOBS]: [job(), job({ id: 42, slot: 1 })],
};

function exclusionCalls(method: string) {
  return fetchMock.mock.calls.filter(
    ([url, init]) =>
      String(url).includes("/fleet/exclusions") &&
      ((init as RequestInit | undefined)?.method ?? "GET") === method,
  );
}

/**
 * Asking takes two clicks because it names two jobs, and a card only knows one.
 *
 * This is the drag of the coming canvas without the canvas: arm one node, pick the other. What it
 * must NOT do is report the rule as made — the daemon answers with a proposal, and nothing about
 * scheduling changes until somebody approves it.
 */
it("asks for an exclusion by picking two cards, and says it is only a request", async () => {
  respondWith({ ...TWO_JOBS, "POST /fleet/exclusions": { proposal_id: 9 } });
  renderFleet();
  await settle();

  fireEvent.click(screen.getAllByRole("button", { name: /not at the same time as…/i })[0]);
  expect(screen.getByText(/pick the job that must not run/i)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /…as this one/i }));
  await settle();

  const [, init] = exclusionCalls("POST")[0];
  expect(JSON.parse(String((init as RequestInit).body))).toEqual({
    job_a: 41,
    job_b: 42,
    paths: [],
  });

  // The next tick brings the request back as a proposal, and it is drawn as a question.
  respondWith({
    ...TWO_JOBS,
    "/proposals": [
      {
        id: 9,
        kind: "fleet-exclusion",
        status: "pending",
        run_id: null,
        session_id: null,
        project_id: "alpha",
        tool_name: null,
        reasoning: "they both touch it",
        tool_input: JSON.stringify({ pair: "41:42", job_low: 41, job_high: 42 }),
        created_at: "t",
        decided_at: null,
      },
    ],
  });
  advance(3000);
  await settle();

  expect(screen.getAllByText(/waiting for approval/i).length).toBe(2);
});

/**
 * A rule in force reads differently at its two ends, and can be lifted.
 *
 * Only the higher id waits, so the same edge says "this one waits" on one card and "that one waits"
 * on the other. Saying the same thing at both ends would describe a deadlock the daemon cannot
 * produce.
 */
it("draws which end of a rule waits, and lifts it", async () => {
  respondWith({
    ...TWO_JOBS,
    "/fleet/exclusions": [
      {
        id: 3,
        project_id: "alpha",
        job_low: 41,
        job_high: 42,
        proposal_id: 9,
        paths: null,
        created_at: "t",
      },
    ],
  });
  renderFleet();
  await settle();

  expect(screen.getByText(/not at the same time as job 42 — that one waits/i)).toBeTruthy();
  expect(screen.getByText(/not at the same time as job 41 — this one waits/i)).toBeTruthy();

  fireEvent.click(screen.getAllByRole("button", { name: /^lift$/i })[0]);
  await settle();

  expect(exclusionCalls("DELETE").length).toBe(1);
  expect(String(exclusionCalls("DELETE")[0][0])).toMatch(/\/fleet\/exclusions\/3$/);
});

/**
 * The daemon's own sentence, not a status code the shell reinterprets.
 *
 * Its two 409s mean opposite things — wait for the approval, or stop clicking because the rule is
 * already in force — and only the sentence separates them.
 */
it("shows the daemon's own words when the ask is refused", async () => {
  respondWith(TWO_JOBS);
  fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
    if (String(url).includes("/fleet/exclusions") && init?.method === "POST") {
      return {
        ok: false,
        status: 409,
        text: async () => "these two jobs already have a request waiting for a decision",
      };
    }
    const body = String(url).includes(CONCURRENCY)
      ? TWO_JOBS[CONCURRENCY]
      : String(url).includes(JOBS)
        ? TWO_JOBS[JOBS]
        : [];
    return { ok: true, status: 200, json: async () => body };
  });
  renderFleet();
  await settle();

  fireEvent.click(screen.getAllByRole("button", { name: /not at the same time as…/i })[0]);
  fireEvent.click(screen.getByRole("button", { name: /…as this one/i }));
  await settle();

  expect(screen.getByText(/already have a request waiting for a decision/i)).toBeTruthy();
});

/** One job in a column has nothing to be paired with, and a run is not a job at all. */
it("offers the pairing action only where it can be used", async () => {
  respondWith({ [CONCURRENCY]: readout([column()]), [JOBS]: [job()] });
  renderFleet();
  await settle();

  expect(screen.queryByRole("button", { name: /not at the same time as…/i })).toBeNull();
});

/**
 * A run's card has no graph: it has a way through to the Runs tab.
 *
 * The `RUNS` key and the `run()` factory are load-bearing — without them `slotDetail` answers
 * `orphaned` and the button never appears.
 */
it("sends a run's card to the Runs tab instead of drawing a graph", async () => {
  respondWith({
    [CONCURRENCY]: readout([
      column({
        slots: [
          {
            project_id: "alpha",
            slot: 0,
            owner_kind: "run",
            owner_id: 7,
            claimed_at: "2026-08-09T00:00:00Z",
          },
        ],
      }),
    ]),
    [RUNS]: [run()],
  });
  const { onOpenRuns } = renderFleet();
  await settle();

  expect(screen.queryByRole("button", { name: /show items/i })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /open in runs/i }));

  expect(onOpenRuns).toHaveBeenCalled();
});
