import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useNow } from "./useNow";

describe("useNow", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 9, 21, 12, 0, 0));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("ticks at the interval it was given", () => {
    const { result } = renderHook(() => useNow(1000));
    const first = result.current;
    expect(first).toBe(Date.now());

    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(result.current).toBe(first + 3000);
  });

  it("does not tick between intervals", () => {
    const { result } = renderHook(() => useNow(30_000));
    const first = result.current;

    act(() => {
      vi.advanceTimersByTime(29_000);
    });
    expect(result.current).toBe(first);
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(result.current).toBe(first + 30_000);
  });

  it("freezes when inactive and unsubscribes", () => {
    const { result, rerender } = renderHook(({ active }) => useNow(1000, active), {
      initialProps: { active: true },
    });
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    const stopped = result.current;

    rerender({ active: false });
    expect(vi.getTimerCount()).toBe(0);
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(result.current).toBe(stopped);

    rerender({ active: true });
    expect(result.current).toBe(Date.now());
  });

  it("shares one timer between subscribers of the same length and clears it on the last", () => {
    const a = renderHook(() => useNow(1000));
    const b = renderHook(() => useNow(1000));
    expect(vi.getTimerCount()).toBe(1);

    a.unmount();
    expect(vi.getTimerCount()).toBe(1);
    b.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("reads the wall clock when it mounts inactive", () => {
    const { result } = renderHook(() => useNow(1000, false));
    expect(result.current).toBe(Date.now());
    expect(vi.getTimerCount()).toBe(0);
  });
});
