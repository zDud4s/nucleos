import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { ModeSwitch } from "./ModeSwitch";

/**
 * What the armed segment says on a project called `alpha` holding three of four slots.
 *
 * Spelled out here rather than obtained by calling `promotionConsequence`: this file
 * asserts that the control SHOWS what it is given, and a test that composed the string
 * with the same function the page uses would agree with itself whatever either one said.
 * `lib/mode.test.ts` is where the sentence itself is pinned.
 */
const LABEL = "alpha acts on its own — 3 of 4 proposal slots, no approval";

/**
 * The one control both surfaces that set a project's autonomy now use.
 *
 * Fake timers, and `fireEvent` rather than `userEvent`, for the reason
 * `ConfirmButton.test.tsx` gives: the third segment is that interlock, its
 * dwell is 300 ms, and a test that really slept for it would be slow here and
 * flaky on a loaded machine.
 */
describe("ModeSwitch", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("three segments, verbs, and the current one pressed", () => {
    const onChoose = vi.fn();
    render(
      <ModeSwitch value="shadow" actAllowed actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    const group = screen.getByRole("group", { name: "Autopilot mode" });
    const off = within(group).getByRole("button", { name: "Turn off" });
    const shadow = within(group).getByRole("button", { name: "Watch in shadow" });
    const act_ = within(group).getByRole("button", { name: "Let it act" });

    // The verbs, and only the verbs — never `off / shadow / active`, which is
    // what the project page used to say about the same decision.
    expect(within(group).getAllByRole("button")).toHaveLength(3);

    // One pressed segment: the setting now. The other two are offers.
    expect(shadow.getAttribute("aria-pressed")).toBe("true");
    expect(off.getAttribute("aria-pressed")).toBe("false");
    expect(act_.getAttribute("aria-pressed")).toBe("false");

    // The pressed one is inert — there is nothing to choose about the setting
    // you are already on.
    expect((shadow as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(off);
    expect(onChoose).toHaveBeenCalledTimes(1);
    expect(onChoose).toHaveBeenCalledWith("off");
  });

  it("letting it act takes two presses and is not green when locked", () => {
    const onChoose = vi.fn();
    const locked = render(
      <ModeSwitch value="shadow" actAllowed={false} actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    const blocked = screen.getByRole("button", { name: "Let it act" });
    expect((blocked as HTMLButtonElement).disabled).toBe(true);
    /*
      And it refuses without a tooltip.

      The blocker used to ride here as a `title`, which made it the third copy of one
      sentence: both callers already print it, visibly, under exactly the condition that
      locks this segment. A copy you have to hover to read, on a disabled control that
      does not reliably receive hover, is the copy nobody reads.
    */
    expect(blocked.getAttribute("title")).toBeNull();
    fireEvent.click(blocked);
    expect(onChoose).not.toHaveBeenCalled();

    /*
      And it is not green while it is refusing.

      jsdom applies no stylesheet, so the only place this claim can be made is
      the sheet itself: the segment keeps `ui-button-approve` — that is what
      makes it green when it CAN be pressed — and `ui.css` takes the paint back
      off it while it is disabled. Acting Green at 45% opacity still reads as
      "this will act".
    */
    expect(blocked.className).toContain("ui-button-approve");
    const ui = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8");
    const rule = /\.ui-button-approve:disabled\s*\{([^}]*)\}/.exec(ui);
    expect(rule).not.toBeNull();
    expect(rule?.[1]).toMatch(/background:\s*none/);

    locked.unmount();

    render(
      <ModeSwitch value="shadow" actAllowed actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    // One press arms and says what the second one will do. It does not act.
    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));
    expect(onChoose).not.toHaveBeenCalled();
    // Armed, it names the project and the ceiling. "It may act on its own" said neither,
    // and was the same sentence as the button it had just replaced.
    const armed = screen.getByRole("button", { name: LABEL });

    // Past the dwell, the second press is the decision.
    act(() => {
      vi.advanceTimersByTime(350);
    });
    fireEvent.click(armed);
    expect(onChoose).toHaveBeenCalledTimes(1);
    expect(onChoose).toHaveBeenCalledWith("active");
  });

  /**
   * Unlocked, the segment that has been earned looks like it.
   *
   * The pair to the locked case above, and the reason that one says anything: `ui.css`
   * takes the green off a disabled `.ui-button-approve`, which is only a rule about
   * refusing if the enabled one still HAS the green. Read out of the sheet for the same
   * reason — jsdom applies no stylesheet, so the class on the element and the paint behind
   * the class are two claims and both have to be made.
   */
  it("unlocked, the third segment keeps the approve tone", () => {
    const onChoose = vi.fn();
    render(
      <ModeSwitch value="shadow" actAllowed actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    const offer = screen.getByRole("button", { name: "Let it act" });
    expect((offer as HTMLButtonElement).disabled).toBe(false);
    expect(offer.className).toContain("ui-button-approve");

    const ui = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8");
    const rule = /\.ui-button-approve\s*\{([^}]*)\}/.exec(ui);
    expect(rule).not.toBeNull();
    expect(rule?.[1]).toMatch(/background:\s*var\(--tone-active-bg\)/);
  });

  /**
   * A project that is already acting has nothing to confirm.
   *
   * The interlock is about the crossing, not about the state: offering "It may
   * act on its own" to somebody whose project is already acting would ask them
   * to confirm a thing that has already happened.
   */
  it("the third segment stops being an interlock once it is the setting", () => {
    const onChoose = vi.fn();
    render(
      <ModeSwitch value="active" actAllowed actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    const acting = screen.getByRole("button", { name: "Let it act" });
    expect(acting.getAttribute("aria-pressed")).toBe("true");
    expect((acting as HTMLButtonElement).disabled).toBe(true);
    expect(acting.className).toContain("ui-switch-seg");
    expect(acting.className).not.toContain("ui-button-approve");
  });

  /** A write in flight makes every segment inert, including the interlock. */
  it("is inert while a write is in flight", () => {
    const onChoose = vi.fn();
    render(
      <ModeSwitch value="shadow" actAllowed busy actConfirmLabel={LABEL} onChoose={onChoose} />,
    );

    for (const name of ["Turn off", "Watch in shadow", "Let it act"]) {
      const seg = screen.getByRole("button", { name });
      expect((seg as HTMLButtonElement).disabled).toBe(true);
      fireEvent.click(seg);
    }
    expect(onChoose).not.toHaveBeenCalled();
  });
});
