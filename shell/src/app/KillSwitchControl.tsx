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
          <ConfirmButton
            label="Release kill switch"
            confirmLabel="Really release — work resumes"
            variant="danger"
            onConfirm={() => set.mutate(false)}
            disabled={set.isPending}
          />
        </>
      ) : (
        <Button
          variant="danger-solid"
          intent="stop"
          onClick={() => set.mutate(true)}
          disabled={set.isPending}
          title="Stop everything autonomous, now"
        >
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
