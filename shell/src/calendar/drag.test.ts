// @vitest-environment node
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { moveFromDrop } from "./drag";
import type { EventOccurrence } from "../data/calendar";

/**
 * What a drop means — the arithmetic, with no pointer and no DOM anywhere near
 * it. Pinned to Lisbon, because half of these are about a day that is not 24
 * hours long.
 */
beforeAll(() => {
  vi.stubEnv("TZ", "Europe/Lisbon");
});

afterAll(() => {
  vi.unstubAllEnvs();
});

/** 10:15–11:15 local on Thursday 20 August 2026. Lisbon is UTC+1 in August. */
function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 4,
    title: "Design review",
    source: "human",
    occurrence_local: "2026-08-20T10:15:00",
    starts_at: "2026-08-20T09:15:00Z",
    ends_at: "2026-08-20T10:15:00Z",
    ...overrides,
  };
}

describe("dropping on a week slot", () => {
  /**
   * A translation, not a snap. Dropping 10:15 on the 14:00 band means 14:15 —
   * rounding to the hour would turn a four-hour drag into a three-and-
   * three-quarter-hour move, silently, every time.
   */
  it("keeps the minutes and takes the hour from the band", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 20), 14)).toEqual({
      toLocal: "2026-08-20T14:15:00",
      durationMinutes: 60,
    });
  });

  it("carries an occurrence to another day at the named hour", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 24), 9)).toEqual({
      toLocal: "2026-08-24T09:15:00",
      durationMinutes: 60,
    });
  });

  /** A move moves; it does not resize. The daemon refuses a non-positive duration. */
  it("never changes how long the occurrence is", () => {
    const long = occurrence({ ends_at: "2026-08-20T11:45:00Z" });
    expect(moveFromDrop(long, new Date(2026, 7, 21), 8)?.durationMinutes).toBe(150);
  });
});

describe("dropping on a month cell", () => {
  /** A month cell can only name a day, so the occurrence keeps the time it had. */
  it("changes the date and nothing else", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 28), null)).toEqual({
      toLocal: "2026-08-28T10:15:00",
      durationMinutes: 60,
    });
  });

  it("carries the time across a month boundary", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 8, 3), null)?.toLocal).toBe(
      "2026-09-03T10:15:00",
    );
  });
});

describe("a drop that changes nothing", () => {
  /**
   * Picking a block up and putting it back is the commonest gesture in any
   * calendar. Left to go through, it writes an exception row saying "this
   * occurrence is exactly where the series already puts it" — a real row, in
   * the database, for every twitch of the hand.
   */
  it("asks for no move when the day and hour are the ones it already has", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 20), 10)).toBeNull();
  });

  it("asks for no move when a month cell names the day it is already on", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 20), null)).toBeNull();
  });

  /** The same hour on a different day IS a move, and must not be mistaken for one. */
  it("still moves when only the day differs", () => {
    expect(moveFromDrop(occurrence(), new Date(2026, 7, 21), 10)).not.toBeNull();
  });
});

describe("a moved occurrence dragged again", () => {
  /**
   * The destination is computed from where the occurrence IS — `placementOf`'s
   * resolved instant — and never from `occurrence_local`, which stays the
   * original. Reading the stamp here would make a second drag offer to move it
   * relative to a time it stopped occupying.
   */
  it("measures from where it now sits, not from where the series put it", () => {
    const moved = occurrence({
      occurrence_local: "2026-08-20T10:15:00",
      starts_at: "2026-08-20T14:15:00Z",
      ends_at: "2026-08-20T15:15:00Z",
    });

    // 14:15Z is 15:15 in the room; a month drop to the 21st keeps that.
    expect(moveFromDrop(moved, new Date(2026, 7, 21), null)?.toLocal).toBe("2026-08-21T15:15:00");
    // And putting it back where it now is asks for nothing.
    expect(moveFromDrop(moved, new Date(2026, 7, 20), 15)).toBeNull();
  });
});

describe("dropping onto a day that is not 24 hours", () => {
  /**
   * The hour comes from the band, and `hourSlots` reads each band's hour back
   * off a real instant — so a column on the day the clocks go forward has no
   * 02 band at all, and one on the day they go back has two, both meaning
   * 02:00. This asserts the arithmetic downstream of that: a local wall clock
   * is what gets sent, and resolving it is `recurrence.rs`'s job, not the
   * grid's.
   */
  it("sends the wall clock the band names, and leaves resolving it to the daemon", () => {
    // 29 March 2026 is Lisbon's 23-hour day.
    expect(moveFromDrop(occurrence(), new Date(2026, 2, 29), 3)).toEqual({
      toLocal: "2026-03-29T03:15:00",
      durationMinutes: 60,
    });
  });

  it("sends 02:15 for either of the two bands an autumn day calls 02", () => {
    // 25 October 2026 is the 25-hour day; both bands mean the same wall clock.
    expect(moveFromDrop(occurrence(), new Date(2026, 9, 25), 2)?.toLocal).toBe(
      "2026-10-25T02:15:00",
    );
  });
});
