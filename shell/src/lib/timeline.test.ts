import { describe, expect, it } from "vitest";
import type { FeedEntry } from "../data/feed";
import {
  DAY,
  HOUR,
  MINUTE,
  buildFeedDays,
  clock,
  quietGaps,
  resolveWindow,
  span,
  timeTicks,
} from "./timeline";

/* A local noon, so no test here straddles a midnight by accident of the machine's time zone. */
const NOON = new Date(2026, 7, 24, 12, 0, 0, 0).getTime();

function line(id: number, at: number, kind = "job_started"): FeedEntry {
  return { id, project_id: "alpha", kind, summary: `line ${id}`, run_id: null, errand_id: null, subject: null, created_at: new Date(at).toISOString() };
}

const seenAt = (at: number) => ({ through: 1, through_created_at: new Date(at).toISOString(), seen_at: new Date(at).toISOString() });

describe("which window 'since you looked' draws", () => {
  it("starts at the marker when it is between twelve hours and seven days old", () => {
    const window = resolveWindow("seen", NOON, seenAt(NOON - 20 * HOUR));
    expect(window.start).toBe(NOON - 20 * HOUR);
    expect(window.basis).toBe("marker");
  });

  it("holds twelve hours at least, and says it clamped", () => {
    const window = resolveWindow("seen", NOON, seenAt(NOON - 10 * MINUTE));
    expect(window.start).toBe(NOON - 12 * HOUR);
    expect(window.basis).toBe("min");
    // The marker itself survives the clamp: the shading still starts where you looked.
    expect(window.lookedAt).toBe(NOON - 10 * MINUTE);
  });

  it("reaches back seven days at most, and says it clamped", () => {
    const window = resolveWindow("seen", NOON, seenAt(NOON - 30 * DAY));
    expect(window.start).toBe(NOON - 7 * DAY);
    expect(window.basis).toBe("max");
  });

  it("is a day when nothing was ever marked, and the fixed presets ignore the marker", () => {
    expect(resolveWindow("seen", NOON, { through: null, through_created_at: null, seen_at: null })).toMatchObject({ start: NOON - DAY, basis: "unmarked" });
    expect(resolveWindow("day", NOON, seenAt(NOON - 3 * DAY)).start).toBe(NOON - DAY);
    expect(resolveWindow("week", NOON, null).start).toBe(NOON - 7 * DAY);
  });
});

describe("the axis", () => {
  it("ticks on clock hours divisible by its step, never on odd ones", () => {
    const ticks = timeTicks(NOON - 15 * HOUR, NOON, 900);
    const hours = ticks.map((tick) => new Date(tick.at).getHours());
    expect(ticks.length).toBeGreaterThan(3);
    const step = hours[1] - hours[0] > 0 ? hours[1] - hours[0] : hours[1] + 24 - hours[0];
    for (const hour of hours) expect(hour % step).toBe(0);
  });

  it("labels a midnight with the day it opens", () => {
    const ticks = timeTicks(NOON - 15 * HOUR, NOON, 900);
    const midnight = ticks.find((tick) => tick.midnight);
    expect(midnight).toBeDefined();
    expect(midnight?.label).not.toMatch(/\d\d:\d\d/);
    expect(new Date(midnight!.at).getHours()).toBe(0);
  });

  it("counts in days over a week, on local midnights", () => {
    const ticks = timeTicks(NOON - 7 * DAY, NOON, 700);
    expect(ticks.length).toBeGreaterThanOrEqual(3);
    for (const tick of ticks) {
      expect(tick.midnight).toBe(true);
      expect(new Date(tick.at).getHours()).toBe(0);
    }
  });

  it("keeps labels apart however narrow the plot", () => {
    for (const width of [420, 700, 1100]) {
      const ticks = timeTicks(NOON - DAY, NOON, width);
      for (let i = 1; i < ticks.length; i += 1) {
        expect(((ticks[i].at - ticks[i - 1].at) / DAY) * width).toBeGreaterThanOrEqual(72);
      }
    }
  });
});

describe("silences", () => {
  it("names a gap of more than two hours between two lines, and nothing at the window's edges", () => {
    const gaps = quietGaps([NOON - 6 * HOUR, NOON - 5 * HOUR, NOON - HOUR, NOON - 50 * MINUTE]);
    expect(gaps).toEqual([{ from: NOON - 5 * HOUR, to: NOON - HOUR }]);
  });

  it("says a span as it is said aloud", () => {
    expect(span(35 * MINUTE)).toBe("35 min");
    expect(span(4 * HOUR + 5 * MINUTE)).toBe("4 h 05");
    expect(span(2 * DAY + 3 * HOUR)).toBe("2 d 3 h");
    expect(clock(new Date(2026, 7, 24, 9, 5).getTime())).toBe("09:05");
  });
});

describe("the list under the trace", () => {
  const isRoutine = (entry: FeedEntry) => entry.kind === "job_started";

  it("folds consecutive routine lines, and leaves a lone one as a line", () => {
    const days = buildFeedDays(
      [
        line(1, NOON - 50 * MINUTE),
        line(2, NOON - 40 * MINUTE),
        line(3, NOON - 30 * MINUTE, "job_gate_failed"),
        line(4, NOON - 20 * MINUTE),
      ],
      { isRoutine, seenThrough: null, lookedAt: null, gapAfter: Infinity },
    );
    expect(days).toHaveLength(1);
    expect(days[0].items.map((item) => item.type)).toEqual(["line", "line", "routine"]);
    const fold = days[0].items[2];
    expect(fold.type === "routine" && fold.entries.map((entry) => entry.id)).toEqual([2, 1]);
  });

  it("puts the seen rule between the newest line you saw and the oldest you did not", () => {
    const days = buildFeedDays(
      [line(1, NOON - 50 * MINUTE), line(2, NOON - 40 * MINUTE), line(3, NOON - 30 * MINUTE)],
      { isRoutine: () => false, seenThrough: 2, lookedAt: NOON - 35 * MINUTE, gapAfter: Infinity },
    );
    const order = days[0].items.map((item) => (item.type === "line" ? item.entry.id : item.type));
    expect(order).toEqual([3, "seen", 2, 1]);
  });

  it("breaks a fold at a silence and names it, but not when the list is narrowed", () => {
    const lines = [line(1, NOON - 5 * HOUR), line(2, NOON - 290 * MINUTE), line(3, NOON - HOUR), line(4, NOON - 50 * MINUTE)];
    const whole = buildFeedDays(lines, { isRoutine, seenThrough: null, lookedAt: null, gapAfter: 2 * HOUR });
    expect(whole[0].items.map((item) => item.type)).toEqual(["routine", "gap", "routine"]);
    const narrowed = buildFeedDays(lines, { isRoutine, seenThrough: null, lookedAt: null, gapAfter: Infinity });
    expect(narrowed[0].items.map((item) => item.type)).toEqual(["routine"]);
  });

  it("starts a new group at a local midnight", () => {
    const midnight = new Date(2026, 7, 24, 0, 0, 0, 0).getTime();
    const days = buildFeedDays([line(1, midnight - 10 * MINUTE), line(2, midnight + 10 * MINUTE)], {
      isRoutine: () => false,
      seenThrough: null,
      lookedAt: null,
      gapAfter: Infinity,
    });
    expect(days.map((day) => day.items.length)).toEqual([1, 1]);
  });
});
