import { afterEach, describe, expect, it, vi } from "vitest";
import { canViewTransition, startViewTransition } from "./viewTransition";

// Loose on purpose: the DOM lib types `startViewTransition` strictly, the tests stub it with spies.
type Doc = { startViewTransition?: unknown };

describe("viewTransition", () => {
  afterEach(() => {
    delete (document as unknown as Doc).startViewTransition;
    vi.unstubAllGlobals();
  });

  it("falls back to calling update directly without browser support", () => {
    const update = vi.fn();
    expect(canViewTransition()).toBe(false);
    startViewTransition(update);
    expect(update).toHaveBeenCalledTimes(1);
  });
  it("uses document.startViewTransition when supported", () => {
    const svt = vi.fn((cb: () => void) => cb());
    (document as unknown as Doc).startViewTransition = svt;
    const update = vi.fn();
    startViewTransition(update);
    expect(svt).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledTimes(1);
  });
  it("skips the transition under reduced motion", () => {
    const svt = vi.fn();
    (document as unknown as Doc).startViewTransition = svt;
    vi.stubGlobal("matchMedia", () => ({ matches: true }));
    const update = vi.fn();
    startViewTransition(update);
    expect(svt).not.toHaveBeenCalled();
    expect(update).toHaveBeenCalledTimes(1);
  });
});
