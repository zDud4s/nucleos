import { describe, expect, it } from "vitest";
import { captureBucket, knownBucket, noteBucket, passesState } from "./item-state";
import type { StateBucket } from "./graph-types";

describe("knownBucket", () => {
  it.each([
    ["active", "in_force"],
    ["live", "in_force"],
    ["proposed", "proposed"],
    ["rejected", "out"],
    ["reverted", "out"],
    ["superseded", "out"],
    ["archived", "out"],
    ["closed", "out"],
    ["expired", "out"],
    ["something_new", "unknown"],
    ["", "unknown"],
  ])("%s -> %s", (status, bucket) => {
    expect(knownBucket(status)).toBe(bucket);
  });
});

describe("noteBucket", () => {
  it.each([
    ["active", "in_force"],
    ["archived", "out"],
    ["weird", "unknown"],
  ])("%s -> %s", (state, bucket) => {
    expect(noteBucket(state)).toBe(bucket);
  });
});

describe("captureBucket", () => {
  it.each([
    ["open", "proposed"],
    ["answered", "out"],
    ["dismissed", "out"],
    ["expired", "out"],
    ["weird", "unknown"],
  ])("%s -> %s", (state, bucket) => {
    expect(captureBucket(state)).toBe(bucket);
  });
});

describe("passesState", () => {
  const buckets: StateBucket[] = ["in_force", "proposed", "out", "unknown"];
  const table: Array<["in_force" | "out" | "all", "list" | "graph", boolean[]]> = [
    // order of results follows `buckets`
    ["all", "list", [true, true, true, true]],
    ["all", "graph", [true, true, true, true]],
    ["in_force", "list", [true, false, false, false]],
    ["in_force", "graph", [true, true, false, false]],
    ["out", "list", [false, false, true, false]],
    ["out", "graph", [false, false, true, false]],
  ];
  it.each(table)("filter %s in the %s", (filter, where, expected) => {
    expect(buckets.map((b) => passesState(b, filter, where))).toEqual(expected);
  });
});
