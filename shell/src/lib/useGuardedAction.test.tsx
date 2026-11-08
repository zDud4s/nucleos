import { describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useGuardedAction } from "./useGuardedAction";

describe("useGuardedAction", () => {
  it("dedups double clicks while in flight and mints a key per run", async () => {
    let release!: () => void;
    const keys: string[] = [];
    const action = vi.fn((intent: { key: string }) => {
      keys.push(intent.key);
      return new Promise<number>((r) => (release = () => r(1)));
    });
    const { result } = renderHook(() => useGuardedAction(action));
    let a!: Promise<number>;
    let b!: Promise<number>;
    act(() => {
      a = result.current.run();
      b = result.current.run();
    });
    expect(a).toBe(b);
    expect(action).toHaveBeenCalledTimes(1);
    expect(result.current.pending).toBe(true);
    await act(async () => {
      release();
      await a;
    });
    expect(result.current.pending).toBe(false);
    act(() => void result.current.run());
    expect(action).toHaveBeenCalledTimes(2);
    expect(keys[0]).not.toBe(keys[1]);
  });
  it("re-arms after a rejection", async () => {
    const action = vi.fn(() => Promise.reject(new Error("x")));
    const { result } = renderHook(() => useGuardedAction(action));
    await act(async () => {
      await result.current.run().catch(() => {});
    });
    expect(result.current.pending).toBe(false);
  });
});
