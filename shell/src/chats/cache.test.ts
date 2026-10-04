import { describe, expect, it } from "vitest";
import { cacheState, leftText } from "./cache";
import type { Turn } from "../lib/turns";

function turn(over: Partial<Turn>): Turn {
  return {
    id: 1,
    asked: "q",
    answer: "a",
    status: "done",
    createdAt: "2026-10-03T10:00:00Z",
    completedAt: null,
    model: null,
    cacheTtl: null,
    did: [],
    ...over,
  } as Turn;
}

const at = (iso: string) => Date.parse(iso);

describe("cacheState", () => {
  it("is null without turns", () => {
    expect(cacheState([], 0)).toBeNull();
    expect(cacheState(undefined, 0)).toBeNull();
  });

  it("counts down from completed_at with the stated ttl", () => {
    const t = turn({ completedAt: "2026-10-03T10:01:00Z", cacheTtl: "1h" });
    expect(cacheState([t], at("2026-10-03T10:19:00Z"))).toEqual({ kind: "warm", ms: 42 * 60_000, approx: false });
  });

  it("assumes 5m and marks it approximate when the ttl is unknown, falling back to created_at", () => {
    const t = turn({});
    expect(cacheState([t], at("2026-10-03T10:02:00Z"))).toEqual({ kind: "warm", ms: 3 * 60_000, approx: true });
  });

  it("is cold at zero", () => {
    expect(cacheState([turn({})], at("2026-10-03T10:05:00Z"))).toEqual({ kind: "cold", approx: true });
  });

  it("shows the full ttl as refreshing while a turn is live", () => {
    const t = turn({ status: "running", cacheTtl: "5m" });
    expect(cacheState([t], at("2026-10-03T12:00:00Z"))).toEqual({ kind: "refreshing", ms: 5 * 60_000, approx: false });
  });
});

describe("leftText", () => {
  it("formats minutes and hours", () => {
    expect(leftText(42 * 60_000)).toBe("42m");
    expect(leftText(10_000)).toBe("1m");
    expect(leftText(65 * 60_000)).toBe("1h 05m");
    expect(leftText(60 * 60_000)).toBe("1h");
  });
});
