// @vitest-environment node
import { describe, expect, it } from "vitest";
import { elapsed, STATE_TEXT, tokens } from "./format";

describe("format", () => {
  it("writes elapsed time compactly", () => {
    expect(elapsed(4_000)).toBe("4s");
    expect(elapsed(125_000)).toBe("2m 05s");
    expect(elapsed(3_725_000)).toBe("1h 02m");
    expect(elapsed(-5)).toBe("0s");
  });
  it("writes context tokens in thousands", () => {
    expect(tokens(96_000)).toBe("96k");
    expect(tokens(850)).toBe("850");
    expect(tokens(null)).toBe("—");
  });
  it("says each state in the owner's words", () => {
    expect(STATE_TEXT).toEqual({
      busy: "running",
      idle: "waiting for you",
      done: "done",
      lost: "ended without closing",
    });
  });
});
