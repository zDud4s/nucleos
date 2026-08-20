import { describe, expect, it } from "vitest";
import { STILL_IN_IT_MS, stillGoing } from "./editor";

const NOW = Date.parse("2026-08-20T12:00:00Z");

describe("stillGoing", () => {
  it("says a session written to a moment ago is one somebody is in", () => {
    expect(stillGoing("2026-08-20T11:59:50Z", NOW)).toBe(true);
  });

  it("says a session from yesterday is not", () => {
    expect(stillGoing("2026-08-19T12:00:00Z", NOW)).toBe(false);
  });

  // The gap between two messages is a person reading and thinking, and the file says nothing while
  // they do. A window that unmarked a session every time somebody paused would flicker at exactly
  // the moment the answer was arriving.
  it("holds through a pause long enough to read an answer", () => {
    expect(stillGoing("2026-08-20T11:59:00Z", NOW)).toBe(true);
  });

  it("lets go once the pause is longer than a pause", () => {
    expect(stillGoing(new Date(NOW - STILL_IN_IT_MS - 1000).toISOString(), NOW)).toBe(false);
  });

  // A clock that disagrees with the daemon's, or a file touched by something else, can put the last
  // line in the future. That is not a reason to call a session dead — it is still the most recent
  // thing that happened.
  it("counts a timestamp slightly ahead of this clock as recent rather than as wrong", () => {
    expect(stillGoing("2026-08-20T12:00:05Z", NOW)).toBe(true);
  });

  // What is known is when a file was last written. A timestamp that cannot be read is not evidence
  // of anything, and claiming somebody is working in a session on the strength of it would be
  // inventing the one fact this is about.
  it("claims nothing when the timestamp cannot be read", () => {
    expect(stillGoing("whenever", NOW)).toBe(false);
    expect(stillGoing("", NOW)).toBe(false);
  });
});
