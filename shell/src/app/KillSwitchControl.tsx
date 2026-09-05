import { OctagonX, Play } from "lucide-react";
import { isApiRefusal } from "../data/client";
import { useKillSwitch, useSetKillSwitch } from "../data/system";
import { Button, ConfirmButton, ErrorNote, RefusalNote } from "../ui";

/**
 * The stop.
 *
 * Asymmetric on purpose, and the asymmetry is the design: **engaging is one
 * click** and **releasing takes two**. Panic is fast — someone reaching for
 * this has just watched an agent do something they did not expect, and an
 * interlock in front of *stopping* would be a UI arguing with a person in a
 * hurry. Releasing is the direction that puts the machine back in motion, so it
 * gets the arm-then-confirm interlock.
 *
 * It is also never hidden. Not while the state is unread, not while a write is
 * in flight, not while the daemon is having a bad minute. A control that
 * disappears exactly when things look wrong is worse than no control, because
 * the person who reached for it has now lost the seconds it took to find out it
 * was gone.
 *
 * Feedback is inline, under the button that caused it. There are no toasts in
 * this app: a message that appears in a corner is a message about nothing in
 * particular, and it leaves before anyone looks up.
 */
export function KillSwitchControl() {
  const kill = useKillSwitch();
  const set = useSetKillSwitch();
  const engaged = kill.data?.engaged;

  const failure = set.error;

  return (
    <div className="app-kill">
      {engaged === true ? (
        <>
          <p className="app-kill-state" role="status">
            kill switch engaged — nothing autonomous starts
          </p>
          {/*
            A drawn mark rather than `intent="go"`, and the swap is the whole of
            the fix: `intent` renders its direction as a text glyph — `▸` and `■`
            from `ui.css` — which is right in the middle of a page and wrong here,
            because at 56px the glyph IS the button and a Unicode square on a red
            slab reads as a font that failed to load. `label` takes a `ReactNode`,
            so the rail can hand its own mark in without the design system
            growing a variant for one control.

            The variant stays `danger`: releasing the stop is still the dangerous
            half, and the mark says which way it points, not how much it costs.
          */}
          <ConfirmButton
            label={
              <>
                <Play className="app-kill-icon" strokeWidth={1.5} aria-hidden="true" />
                Release kill switch
              </>
            }
            confirmLabel={
              <>
                <Play className="app-kill-icon" strokeWidth={1.5} aria-hidden="true" />
                Really release — work resumes
              </>
            }
            variant="danger"
            onConfirm={() => set.mutate(false)}
            disabled={set.isPending}
          />
        </>
      ) : (
        <Button
          variant="danger-solid"
          onClick={() => set.mutate(true)}
          disabled={set.isPending}
          title="Stop everything autonomous, now"
        >
          {/*
            The octagon is the one mark nobody has to be taught, and it is the
            same 16px lucide line the rest of the rail is drawn in — so collapsed,
            where the words are sized away, the stop still belongs to the column
            of marks above it instead of sitting in it as a foreign glyph.
          */}
          <OctagonX className="app-kill-icon" strokeWidth={1.5} aria-hidden="true" />
          Kill switch
        </Button>
      )}

      {/*
        The state is unread — the first poll has not landed, or the last one
        failed. Said out loud rather than guessed at, and the engage button
        stays exactly where it was: not knowing whether the machine is stopped
        is not a reason to make stopping it harder.
      */}
      {engaged === undefined ? <p className="app-kill-unread">state unread</p> : null}

      {failure === null ? null : isApiRefusal(failure) ? (
        <RefusalNote refusal={failure} />
      ) : (
        <ErrorNote>the kill switch could not be set — the daemon did not answer</ErrorNote>
      )}
    </div>
  );
}
