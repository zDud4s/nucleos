import { describe, expect, it } from "vitest";
import {
  clockOfInstant, clockOfStamp, hourMarks, hourSlots, hoursInSpan, inputFromStamp, localDateOfStamp,
  localStamp, monthMatrix, nowFraction, occurrenceMinutes, overlapLanes, placeInDay, sameDay,
  stampFromInput, weekOf, weekdayLabels,
} from "./calendar-grid";

const HOUR = 3_600_000;

/** A span of `hours` starting at an arbitrary fixed instant — no time zone required. */
function span(hours: number): [Date, Date] {
  const start = new Date("2026-08-03T00:00:00Z");
  return [start, new Date(start.getTime() + hours * HOUR)];
}

describe("day length", () => {
  it("counts an ordinary day as 24 hours", () => {
    const [start, end] = span(24);
    expect(hoursInSpan(start, end)).toBe(24);
  });

  /// The two days a year a grid built on a hard-coded 24 draws everything at the wrong height.
  it("counts the short and long days as 23 and 25", () => {
    expect(hoursInSpan(...span(23))).toBe(23);
    expect(hoursInSpan(...span(25))).toBe(25);
  });

  it("spreads the hour lines over the day's own span, not over 24", () => {
    const short = hourMarks(...span(23));
    expect(short).toHaveLength(22);
    expect(short[0].fraction).toBeCloseTo(1 / 23);
    expect(short[short.length - 1].fraction).toBeCloseTo(22 / 23);
  });
});

describe("placing a block in a day", () => {
  const [dayStart, dayEnd] = span(24);

  it("puts a mid-morning hour where it belongs", () => {
    const place = placeInDay(
      new Date(dayStart.getTime() + 9 * HOUR),
      new Date(dayStart.getTime() + 10 * HOUR),
      dayStart,
      dayEnd,
    );
    expect(place?.top).toBeCloseTo(9 / 24);
    expect(place?.height).toBeCloseTo(1 / 24);
  });

  it("reports nothing for a block on another day", () => {
    const start = new Date(dayEnd.getTime() + HOUR);
    expect(placeInDay(start, new Date(start.getTime() + HOUR), dayStart, dayEnd)).toBeNull();
  });

  /// A meeting running past midnight belongs to both columns, drawn to the edge of each.
  it("clamps a block that starts before the day and ends inside it", () => {
    const place = placeInDay(
      new Date(dayStart.getTime() - 2 * HOUR),
      new Date(dayStart.getTime() + HOUR),
      dayStart,
      dayEnd,
    );
    expect(place?.top).toBe(0);
    expect(place?.height).toBeCloseTo(1 / 24);
  });

  it("clamps a block that runs past the end of the day", () => {
    const place = placeInDay(
      new Date(dayEnd.getTime() - HOUR),
      new Date(dayEnd.getTime() + 5 * HOUR),
      dayStart,
      dayEnd,
    );
    expect(place?.top).toBeCloseTo(23 / 24);
    expect((place?.top ?? 0) + (place?.height ?? 0)).toBeCloseTo(1);
  });

  /// An event you cannot see is worse than one drawn a little too tall.
  it("gives a zero-length event a visible floor", () => {
    const at = new Date(dayStart.getTime() + 9 * HOUR);
    expect(placeInDay(at, at, dayStart, dayEnd)).toBeNull();
    const sliver = placeInDay(at, new Date(at.getTime() + 60_000), dayStart, dayEnd);
    expect(sliver?.height).toBeGreaterThanOrEqual(0.01);
  });
});

