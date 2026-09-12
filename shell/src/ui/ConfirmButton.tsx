import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button, type ButtonIntent, type ButtonVariant } from "./Button";

/**
 * How long an armed control stays armed, at least.
 *
 * Long enough to read the second label and mean it; short enough that a control
 * you armed and walked away from is not still live when you come back. An
 * interlock that never expires is a single-click delete with extra steps.
 */
const ARM_WINDOW_MS = 4000;

/**
 * The time the window allows per word of what it has to say.
 *
 * 400 ms a word is 150 words a minute, which is slower than any screen reader's default rate —
 * so the window outlasts the speech at every rate anybody actually uses, and the slack grows
 * with the sentence instead of being a constant somebody has to guess right. It is an
 * allowance, not a claim about how fast anyone reads.
 *
 * The number this joins was 4 s flat, and it was set when the announcement was two words.
 * Three rounds later the roster's interlock says "armed: alpha acts on its own — 3 of its 4
 * proposal slots already in use, no approval — press again to confirm": twenty-three words,
 * about six and a half seconds at 180 wpm, inside a four-second window. The control expired
 * while it was still explaining itself, and then queued "disarmed — nothing changed" behind
 * the sentence it had interrupted.
 */
const MS_PER_WORD = 400;

/**
 * How long before the window closes the region says so.
 *
 * Exactly one second, because the sentence says "one second left" and a lead time that
 * drifted from it would make the announcement a lie. One warning per armed window, three
 * words long — it cannot still be speaking when "disarmed" lands a second later. The floor
 * is four seconds and this is one, so there is always a window to warn inside.
 */
const CLOSING_LEAD_MS = 1000;

/** The window this announcement needs: the floor, or a word at a time, whichever is longer. */
function armWindowFor(said: string): number {
  const words = said.trim().split(/\s+/).length;
  return Math.max(ARM_WINDOW_MS, words * MS_PER_WORD);
}

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

interface ConfirmButtonBase {
  /** What it says at rest. */
  label: ReactNode;
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
 * What it says once armed, and what it SAYS OUT LOUD once armed.
 *
 * Two channels, and they were the same string until it turned out they could not be. The
 * label is drawn in the button — short by construction, because inside `ModeSwitch` it is a
 * segment of a fixed track — while the live region is the only thing anyone not looking at
 * the screen gets. So the eye read "alpha acts on its own — 3 of its 4 proposal slots already
 * in use, no approval" and the ear got "armed: Let alpha act", which names the project and
 * not one consequence of letting it act.
 *
 * A union rather than an optional prop, because the degenerate case was silent: a
 * `confirmLabel` that is not a string cannot be interpolated, and the kill switch — an icon
 * and "Really release — work resumes" in a fragment — announced "armed — press again to
 * confirm", with no object at all, on the control that restarts every autonomous thing in the
 * app. With this, a non-string label without `sayAs` does not compile.
 *
 * Say what will happen, not "Confirm".
 *
 * `subject` is the third thing either channel can carry, and it is the row's own identifier
 * rather than a sentence: `#101`, a run id, an agent's name. The page this was written for
 * shows five byte-identical `git status` cards, and an interlock that says "Let this action
 * happen" on all five names the action and not the row — so the eye loses the identifier the
 * rest label was showing a click ago, and the ear never had it. It is on the string arm only,
 * and `never` on the other, because the identifier is composed into the label AS TEXT: a
 * label that is not text cannot have it appended, and a `.ui-*` span to hold it would put a
 * class in this file, which is not where classes live.
 */
type ConfirmSpeech =
  | { confirmLabel: string; sayAs?: string; subject?: string }
  | { confirmLabel: ReactNode; sayAs: string; subject?: never };

export type ConfirmButtonProps = ConfirmButtonBase & ConfirmSpeech;

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
  sayAs,
  subject,
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
  const closingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const disarmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // What a screen reader is told, and the only channel that carries it: the label swap is
  // silent to anyone not looking at the button, and the 4 s window used to expire without a
  // word. Prefixed rather than bare so it is not a second copy of the label on the page —
  // `getByText` matches on the whole string, and two exact matches would be ambiguous.
  //
  // `sayAs` first, because the consequence is what somebody needs before they press again;
  // the label is the fallback and is only ever the whole story where the two are the same
  // sentence. The objectless third branch is unreachable from TypeScript — the props union
  // requires `sayAs` for a non-string label — and is kept because unreachable is a claim
  // about the type checker, and this is the announcement on a control that cannot be undone.
  //
  // The subject comes FIRST in what is said and LAST in what is drawn, and the asymmetry is
  // the point. The ear needs to know which row before it is told what will happen to it,
  // because by the time the consequence is read the question is already "which one?". The eye
  // has the opposite problem: the rest label ended in the identifier ("Approve #101"), so
  // keeping it at the end holds the anchor still while the verb changes under it.
  //
  // The separator is `·` and not a dash: the label already contains an em dash at several
  // sites, and it is for the eye only — the announcement spells the same two facts with the
  // dashes it has always used, so nothing depends on a screen reader pronouncing a middle dot.
  const [said, setSaid] = useState("");
  const spoken = sayAs ?? (typeof confirmLabel === "string" ? confirmLabel : null);
  // The `typeof` guard is unreachable from TypeScript — the props union puts `subject` on the
  // string arm only — and is kept for the same reason the objectless branch below is:
  // unreachable is a claim about the type checker, and this is the label on a control that
  // cannot be undone.
  const armedLabel =
    subject === undefined || typeof confirmLabel !== "string"
      ? confirmLabel
      : `${confirmLabel} · ${subject}`;
  const armedSaid =
    spoken === null
      ? "armed — press again to confirm"
      : subject === undefined
        ? `armed: ${spoken} — press again to confirm`
        : `armed: ${subject} — ${spoken} — press again to confirm`;

