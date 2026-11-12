import { describe, expect, it, vi } from "vitest";
import { fireEvent, renderHook } from "@testing-library/react";
import { useHistoryNavigation } from "./useHistoryNavigation";

describe("useHistoryNavigation", () => {
  it("maps mouse buttons 3/4 and Alt+Arrows to back/forward", () => {
    const nav = { back: vi.fn(), forward: vi.fn() };
    renderHook(() => useHistoryNavigation(nav));
    fireEvent.mouseUp(window, { button: 3 });
    fireEvent.mouseUp(window, { button: 4 });
    fireEvent.keyDown(window, { key: "ArrowLeft", altKey: true });
    fireEvent.keyDown(window, { key: "ArrowRight", altKey: true });
    expect(nav.back).toHaveBeenCalledTimes(2);
    expect(nav.forward).toHaveBeenCalledTimes(2);
  });
  it("ignores plain arrows and Alt+Arrow inside inputs", () => {
    const nav = { back: vi.fn(), forward: vi.fn() };
    renderHook(() => useHistoryNavigation(nav));
    const input = document.createElement("input");
    document.body.appendChild(input);
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    fireEvent.keyDown(input, { key: "ArrowLeft", altKey: true });
    expect(nav.back).not.toHaveBeenCalled();
    input.remove();
  });
});
