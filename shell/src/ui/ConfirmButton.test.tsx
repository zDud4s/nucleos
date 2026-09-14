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
        variant="danger"
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

  it("reports the disarm when an armed control unmounts", () => {
    const onArmedChange = vi.fn();
    const { unmount } = render(
      <ConfirmButton
        label="Approve 2"
        confirmLabel="Really approve"
        variant="danger"
        onConfirm={() => {}}
        onArmedChange={onArmedChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Approve 2" }));
    expect(onArmedChange).toHaveBeenLastCalledWith(true);

    // The batch interlock unmounts the moment the selection empties under it — `Clear
    // selection`, or unticking the last box, both while it is armed. The armed report is a
    // pair: without the second half the queue's count never returns to zero and the section
    // keeps its sort order frozen for the rest of the page's life.
    unmount();

    expect(onArmedChange).toHaveBeenLastCalledWith(false);
  });

  it("arming is a label swap, not a pressed state", () => {
    setup();

    /*
      This used to assert the opposite — `aria-pressed="false"` at rest, `"true"` armed.

      It was wrong in the one place the attribute mattered most. Inside `ModeSwitch`'s
      `role="group"` the two setting segments carry `aria-pressed` to mean "this is the
      setting now", so an armed third segment announcing "pressed" told a screen reader that
      the project was acting on its own at the exact moment it was not — the moment the
      interlock exists to hold open. An interlock halfway through is not a state anything is
      in, so it claims none: what is armed is said by the label, and by `.ui-confirm-armed`
      for the eye.
    */
    const button = screen.getByRole("button", { name: "Delete series" });
    expect(button.getAttribute("aria-pressed")).toBeNull();

    fireEvent.click(button);
    const armed = screen.getByRole("button", { name: "Really delete" });
    expect(armed.getAttribute("aria-pressed")).toBeNull();
    // And the swap itself is still the whole signal: a different word, and the armed ring.
    expect(armed.closest("span")?.className).toContain("ui-confirm-armed");
  });

  /**
   * Armed is said out loud, not only drawn.
   *
   * The label swap and `.ui-confirm-armed` are both things you have to be LOOKING at the
   * button to notice. A screen reader was told nothing at all: the button's accessible name
   * changed under it with no announcement, so the one control on the page that deliberately
   * asks for a second press gave no sign it was waiting for one.
   */
  it("the armed state is announced", () => {
    setup();

    // Nothing is claimed before anything happens — the region exists so it can speak, not
    // so it can narrate a control at rest.
    expect(screen.getByRole("status").textContent).toBe("");

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));

    expect(screen.getByRole("status").textContent).toBe(
      "armed: Really delete — press again to confirm",
    );

    // And the prefix is load-bearing: the announcement is not a second exact copy of the
    // label, so `getByText` on the label still finds one node. That node is the label's own
    // span rather than the button since the width was reserved — both labels live in the
    // stack now — so what is asserted is that it is the span inside THIS button, and that
    // the region did not become a second copy of the words on it.
    const drawn = screen.getByText("Really delete");
    expect(drawn.tagName).toBe("SPAN");
    expect(drawn.closest("button")).toBe(screen.getByRole("button", { name: "Really delete" }));
  });

  /**
   * The window closing is the event nobody sees.
   *
   * A confirm is followed by whatever the confirm does — a row disappears, a page moves —
   * so it says for itself that it happened. An expiry is four seconds of nothing, after
   * which the next click arms again instead of acting, and until this region existed the
   * only way to find that out was to press the button and watch it not fire.
   */
  it("an expiry is announced and a confirm is not", () => {
    const { onConfirm } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(4000);
    });

    expect(screen.getByRole("status").textContent).toBe("disarmed — nothing changed");

    // Arm it again and go through with it. The confirm path CLEARS the region rather than
    // reporting an expiry that did not happen: "nothing changed" after a delete would be
    // the one wrong thing this region could possibly say.
    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));

    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  /**
   * The consequence is the caller's sentence, and the button can point at it.
   *
   * Inside `ModeSwitch` the label is a segment of a fixed track and says only WHICH project;
   * what confirming would mean is a sentence the page prints elsewhere. `describedBy` is how
   * the two are joined without the interlock having to know what the sentence is.
   */
  it("the description is carried to the button", () => {
    render(
      <ConfirmButton
        label="Let it act"
        confirmLabel="Let alpha act"
        variant="approve"
        describedBy="consequence-1"
        onConfirm={vi.fn()}
      />,
    );

    const button = screen.getByRole("button", { name: "Let it act" });
    expect(button.getAttribute("aria-describedby")).toBe("consequence-1");
    // A description is not a name: the button is still found by the words on it. `textContent`
    // is no longer that question — it reads both labels now, because the hidden one is what
    // holds the width — so ask what is SHOWING.
    const showing = button.querySelectorAll(".ui-confirm-stack > :not([aria-hidden])");
    expect(showing).toHaveLength(1);
    expect(showing[0].textContent).toBe("Let it act");
  });

  it("the consequence is announced, not the label", () => {
    render(
      <ConfirmButton
        label="Let it act"
        confirmLabel="Let alpha act"
        sayAs="alpha acts on its own — 3 of its 4 proposal slots already in use, no approval"
        variant="approve"
        onConfirm={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));

    // The eye gets the label; the ear used to get the same two words. What somebody needs
    // before pressing again is what it MEANS, and that is the sentence the caller prints.
    const said = screen.getByText(/^armed: alpha acts on its own/);
    expect(said.getAttribute("role")).toBe("status");
    expect(said.textContent).toBe(
      "armed: alpha acts on its own — 3 of its 4 proposal slots already in use, no approval — press again to confirm",
    );
  });

  it("a label that is not a string still announces its object", () => {
    render(
      <ConfirmButton
        label={
          <>
            <span aria-hidden="true">▶</span>Release kill switch
          </>
        }
        confirmLabel={
          <>
            <span aria-hidden="true">▶</span>Really release — work resumes
          </>
        }
        sayAs="Really release — work resumes"
        variant="danger-solid"
        onConfirm={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Release kill switch" }));

    // A fragment cannot be interpolated, so this used to say "armed — press again to confirm"
    // and name nothing at all — on the control that restarts every autonomous thing there is.
    const said = screen.getByText(/^armed: Really release/);
    expect(said.getAttribute("role")).toBe("status");
    expect(screen.queryByText("armed — press again to confirm")).toBeNull();
  });

  it("arming swaps only danger to danger-solid", () => {
    setup();
    render(
      <ConfirmButton
        label="Archive"
        confirmLabel="Archive it"
        variant="quiet"
        onConfirm={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));

    expect(screen.getByRole("button", { name: "Really delete" }).className).toContain("ui-button-danger-solid");
    expect(screen.getByRole("button", { name: "Archive it" }).className).toContain("ui-button-quiet");
    expect(screen.getByRole("button", { name: "Archive it" }).className).not.toContain("ui-button-danger-solid");
  });

  it("the interlock says which row it is armed on", () => {
    render(
      <ConfirmButton
        label="Approve #101"
        confirmLabel="Let this action happen"
        subject="#101"
        variant="approve"
        onConfirm={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Approve #101" }));

    // The eye keeps the anchor where the rest label had it, at the end.
    expect(screen.getByRole("button", { name: "Let this action happen · #101" })).toBeDefined();

    // The ear gets it first: by the time the consequence has been read the question is
    // already "which one?", and five cards on this page carry the same consequence.
    const said = screen.getByText(/^armed: #101/);
    expect(said.getAttribute("role")).toBe("status");
    expect(said.textContent).toBe(
      "armed: #101 — Let this action happen — press again to confirm",
    );
  });

  it("a long announcement gets a longer window, and a short one still gets four seconds", () => {
    render(
      <ConfirmButton
        label="Let it act"
        confirmLabel="Let alpha act"
        sayAs="alpha acts on its own — 3 of its 4 proposal slots already in use, no approval"
        variant="approve"
        onConfirm={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));

    // Twenty-three words of announcement is about six and a half seconds at 180 wpm. The
    // window this replaces was four, so the control expired while it was still talking.
    act(() => {
      vi.advanceTimersByTime(4000);
    });
    expect(screen.getByRole("button", { name: "Let alpha act" })).toBeDefined();

    act(() => {
      vi.advanceTimersByTime(5200);
    });
    expect(screen.getByRole("button", { name: "Let it act" })).toBeDefined();
  });

  it("a sentence that changes under an armed control gets its own window", () => {
    const first = "alpha acts on its own — no approval";
    const second = "alpha acts on its own — 1 of its 4 proposal slots already in use";
    const { rerender } = render(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs={first} variant="approve" onConfirm={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));
    act(() => {
      vi.advanceTimersByTime(3000);
    });

    // A roster tick rewrites the consequence with a finger over the button.
    rerender(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs={second} variant="approve" onConfirm={vi.fn()} />,
    );
    expect(screen.getByRole("status").textContent).toBe(
      `armed: ${second} — press again to confirm`,
    );

    // The new sentence gets its own window rather than the remainder of the old one's. The
    // arithmetic matters: the FIRST announcement is fourteen words, so its own window ran to
    // 5600 ms and the 5000 ms this used to assert at was still inside it — it proved the
    // first window was long, not that a second one had opened. At 6000 ms the first window is
    // over and this control is still armed, which only the restart can explain.
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(screen.getByRole("button", { name: "Let alpha act" })).toBeDefined();
  });

  it("the window says it is closing before it closes", () => {
    const { onConfirm } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));

    // Eight words, so this one is still the floor: the allowance is for sentences that need
    // it, not a longer window for everything.
    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(screen.getByRole("status").textContent).toBe(
      "armed: Really delete — press again to confirm",
    );

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.getByRole("status").textContent).toBe("one second left");

    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByRole("status").textContent).toBe("disarmed — nothing changed");

    // And a confirm says neither: the action is about to happen and will speak for itself.
    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    fireEvent.click(screen.getByRole("button", { name: "Really delete" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  /**
   * The width is spent before it is needed.
   *
   * The armed label is longer than the rest label at 48 of the 53 sites, so arming used to
   * grow the button under the finger that had just pressed it — at the row this was measured
   * on, into most of the box that said "Reject" a moment earlier. Both labels are in the cell
   * from the first paint now, so the box is the wider of the two and arming moves nothing.
   */
  it("both labels are in the button, and only one of them is showing", () => {
    setup();

    const labels = () =>
      screen
        .getByRole("button", { name: /Delete series|Really delete/ })
        .querySelectorAll(".ui-confirm-stack > *");

    const atRest = labels();
    expect(atRest).toHaveLength(2);
    expect(atRest[0].textContent).toBe("Delete series");
    expect(atRest[0].getAttribute("aria-hidden")).toBeNull();
    expect(atRest[1].textContent).toBe("Really delete");
    expect(atRest[1].getAttribute("aria-hidden")).toBe("true");

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));

    // Neither label ever leaves the DOM — that IS the width. All that moves is which of the
    // two is hidden.
    const armed = labels();
    expect(armed).toHaveLength(2);
    expect(armed[0].textContent).toBe("Delete series");
    expect(armed[0].getAttribute("aria-hidden")).toBe("true");
    expect(armed[1].textContent).toBe("Really delete");
    expect(armed[1].getAttribute("aria-hidden")).toBeNull();
  });

  /**
   * Two labels for the eye, one for the ear.
   *
   * The hidden twin is a layout device, and a layout device that is read out loud is a second
   * label on a control that cannot be undone. `aria-hidden` is what takes it out of the name,
   * and it is the same attribute the stylesheet hides it by, so the two channels cannot drift.
   */
  it("the hidden label is hidden from the ear too", () => {
    setup();

    expect(screen.getByRole("button", { name: "Delete series" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Delete series Really delete" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));

    expect(screen.getByRole("button", { name: "Really delete" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Delete series Really delete" })).toBeNull();
  });

  /**
   * Escape is how a person says no.
   *
   * A control that is live for four seconds and can only be stood down by the clock is an
   * interlock that runs one way. Escape costs nothing to honour, and it says the same thing
   * the expiry says, because from the outside they are the same event: nothing happened.
   */
  it("Escape disarms an armed control and says so", () => {
    const { onConfirm, onArmedChange } = setup();

    fireEvent.click(screen.getByRole("button", { name: "Delete series" }));
    act(() => {
      vi.advanceTimersByTime(300);
    });

    fireEvent.keyDown(screen.getByRole("button", { name: "Really delete" }), { key: "Escape" });

    expect(screen.getByRole("button", { name: "Delete series" })).toBeDefined();
    expect(screen.getByRole("status").textContent).toBe("disarmed — nothing changed");
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onArmedChange).toHaveBeenLastCalledWith(false);
  });

  /**
   * And it says nothing when there is nothing to say.
   *
   * Escape on a control at rest is somebody dismissing something else — a sheet, a menu, a
   * search box. Announcing "disarmed" there would report an event that did not happen, in the
   * one region this component keeps for the two events that are otherwise silent.
   */
  it("Escape on a control at rest does nothing", () => {
    const { onConfirm, onArmedChange } = setup();

    fireEvent.keyDown(screen.getByRole("button", { name: "Delete series" }), { key: "Escape" });

    expect(screen.getByRole("button", { name: "Delete series" })).toBeDefined();
    expect(screen.getByRole("status").textContent).toBe("");
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onArmedChange).not.toHaveBeenCalled();
  });

  /**
   * The restart has a ceiling, and the announcement does not.
   *
   * `sayAs` is a prop, and the live one on this app is recomputed from a poll every three
   * seconds. Restarting the window on every rewrite — which is right for the first one —
   * handed an armed control a fresh window for ever on any project whose figures move. Two
   * windows: the first, and one restart.
   */
  it("a second rewrite re-announces but opens no third window", () => {
    const { rerender } = render(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs="one" variant="approve" onConfirm={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));

    // Window 1 closes at 4000. The first rewrite lands at 2000 and opens window 2, to 6000.
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    rerender(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs="two" variant="approve" onConfirm={vi.fn()} />,
    );

    // The second rewrite lands at 4000 and is SAID, and buys nothing: without the ceiling it
    // would have pushed the close out to 8000 and this control would still be live at 6000.
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    rerender(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs="three" variant="approve" onConfirm={vi.fn()} />,
    );
    expect(screen.getByRole("status").textContent).toBe("armed: three — press again to confirm");

    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(screen.getByRole("button", { name: "Let it act" })).toBeDefined();
    expect(screen.getByRole("status").textContent).toBe("disarmed — nothing changed");
  });

  /**
   * One warning per arming, whatever the sentence does in the middle.
   *
   * The first window's warning was scheduled for 3000 and the restart at 2000 must cancel it:
   * a "one second left" fired inside a window that has just restarted is a lie about the
   * window it is in, and the ear has no way to tell it from the true one that follows.
   */
  it("says `one second left` exactly once, even across a restart", () => {
    const { rerender } = render(
      <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs="one" variant="approve" onConfirm={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Let it act" }));

    const heard: string[] = [];
    for (let elapsed = 250; elapsed <= 6500; elapsed += 250) {
      act(() => {
        vi.advanceTimersByTime(250);
      });
      if (elapsed === 2000) {
        rerender(
          <ConfirmButton label="Let it act" confirmLabel="Let alpha act" sayAs="two" variant="approve" onConfirm={vi.fn()} />,
        );
      }
      heard.push(screen.getByRole("status").textContent ?? "");
    }

    // Transitions INTO the warning, not samples of it: the region holds its last sentence, so
    // the true warning is read by four consecutive samples and a count of samples would say
    // four whether the cancelled one fired or not.
    const warnings = heard.filter((said, i) => said === "one second left" && heard[i - 1] !== "one second left");
    expect(warnings).toHaveLength(1);
    expect(heard[heard.length - 1]).toBe("disarmed — nothing changed");
  });
});
