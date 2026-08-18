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
  variant?: ButtonVariant;
  intent?: ButtonIntent;
  disabled?: boolean;
  title?: string;
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
  variant = "danger",
  intent,
  disabled,
  title,
}: ConfirmButtonProps) {
  const [armed, setArmed] = useState(false);
  // A ref rather than state: the dwell must not cause a render, and the click
  // handler has to see the *current* value rather than the one captured by the
  // render it belongs to.
  const dwelling = useRef(false);
  const dwellTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const disarmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

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
  }

  function handleClick() {
    if (!armed) {
      setArmed(true);
      onArmedChange?.(true);
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
    onConfirm();
  }

  return (
    <span className={armed ? "ui-confirm ui-confirm-armed" : "ui-confirm"}>
      <Button
        variant={armed && variant === "danger" ? "danger-solid" : variant}
        intent={intent}
        disabled={disabled}
        title={title}
        aria-pressed={armed}
        onClick={handleClick}
      >
        {armed ? confirmLabel : label}
      </Button>
    </span>
  );
}
