// @vitest-environment node
import { describe, expect, it } from "vitest";
import { backoffFor, CARD_ZOOM, GROUP_THRESHOLD, RING_ROWS, SILENCE_MS, UNREACHABLE_AFTER } from "./limits";

describe("fleet limits", () => {
  it("names the constants the spec fixes", () => {
    expect(GROUP_THRESHOLD).toBe(6);
    expect(CARD_ZOOM).toBe(0.6);
    expect(SILENCE_MS).toBe(30_000);
    expect(RING_ROWS).toBe(12);
    expect(UNREACHABLE_AFTER).toBe(3);
  });

  it("backs off 1, 2, 4, 8 and then stays at 10 seconds", () => {
    expect([0, 1, 2, 3, 4, 5, 9].map(backoffFor)).toEqual([1000, 2000, 4000, 8000, 10_000, 10_000, 10_000]);
  });
});