  function clearTimers() {
    if (dwellTimer.current !== null) {
      clearTimeout(dwellTimer.current);
      dwellTimer.current = null;
    }
    if (closingTimer.current !== null) {
      clearTimeout(closingTimer.current);
      closingTimer.current = null;
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
      if (closingTimer.current !== null) clearTimeout(closingTimer.current);
      if (disarmTimer.current !== null) clearTimeout(disarmTimer.current);
    };
  }, []);

  function disarm() {
    clearTimers();
    setArmed(false);
    onArmedChange?.(false);
    setSaid("disarmed — nothing changed");
  }

  /*
    The window is a function of what has to be said, and it restarts when that changes.

    One effect and not two places: arming and re-announcing are the same event as far as the
    clock is concerned, and the flat 4 s this replaces was set in `handleClick`, where it could
    only ever be a constant. `sayAs` is a PROP, so it can be rewritten under an armed control —
    a roster tick can change the consequence while a finger is over the button — and
    re-announcing without restarting the clock would hand a new sentence the remainder of the
    old sentence's window, which is this whole defect in miniature.

    `armed` and `armedSaid` are the whole dependency list on purpose. `disarm` is redefined on
    every render, so listing it would restart the window on every render and the control would
    never expire; `onArmedChange` is a caller's prop and is an inline arrow at some sites, with
    the same result. The house carries this exemption in three other files.
  */
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!armed) return;
    setSaid(armedSaid);
    const span = armWindowFor(armedSaid);
    if (closingTimer.current !== null) clearTimeout(closingTimer.current);
    if (disarmTimer.current !== null) clearTimeout(disarmTimer.current);
    closingTimer.current = setTimeout(() => {
      closingTimer.current = null;
      // Said before it happens, not after. An expiry announced at the moment it lands tells
      // somebody the window they were inside is already gone; a second earlier it is still a
      // window. Three words, because the warning must not outlast what it warns about.
      setSaid("one second left");
    }, span - CLOSING_LEAD_MS);
    disarmTimer.current = setTimeout(disarm, span);
  }, [armed, armedSaid]);

  function handleClick() {
    if (!armed) {
      setArmed(true);
      onArmedChange?.(true);
      dwelling.current = true;
      dwellTimer.current = setTimeout(() => {
        dwellTimer.current = null;
        dwelling.current = false;
      }, DWELL_MS);
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
    // the two events that are otherwise silent.
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

        The interlock itself is untouched — arm, 300 ms dwell, a window of at least 4 s,
        `onArmedChange`.
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
        {armed ? armedLabel : label}
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
