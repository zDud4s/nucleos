import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { ConfirmButton } from "./ConfirmButton";

/**
 * The interlock, tested as a clock.
 *
 * Fake timers rather than waiting: the two windows this component defends are
 * 300 ms and 4 s, and a test that really slept for them would be four seconds
 * of suite time per case and flaky on a loaded machine anyway.
 *
 * `fireEvent` rather than `userEvent`: user-event schedules its own delays on
 * the real clock, and pairing it with fake timers needs an `advanceTimers`
 * bridge that adds a moving part to the thing under test.
 */
describe("ConfirmButton", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function setup() {
    const onConfirm = vi.fn();
    const onArmedChange = vi.fn();
    render(
      <ConfirmButton
        label="Delete series"
        confirmLabel="Really delete"
        onConfirm={onConfirm}
        onArmedChange={onArmedChange}
      />,
    );
    return { onConfirm, onArmedChange };
  }

  it("arms on the first click without acting", () => {
    const { onConfirm, onArmedChange } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));

    // The label is the interlock's whole explanation: the button now says what
    // the next click will do.
    expect(screen.getByRole("button", { name: "Really delete" })).toBeDefined();
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onArmedChange).toHaveBeenCalledWith(true);
  });

  it("confirms on a second click once the dwell has passed", () => {
    const { onConfirm, onArmedChange } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));

    expect(onConfirm).toHaveBeenCalledTimes(1);
    // And it disarms itself: the control is back to needing two clicks.
    expect(screen.getByRole("button", { name: "Delete series" })).toBeDefined();
    expect(onArmedChange).toHaveBeenLastCalledWith(false);
  });

  it("ignores a confirm inside the 300 ms dwell and stays armed", () => {
    const { onConfirm } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    // 299 ms: the tail of a double-click, not a decision.
    act(() => {
      vi.advanceTimersByTime(299);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));

    expect(onConfirm).not.toHaveBeenCalled();
    // Ignored, not disarmed — a reflex click must not make the control feel broken.
    expect(screen.getByRole("button", { name: "Really delete" })).toBeDefined();

    // The arming window is still the original one, so the deliberate second
    // click a moment later still lands.
    act(() => {
      vi.advanceTimersByTime(200);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("disarms itself at 4 s", () => {
    const { onConfirm, onArmedChange } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(4000);
    });

    expect(screen.getByRole("button", { name: "Delete series" })).toBeDefined();
    expect(onArmedChange).toHaveBeenLastCalledWith(false);

    // A click after the window re-arms rather than confirming — which is the
    // point of the timeout: an armed control you walked away from is not still
    // one click from deleting something.
    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Really delete" })).toBeDefined();
  });

  it("reports the armed state so a list can freeze its order", () => {
    const { onArmedChange } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));

    expect(onArmedChange.mock.calls.map(([armed]) => armed)).toEqual([true, false]);
  });

  it("exposes armed as a pressed state, not only as a label", () => {
    setup();

    const button = screen.getByRole("button", { name: "Delete series" });
    expect(button.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(button);
    expect(screen.getByRole("button", { name: "Really delete" }).getAttribute("aria-pressed")).toBe("true");
  });
});
