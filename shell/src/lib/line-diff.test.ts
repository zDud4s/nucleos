import { describe, expect, it } from "vitest";

import type { DiffLine } from "./diff";
import { lineDiff } from "./line-diff";

// Shorthands so each expectation reads as the diff a person would draw, line by line.
const ctx = (text: string): DiffLine => ({ kind: "context", text });
const add = (text: string): DiffLine => ({ kind: "added", text });
const del = (text: string): DiffLine => ({ kind: "removed", text });

describe("lineDiff", () => {
  it("identical texts are all context", () => {
    expect(lineDiff("one\ntwo\nthree", "one\ntwo\nthree")).toEqual([ctx("one"), ctx("two"), ctx("three")]);
  });

  it("an insertion and a deletion are marked", () => {
    // A line only `after` has is added, and the lines around it stay context.
    expect(lineDiff("a\nc", "a\nb\nc")).toEqual([ctx("a"), add("b"), ctx("c")]);
    // A line only `before` has is removed.
    expect(lineDiff("a\nb\nc", "a\nc")).toEqual([ctx("a"), del("b"), ctx("c")]);
  });

  it("a replaced line is removed then added", () => {
    expect(lineDiff("a\nb\nc", "a\nx\nc")).toEqual([ctx("a"), del("b"), add("x"), ctx("c")]);
    // A replaced BLOCK reads the same way: every old line first, then every new one, never
    // interleaved -- interleaving is what makes a revision unreadable at a glance.
    expect(lineDiff("a\nb\nc\nd", "a\nx\ny\nd")).toEqual([ctx("a"), del("b"), del("c"), add("x"), add("y"), ctx("d")]);
  });

  it("empty sides", () => {
    // An empty string is no lines at all, not one empty line: a first revision is all added,
    // and a withdrawn answer is all removed, with no phantom blank row on either side.
    expect(lineDiff("", "a\nb")).toEqual([add("a"), add("b")]);
    expect(lineDiff("a\nb", "")).toEqual([del("a"), del("b")]);
    expect(lineDiff("", "")).toEqual([]);
  });
});
