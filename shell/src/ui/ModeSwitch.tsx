import { ConfirmButton } from "./ConfirmButton";

/** The three settings, spelled as this control needs them. Structurally `AutopilotMode`. */
export type SwitchMode = "off" | "shadow" | "active";

export interface ModeSwitchProps {
  value: SwitchMode;
  /** Whether the third segment has been earned — the daemon's arithmetic, never recomputed. */
  actAllowed: boolean;
  /** Why it has not, in the daemon's own terms. Rendered by the caller, carried here as a title. */
  actBlocker?: string;
  /** A write is in flight; every segment is inert. */
  busy?: boolean;
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
 * The third segment is the one that differs, twice over. It is a `ConfirmButton` while it is
 * still something you could do — letting a project act on its own is the setting here that is
 * hardest to take back — and a plain pressed segment once it IS the setting, because there is then
 * nothing left to confirm. What it never is, is green while locked: see `.ui-button-approve:disabled`
 * in `ui.css`. A control that cannot be pressed does not advertise the consequence of pressing it.
 */
export function ModeSwitch({ value, actAllowed, actBlocker, busy, onChoose }: ModeSwitchProps) {
  return (
    <div className="ui-switch" role="group" aria-label="Autopilot mode">
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={value === "off"}
        disabled={value === "off" || busy}
        onClick={() => onChoose("off")}
      >
        Turn off
      </button>
      <button
        type="button"
        className="ui-switch-seg"
        aria-pressed={value === "shadow"}
        disabled={value === "shadow" || busy}
        onClick={() => onChoose("shadow")}
      >
        Watch in shadow
      </button>
      {value === "active" ? (
        <button type="button" className="ui-switch-seg" aria-pressed disabled>
          Let it act
        </button>
      ) : (
        // Wrapped rather than classed: `ConfirmButton` takes no `className`, and it is not this
        // packet's file to change. The wrapper is the segment as far as the track is concerned.
        <span className="ui-switch-seg-wrap">
          <ConfirmButton
            label="Let it act"
            confirmLabel="It may act on its own"
            variant="approve"
            disabled={!actAllowed || busy}
            title={actAllowed ? undefined : actBlocker}
            onConfirm={() => onChoose("active")}
          />
        </span>
      )}
    </div>
  );
}
