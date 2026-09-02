import { useState } from "react";

import { isApiRefusal } from "../data/client";
import { useProjectRecord, useRemoveProject } from "../data/projects";
import { heldBy, onRecord } from "../data/roster";
import { Button } from "../ui/Button";
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
 * **And not a `ConfirmButton` either**, which is the app's other interlock. That one is arm-then-
 * confirm on a control whose two labels are all there is to read; this has a folder path, a count of
 * what would be forgotten and a checkbox in between. The reading IS the interlock here. `Delete the
 * folder` — the irreversible act, inside the project rather than on this page — is where the
 * two-click arming earns its place.
 */
export interface RemoveProjectProps {
  projectId: string;
  /** The recorded root, or `null` for a project nobody ever pointed anywhere. */
  projectRoot: string | null;
  onDone: () => void;
  onCancel: () => void;
}

export function RemoveProject({ projectId, projectRoot, onDone, onCancel }: RemoveProjectProps) {
  const record = useProjectRecord(projectId);
  const removal = useRemoveProject();
  const [forget, setForget] = useState(false);

  const held = record.data === undefined ? null : heldBy(record.data.holds);
  const has = record.data === undefined ? null : onRecord(record.data.forgets);

  return (
    <div
      role="group"
      aria-label={`Remove ${projectId} from NucleOS`}
      className="flex flex-col gap-3 rounded-md border border-border bg-surface-sunken p-3"
    >
      <p className="max-w-prose text-sm text-text">
        {/*
          The folder first, because it is the thing somebody is afraid of. A control called `remove`
          sitting on a page full of paths reads as "delete that" until it says otherwise, and the
          sentence that says otherwise has to arrive before the button does.
        */}
        {projectRoot === null ? (
          <>
            This project has no folder named, so there is nothing on a disk for this to touch. Adding
            it again brings it back.
          </>
        ) : (
          <>
            The folder <code className="font-mono text-xs">{projectRoot}</code> stays exactly where
            it is. Nothing on disk is touched, and adding it again brings the project back.
          </>
        )}
      </p>

      {/*
        The record, and the checkbox over it. Never the checkbox alone: "forget the history too"
        with no number asks somebody to agree to lose an amount they cannot see. A project with
        nothing on record is offered no checkbox at all, because there is nothing to decide.
      */}
      {record.isError ? (
        <p className="text-xs text-text-faint">
          the núcleo did not say what {projectId} has on record — removing it still works, and the
          history is kept
        </p>
      ) : record.data === undefined ? (
        <p className="text-xs text-text-faint">reading what {projectId} has on record…</p>
      ) : has === null ? (
        <p className="text-xs text-text-faint">{projectId} has nothing on record.</p>
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
          <p className="text-xs text-text-faint">the núcleo did not answer — nothing was removed</p>
        ))}

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="danger"
          disabled={removal.isPending || held !== null}
          onClick={() => {
            removal.mutate(
              { projectId, forgetHistory: forget },
              // Only on success: a refusal has to leave the panel open, or the sentence explaining
              // it would be drawn and unmounted in the same tick.
              { onSuccess: onDone },
            );
          }}
        >
          {removal.isPending ? "removing…" : forget ? "remove and forget" : "remove"}
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          cancel
        </Button>
      </div>
    </div>
  );
}
