import { listen } from "@tauri-apps/api/event";
import { OctagonX, Play } from "lucide-react";
import { useEffect, useRef } from "react";
import { isApiRefusal } from "../data/client";
import { useKillSwitch, useSetKillSwitch } from "../data/system";
import { Button, ConfirmButton, ErrorNote, RefusalNote } from "../ui";

/** The tray's "Engage kill switch" item emits this; `src-tauri/src/lib.rs` names the same string. */
export const KILL_ENGAGE_EVENT = "kill://engage";

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
 *
 * It can also be reached without the window: Ctrl+Alt+K (Cmd+Alt+K on macOS) and the tray item
 * both engage, and neither ever releases.
 */
export function KillSwitchControl() {
  const kill = useKillSwitch();
  const set = useSetKillSwitch();
  const engaged = kill.data?.engaged;

  const failure = set.error;

  // The two ways in that are not a click. Held in a ref so the listeners subscribe once and still
  // see the current state; both engage only, and only when not already engaged.
  const engage = useRef(() => {});
  engage.current = () => {
    if (engaged === true || set.isPending) return;
    set.mutate(true);
  };

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.altKey && (event.ctrlKey || event.metaKey) && event.code === "KeyK") {
        event.preventDefault();
        engage.current();
      }
    };
    document.addEventListener("keydown", onKey);

    let stop: (() => void) | undefined;
    let gone = false;
    listen(KILL_ENGAGE_EVENT, () => engage.current())
      .then((off) => {
        if (gone) off();
        else stop = off;
      })
      // Swallowed: subscribing needs a Tauri runtime, and a browser or a test harness has none.
      // What is lost is the tray item; the button and the chord are unaffected.
      .catch(() => undefined);

    return () => {
      gone = true;
      document.removeEventListener("keydown", onKey);
      stop?.();
    };
  }, []);

  return (
    <div className="app-kill">
      {engaged === true ? (
        <>
          <p className="app-kill-state" role="status">
            kill switch engaged — nothing autonomous starts
          </p>
          {/* The engaged state is alarming, so its release control is the loud one. */}
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
            /* The label is a fragment, so there is nothing to interpolate: this control
               announced "armed — press again to confirm", with no object, on the button that
               restarts everything autonomous in the app. */
            sayAs="Really release — work resumes"
            variant="danger-solid"
            onConfirm={() => set.mutate(false)}
            disabled={set.isPending}
          />
        </>
      ) : (
        <Button
          variant="danger"
          onClick={() => set.mutate(true)}
          disabled={set.isPending}
          title="Stop everything autonomous, now (Ctrl+Alt+K, Cmd+Alt+K on macOS)"
        >
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
