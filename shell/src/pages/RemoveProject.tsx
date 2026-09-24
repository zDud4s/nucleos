import { useId, useState, type KeyboardEvent } from "react";

import { isApiRefusal } from "../data/client";
import { useProjectRecord, useRemoveProject } from "../data/projects";
import { heldBy, onRecord } from "../data/roster";
import { Button } from "../ui/Button";
import { ConfirmButton } from "../ui/ConfirmButton";
import { ErrorNote } from "../ui/ErrorNote";
import { RefusalNote } from "../ui/RefusalNote";

/**
 * Taking a project off the roster, asked for where the project is compared with the others.
 *
 * **Inline, and not a dialog.** `ConfirmButton` already wrote this argument down for the whole app:
 * a modal asking *are you sure?* trains people to click through it, and it takes the decision away
 * from the thing that caused it. The workflows guard is the same shape — a `role="group"` that
 * opens under the control, with the page still on screen behind it. This follows both, and the row
 * it belongs to stays visible while somebody reads it, which is the entire reason the decision is
 * being taken on a page that shows every project at once.
 *
 * **A plain remove is one press; remove-and-forget is armed and confirmed.** The plain remove is
 * reversible — the folder stays, the history stays, adding the project back finds both — so the
 * reading above the button IS its interlock: a folder path, what is on record, and a checkbox in
 * between. Ticking that box turns it into the one irreversible act on this page, and this panel used
 * to give it the weaker ceremony of the two: one click deleted 58 runs under a sentence that still
 * said "nothing on disk is touched". So with the box ticked the button becomes a `ConfirmButton`,
 * which is where the two-click arming earns its place, and the sentence stops promising what is no
 * longer true.
 */
export interface RemoveProjectProps {
  /** The panel's id, so the toggle that opened it can point at it with `aria-controls`. */
  id?: string;
  projectId: string;
  /** The recorded root, or `null` for a project nobody ever pointed anywhere. */
  projectRoot: string | null;
  /** Told what left and how, so the page can say so where the row was. */
  onDone: (removed: Removed) => void;
  onCancel: () => void;
}

/** A removal the núcleo accepted, as the page reports it back. */
export interface Removed {
  projectId: string;
  projectRoot: string | null;
  /** Whether its history went with it. */
  forgot: boolean;
}

