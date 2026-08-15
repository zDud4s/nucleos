import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";

import RunTail from "./RunTail";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

interface Answer {
  /** `0` is not a status: it is the daemon not answering at all. */
  status: number;
  body?: unknown;
}

/** Answers the tail route once per entry, in order; the last entry then repeats. */
function daemonSays(...answers: Answer[]) {
  let turn = 0;
  fetchMock.mockImplementation(async () => {
    const answer = answers[Math.min(turn, answers.length - 1)];
    turn += 1;
    if (answer.status === 0) throw new TypeError("Failed to fetch");
    return {
      ok: answer.status < 400,
      status: answer.status,
      json: async () => answer.body,
    };
  });
}

function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

async function settle(rounds = 4) {
  for (let round = 0; round < rounds; round += 1) await act(async () => {});
}

function stream(): string | null {
  return document.querySelector(".rd-tail__stream")?.textContent ?? null;
}

/** The `since` each request carried, in order. */
function offsets(): (string | null)[] {
  return fetchMock.mock.calls.map((call) => new URL(String(call[0])).searchParams.get("since"));
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

/**
 * The panel grows; it does not redraw.
 *
 * The offset it sends back is the daemon's `next` and never the length of the string it received.
 * `"três\n"` is five characters and six bytes, so a panel counting characters would ask for byte 5
 * on the next tick and draw the tail end of `ê` again — which is why the fixture is not ASCII.
 */
it("adds what is new at each tick and never redraws what it has already shown", async () => {
  daemonSays(
    { status: 200, body: { text: "três\n", next: 6, live: true } },
    { status: 200, body: { text: "quatro\n", next: 13, live: true } },
    { status: 200, body: { text: "", next: 13, live: true } },
  );

  render(<RunTail token="t" runId={7} live />);
  await settle();
  expect(stream()).toBe("três\n");

  advance(3000);
  await settle();
  expect(stream()).toBe("três\nquatro\n");

  // Nothing was written since, and nothing on screen moves.
  advance(3000);
  await settle();
  expect(stream()).toBe("três\nquatro\n");
  expect(offsets()).toEqual(["0", "6", "13"]);
});

/**
 * A 204 is an absent tail, not an empty one.
 *
 * The run is still `running` as far as the caller can see, so there IS output — this daemon just
 * does not hold it, most often because it was restarted under a run another process began. Drawing
 * an empty transcript there would claim a working run has written nothing.
 */
it("says the output is recorded rather than drawing an empty transcript", async () => {
  daemonSays({ status: 204 });

  render(<RunTail token="t" runId={7} live />);
  await settle();

  expect(stream()).toBeNull();
  expect(document.body.textContent).toMatch(/recorded/i);
});

/**
 * And for a run that has already ended, it draws nothing at all.
 *
 * The detail view around it already shows `stdout` in full. A line reading "recorded" under every
 * finished run would be chrome pointing at the block directly beneath it.
 */
it("draws nothing for a finished run whose output is already on the page", async () => {
  daemonSays({ status: 204 });

  const { container } = render(<RunTail token="t" runId={7} live={false} />);
  await settle();

  expect(container.textContent).toBe("");
  advance(3000);
  await settle();
  // One read, and no poll: a finished run gains no tail by being asked again.
  expect(fetchMock.mock.calls.length).toBe(1);
});

/**
 * An unreachable daemon is a third answer, and it is not "recorded".
 *
 * A failed read says nothing about where the output is. Calling it recorded would send someone
 * looking in `runs.stdout` for lines the run has not finished writing.
 */
it("keeps the lines it has when the daemon stops answering, and does not call them recorded", async () => {
  daemonSays({ status: 200, body: { text: "três\n", next: 6, live: true } }, { status: 0 });

  render(<RunTail token="t" runId={7} live />);
  await settle();
  advance(3000);
  await settle();

  expect(stream()).toBe("três\n");
  expect(document.body.textContent).not.toMatch(/recorded/i);
  expect(document.body.textContent).toMatch(/did not answer/i);
});

/**
 * The last read happens as the run settles, not before it.
 *
 * The tail is dropped when the run's task ends, so the tick that finds it gone is the one that has
 * the whole story. Stopping the poll a tick early would leave the panel claiming to be live over a
 * run that had finished.
 */
it("reads once more when the run stops moving", async () => {
  daemonSays({ status: 200, body: { text: "três\n", next: 6, live: true } });

  const panel = render(<RunTail token="t" runId={7} live />);
  await settle();
  const whileLive = fetchMock.mock.calls.length;

  daemonSays({ status: 204 });
  panel.rerender(<RunTail token="t" runId={7} live={false} />);
  await settle();

  expect(whileLive).toBe(1);
  expect(fetchMock.mock.calls.length).toBe(2);
  expect(stream()).toBe("três\n");
  expect(document.body.textContent).toMatch(/recorded/i);
});
