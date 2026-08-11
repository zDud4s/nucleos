import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

import JobGraph from "./JobGraph";
import { advance, detail, fetchMock, respondWith, settle } from "./test-fleet";
import type { Proposal } from "./api";

const JOB = "/jobs/41";
const SKIPPED = "/proposals/skipped-items";

function proposal(over: Partial<Proposal> = {}): Proposal {
  return {
    id: 3,
    kind: "skipped-item",
    status: "pending",
    run_id: 5,
    session_id: null,
    project_id: "alpha",
    tool_name: null,
    reasoning: "the file it was told to change was not there",
    tool_input: null,
    created_at: "2026-08-09T00:00:00Z",
    decided_at: null,
    ...over,
  };
}

function renderGraph(live = true) {
  return render(<JobGraph token="test-token" jobId={41} live={live} />);
}

function jobReads() {
  return fetchMock.mock.calls.filter((call) => String(call[0]).includes(JOB)).length;
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

/**
 * The queue moves under whoever is reading it — an item goes from running to passed while you look
 * — so an open row keeps up. A finished one never changes again, and polling it would be a request
 * per tick for a row that has said everything it has to say.
 */
it("polls an open job's queue only while the job is live", async () => {
  respondWith({ [JOB]: detail(), [SKIPPED]: [] });
  const live = renderGraph(true);
  await settle();
  const afterFirst = jobReads();

  advance(3000);
  await settle();
  expect(jobReads()).toBeGreaterThan(afterFirst);

  live.unmount();
  fetchMock.mockClear();

  renderGraph(false);
  await settle();
  const once = jobReads();
  advance(3000);
  await settle();
  expect(jobReads()).toBe(once);
});

/**
 * The three gate outcomes are three things. An item reading `passed` with no gate status was NOT
 * measured — the project has no gate command, or it was an intermediate item under
 * `gate_after_each_item: false` — and reading that as green is the same class of error as saying
 * `clean` without having measured.
 */
it("keeps passed, failed and could-not-measure as three separate readings", async () => {
  respondWith({ [JOB]: detail(), [SKIPPED]: [] });
  renderGraph();
  await settle();

  const readings = Array.from(
    document.querySelectorAll(".item-row__reading"),
    (node) => node.textContent,
  );
  expect(readings[0]).toBe("passed");
  expect(readings[1]).toBe("not measured");
  expect(readings[2]).toBe("the gate failed");
  expect(readings[3]).toBe("the gate could not run");
  expect(new Set(readings.slice(0, 4)).size).toBe(4);
});

/** The round distinguishes "asked for eight at once" from "asked for five and replanned". */
it("marks where one round ended and the next began", async () => {
  respondWith({ [JOB]: detail(), [SKIPPED]: [] });
  renderGraph();
  await settle();

  const marks = document.querySelectorAll(".item-round");
  // One mark, and it sits between the last round-0 item and the first round-1 one.
  expect(marks.length).toBe(1);
  expect(marks[0].textContent).toMatch(/round 2/i);
});

/** A skipped item carries the proposal that explains it, joined by `run_id`. */
it("explains a skipped item with the proposal that stands for it", async () => {
  respondWith({ [JOB]: detail(), [SKIPPED]: [proposal()] });
  const withProposal = renderGraph();
  await settle();
  expect(screen.getByText(/the file it was told to change was not there/)).toBeTruthy();
  withProposal.unmount();

  respondWith({ [JOB]: detail(), [SKIPPED]: [] });
  const withoutProposal = renderGraph();
  await settle();
  expect(screen.getByText(/has been put away/)).toBeTruthy();
  withoutProposal.unmount();

  // A failed read is neither: it is not known whether the proposal is there or not.
  respondWith({ [JOB]: detail(), [SKIPPED]: null });
  renderGraph();
  await settle();
  expect(screen.queryByText(/has been put away/)).toBeNull();
  expect(screen.queryByText(/the file it was told to change was not there/)).toBeNull();
});
