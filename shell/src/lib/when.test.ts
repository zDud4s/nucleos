import { describe, expect, it } from "vitest";
import { bandOf, inBands } from "./when";

/** A local time, built the way a person means one — never a UTC string with a Z on it. */
function local(y: number, m: number, d: number, h: number, min = 0): number {
  return new Date(y, m - 1, d, h, min, 0, 0).getTime();
}

/** The same instant as the daemon would send it. */
function sent(ms: number): string {
  return new Date(ms).toISOString();
}

describe("bandOf", () => {
  const now = local(2026, 8, 24, 14, 30);

  it("puts this calendar day in today, however long ago in the day it was", () => {
    expect(bandOf(sent(local(2026, 8, 24, 14, 29)), now)).toBe("today");
    expect(bandOf(sent(local(2026, 8, 24, 0, 1)), now)).toBe("today");
  });

  it("counts calendar days, not elapsed hours", () => {
    // Twenty minutes apart in real time, and on opposite sides of a midnight. An
    // elapsed-hours rule files this under today, which is not what anybody calls it.
    const lateLastNight = local(2026, 8, 23, 23, 50);
    expect(bandOf(sent(lateLastNight), local(2026, 8, 24, 0, 10))).toBe("yesterday");
  });

  it("stops naming days after yesterday", () => {
    expect(bandOf(sent(local(2026, 8, 23, 9, 0)), now)).toBe("yesterday");
    expect(bandOf(sent(local(2026, 8, 22, 9, 0)), now)).toBe("earlier");
    expect(bandOf(sent(local(2020, 1, 1, 9, 0)), now)).toBe("earlier");
  });

  it("reads a conversation nobody has spoken in as its own thing", () => {
    // Not "earlier": it was never said at all, and filing it under a day would date it.
    expect(bandOf(null, now)).toBe("quiet");
  });

  it("does not invent a band for a clock that runs ahead", () => {
    // The daemon's clock and this window's disagree on a machine that just woke up.
    expect(bandOf(sent(local(2026, 8, 25, 9, 0)), now)).toBe("today");
  });

  it("keeps a row it cannot read rather than losing it", () => {
    expect(bandOf("not a timestamp", now)).toBe("earlier");
  });
});

describe("inBands", () => {
  const now = local(2026, 8, 24, 14, 30);
  const at = (row: { at: string | null }) => row.at;

  it("cuts a sorted list without reordering it", () => {
    const rows = [
      { name: "a", at: sent(local(2026, 8, 24, 12, 0)) },
      { name: "b", at: sent(local(2026, 8, 24, 9, 0)) },
      { name: "c", at: sent(local(2026, 8, 23, 18, 0)) },
      { name: "d", at: sent(local(2026, 8, 20, 18, 0)) },
      { name: "e", at: null },
    ];
    expect(inBands(rows, at, now).map((cut) => [cut.band, cut.rows.map((r) => r.name)])).toEqual([
      ["today", ["a", "b"]],
      ["yesterday", ["c"]],
      ["earlier", ["d"]],
      ["quiet", ["e"]],
    ]);
  });

  it("emits no heading for a day with nothing in it", () => {
    const rows = [
      { name: "a", at: sent(local(2026, 8, 24, 12, 0)) },
      { name: "b", at: sent(local(2026, 8, 20, 12, 0)) },
    ];
    expect(inBands(rows, at, now).map((cut) => cut.band)).toEqual(["today", "earlier"]);
  });

  it("says nothing at all about an empty list", () => {
    expect(inBands([], at, now)).toEqual([]);
  });
});
