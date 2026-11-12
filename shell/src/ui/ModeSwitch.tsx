import { useId, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "./Button";
import { ConfirmButton } from "./ConfirmButton";

/** The three settings, spelled as this control needs them. Structurally `AutopilotMode`. */
export type SwitchMode = "off" | "shadow" | "active";

export interface ModeSwitchProps {
  value: SwitchMode;
  /**
   * Whether the shadow proof for the third segment is enough — the daemon's arithmetic, never
   * recomputed. Never a lock: a proven offer is the hold interlock, an unproven one is a live
   * segment that warns first (`shortReason`) and lets the person go ahead anyway.
   */
  proven: boolean;
  /**
   * Why the proof is not enough, in the daemon's own terms (`promotionBlocker`, `lib/mode.ts`).
   * Said in the warning an unproven press opens; unused while `proven`.
   */
  shortReason: string;
  /**
   * What the armed segment says — short, one line, and it names the project
   * (`promotionConfirmLabel`, `lib/mode.ts`). Required, because a confirmation that does not name
   * what it is confirming is only a second click.
   *
   * It is a LABEL and not a sentence, and the difference is load-bearing: this string is drawn
   * inside a segment of a fixed track, so anything long enough to wrap grows the control while a
   * pointer is resting on it. The consequence sentence belongs under the control — see the
   * component's own docstring.
   */
  actArmedLabel: string;
  /**
   * The sentence the live region says when the third segment arms.
   *
   * Not the same string as `actArmedLabel` and that is the whole point: the label is drawn
   * inside a segment of a fixed track and can only be a few words, while this is what the
   * choice MEANS and is the only thing anyone not looking at the screen gets. The caller
   * already prints it under the control — this is the same sentence, said.
   */
  actConsequence: string;
  /**
   * Told when the third segment arms and disarms, straight from `ConfirmButton`.
   *
   * How a caller renders the consequence sentence at the right moment without this control
   * having to know what that sentence is or where it goes.
   */
  onArmedChange?: (armed: boolean) => void;
  /**
   * The consequence sentence the caller prints under the control, named for a screen reader.
   *
   * The armed label is one line inside a fixed track and says WHICH project; what letting it
   * act would MEAN is a sentence with room, and it lives at the other end of the row. Passed
   * ALWAYS, not only while armed: the caller renders the element at rest — hidden — and points
   * at it the whole time, because a description added at the moment of arming lands on a
   * button that already has focus and is not re-announced by any major screen reader.
   */
  actDescribedBy?: string;
  /** A write is in flight; every segment is inert. */
  busy?: boolean;
  /**
   * Inert segments keep their tab stop: `aria-disabled="true"` and a click that does nothing,
   * instead of native `disabled`.
   *
   * Inert means the pressed segment, all three while `busy`, the pressed "Let it act" once the
   * project is `active`. Native `disabled` drops focus to `<body>` the moment the segment you
   * just pressed becomes the setting, and it takes a busy segment out of the tab order together
   * with the `actDescribedBy` sentence that describes it. Off or absent, the rendered output is
   * exactly what it was before this existed.
   */
  focusableWhenInert?: boolean;
  onChoose: (mode: SwitchMode) => void;
}

/**
 * One control for a project's autonomy, used by every surface that sets it.
 *
 * Three positions of one thing, in verbs — `Turn off`, `Watch in shadow`, `Let it act`. Before
 * this there were two renderings of the same decision in two vocabularies (the roster said the
 * verbs, a project's own page said `off / shadow / active`), and a reader had to learn that they
 * were the same decision. `lib/mode.ts` still owns what the words MEAN and the promotion
 * arithmetic; this owns what the choice looks like.
 *
 * **`aria-pressed` on buttons inside a `role="group"`, and deliberately not a radio group.** A
 * radio group announces "3 of 3" and expects selection to be instant, and the third segment is a
 * two-step interlock whose label changes mid-interaction — a radio that renamed itself when you
 * focused it would be a worse lie than a pressed button. `aria-pressed` says exactly what is true:
 * this is the setting now.
 *
 * Which is why only the two SETTING segments carry it, and the interlock carries none. An armed
 * `ConfirmButton` used to report `aria-pressed="true"`, so a screen reader was told the project
 * was acting on its own at the exact moment it was not — the moment the interlock exists to hold
 * open. In this group the attribute has a meaning, and "halfway through arming" is not it. Armed
 * is the label swap plus `.ui-confirm-armed`; the setting is `aria-pressed`.
 *
 * **The consequence sentence is the CALLER's to render, under the control.** It used to be the
 * armed label, which put 52 characters into a segment of a fixed track: it wrapped, and the row
 * grew from 90.6 to 125.0 pixels — under a pointer that has four seconds left to press the same
 * button. So this takes a short `actArmedLabel` and hands the armed flag back through
 * `onArmedChange`; `Autopilot.tsx` prints the sentence on a full-width line under the row and
 * `project/Settings.tsx` as a paragraph under the switch. Nothing above the button moves.
 *
 * The caller renders that element AT REST, hidden (`.sr-only`, absolutely positioned, so it
 * takes no track and the row keeps its height), and swaps the class when armed. This control
 * does both halves of the sentence's other life: it SAYS it, through `actConsequence` on the
 * interlock's live region, and it POINTS at it, through `actDescribedBy`, from the first
 * render — a description that arrives with the arming arrives too late to be heard.
 *
 * The third segment is the one that differs, three ways. With enough shadow proof it is a
 * `ConfirmButton` — letting a project act on its own is the setting here that is hardest to take
 * back. Without it, it is a live plain segment (never green, never locked: the owner's rule is
 * that nothing blocks activating) whose press opens a warning naming what is missing, with
 * "Activate anyway" and "Cancel". And it is a plain pressed segment once it IS the setting,
 * because there is then nothing left to confirm.
 *
 * The warning is a disclosure of that segment (`aria-expanded`/`aria-controls`): pressing the
 * segment again, Cancel, or Escape folds it away and returns the focus to the segment, choosing
 * another segment closes it, and "Activate anyway" does nothing while a write is in flight.
 *
 * `focusableWhenInert` exists for one caller, the Autopilot carousel: it shows one switch on
 * screen at a time, so the focus on the segment somebody just pressed has to survive the change
 * that press made, rather than falling to `<body>` when that segment becomes the setting.
 */
export function ModeSwitch({
  value,
  proven,
  shortReason,
  actArmedLabel,
  actConsequence,
  onArmedChange,
  actDescribedBy,
  busy,
  focusableWhenInert,
  onChoose,
}: ModeSwitchProps) {
  const focusable = focusableWhenInert === true;
  // How an inert segment says so. Without the opt-in this is `disabled`, the exact expression
  // each segment carried before; with it the segment keeps its tab stop and says it with
  // `aria-disabled` instead, and `choose` below is what makes the press do nothing.
  function inertness(inert: boolean | undefined) {
    return focusable
      ? { "aria-disabled": inert === true ? ("true" as const) : undefined }
      : { disabled: inert };
  }
  const [warning, setWarning] = useState(false);
  // The warning is only ever drawn for an offer that is not proven and not already the setting.
  // When that stops being true the state is cleared too, not merely hidden, so a project that
  // becomes unproven again later does not reopen a warning nobody asked for.
  const eligible = !proven && value !== "active";
  if (warning && !eligible) setWarning(false);
  const warningOpen = warning && eligible;
  const warningId = useId();
  const consequenceId = useId();
  const offer = useRef<HTMLButtonElement>(null);
  function choose(mode: SwitchMode, inert: boolean | undefined) {
    if (inert === true) return;
    // Another segment is a different decision: the warning about the third one goes with it.
    setWarning(false);
    onChoose(mode);
  }
  // Closing the warning unmounts the button that was pressed, which would drop the focus to
  // `<body>`; it goes back to the segment that opened it instead.
  function dismiss() {
    offer.current?.focus();
    setWarning(false);
  }
  function activateAnyway() {
    // A write is already in flight: a second POST must not leave from here.
    if (busy === true) return;
    offer.current?.focus();
    setWarning(false);
    onChoose("active");
  }
  function onEscape(event: KeyboardEvent<HTMLElement>) {
    if (event.key !== "Escape" || !warningOpen) return;
    event.stopPropagation();
    dismiss();
  }
  const offInert = value === "off" || busy;
  const shadowInert = value === "shadow" || busy;
  const track = (
    <div className="ui-switch" role="group" aria-label="Autopilot mode" onKeyDown={onEscape}>
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={value === "off"}
        {...inertness(offInert)}
        onClick={() => choose("off", offInert)}
      >
        Turn off
      </button>
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={value === "shadow"}
        {...inertness(shadowInert)}
        onClick={() => choose("shadow", shadowInert)}
      >
        Watch in shadow
      </button>
      {value === "active" ? (
        <button type="button" className="ui-switch-seg" aria-pressed {...inertness(true)}>
          Let it act
        </button>
      ) : proven ? (
        // Wrapped rather than classed: `ConfirmButton` takes no `className`, and it is not this
        // packet's file to change. The wrapper is the segment as far as the track is concerned.
        <span className="ui-switch-seg-wrap">
          <ConfirmButton
            label="Let it act"
            confirmLabel={actArmedLabel}
            sayAs={actConsequence}
            variant="approve"
            disabled={focusable ? undefined : busy}
            unavailable={focusable ? busy === true : undefined}
            onArmedChange={onArmedChange}
            describedBy={actDescribedBy}
            onConfirm={() => onChoose("active")}
          />
        </span>
      ) : (
        // Not proven: a live segment, not green (nothing here is earned), whose press asks
        // "are you sure?" through the warning below instead of holding.
        <button
          ref={offer}
          type="button"
          className="ui-switch-seg"
          aria-describedby={actDescribedBy}
          aria-expanded={warningOpen}
          aria-controls={warningOpen ? warningId : undefined}
          {...inertness(busy)}
          onClick={() => {
            if (busy === true) return;
            // A second press folds the warning away again.
            setWarning((open) => !open);
          }}
        >
          Let it act
        </button>
      )}
    </div>
  );
  if (!eligible) return track;
  const reason = shortReason.trim();
  const said = `Not enough shadow evidence: ${reason}${/[.!?…]$/.test(reason) ? "" : "."}`;
  return (
    <>
      {track}
      {/* Mounted before it has anything to say, because a live region that arrives already full
          is not reliably announced. It says the warning once, when it opens. */}
      <span role="status" className="sr-only">
        {warningOpen ? `${said} ${actConsequence}` : ""}
      </span>
      {warningOpen ? (
        <div
          id={warningId}
          className="ui-switch-warning"
          role="group"
          aria-label="Not enough shadow evidence"
          onKeyDown={onEscape}
        >
          <p className="ui-switch-warning-text">{said}</p>
          <p className="ui-switch-warning-text" id={consequenceId}>
            {actConsequence}
          </p>
          <div className="ui-switch-warning-actions">
            <Button
              aria-describedby={consequenceId}
              aria-disabled={busy === true ? "true" : undefined}
              onClick={activateAnyway}
            >
              Activate anyway
            </Button>
            <Button onClick={dismiss}>Cancel</Button>
          </div>
        </div>
      ) : null}
    </>
  );
}
