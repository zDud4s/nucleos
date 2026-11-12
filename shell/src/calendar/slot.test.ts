// @vitest-environment node
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { compareOccurrences, dateKeyOf, groupByLocalDay, placementOf, slotStamp } from "./slot";
import type { EventOccurrence } from "../data/calendar";

/**
 * Where an occurrence is DRAWN, which the page got wrong.
 *
 * Every test here is pinned to Lisbon, because the whole subject is the
 * difference between a local wall clock, an instant, and this machine's idea
 * of what that instant is called. A suite that ran under whatever zone the
 * machine happened to have would pass in London and fail in Lisbon for
 * reasons that look like the code's.
 */
beforeAll(() => {
  vi.stubEnv("TZ", "Europe/Lisbon");
});

afterAll(() => {
  vi.unstubAllEnvs();
});

function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 1,
    title: "Standup",
    source: "human",
    occurrence_local: "2026-08-20T09:00:00",
    // Lisbon is UTC+1 in August, so 09:00 local is 08:00Z. Written out rather
    // than computed: a fixture that derived this would be using the same
    // arithmetic as the code it is checking.
    starts_at: "2026-08-20T08:00:00Z",
    ends_at: "2026-08-20T08:30:00Z",
    ...overrides,
  };
}

describe("placementOf", () => {
  it("reads an unmoved occurrence off its own local stamp", () => {
    const placement = placementOf(occurrence());

    expect(placement.moved).toBe(false);
    // The exact text the daemon wrote, not a re-derivation through a Date.
    expect(placement.clock).toBe("09:00");
    expect(dateKeyOf(placement.day)).toBe("2026-08-20");
  });

  it("draws a MOVED occurrence on the day it was moved TO, not the day it came from", () => {
    /*
      The regression this whole module exists for. `recurrence.rs`'s `expand`
      decides window membership with the RESOLVED instant and then reports the
      ORIGINAL local start beside it, so an occurrence moved from the 4th to
      the 20th comes back in the 20th's window still carrying "...-04T09:00".
      Grouping the grid by that string drew it on the 4th — a day the daemon
      did not return it for.
    */
    const moved = occurrence({
      occurrence_local: "2026-08-04T09:00:00",
      starts_at: "2026-08-20T14:00:00Z",
      ends_at: "2026-08-20T15:00:00Z",
    });

    const placement = placementOf(moved);

    expect(placement.moved).toBe(true);
    expect(dateKeyOf(placement.day)).toBe("2026-08-20");
    // 14:00Z in August Lisbon is 15:00 on the clock in the room.
    expect(placement.clock).toBe("15:00");
  });
});

describe("groupByLocalDay", () => {
  it("keys on the LOCAL day, not on the UTC date the instant is spelled with", () => {
    /*
      The discriminating case, and the reason a fixture whose two dates agree
      proves nothing: half past midnight in Lisbon in August is half past
      eleven the previous evening in UTC. Anything that grouped by slicing
      `starts_at` puts this on the 20th.
    */
    const justAfterMidnight = occurrence({
      occurrence_local: "2026-08-21T00:30:00",
      starts_at: "2026-08-20T23:30:00Z",
      ends_at: "2026-08-21T00:00:00Z",
    });

    const grouped = groupByLocalDay([justAfterMidnight]);

    expect([...grouped.keys()]).toEqual(["2026-08-21"]);
  });

  it("puts a moved occurrence in its new day and leaves nothing behind in the old one", () => {
    const stayed = occurrence({ event_id: 1, title: "Standup" });
    const moved = occurrence({
      event_id: 2,
      title: "Review",
      occurrence_local: "2026-08-04T09:00:00",
      starts_at: "2026-08-20T14:00:00Z",
      ends_at: "2026-08-20T15:00:00Z",
    });

    const grouped = groupByLocalDay([stayed, moved]);

    expect(grouped.get("2026-08-04")).toBeUndefined();
    expect(grouped.get("2026-08-20")?.map((row) => row.title)).toEqual(["Standup", "Review"]);
  });

  it("sorts each day by the resolved instant, so a move re-sorts within the day", () => {
    /*
      Sorted on `starts_at` and not on `occurrence_local`: the afternoon one
      below was written for 08:00 and moved to 16:00, and ordering by the stamp
      would file it first — among the events of an hour it no longer occupies.
    */
    const nine = occurrence({ event_id: 1, title: "Standup" });
    const movedToAfternoon = occurrence({
      event_id: 2,
      title: "Retro",
      occurrence_local: "2026-08-20T07:00:00",
      starts_at: "2026-08-20T15:00:00Z",
      ends_at: "2026-08-20T15:30:00Z",
    });

    const grouped = groupByLocalDay([movedToAfternoon, nine]);

    expect(grouped.get("2026-08-20")?.map((row) => row.title)).toEqual(["Standup", "Retro"]);
  });
});

describe("compareOccurrences", () => {
  it("never returns 0 for two different occurrences at the same instant", () => {
    /*
      Two meetings at 09:00 are ordinary — that is what the overlap lanes are
      for. A comparator that tied them would leave their order to the daemon's,
      which is to say it would change between polls and shuffle the chips under
      the cursor every thirty seconds.
    */
    const a = occurrence({ event_id: 7, title: "Review" });
    const b = occurrence({ event_id: 3, title: "Review" });

    expect(compareOccurrences(a, b)).not.toBe(0);
    // And the order is stable in both directions.
    expect(Math.sign(compareOccurrences(a, b))).toBe(-Math.sign(compareOccurrences(b, a)));
  });
});

describe("slotStamp", () => {
  it("uses the slot's own hour when it has one", () => {
    expect(slotStamp({ day: new Date(2026, 7, 20), hour: 14 }, 9)).toBe("2026-08-20T14:00:00");
  });

  it("falls back to the working-day hour when no hour was named", () => {
    // `null` is "no hour was named", not midnight — a day picked from the month
    // opens at the start of the working day rather than at 00:00.
    expect(slotStamp({ day: new Date(2026, 7, 20), hour: null }, 9)).toBe("2026-08-20T09:00:00");
  });
});
