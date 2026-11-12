import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

const tauri = vi.hoisted(() => ({
  currentMonitor: vi.fn(),
  innerPosition: vi.fn(),
  scaleFactor: vi.fn(),
  onMoved: vi.fn(),
  onResized: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  currentMonitor: tauri.currentMonitor,
  getCurrentWindow: () => ({
    innerPosition: tauri.innerPosition,
    scaleFactor: tauri.scaleFactor,
    onMoved: tauri.onMoved,
    onResized: tauri.onResized,
  }),
}));

import { useScreenLine } from "./screen-line";

beforeEach(() => {
  for (const mock of Object.values(tauri)) mock.mockReset();
  tauri.onMoved.mockResolvedValue(() => {});
  tauri.onResized.mockResolvedValue(() => {});
});

/** A 1920x1080 monitor at 125%, a 60px taskbar at the bottom: the owner's own screen, physically. */
function monitor() {
  return {
    workArea: { position: { x: 0, y: 0 }, size: { width: 1920, height: 1020 } },
    scaleFactor: 1.25,
  };
}

describe("useScreenLine", () => {
  /**
   * The same line `notch.rs` centres the floating notch on — the middle of the work area — measured
   * from where this window's page starts, in CSS pixels. A window whose content starts 60 physical
   * pixels down (a title bar) sees the middle 510 - 60 = 450 physical pixels below its top, which at
   * 125% is 360 CSS pixels. Centred in its own page instead, as it was, the notch sat 30 CSS pixels
   * lower than the floating one on the same screen.
   */
  it("answers the work area's middle, measured from this window's page", async () => {
    tauri.currentMonitor.mockResolvedValue(monitor());
    tauri.innerPosition.mockResolvedValue({ x: 0, y: 60 });
    tauri.scaleFactor.mockResolvedValue(1.25);

    const { result } = renderHook(() => useScreenLine(true));
    await waitFor(() => expect(result.current).toBe(360));
  });

  /** Moving the window moves the line within it, so the notch stays put on the screen. */
  it("measures again when the window moves", async () => {
    tauri.currentMonitor.mockResolvedValue(monitor());
    tauri.innerPosition.mockResolvedValue({ x: 0, y: 60 });
    tauri.scaleFactor.mockResolvedValue(1.25);
    let moved: () => void = () => {};
    tauri.onMoved.mockImplementation(async (handler: () => void) => {
      moved = handler;
      return () => {};
    });

    const { result } = renderHook(() => useScreenLine(true));
    await waitFor(() => expect(result.current).toBe(360));

    tauri.innerPosition.mockResolvedValue({ x: 0, y: 260 });
    moved();
    await waitFor(() => expect(result.current).toBe(200));
  });

  /** Outside Tauri, or refused, there is no line: the notch centres in its window instead. */
  it("answers nothing when the window API cannot", async () => {
    tauri.currentMonitor.mockRejectedValue(new Error("not inside Tauri"));
    tauri.innerPosition.mockResolvedValue({ x: 0, y: 60 });
    tauri.scaleFactor.mockResolvedValue(1.25);

    const { result } = renderHook(() => useScreenLine(true));
    await waitFor(() => expect(tauri.currentMonitor).toHaveBeenCalled());
    expect(result.current).toBeUndefined();
  });

  /**
   * Dragged along the edge, the line moves with it — and without asking Tauri again: the geometry
   * was measured once, and a drag is arithmetic on it every frame.
   */
  it("follows the notch along the edge without measuring again", async () => {
    tauri.currentMonitor.mockResolvedValue(monitor());
    tauri.innerPosition.mockResolvedValue({ x: 0, y: 60 });
    tauri.scaleFactor.mockResolvedValue(1.25);

    const { result, rerender } = renderHook(({ along }) => useScreenLine(true, along), {
      initialProps: { along: 0.5 },
    });
    await waitFor(() => expect(result.current).toBe(360));
    const asked = tauri.currentMonitor.mock.calls.length;

    // A quarter of 1020 is 255 physical pixels down, 195 below this page's top: 156 CSS pixels.
    rerender({ along: 0.25 });
    expect(result.current).toBe(156);
    expect(tauri.currentMonitor.mock.calls.length).toBe(asked);
  });

  /**
   * On the top or bottom edge the line is a column: `along` of the way across the work area,
   * measured from where this page starts on the left. A window 160 physical pixels in from the left
   * sees the middle of 1920 at 960 - 160 = 800 physical pixels, 640 CSS pixels at 125%.
   */
  it("answers a column of the screen for a notch on the top or bottom edge", async () => {
    tauri.currentMonitor.mockResolvedValue(monitor());
    tauri.innerPosition.mockResolvedValue({ x: 160, y: 60 });
    tauri.scaleFactor.mockResolvedValue(1.25);

    const { result } = renderHook(() => useScreenLine(true, 0.5, "top"));
    await waitFor(() => expect(result.current).toBe(640));
  });

  /** With the notch floating there is nothing in this window to hang, so nothing is asked. */
  it("asks nothing while the notch is not contained", () => {
    const { result } = renderHook(() => useScreenLine(false));
    expect(result.current).toBeUndefined();
    expect(tauri.currentMonitor).not.toHaveBeenCalled();
  });
});