describe("side-by-side columns", () => {
  const at = (hour: number) => new Date(2026, 7, 3, hour);
  const block = (from: number, to: number) => ({ from: at(from), to: at(to) });
  const lanesOf = (items: { from: Date; to: Date }[]) =>
    overlapLanes(items, (item) => item.from, (item) => item.to);

  it("leaves a lone event full width", () => {
    const one = block(9, 10);
    expect(lanesOf([one]).get(one)).toEqual({ lane: 0, lanes: 1 });
  });

  /// The reason a day with a 09:00 and a 15:00 must not be drawn half-width.
  it("reuses the column when two events do not overlap", () => {
    const morning = block(9, 10);
    const afternoon = block(15, 16);
    const lanes = lanesOf([morning, afternoon]);
    expect(lanes.get(morning)).toEqual({ lane: 0, lanes: 1 });
    expect(lanes.get(afternoon)).toEqual({ lane: 0, lanes: 1 });
  });

  it("splits two overlapping events into two columns", () => {
    const first = block(9, 11);
    const second = block(10, 12);
    const lanes = lanesOf([first, second]);
    expect(lanes.get(first)?.lane).toBe(0);
    expect(lanes.get(second)?.lane).toBe(1);
    expect(lanes.get(first)?.lanes).toBe(2);
  });

  /// Every member of a cluster has to agree on the width, including the one that got lane 0 before
  /// the third event proved the cluster needed three columns.
  it("widens the whole cluster when a third event joins it", () => {
    const first = block(9, 12);
    const second = block(10, 12);
    const third = block(11, 12);
    const lanes = lanesOf([first, second, third]);
    expect(lanes.get(first)?.lanes).toBe(3);
    expect(lanes.get(second)?.lanes).toBe(3);
    expect(lanes.get(third)?.lanes).toBe(3);
  });

  it("keeps two clusters independent", () => {
    const morningA = block(9, 11);
    const morningB = block(10, 11);
    const evening = block(16, 17);
    const lanes = lanesOf([morningA, morningB, evening]);
    expect(lanes.get(morningA)?.lanes).toBe(2);
    expect(lanes.get(evening)?.lanes).toBe(1);
  });
});

describe("the month grid", () => {
  it("always has six rows, so paging does not change the height", () => {
    for (const month of [0, 1, 4, 11]) {
      expect(monthMatrix(new Date(2026, month, 1))).toHaveLength(6);
    }
  });

  it("starts on a Monday and includes the tail of the previous month", () => {
    // 1 August 2026 is a Saturday, so the first row reaches back to 27 July.
    const weeks = monthMatrix(new Date(2026, 7, 1));
    expect(weeks[0][0].getDay()).toBe(1);
    expect(weeks[0][0].getDate()).toBe(27);
    expect(weeks[0][0].getMonth()).toBe(6);
  });

  it("covers the whole month it was asked for", () => {
    const days = monthMatrix(new Date(2026, 7, 1)).flat();
    const august = days.filter((day) => day.getMonth() === 7);
    expect(august).toHaveLength(31);
  });
});

describe("the week", () => {
  it("runs Monday to Sunday whichever day it is anchored on", () => {
    // 5 August 2026 is a Wednesday.
    const week = weekOf(new Date(2026, 7, 5));
    expect(week).toHaveLength(7);
    expect(week[0].getDay()).toBe(1);
    expect(week[0].getDate()).toBe(3);
    expect(week[6].getDay()).toBe(0);
    expect(week[6].getDate()).toBe(9);
  });
});

describe("the now line", () => {
  it("is absent on any day but today", () => {
    expect(nowFraction(new Date(2026, 7, 3, 12), new Date(2026, 7, 4))).toBeNull();
  });

  it("sits halfway down at midday", () => {
    expect(nowFraction(new Date(2026, 7, 3, 12), new Date(2026, 7, 3))).toBeCloseTo(0.5, 2);
  });
});

describe("the stamp sent to the daemon", () => {
  /// `toISOString` would convert to UTC and land the event an hour away twice a year. The daemon
  /// stores a wall clock, so the shell has to send one.
  it("is a local wall clock with no offset", () => {
    expect(localStamp(new Date(2026, 7, 3), 9)).toBe("2026-08-03T09:00:00");
    expect(localStamp(new Date(2026, 0, 5), 7, 30)).toBe("2026-01-05T07:30:00");
  });
});

describe("sameDay", () => {
  it("separates the same date in different months", () => {
    expect(sameDay(new Date(2026, 7, 3), new Date(2026, 8, 3))).toBe(false);
    expect(sameDay(new Date(2026, 7, 3, 1), new Date(2026, 7, 3, 23))).toBe(true);
  });
});

/**
 * The move control writes a stamp the daemon parses with `%Y-%m-%dT%H:%M:%S`, which does not treat
 * the seconds as optional — and `<input type="datetime-local">` omits them.
 */
describe("stampFromInput", () => {
  it("adds the seconds the control leaves out", () => {
    expect(stampFromInput("2026-08-03T09:30")).toBe("2026-08-03T09:30:00");
  });

  it("leaves a stamp that already has them alone", () => {
    expect(stampFromInput("2026-08-03T09:30:45")).toBe("2026-08-03T09:30:45");
  });

  /** Rejected here rather than at the daemon: the shape of the string already says it is wrong. */
  it("refuses anything that is not a local stamp", () => {
    expect(stampFromInput("")).toBeNull();
    expect(stampFromInput("2026-08-03")).toBeNull();
    expect(stampFromInput("2026-08-03T09:30:00Z")).toBeNull();
    expect(stampFromInput("tomorrow")).toBeNull();
  });
});

