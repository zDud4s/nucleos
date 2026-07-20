import { describe, it, expect } from "vitest";

describe("vitest smoke", () => {
  it("runs the frontend test gate", () => {
    expect(1 + 1).toBe(2);
  });
});
