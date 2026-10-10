import { describe, expect, it } from "vitest";
import { shouldMount } from "./frame";

describe("panel frame guard", () => {
  it("panel frame guard mounts in the top frame only", () => {
    const top = {} as Window;
    (top as { top: Window }).top = top;
    (top as { self: Window }).self = top;
    expect(shouldMount(top)).toBe(true);
    const child = { top, self: {} } as unknown as Window;
    expect(shouldMount(child)).toBe(false);
  });
  it("panel frame guard refuses when reading top throws", () => {
    const hostile = {
      get top(): Window {
        throw new Error("cross-origin");
      },
      self: {},
    } as unknown as Window;
    expect(shouldMount(hostile)).toBe(false);
  });
});
