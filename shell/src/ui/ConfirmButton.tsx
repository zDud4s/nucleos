import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from "react";
import { Button, type ButtonIntent, type ButtonVariant } from "./Button";

/**
 * How long a press has to be held before it counts, in milliseconds.
 *
 * The interlock is a press-and-hold: the pointer or the key goes down, a fill sweeps across the
 * button, and only when it completes does `onConfirm` run. Letting go early is a "no" and fires
 * nothing. A second is long enough that a reflex click or a double-click cannot complete it, and
 * short enough that a person who means it is not made to wait.
 */
const HOLD_MS = 1000;

/**
 * The longer hold for the actions that stop something or destroy data.
 *
 * `intent="stop"` and the two danger variants. The component already distinguishes them (a
 * danger button swaps to `danger-solid` when armed), so the distinction costs nothing here: half
 * a second more is the price of an irreversible action, and it is paid only by those.
 */
const HOLD_DESTRUCTIVE_MS = 1500;

/**
 * How long a pointer or key event vouches for the `click` that follows it, in milliseconds.
 *
 * The browser dispatches a press's `click` in the same task as the release that causes it, so
 * this only has to outlast that. It is a window and not a flag on purpose: a flag waits for its
 * click to clear it, and a press released OUTSIDE the button (no click ever follows) would leave
 * it set and swallow a later screen-reader activation.
 */
const GUARD_MS = 150;

/**
 * How long an armed control stays armed, at least — the screen-reader FALLBACK path only.
 *
 * Since the control became a press-and-hold, the pointer and the keyboard no longer arm
 * anything and nothing times out on them: a hold is its own clock. The two-step arm below
 * survives for one reason, a click that never had a hold in front of it (see `handleClick`).
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
 * while it was still explaining itself, and then queued "disarmed: nothing changed" behind
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

/**
 * How many windows one arming may open: the first, and one restart.
 *
 * The window restarts when the sentence changes, and that much is right — handing a new
 * sentence the remainder of an old sentence's window is the defect this whole clock exists to
 * avoid. What was missing is the bound. `sayAs` is a prop, and the one live one on this app is
 * a roster consequence recomputed from a poll every three seconds: on a project whose proposal
 * count moves, an armed "Let alpha act" was handed a fresh ten-second window every tick, for
 * ever. An interlock that never expires is a single-click delete with extra steps, which is the
 * sentence at the top of this file.
 *
 * Two and not "cap the total": a cap on total armed time lets the last rewrite inherit whatever
 * is left of the old window, which is the thing the paragraph above refuses. Two and not "only
 * when the word count changes": two different sentences of the same length would restart each
 * other for ever, so that is not a bound at all.
 *
 * A rewrite past the second is still SAID — the ear must hear the sentence that is true now —
 * it just does not buy more time.
 */
const MAX_ARM_WINDOWS = 2;

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
   * Told whenever the engaged state changes: `true` while a press is being held (or, on the
   * screen-reader fallback, while the control is armed), `false` on release, completion or expiry.
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
  /**
   * Cannot be pressed now, and still reachable: `aria-disabled="true"` instead of `disabled`.
   *
   * The control stays in the tab order and keeps its `describedBy`, so the sentence that says
   * WHY it cannot be pressed is attached to something a screen reader can land on. It never
   * arms and never confirms, and if it turns true while armed the control disarms and says so
   * — a permission taken away under a finger is the same event as an expiry.
   *
   * Native `disabled` keeps its meaning and is still the right answer almost everywhere. This
   * is for the case it gets wrong: a control that has just been used, or whose refusal is the
   * point, where `disabled` drops focus to `<body>` and silences the description with it.
   */
  unavailable?: boolean;
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

/** What is holding the control down: a pointer (by id, so a second finger is not the first) or a key. */
type HoldSource = { kind: "pointer"; id: number } | { kind: "key" };

/** The keys that press a button. Space and Enter are what a native button answers to. */
function isActivationKey(key: string): boolean {
  return key === "Enter" || key === " " || key === "Spacebar";
}