describe("inputFromStamp", () => {
  /** Truncated, never re-parsed: a `Date` would apply this machine's offset to local wall-clock. */
  it("hands the control back the minutes without a timezone ever being applied", () => {
    expect(inputFromStamp("2026-08-03T09:30:00")).toBe("2026-08-03T09:30");
  });

  it("round-trips with stampFromInput", () => {
    expect(stampFromInput(inputFromStamp("2026-12-31T23:59:00"))).toBe("2026-12-31T23:59:00");
  });
});

/** The daemon rejects a move with a non-positive duration, so this number has to be right. */
describe("occurrenceMinutes", () => {
  it("measures the occurrence in whole minutes", () => {
    expect(occurrenceMinutes("2026-08-03T09:00:00Z", "2026-08-03T09:30:00Z")).toBe(30);
    expect(occurrenceMinutes("2026-08-03T09:00:00Z", "2026-08-03T10:00:00Z")).toBe(60);
  });

  /** Rounded, not floored: a 30-minute block whose ends drift by a millisecond is still 30. */
  it("rounds rather than truncating", () => {
    expect(occurrenceMinutes("2026-08-03T09:00:00.000Z", "2026-08-03T09:29:59.600Z")).toBe(30);
  });

  it("answers zero for a pair it cannot read, rather than NaN", () => {
    expect(occurrenceMinutes("not a date", "2026-08-03T09:30:00Z")).toBe(0);
  });
});

/* --------------------------------------------------- what the week view needed -- */

describe("hourSlots", () => {
  it("gives an ordinary day 24 bands that tile it exactly", () => {
    const slots = hourSlots(...span(24));
    expect(slots).toHaveLength(24);
    expect(slots[0].top).toBe(0);
    expect(slots[0].height).toBeCloseTo(1 / 24);
    // No gap and no overlap: the last band ends exactly at the bottom.
    expect(slots[23].top + slots[23].height).toBeCloseTo(1);
  });

  /**
   * A spring-forward day has 23 bands, not 24 with one hidden — which is the
   * whole reason the week grid asks each column for its own rather than
   * sharing one set across the row.
   */
  it("gives a short day 23 bands and a long day 25", () => {
    expect(hourSlots(...span(23))).toHaveLength(23);
    expect(hourSlots(...span(25))).toHaveLength(25);
  });
});

describe("weekdayLabels", () => {
  /** Monday first, because `monthMatrix` and `weekOf` are — the header has to agree with the grid. */
  it("starts on Monday and ends on Sunday", () => {
    const labels = weekdayLabels("en-GB");
    expect(labels).toHaveLength(7);
    expect(labels[0].long).toBe("Monday");
    expect(labels[6].long).toBe("Sunday");
  });

  it("answers in the locale it is given, rather than in seven English literals", () => {
    expect(weekdayLabels("pt-PT")[0].long.toLowerCase()).toContain("segunda");
  });
});

describe("clockOfStamp and clockOfInstant", () => {
  it("slices the stamp without ever building a Date from it", () => {
    expect(clockOfStamp("2026-08-20T09:05:00")).toBe("09:05");
  });

  it("reads an instant back in this machine's own clock", () => {
    expect(clockOfInstant(new Date(2026, 7, 20, 9, 5))).toBe("09:05");
  });
});

describe("localDateOfStamp", () => {
  /** Its only job is to be COMPARED with `starts_at` — see `slot.ts`'s `placementOf`. */
  it("reads a local stamp as the instant this machine would call it", () => {
    const at = localDateOfStamp("2026-08-20T09:00:00");
    expect(at?.getFullYear()).toBe(2026);
    expect(at?.getMonth()).toBe(7);
    expect(at?.getDate()).toBe(20);
    expect(at?.getHours()).toBe(9);
  });

  it("accepts the seconds-less spelling a datetime-local control produces", () => {
    expect(localDateOfStamp("2026-08-20T09:00")?.getHours()).toBe(9);
  });

  /** An instant with an offset in it is not a local stamp, and must not be read as one. */
  it("answers null for anything that is not a local stamp", () => {
    expect(localDateOfStamp("2026-08-20T08:00:00Z")).toBeNull();
    expect(localDateOfStamp("")).toBeNull();
  });
});
