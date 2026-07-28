import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import { ConfirmButton } from "./ConfirmButton";

/** Timer callbacks flip React state, so they belong inside an act() batch. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

/**
 * This component is the app's only interlock: every irreversible control —
 * reject and discard the worktree, disengage the kill switch, approve and
 * resume a run — is this button. So these tests are about what it REFUSES,
 * not about what it renders.
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
    render(
      <ConfirmButton confirmLabel="Discard worktree?" onConfirm={onConfirm}>
        Reject
      </ConfirmButton>,
    );
    return { onConfirm, button: screen.getByRole("button") };
  }

  it("arms on the first click and asks the question instead of acting", () => {
    const { onConfirm, button } = setup();

    fireEvent.click(button);

    expect(button.textContent).toBe("Discard worktree?");
    expect(button.className).toContain("is-armed");
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("swallows a double-click: two clicks 50ms apart are one accident", () => {
    const { onConfirm, button } = setup();

    fireEvent.click(button);
    advance(50);
    fireEvent.click(button);

    expect(onConfirm).not.toHaveBeenCalled();
    // Still armed rather than disarmed: the accident must not cost the
    // deliberate second click that follows it.
    expect(button.textContent).toBe("Discard worktree?");
  });

  it("confirms on a second click once the dwell has passed", () => {
    const { onConfirm, button } = setup();

    fireEvent.click(button);
    advance(400);
    fireEvent.click(button);

    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(button.textContent).toBe("Reject");
  });

  it("disarms itself when the second click never comes", () => {
    const { onConfirm, button } = setup();

    fireEvent.click(button);
    advance(4000);

    expect(button.textContent).toBe("Reject");
    // The next click re-arms rather than confirming: a stale arming is gone,
    // not merely invisible.
    fireEvent.click(button);
    expect(onConfirm).not.toHaveBeenCalled();
    expect(button.textContent).toBe("Discard worktree?");
  });

  it("cancels the browser's auto-repeat so a held Enter cannot arm and confirm", () => {
    const { button } = setup();

    const held = new KeyboardEvent("keydown", {
      key: "Enter",
      repeat: true,
      bubbles: true,
      cancelable: true,
    });
    button.dispatchEvent(held);

    // A held Enter fires keydown at ~30/s and every one of them synthesises a
    // click; cancelling the repeat is what stops those clicks from existing.
    expect(held.defaultPrevented).toBe(true);
  });

  it("lets the first, non-repeated key press through", () => {
    const { button } = setup();

    const first = new KeyboardEvent("keydown", {
      key: "Enter",
      repeat: false,
      bubbles: true,
      cancelable: true,
    });
    button.dispatchEvent(first);

    expect(first.defaultPrevented).toBe(false);
  });

  it("reports arming to a caller that must hold still while a decision is open", () => {
    const onArmedChange = vi.fn();
    render(
      <ConfirmButton
        confirmLabel="Discard worktree?"
        onConfirm={vi.fn()}
        onArmedChange={onArmedChange}
      >
        Reject
      </ConfirmButton>,
    );
    const button = screen.getByRole("button");

    fireEvent.click(button);
    expect(onArmedChange).toHaveBeenLastCalledWith(true);

    advance(4000);
    expect(onArmedChange).toHaveBeenLastCalledWith(false);
  });
});
