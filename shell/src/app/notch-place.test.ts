import { afterEach, describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { clampAlong, MIDDLE, readAlong, readEdge, useNotchPlace, writeAlong } from "./notch-place";

const KEY = "nucleos.notch-along";

afterEach(() => {
  window.localStorage.removeItem(KEY);
  window.localStorage.removeItem("nucleos.notch-edge");
});

describe("notch-place", () => {
  /** Nobody has moved it: the middle of the edge, where the notch hung before this existed. */
  it("hangs in the middle until somebody moves it", () => {
    expect(readAlong()).toBe(MIDDLE);
  });

  /**
   * The stored value is text somebody can edit, so it is checked: not a number is the middle, and
   * outside [0, 1] is the nearer end — never a notch placed off the screen.
   */
  it("reads back a fraction, and never a place off the edge", () => {
    writeAlong(0.3);
    expect(readAlong()).toBe(0.3);
    window.localStorage.setItem(KEY, "banana");
    expect(readAlong()).toBe(MIDDLE);
    window.localStorage.setItem(KEY, "4");
    expect(readAlong()).toBe(1);
    expect(clampAlong(-2)).toBe(0);
    expect(clampAlong(Number.NaN)).toBe(MIDDLE);
  });

  /**
   * A drag is many positions and one decision: the steps move the notch and write nothing, and
   * the drop is what the next launch finds.
   */
  it("remembers the drop, and not the steps of the drag", () => {
    const { result } = renderHook(() => useNotchPlace());
    act(() => result.current[1]({ edge: "right", along: 0.2 }, false));
    expect(result.current[0]).toEqual({ edge: "right", along: 0.2 });
    expect(window.localStorage.getItem(KEY)).toBeNull();

    act(() => result.current[1]({ edge: "top", along: 0.7 }, true));
    expect(result.current[0]).toEqual({ edge: "top", along: 0.7 });
    expect(window.localStorage.getItem(KEY)).toBe("0.7");
    expect(readEdge()).toBe("top");
  });

  /** An edge somebody typed into storage by hand is checked like the fraction: anything else is the right. */
  it("reads an unknown edge as the right one", () => {
    window.localStorage.setItem("nucleos.notch-edge", "diagonal");
    expect(readEdge()).toBe("right");
    window.localStorage.setItem("nucleos.notch-edge", "bottom");
    expect(readEdge()).toBe("bottom");
  });

  /**
   * The other window hears it. Dragged while floating, the notch docks back in the same place,
   * because the main window was listening to the storage the floating one wrote.
   */
  it("follows a position written by the other window", () => {
    const { result } = renderHook(() => useNotchPlace());
    act(() => {
      window.localStorage.setItem(KEY, "0.8");
      window.localStorage.setItem("nucleos.notch-edge", "left");
      window.dispatchEvent(new StorageEvent("storage", { key: KEY }));
    });
    expect(result.current[0]).toEqual({ edge: "left", along: 0.8 });
  });
});
