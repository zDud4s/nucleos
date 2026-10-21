// @vitest-environment node
import { describe, expect, it } from "vitest";
import { freshLines, sameLine } from "./dedupe";
import type { TermLine, TranscriptEntry } from "./types";

const entry = (kind: TranscriptEntry["kind"], text: string): TranscriptEntry => ({ kind, text, detail: null });
const live = (kind: TermLine["kind"], text: string): TermLine => ({ kind, text });

describe("sameLine", () => {
  it("matches the same kind and text", () => {
    expect(sameLine(entry("tool", "Read x"), live("tool", "Read x"))).toBe(true);
  });
  it("does not match across kinds", () => {
    expect(sameLine(entry("tool", "Read x"), live("result", "Read x"))).toBe(false);
  });
  it("matches a cut stream line against the uncut entry", () => {
    const long = "word ".repeat(80).trim();
    expect(sameLine(entry("text", long), live("text", long.slice(0, 50) + "…"))).toBe(true);
    expect(sameLine(entry("text", "different"), live("text", long.slice(0, 50) + "…"))).toBe(false);
  });
});

describe("freshLines", () => {
  it("drops lines the page already shows and keeps the rest in order", () => {
    const page = [entry("prompt", "go"), entry("tool", "Read a")];
    const buffered = [live("tool", "Read a"), live("result", "12 lines"), live("text", "done")];
    expect(freshLines(page, buffered)).toEqual([live("result", "12 lines"), live("text", "done")]);
  });
  it("only compares against the last twelve entries", () => {
    const page = Array.from({ length: 30 }, (_, i) => entry("text", `l${i}`));
    expect(freshLines(page, [live("text", "l0")])).toEqual([live("text", "l0")]);
  });
});
