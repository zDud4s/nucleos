import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { ModeSwitch } from "./ModeSwitch";

/**
 * What the armed segment says on a project called `alpha`.
 *
 * Spelled out here rather than obtained by calling `promotionConfirmLabel`: this file
 * asserts that the control SHOWS what it is given, and a test that composed the string
 * with the same function the page uses would agree with itself whatever either one said.
 * `lib/mode.test.ts` is where the words themselves are pinned.
 *
 * Short, and it used to be the whole consequence sentence — "alpha acts on its own — 3 of 4
 * proposal slots, no approval", 52 characters inside a segment of a fixed track. It wrapped,
 * and the roster row grew from 90.6 to 125.0 pixels while armed: the button moving out from
 * under a pointer that has four seconds left to press it a second time. The sentence is the
 * caller's to print under the control now; this is what the segment says.
 */
const LABEL = "Let alpha act";

/**
 * And what the ear gets when that segment arms.
 *
 * The other channel, and a different string on purpose: the label is drawn inside a segment
 * of a fixed track, so it can only be a few words, while this is what letting alpha act
 * MEANS. `ModeSwitch` takes it as `actConsequence` and hands it to the interlock's live
 * region — the eye reads the label, the ear is told the consequence.
 */
const CONSEQUENCE =
  "alpha acts on its own — 3 of its 4 proposal slots already in use, no approval";

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
      <ModeSwitch
        value="shadow"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
    );

    const group = screen.getByRole("group", { name: "Autopilot mode" });
    const off = within(group).getByRole("button", { name: "Turn off" });
    const shadow = within(group).getByRole("button", { name: "Watch in shadow" });
    const act_ = within(group).getByRole("button", { name: "Let it act" });

    // The verbs, and only the verbs — never `off / shadow / active`, which is
    // what the project page used to say about the same decision.
    expect(within(group).getAllByRole("button")).toHaveLength(3);

    // One pressed segment: the setting now. The other is an offer.
    expect(shadow.getAttribute("aria-pressed")).toBe("true");
    expect(off.getAttribute("aria-pressed")).toBe("false");

    /*
      And the third carries no `aria-pressed` at all, while it is still an offer.

      It used to say `"false"`, which sounds harmless and is not: this is the segment that
      becomes a `ConfirmButton`, and an armed one reported `"true"` — telling a screen reader
      that the project was acting on its own at the exact moment it was not, the moment the
      interlock exists to hold open. In this group the attribute means "this IS the setting",
      and an interlock halfway through is not a setting anything is on. So the interlock claims
      nothing, and the label plus `.ui-confirm-armed` say what is armed.
    */
    expect(act_.getAttribute("aria-pressed")).toBeNull();

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
      <ModeSwitch
        value="shadow"
        actAllowed={false}
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
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
      <ModeSwitch
        value="shadow"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
    );

    // One press arms and says what the second one will do. It does not act.
    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));
    expect(onChoose).not.toHaveBeenCalled();
    // Armed, it names the project — the one word that says which of four rows is about to be
    // let loose, and not the words of the button it had just replaced. What letting it act
    // would MEAN is the caller's sentence, printed under the control where it has room.
    const armed = screen.getByRole("button", { name: LABEL });
    expect(armed.getAttribute("aria-pressed")).toBeNull();

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
      <ModeSwitch
        value="shadow"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
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
      <ModeSwitch
        value="active"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
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
      <ModeSwitch
        value="shadow"
        actAllowed
        busy
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onChoose={onChoose}
      />,
    );

    for (const name of ["Turn off", "Watch in shadow", "Let it act"]) {
      const seg = screen.getByRole("button", { name });
      expect((seg as HTMLButtonElement).disabled).toBe(true);
      fireEvent.click(seg);
    }
    expect(onChoose).not.toHaveBeenCalled();
  });

  /**
   * The caller is told when the segment arms, so it can say what confirming would do.
   *
   * The consequence sentence has to appear at the moment the interlock opens and go away with
   * it, and it is not this control's to render — it is 52 characters and this is a segment of a
   * fixed track. `ConfirmButton` already reported the armed state (the approval queue freezes
   * its sort order on it), so this is a pass-through and not new plumbing; what is new is that
   * the mode switch hands it on to a page that has a full-width line to print on.
   */
  it("tells the caller when the third segment arms and disarms", () => {
    const onChoose = vi.fn();
    const onArmedChange = vi.fn();
    render(
      <ModeSwitch
        value="shadow"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        onArmedChange={onArmedChange}
        onChoose={onChoose}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));
    expect(onArmedChange.mock.calls.map(([armed]) => armed)).toEqual([true]);

    // Past the dwell, the second press confirms — and disarms on the way.
    act(() => {
      vi.advanceTimersByTime(350);
    });
    fireEvent.click(screen.getByRole("button", { name: LABEL }));
    expect(onArmedChange.mock.calls.map(([armed]) => armed)).toEqual([true, false]);
    expect(onChoose).toHaveBeenCalledWith("active");
  });

  /**
   * And the sentence it prints is the armed button's description, from the first render.
   *
   * `onArmedChange` puts the consequence on the page; this is the other half — the segment
   * naming it, so a screen reader hears what confirming would do instead of a two-word label
   * and a paragraph it has no reason to associate with the button.
   *
   * Rendered through a caller because the class swap is the caller's: the element is in the
   * document the whole time and only its visibility changes. That is the change this case
   * pins — it used to assert the attribute was absent at rest, which was the bug written
   * down as a guarantee.
   */
  it("the armed segment is described at rest and announced on arm", () => {
    function Caller() {
      const [armed, setArmed] = useState(false);
      return (
        <>
          <ModeSwitch
            value="shadow"
            actAllowed
            actArmedLabel={LABEL}
            actConsequence={CONSEQUENCE}
            onArmedChange={setArmed}
            actDescribedBy="consequence"
            onChoose={vi.fn()}
          />
          <p id="consequence" className={armed ? undefined : "sr-only"}>
            {CONSEQUENCE}
          </p>
        </>
      );
    }

    render(<Caller />);

    /*
      Described BEFORE the press, which is the whole change.

      The attribute used to arrive in the same render that swapped the label, on a button that
      already had focus — and a description added to a focused element is not re-announced by
      any major screen reader. So the sentence was attached at the one moment it could not be
      heard. It is a standing description now; the caller hides the element, it does not
      withhold it.
    */
    const offer = screen.getByRole("button", { name: "Let it act" });
    expect(offer.getAttribute("aria-describedby")).toBe("consequence");
    expect(document.getElementById("consequence")?.textContent).toBe(CONSEQUENCE);
    expect(document.getElementById("consequence")?.className).toBe("sr-only");

    fireEvent.click(offer);

    // And the arming itself is announced with the consequence, not with the segment's label.
    const said = screen.getByText(new RegExp(`^armed: ${CONSEQUENCE.slice(0, 20)}`));
    expect(said.getAttribute("role")).toBe("status");
    expect(document.getElementById("consequence")?.className).toBe("");
    expect(screen.getByRole("button", { name: LABEL }).getAttribute("aria-describedby")).toBe(
      "consequence",
    );
  });

  /**
   * The armed ring is drawn INSIDE the button, because the track it sits in clips.
   *
   * `.ui-confirm-armed > .ui-button` puts a 3px ring outside the button, and `.ui-switch`
   * carries `overflow: hidden` so that three segments read as one box — so in here the outer
   * ring was cut off entirely and the armed segment looked exactly like the green segment it
   * had been a moment earlier. jsdom applies no stylesheet, so the sheet is the only place
   * this claim can be made; the `overflow: hidden` half is asserted too, because an inset
   * ring is only the right answer while the track still clips.
   */
  it("the armed ring is inset, because the track clips", () => {
    const ui = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8");

    const inside =
      /\.ui-switch-seg-wrap\s*>\s*\.ui-confirm-armed\s*>\s*\.ui-button\s*\{([^}]*)\}/.exec(ui);
    expect(inside).not.toBeNull();
    expect(inside?.[1]).toMatch(/box-shadow:\s*inset\s+0\s+0\s+0\s+2px\s+var\(--text\)/);

    const track = /\.ui-switch\s*\{([^}]*)\}/.exec(ui);
    expect(track).not.toBeNull();
    expect(track?.[1]).toMatch(/overflow:\s*hidden/);

    // The outer ring is untouched — everywhere that is not a track, it is still the one
    // that draws.
    const outside = /\.ui-confirm-armed\s*>\s*\.ui-button\s*\{([^}]*)\}/.exec(ui);
    expect(outside?.[1]).toMatch(/box-shadow:\s*0\s+0\s+0\s+3px/);
  });

  /**
   * Focusable when inert: a segment that cannot be pressed still keeps its tab stop.
   *
   * A native `disabled` drops a segment out of the tab order, so arrowing or tabbing through the
   * track skips exactly the segment whose refusal needs explaining — and the blocker sentence the
   * caller points at through `actDescribedBy` is attached to a control nobody can land on. With
   * `focusableWhenInert` the locked segment says it is unavailable with `aria-disabled` instead,
   * is still described, and still does nothing when pressed.
   */
  it("focusable when inert, a locked let it act is aria-disabled, described, and does nothing", () => {
    const onChoose = vi.fn();
    render(
      <ModeSwitch
        value="shadow"
        actAllowed={false}
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        actDescribedBy="gate"
        focusableWhenInert
        onChoose={onChoose}
      />,
    );

    const locked = screen.getByRole("button", { name: "Let it act" });
    expect(locked.getAttribute("aria-disabled")).toBe("true");
    expect((locked as HTMLButtonElement).disabled).toBe(false);
    expect(locked.getAttribute("aria-describedby")).toBe("gate");

    // The pair that confirms an unlocked segment, on the same node.
    fireEvent.click(locked);
    act(() => {
      vi.advanceTimersByTime(350);
    });
    fireEvent.click(locked);
    expect(onChoose).not.toHaveBeenCalled();
  });

  it("focusable when inert, the pressed segment and a busy switch keep their tab stops and do nothing", () => {
    const onChoose = vi.fn();
    const { rerender } = render(
      <ModeSwitch
        value="shadow"
        actAllowed
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        focusableWhenInert
        onChoose={onChoose}
      />,
    );

    // The setting you are already on is still inert — just not unreachable.
    const pressed = screen.getByRole("button", { name: "Watch in shadow" });
    expect(pressed.getAttribute("aria-pressed")).toBe("true");
    expect((pressed as HTMLButtonElement).disabled).toBe(false);
    expect(pressed.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(pressed);
    expect(onChoose).not.toHaveBeenCalled();

    rerender(
      <ModeSwitch
        value="shadow"
        actAllowed
        busy
        actArmedLabel={LABEL}
        actConsequence={CONSEQUENCE}
        focusableWhenInert
        onChoose={onChoose}
      />,
    );

    // A write in flight: every segment keeps its tab stop, none of them does anything.
    const segments = ["Turn off", "Watch in shadow", "Let it act"].map((name) =>
      screen.getByRole("button", { name }),
    );
    for (const seg of segments) {
      expect((seg as HTMLButtonElement).disabled).toBe(false);
      expect(seg.getAttribute("aria-disabled")).toBe("true");
      fireEvent.click(seg);
    }
    // And the interlock's second press, past the dwell, confirms nothing either.
    act(() => {
      vi.advanceTimersByTime(350);
    });
    fireEvent.click(segments[2]);
    expect(onChoose).not.toHaveBeenCalled();
  });

  /**
   * The paint follows the refusal, whichever attribute says it.
   *
   * `ui.css` takes the green off a `:disabled` approve button; a locked segment that keeps its
   * focus is not `:disabled`, so the same rule has to name `[aria-disabled="true"]` too, or the
   * refusing segment is Acting Green again. Read out of the sheet because jsdom applies none.
   */
  it("a locked segment that keeps its focus is not green either", () => {
    const ui = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8");
    const rule =
      /\.ui-button-approve\[aria-disabled="true"\],\s*\.ui-button-approve:disabled\s*\{([^}]*)\}/.exec(ui);
    expect(rule).not.toBeNull();
    expect(rule?.[1]).toMatch(/background:\s*none/);
  });
});