/**
 * A press-and-hold interlock for the actions that cannot be undone.
 *
 * Press and keep pressing: a fill sweeps across the button, and when it completes `onConfirm`
 * runs. Letting go before that, or sliding off the button, cancels with nothing fired. There
 * is no dialog: a modal that asks "are you sure?" trains people to click through it, and it
 * moves the decision away from the control that caused it. Here the cost of the decision is
 * time, and it is paid on the control itself, in the place the finger already is.
 *
 * This replaced a two-click arm-then-confirm. The public props are unchanged on purpose, so
 * none of the call sites had to learn about it; `confirmLabel` is now what the button says WHILE
 * it is held, and `onArmedChange` is told `true` for as long as the hold lasts.
 *
 * Three ways in, one rule each:
 * - Pointer: `pointerdown` starts the hold with pointer capture; `pointerup`, `pointercancel`,
 *   or sliding off the button before the fill completes cancel it.
 * - Keyboard: holding Space or Enter is the same hold. Auto-repeat is ignored (it is the same
 *   press, not a new one), and releasing the key, Escape, or losing focus cancels.
 * - Assistive technology: see `handleClick`. A screen reader's synthetic activation has no hold
 *   in it, and gets the old two-step instead of a dead control.
 *
 * The resting label is `label` alone, and not a label built from `confirmLabel` ("Hold to ...").
 * The `confirmLabel` strings are written to be read AFTER arming ("Really release — work
 * resumes", "Let this action happen"), so putting "Hold to" in front of them is either
 * ungrammatical or an essay at rest. A visible "hold" hint was tried and dropped at the owner's
 * call (2026-10-09): the fill that starts on press is the instruction. The accessible name
 * stays `label`, and `aria-roledescription="hold button"` tells a screen reader how to operate it
 * (a description id would have changed `aria-describedby`, which callers own and tests read).
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
  unavailable,
  title,
  describedBy,
}: ConfirmButtonProps) {
  // The two-step state below belongs to the screen-reader fallback ONLY.
  const [armed, setArmed] = useState(false);
  // The hold. State for the render (the fill, the label), and the refs beside it for the timers
  // and handlers, which have to see what is true now and not what the render that made them saw.
  const [holding, setHolding] = useState(false);
  const holdSource = useRef<HoldSource | null>(null);
  const holdTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // When the last pointer / key event on this button landed. The browser dispatches the `click`
  // that follows a press in the same task as the release, so "within `GUARD_MS`" means "this
  // click is the tail of a press I have already handled" and anything older is not. A flag
  // that waited for its click to clear it would be left behind by a press released elsewhere.
  const lastPointerAt = useRef(Number.NEGATIVE_INFINITY);
  const lastKeyAt = useRef(Number.NEGATIVE_INFINITY);
  // A ref rather than state: the dwell must not cause a render, and the click
  // handler has to see the *current* value rather than the one captured by the
  // render it belongs to.
  const dwelling = useRef(false);
  const dwellTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const closingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const disarmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // How many windows this arming has already opened. A ref and not state: it is read and
  // written inside the effect that schedules the clock, and a render is exactly what it must
  // not cause.
  const windows = useRef(0);
  // What `onArmedChange` was last told, so the pair stays a pair however the two paths
  // interleave: a hold that starts over an open fallback arming is one continuous "armed", not
  // `true, true, false`, which would leave the queue's freeze counter stuck at one.
  const reported = useRef(false);
  // The latest callbacks, for the code that outlives a render: the hold timer and the unmount
  // cleanup. The timer is set in one render and fires in a later one, and `onConfirm` is an
  // inline arrow at most call sites; confirming through the stale one would act on the
  // selection of a second ago.
  const latest = useRef({ onConfirm, onArmedChange });
  useEffect(() => {
    latest.current = { onConfirm, onArmedChange };
  });

  const holdMs =
    intent === "stop" || variant === "danger" || variant === "danger-solid"
      ? HOLD_DESTRUCTIVE_MS
      : HOLD_MS;

  // What a screen reader is told, and the only channel that carries it: the label swap is
  // silent to anyone not looking at the button. Prefixed rather than bare so it is not a second
  // copy of the label on the page; `getByText` matches on the whole string, and two exact
  // matches would be ambiguous.
  //
  // `sayAs` first, because the consequence is what somebody needs before they commit; the label
  // is the fallback and is only ever the whole story where the two are the same sentence. The
  // objectless branch is unreachable from TypeScript (the props union requires `sayAs` for a
  // non-string label) and is kept because unreachable is a claim about the type checker, and
  // this is the announcement on a control that cannot be undone.
  //
  // The subject comes FIRST in what is said and LAST in what is drawn, and the asymmetry is
  // the point. The ear needs to know which row before it is told what will happen to it,
  // because by the time the consequence is read the question is already "which one?". The eye
  // has the opposite problem: the rest label ended in the identifier ("Approve #101"), so
  // keeping it at the end holds the anchor still while the verb changes under it.
  //
  // The separator is `·` and not a dash: the label already contains an em dash at several
  // sites, and it is for the eye only. The announcement spells the same two facts with the
  // dashes it has always used, so nothing depends on a screen reader pronouncing a middle dot.
  const [said, setSaid] = useState("");
  const spoken = sayAs ?? (typeof confirmLabel === "string" ? confirmLabel : null);
  // The `typeof` guard is unreachable from TypeScript (the props union puts `subject` on the
  // string arm only) and is kept for the same reason the objectless branch is.
  const armedLabel =
    subject === undefined || typeof confirmLabel !== "string"
      ? confirmLabel
      : `${confirmLabel} · ${subject}`;
  const armedSaid =
    spoken === null
      ? "armed: press again to confirm"
      : subject === undefined
        ? `armed: ${spoken}. Press again to confirm`
        : `armed: ${subject}. ${spoken}. Press again to confirm`;
  // The hold's own announcement. It says "hold" so that somebody who pressed and heard nothing
  // else knows the gesture is a duration, and it names the consequence for the same reason the
  // armed one does.
  const holdSaid =
    spoken === null
      ? "hold to confirm"
      : subject === undefined
        ? `hold to confirm: ${spoken}`
        : `hold to confirm: ${subject}. ${spoken}`;

  function report(next: boolean) {
    if (reported.current === next) return;
    reported.current = next;
    latest.current.onArmedChange?.(next);
  }

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

  // A control that unmounts mid-hold or armed (the row it belonged to was approved elsewhere,
  // the page navigated) must not leave a timer that calls setState on a component that is
  // gone, and above all must not let the hold timer COMPLETE and confirm something nobody can
  // see any more.
  //
  // And it has to say it disarmed on the way out. `onArmedChange` is a PAIR: the queue
  // holds its sort order for as long as the count is above zero, so a control that
  // vanishes engaged leaves that count stuck at one for the life of the page, and the list
  // never unfreezes again. Waiting's batch interlock is exactly that shape: it unmounts the
  // moment the selection empties under it, which `Clear selection` and unticking the last box
  // both do while it is engaged.
  useEffect(() => {
    return () => {
      if (dwellTimer.current !== null) clearTimeout(dwellTimer.current);
      if (closingTimer.current !== null) clearTimeout(closingTimer.current);
      if (disarmTimer.current !== null) clearTimeout(disarmTimer.current);
      if (holdTimer.current !== null) clearTimeout(holdTimer.current);
      if (reported.current) latest.current.onArmedChange?.(false);
    };
  }, []);

  function disarm() {
    clearTimers();
    setArmed(false);
    report(false);
    setSaid("disarmed: nothing changed");
  }

  /*
    The fallback's window is a function of what has to be said, and it restarts when that changes.

    This clock belongs to the screen-reader fallback only; a hold has no timeout beyond the hold.

    One effect and not two places: arming and re-announcing are the same event as far as the
    clock is concerned. `sayAs` is a PROP, so it can be rewritten under an armed control, since
    a roster tick can change the consequence while a finger is over the button, and
    re-announcing without restarting the clock would hand a new sentence the remainder of the
    old sentence's window.

    `armed` and `armedSaid` are the whole dependency list on purpose. `disarm` is redefined on
    every render, so listing it would restart the window on every render and the control would
    never expire; `onArmedChange` is a caller's prop and is an inline arrow at some sites, with
    the same result. The house carries this exemption in three other files.

    The restart has a ceiling of `MAX_ARM_WINDOWS`, and a rewrite past it is still announced and
    simply buys no more time; a consequence fed by a three-second poll would otherwise keep an
    armed control live for ever. The counter resets the moment `armed` goes false, which is one
    place for three events: a confirm, an expiry and an Escape all come back through here.
  */
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!armed) {
      windows.current = 0;
      return;
    }
    setSaid(armedSaid);
    if (windows.current >= MAX_ARM_WINDOWS) return;
    windows.current += 1;
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

  /**
   * Ends a hold that did not complete: nothing is fired, and it says so.
   *
   * Idempotent, because three events can land for one release (`pointerup`, then
   * `lostpointercapture`, then a `blur`) and only the first has anything to cancel.
   */
  function cancelHold() {
    if (holdSource.current === null) return;
    holdSource.current = null;
    if (holdTimer.current !== null) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
    setHolding(false);
    report(false);
    setSaid("released: nothing changed");
  }

  /** The fill completed with the press still down: this is the decision. */
  function completeHold() {
    holdTimer.current = null;
    holdSource.current = null;
    setHolding(false);
    report(false);
    // Completion is announced, which the old confirm path (a silent clearing) did not do: the
    // fill is a visual event, and somebody on a screen reader needs to hear that it landed.
    setSaid("confirmed");
    latest.current.onConfirm();
  }

  function startHold(source: HoldSource) {
    // Unavailable swallows every press: it never starts a hold, and so can never confirm.
    if (unavailable === true || disabled === true || holdSource.current !== null) return;
    // A fallback arming still open is superseded, not stacked: the hold is the better evidence
    // of intent. `reported` stays true across the swap, so the pair is not broken.
    if (armed) {
      clearTimers();
      setArmed(false);
    }
    holdSource.current = source;
    setHolding(true);
    report(true);
    setSaid(holdSaid);
    holdTimer.current = setTimeout(completeHold, holdMs);
  }

  /*
    Becoming unavailable or disabled while engaged stands the control down, said out loud.

    A poll can take the permission away with a finger over the button. Staying engaged would keep
    a live confirm on something that can no longer be done, and a hold timer that completes on a
    disabled button would fire an action the page had just refused. So it goes through the same
    exits as everything else, and the pair's closing half reaches `onArmedChange`.

    `[unavailable, disabled]` alone, for the reason the effect above gives: the handlers are
    redefined on every render, and listing them would re-run this on renders that changed nothing.
  */
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (unavailable !== true && disabled !== true) return;
    if (armed) disarm();
    cancelHold();
  }, [unavailable, disabled]);

  function fresh(at: number): boolean {
    return Date.now() - at < GUARD_MS;
  }

  function handlePointerDown(event: PointerEvent<HTMLButtonElement>) {
    lastPointerAt.current = Date.now();
    // Primary button only; a right-click or a second finger is not a decision.
    if (event.button !== 0 || event.isPrimary === false) return;
    // Capture, so the release reaches this button even when the pointer has left it, and the
    // browser does not hand the gesture to something else mid-hold. Optional-called because the
    // test DOM does not implement it, and a missing capture only costs the slide-off cancel
    // below, which `onPointerLeave` then covers.
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // A pointer that is already gone cannot be captured; the hold simply cancels on release.
    }
    startHold({ kind: "pointer", id: event.pointerId });
  }

  function ownsPointer(event: PointerEvent<HTMLButtonElement>): boolean {
    const source = holdSource.current;
    return source !== null && source.kind === "pointer" && source.id === event.pointerId;
  }

  function handlePointerEnd(event: PointerEvent<HTMLButtonElement>) {
    lastPointerAt.current = Date.now();
    if (ownsPointer(event)) cancelHold();
  }

  /*
    Sliding off the button cancels, and capture is why this has to be measured by hand.

    A captured pointer keeps its events on the button wherever it goes, and the browser then
    reports no `pointerleave` at all, so the cancel the gesture promises ("slide off to call it
    off") would silently not happen and the hold would complete under a finger that is no longer
    on the control. `onPointerLeave` still covers the case where capture was refused.
  */
  function handlePointerMove(event: PointerEvent<HTMLButtonElement>) {
    lastPointerAt.current = Date.now();
    if (!ownsPointer(event)) return;
    const box = event.currentTarget.getBoundingClientRect();
    // A zero-size box is a layout-less environment (the test DOM), not a pointer that left.
    if (box.width === 0 && box.height === 0) return;
    const outside =
      event.clientX < box.left ||
      event.clientX > box.right ||
      event.clientY < box.top ||
      event.clientY > box.bottom;
    if (outside) cancelHold();
  }

  /*
    The one entry for anything that is not a pointer or a key.

    A screen reader's "activate" does not press and hold: it dispatches a bare `click`, where
    `detail` is 0 or no pointer or key event came before it. If the hold were the only way in,
    that would be a dead control for exactly the people who cannot see the fill, and a button
    that does nothing when activated is worse than one with a weaker interlock. So this path
    keeps the old two-step, for that click alone: the first activation arms and says
    `confirmLabel` (and the live region reads the consequence), the second confirms, and the old
    window disarms it if nobody comes back.

    A click that IS the tail of a hold is recognised and ignored: a pointer one by `detail > 0`
    with a pointer event just before it, a keyboard one by a key event just before it. That
    covers the click the browser dispatches after every release, whether the hold completed
    (already confirmed) or was cancelled (nothing to do).
  */
  function handleClick(event: { detail: number }) {
    if (fresh(lastKeyAt.current)) return;
    if (event.detail > 0 && fresh(lastPointerAt.current)) return;
    // Unavailable swallows the activation: it does not arm, and it cannot confirm either, because
    // it can never be armed; the effect above stands it down the moment it turns unavailable.
    if (unavailable === true) return;
    if (!armed) {
      setArmed(true);
      report(true);
      dwelling.current = true;
      dwellTimer.current = setTimeout(() => {
        dwellTimer.current = null;
        dwelling.current = false;
      }, DWELL_MS);
      return;
    }

    // Inside the dwell this click is the tail of a double-activation, not a decision.
    // Swallow it and stay armed.
    if (dwelling.current) return;

    clearTimers();
    setArmed(false);
    report(false);
    // The fallback keeps the old silent clearing: a confirm is not an expiry, the action is about
    // to happen, and the region is kept for the events that are otherwise silent. (The hold's
    // completion IS announced, in `completeHold`: a sweep is a visual event with no click to
    // hear.)
    setSaid("");
    latest.current.onConfirm();
  }

  /*
    Holding Space or Enter is the same hold as the pointer's.

    `preventDefault` on every keydown of those two, repeats included: the native button would
    otherwise click on Enter's keydown (and on every auto-repeat of it) and on Space's keyup, and
    each of those clicks is exactly the single-press confirm this control no longer has. The
    repeat itself is ignored, since a held key is one press and not a stream of new ones.

    Escape calls it off, and so does leaving. It is on the button and not on the wrapper: the
    wrapper is not focusable, so a keydown only reaches it when focus is already here. Not on
    the document either; an Escape aimed at a dialog would stand down every control behind it.

    The event is not stopped. Inside a sheet, Escape should still close the sheet; closing it
    unmounts this control, which stands it down anyway.

    Escape also disarms the fallback arming, as it always did.
  */
  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "Escape") {
      if (armed) disarm();
      cancelHold();
      return;
    }
    if (!isActivationKey(event.key) || event.ctrlKey || event.metaKey || event.altKey) return;
    lastKeyAt.current = Date.now();
    event.preventDefault();
    if (event.repeat) return;
    startHold({ kind: "key" });
  }

  function handleKeyUp(event: KeyboardEvent<HTMLButtonElement>) {
    if (!isActivationKey(event.key)) return;
    lastKeyAt.current = Date.now();
    event.preventDefault();
    if (holdSource.current?.kind === "key") cancelHold();
  }

  /*
    Blur cancels a KEY hold, and only that.

    The keyup that would end it goes to whatever has focus by then, so a hold that outlived its
    focus could never be released and would complete on its own. This is not the blur-disarms
    rule the fallback refuses: that one took the control away from somebody reading the
    consequence, and a hold has no reading to protect; it is a gesture in progress.
  */
  function handleBlur() {
    if (holdSource.current?.kind === "key") cancelHold();
  }

  const engaged = armed || holding;
  const classes = ["ui-confirm"];
  if (armed) classes.push("ui-confirm-armed");
  if (holding) classes.push("ui-confirm-holding");

  return (
    <span
      className={classes.join(" ")}
      style={{ "--ui-confirm-hold": `${holdMs}ms` } as CSSProperties}
    >
      {/*
        No `aria-pressed`. Holding and armed are a fill and a label swap, not a toggle.

        This button used to carry `aria-pressed={armed}`, and inside `ModeSwitch`'s
        `role="group"` that is a lie at the worst possible moment: the two setting segments
        beside it use `aria-pressed` to mean "this IS the setting now", so an engaged interlock
        announcing "pressed" tells a screen reader the project is acting, at the one moment
        nothing has happened yet and the whole point is that it still has to be confirmed.
        What is happening is said by the label and the live region, which is where a caller can
        also read it.
      */}
      <Button
        variant={armed && variant === "danger" ? "danger-solid" : variant}
        intent={intent}
        disabled={disabled}
        aria-disabled={unavailable === true ? "true" : undefined}
        title={title}
        aria-describedby={describedBy}
        aria-roledescription="hold button"
        onClick={handleClick}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onBlur={handleBlur}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
        onPointerLeave={handlePointerEnd}
        onLostPointerCapture={handlePointerEnd}
        onContextMenu={(event) => {
          // A long press on touch opens the context menu, which would cancel the hold it is
          // part of.
          if (holdSource.current !== null) event.preventDefault();
        }}
      >
        {/*
          The labels, always, stacked in one cell, and only one of them showing.

          The held label says what will happen instead of what the control is, so it is longer
          at 48 of 53 sites: `Approve #101` becomes `Let this action happen · #101` and the button
          grew about 198 device px under the finger that had just pressed it, taking most of the
          box that said `Reject #101` a moment earlier, so a click aimed at Reject landed on a
          live confirm. Nothing in this app reserved that width; `ModeSwitch` only survives it by
          keeping its label short by contract and clipping what does not fit.

          So the width is spent once, at first paint, on the widest of them, and the hold becomes
          a swap inside a box that does not move. `visibility: hidden` rather than `display: none`
          because the hidden one still has to occupy the cell (that IS the mechanism), and
          `aria-hidden` beside it because a label nobody can see must not be read out either: the
          pair is what keeps every `getByRole("button", { name })` in the suite naming the label
          that is showing.

          The third cell, "Keep holding…", is only ever seen under `prefers-reduced-motion`, where
          the fill does not sweep and the label has to carry the "this is in progress" that the
          sweep otherwise would. It is always `aria-hidden`: the live region already says it.
        */}
        <span className="ui-confirm-stack">
          <span className="ui-confirm-rest" aria-hidden={engaged ? "true" : undefined}>
            {label}
          </span>
          <span aria-hidden={engaged ? undefined : "true"}>{armedLabel}</span>
          <span className="ui-confirm-keep" aria-hidden="true">
            Keep holding…
          </span>
        </span>
      </Button>
      {/*
        Said, not shown. `base.css`'s `.sr-only` rather than a `.ui-*` twin: `ui.css` already
        carries one copy of those ten lines (`.ui-field-said`, owned by `Field`) and a third
        would be the duplication this app keeps paying for. `role="status"` is the house idiom
        for a polite region (58 sites use it), and it is absolutely positioned, so it cannot
        move the button it belongs to.
      */}
      <span className="sr-only" role="status">
        {said}
      </span>
    </span>
  );
}