export function RemoveProject({ id, projectId, projectRoot, onDone, onCancel }: RemoveProjectProps) {
  const record = useProjectRecord(projectId);
  const removal = useRemoveProject();
  const [forget, setForget] = useState(false);
  const consequenceId = useId();

  const held = record.data === undefined ? null : heldBy(record.data.holds);
  const has = record.data === undefined ? null : onRecord(record.data.forgets);
  /*
    What will actually be sent, not what the box last said. The box is only drawn over a record the
    núcleo returned with something in it; if a re-read of the record fails or comes back empty, the
    box goes and so must the forgetting — or the next press would delete a history nobody was shown.
  */
  const forgetting = forget && !record.isError && has !== null;

  const remove = () => {
    removal.mutate(
      { projectId, forgetHistory: forgetting },
      // Only on success: a refusal has to leave the panel open, or the sentence explaining it
      // would be drawn and unmounted in the same tick.
      { onSuccess: () => onDone({ projectId, projectRoot, forgot: forgetting }) },
    );
  };

  /*
    Escape is the key every person in this app already presses to mean "no". On the group rather
    than the document, so it only closes the panel somebody is inside. An armed `ConfirmButton` in
    here disarms on the same key and does not stop it — which is what it documents for a sheet:
    closing the panel unmounts the control, and both things happen.
  */
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    onCancel();
  };

  const disabled = removal.isPending || held !== null;

  return (
    <div
      id={id}
      role="group"
      aria-label={`Remove ${projectId} from NucleOS`}
      onKeyDown={onKeyDown}
      className="flex flex-col gap-3 rounded-md border border-border bg-surface-sunken p-3"
    >
      <p id={consequenceId} className="max-w-prose text-sm text-text">
        {/*
          The folder first, because it is the thing somebody is afraid of. A control called `remove`
          sitting on a page full of paths reads as "delete that" until it says otherwise, and the
          sentence that says otherwise has to arrive before the button does.

          And it changes with the box. "Nothing on disk is touched" stopped being true the moment
          the history was ticked — the runs live in the núcleo's own database, which is on a disk —
          and the reassurance must never sit above the one act it no longer describes.
        */}
        {projectRoot === null ? (
          <>This project has no folder named, so there is no folder for this to touch. </>
        ) : (
          <>
            The folder <code className="font-mono text-xs">{projectRoot}</code> stays exactly where
            it is.{" "}
          </>
        )}
        {forgetting ? (
          <>
            Its {has} will be deleted from the núcleo and cannot be brought back; adding it again
            starts with no history.
          </>
        ) : projectRoot === null ? (
          <>Adding it again brings it back.</>
        ) : (
          <>Nothing on disk is touched, and adding it again brings the project back.</>
        )}
      </p>

      {/*
        The record, and the checkbox over it. Never the checkbox alone: "forget the history too"
        with no number asks somebody to agree to lose an amount they cannot see. A project with
        nothing on record is offered no checkbox at all, because there is nothing to decide.
      */}
      {record.isError ? (
        <p className="text-xs text-text-muted">
          the núcleo did not say what {projectId} has on record — removing it still works, and the
          history is kept
        </p>
      ) : record.data === undefined ? (
        <p className="text-xs text-text-faint">reading what {projectId} has on record…</p>
      ) : has === null ? (
        <p className="text-xs text-text-muted">{projectId} has nothing on record.</p>
      ) : (
        <label className="flex max-w-prose items-baseline gap-2 text-sm text-text-muted">
          <input
            type="checkbox"
            checked={forget}
            onChange={(event) => setForget(event.target.checked)}
            className="mt-0.5"
          />
          <span>
            {projectId} has {has} on record. <span className="text-text">Forget those too</span> —
            otherwise they stay, and adding the project again finds them where they were.
          </span>
        </label>
      )}

      {/*
        Why the button is off, said before it is pressed. The daemon refuses this too and writes its
        own sentence — but in the past tense, about a removal that did not happen. Somebody should
        not have to press a button in order to be told they could not.
      */}
      {held !== null && (
        <p className="max-w-prose text-xs text-tone-paused-fg">
          Work is still in flight here — {held}. Removing waits until it finishes; nothing here would
          stop it.
        </p>
      )}

      {removal.isError &&
        (isApiRefusal(removal.error) ? (
          <RefusalNote
            refusal={removal.error}
            sentences={{
              // Two sharper than the shared floor, because only this route knows what its own
              // statuses mean here. `not_found` in particular: the generic "there is nothing there
              // to act on" reads as a bug, and the true answer — somebody else already removed it —
              // is a thing that resolves itself.
              in_flight: "work is still in flight here — nothing was removed",
              not_registered: `${projectId} is already off the roster`,
              not_found: `${projectId} is already off the roster`,
              history_referenced:
                "something outside this project still points at its history — untick the box and" +
                " remove it without forgetting, which is what happens by default",
            }}
          />
        ) : (
          // The app tried and could not: the error rung, next to the button that caused it. It was
          // faint 12px text — the one failure on this page that read quieter than a column label.
          <ErrorNote>the núcleo did not answer — nothing was removed</ErrorNote>
        ))}

      <div className="flex flex-wrap items-center gap-2">
        {forgetting ? (
          <ConfirmButton
            variant="danger"
            label={removal.isPending ? "removing…" : "remove and forget"}
            confirmLabel={`delete ${has} for good`}
            subject={projectId}
            describedBy={consequenceId}
            disabled={disabled}
            onConfirm={remove}
          />
        ) : (
          <Button variant="danger" disabled={disabled} onClick={remove}>
            {removal.isPending ? "removing…" : "remove"}
          </Button>
        )}
        <Button variant="quiet" onClick={onCancel}>
          cancel
        </Button>
      </div>
    </div>
  );
}
