import { afterEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { movingTowardRect, pointInTriangle, useSafeTriangle } from "./SafeTriangle";

const rect = { left: 100, right: 200, top: 0, bottom: 100 };

describe("SafeTriangle geometry", () => {
  it("detects points in a triangle", () => {
    const a = { x: 0, y: 0 };
    const b = { x: 10, y: 0 };
    const c = { x: 0, y: 10 };
    expect(pointInTriangle({ x: 2, y: 2 }, a, b, c)).toBe(true);
    expect(pointInTriangle({ x: 9, y: 9 }, a, b, c)).toBe(false);
  });
  it("treats movement toward the flyout as intent", () => {
    expect(movingTowardRect({ x: 50, y: 50 }, { x: 80, y: 55 }, rect)).toBe(true);
    expect(movingTowardRect({ x: 50, y: 50 }, { x: 60, y: 300 }, rect)).toBe(false);
  });
});

describe("useSafeTriangle", () => {
  afterEach(() => vi.useRealTimers());

  it("closes at once without a flyout", () => {
    const onClose = vi.fn();
    const { result } = renderHook(() => useSafeTriangle({ getRect: () => null, onClose }));
    act(() => result.current.onTriggerLeave({ clientX: 1, clientY: 1 }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it("stays open inside the cone and closes when it is left", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    const { result } = renderHook(() => useSafeTriangle({ getRect: () => rect, onClose }));
    act(() => result.current.onTriggerMove({ clientX: 50, clientY: 50 }));
    act(() => result.current.onTriggerLeave({ clientX: 55, clientY: 50 }));
    act(() => result.current.onDocumentMove({ clientX: 80, clientY: 55 }));
    expect(onClose).not.toHaveBeenCalled();
    act(() => result.current.onDocumentMove({ clientX: 60, clientY: 300 }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it("closes after the delay if the pointer stalls, and cancel() stops it", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    const { result } = renderHook(() => useSafeTriangle({ getRect: () => rect, onClose, delay: 100 }));
    act(() => result.current.onTriggerLeave({ clientX: 50, clientY: 50 }));
    act(() => result.current.cancel());
    act(() => void vi.advanceTimersByTime(200));
    expect(onClose).not.toHaveBeenCalled();
    act(() => result.current.onTriggerLeave({ clientX: 50, clientY: 50 }));
    act(() => void vi.advanceTimersByTime(150));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
