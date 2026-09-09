import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button, type ButtonIntent, type ButtonVariant } from "./Button";

/**
 * How long an armed control stays armed.
 *
 * Long enough to read the second label and mean it; short enough that a control
 * you armed and walked away from is not still live when you come back. An
 * interlock that never expires is a single-click delete with extra steps.
 */
const ARM_WINDOW_MS = 4000;

/**
 * The dead time immediately after arming.
 *
 * A double-click is one gesture, and without this a double-click on a delete
 * button both arms and confirms it — the interlock would defend against nothing
 * and would feel, to the person who lost the row, exactly like no interlock at
 * all. Clicks inside the dwell are *ignored*: they do not confirm, and they do
 * not disarm either, because disarming would punish the reflex and make the
 * control feel broken.
 */
const DWELL_MS = 300;

export interface ConfirmButtonProps {
  /** What it says at rest. */
  label: ReactNode;
  /** What it says once armed. Say what will happen, not "Confirm". */
  confirmLabel: ReactNode;
  onConfirm: () => void;
  /**
   * Told whenever the armed state changes.
   *
   * Not decoration: the approval queue freezes its sort order while any card is
   * armed, so that a list re-ordering under a poll tick cannot move a different
   * row under the finger that is about to confirm.
   */
  onArmedChange?: (armed: boolean) => void;
  /**
   * Which of the four this action is. Not optional, deliberately.
   *
   * `quiet` when it is reversible or recoverable — skip, requeue, archive,
   * close, release, put away, revert a note the ledger keeps. `ghost` when it
   * is neutral and not the point of the screen — cancel, refuse, reject, block,
   * read-only. `approve` when it is affirmative — approve, hire, merge, send,
   * allow. `danger` ONLY where data is destroyed or an irreversible end is made
   * — delete, revoke, forget, end the turns.
   *
   * `calendar/DaySheet.tsx` is the reference: "Skip this occurrence" is quiet
   * and "Delete whole series" is danger, on the same row. There is no default
   * because there was one: 33 of 53 sites inherited `danger` by saying nothing,
   * and once red also means "this control has an interlock" it stops meaning
   * danger anywhere.
   */
  variant: ButtonVariant;
  intent?: ButtonIntent;
  disabled?: boolean;
  title?: string;
  /**
   * An element that says what confirming would do, named for a screen reader.
   *
   * The interlock's own label is short by construction — inside `ModeSwitch` it is a segment
   * of a fixed track — so the consequence is the caller's to render and the caller's to point
   * at. Passed straight through as `aria-describedby`.
   */
  describedBy?: string;
}

/**
 * A two-click interlock for the actions that cannot be undone.
 *
 * Arm, then confirm. There is no dialog: a modal that asks "are you sure?"
 * trains people to click through it, and it moves the decision away from the
 * control that caused it. Here the button itself changes what it says, in
 * place, and goes back to what it was if you do nothing.
 */
export function ConfirmButton({
  label,
  confirmLabel,
  onConfirm,
  onArmedChange,
  variant,
  intent,
  disabled,
  title,
  describedBy,
}: ConfirmButtonProps) {
  const [armed, setArmed] = useState(false);
  // A ref rather than state: the dwell must not cause a render, and the click
  // handler has to see the *current* value rather than the one captured by the
  // render it belongs to.
  const dwelling = useRef(false);
  const dwellTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const disarmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // What a screen reader is told, and the only channel that carries it: the label swap is
  // silent to anyone not looking at the button, and the 4 s window used to expire without a
  // word. Prefixed rather than bare so it is not a second copy of the label on the page —
  // `getByText` matches on the whole string, and two exact matches would be ambiguous.
  const [said, setSaid] = useState("");
  const armedSaid =
    typeof confirmLabel === "string"
      ? `armed: ${confirmLabel} — press again to confirm`
      : "armed — press again to confirm";

  function clearTimers() {
    if (dwellTimer.current !== null) {
      clearTimeout(dwellTimer.current);
      dwellTimer.current = null;
    }
    if (disarmTimer.current !== null) {
      clearTimeout(disarmTimer.current);
      disarmTimer.current = null;
    }
    dwelling.current = false;
  }

  // An armed control that unmounts — the row it belonged to was approved
  // elsewhere, the page navigated — must not leave a timer that calls setState
  // on a component that is gone.
  useEffect(() => {
    return () => {
      if (dwellTimer.current !== null) clearTimeout(dwellTimer.current);
      if (disarmTimer.current !== null) clearTimeout(disarmTimer.current);
    };
  }, []);

  function disarm() {
    clearTimers();
    setArmed(false);
    onArmedChange?.(false);
    setSaid("disarmed — nothing changed");
  }

  function handleClick() {
    if (!armed) {
      setArmed(true);
      onArmedChange?.(true);
      setSaid(armedSaid);
      dwelling.current = true;
      dwellTimer.current = setTimeout(() => {
        dwellTimer.current = null;
        dwelling.current = false;
      }, DWELL_MS);
      disarmTimer.current = setTimeout(disarm, ARM_WINDOW_MS);
      return;
    }

    // Inside the dwell this click is the tail of a double-click, not a decision.
    // Swallow it and stay armed.
    if (dwelling.current) return;

    clearTimers();
    setArmed(false);
    onArmedChange?.(false);
    // A confirm is not an expiry: the action is about to happen, so there is nothing to
    // report about it not happening. Clearing rather than announcing keeps the region for
    // the one event that is otherwise silent.
    setSaid("");
    onConfirm();
  }

  return (
    <span className={armed ? "ui-confirm ui-confirm-armed" : "ui-confirm"}>
      {/*
        No `aria-pressed`. Armed is a label swap plus `.ui-confirm-armed`, not a toggle.

        This button used to carry `aria-pressed={armed}`, and inside `ModeSwitch`'s
        `role="group"` that is a lie at the worst possible moment: the two setting segments
        beside it use `aria-pressed` to mean "this IS the setting now", so an armed interlock
        announcing "pressed" tells a screen reader the project is acting — at the one moment
        nothing has happened yet and the whole point is that it still has to be confirmed.
        Everywhere else the attribute was equally wrong for the same reason: an interlock
        halfway through is not a state anything is in.

        The interlock itself is untouched — arm, 300 ms dwell, 4 s window, `onArmedChange`.
        What is armed is said by the label, which is where a caller can also read it.
      */}
      <Button
        variant={armed && variant === "danger" ? "danger-solid" : variant}
        intent={intent}
        disabled={disabled}
        title={title}
        aria-describedby={describedBy}
        onClick={handleClick}
      >
        {armed ? confirmLabel : label}
      </Button>
      {/*
        Said, not shown. `base.css`'s `.sr-only` rather than a `.ui-*` twin: `ui.css` already
        carries one copy of those ten lines (`.ui-field-said`, owned by `Field`) and a third
        would be the duplication this app keeps paying for. `role="status"` is the house idiom
        for a polite region — 58 sites use it — and it is absolutely positioned, so it cannot
        move the button it belongs to.
      */}
      <span className="sr-only" role="status">
        {said}
      </span>
    </span>
  );
}
