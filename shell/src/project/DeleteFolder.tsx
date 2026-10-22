import { useEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";

import { isApiRefusal } from "../data/client";
import { useDeleteProjectFolder, useProjectFolder } from "../data/projects";
import { heldBy } from "../data/roster";
import { Button } from "../ui/Button";
import { Field } from "../ui/Field";
import { RefusalNote } from "../ui/RefusalNote";
import { DAEMON_NAME } from "../copy";

/**
 * Deleting a project's folder from the disk.
 *
 * **The only irreversible thing this app does, and everything about where it sits says so.** It is
 * not on the roster: that page compares projects, and a control that destroys one has no business
 * sitting in a column beside three that describe it. It is the panel the workspace header's
 * `More actions` menu opens in the shared `Modal` (through `LeavingDialog`, which owns the open
 * state and the focus), reached only by somebody who is already inside the project they mean.
 * `LeaveRoster`, the reversible exit, opens from the same menu for the same reason, and the two
 * are not one control with a checkbox because only one of them can be undone.
 *
 * **Typing the name is the interlock, and it is the only one.** `ConfirmButton`'s arm-then-confirm
 * is the app's interlock for actions whose two labels are all there is to read; this has a path, a
 * count of what exists nowhere else, and a field. Stacking both would be theatre — a person who has
 * typed `nucleos` into a box under a red heading has not done it by accident, and a third gesture
 * teaches them the gestures are the ritual rather than the thought.
 *
 * The reading comes first and the field second on purpose. A confirmation that asks before it
 * informs is a confirmation somebody completes and then reads.
 */
export interface DeleteFolderProps {
  projectId: string;
  /** Stands the panel down (cancel, Escape); the dialog around it owns the rest of the closing. */
  onClose: () => void;
  /** Told whether the request is in flight, so the dialog can refuse to close over it. */
  onBusyChange?: (busy: boolean) => void;
  /** The warning paragraph, handed to the dialog as the element focus lands on when it opens. */
  warningRef?: RefObject<HTMLParagraphElement | null>;
}

export function DeleteFolder({ projectId, onClose, onBusyChange, warningRef }: DeleteFolderProps) {
  const [typed, setTyped] = useState("");
  const [forget, setForget] = useState(false);
  // The panel only exists while its dialog is open, so the folder is read as soon as it mounts:
  // two git subprocesses and a `stat` are not something a page runs to draw a menu item nobody pressed.
  const folder = useProjectFolder(projectId, true);
  const deletion = useDeleteProjectFolder();

  // Local ref kept when the dialog passes none; both are set through one callback ref.
  const localWarning = useRef<HTMLParagraphElement | null>(null);
  const setWarning = (node: HTMLParagraphElement | null) => {
    localWarning.current = node;
    if (warningRef !== undefined) warningRef.current = node;
  };

  const pending = deletion.isPending;
  useEffect(() => {
    onBusyChange?.(pending);
  }, [pending, onBusyChange]);

  function close() {
    setTyped("");
    setForget(false);
    deletion.reset();
    onClose();
  }

  /*
    Escape stands the panel down, as it does every disclosure in this app — except while the
    request is in flight, when closing would hide the answer to something already sent.
  */
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Escape" || deletion.isPending) return;
    event.preventDefault();
    close();
  }

  const reading = folder.data;
  const held = reading === undefined ? null : heldBy(reading.holds);
  const named = reading?.root ?? projectId;
  const stopped = reading?.blocked ?? null;
  const gone = reading !== undefined && !reading.exists;
  const armed = typed === projectId && stopped === null && held === null && !gone;

  /*
    The panel is drawn inside `LeavingDialog`'s modal over a dimmed page, the owner's call: the one
    irreversible act in the app should take the page away until it is answered. The group keeps its
    name and its own Escape; the modal's Escape, scrim and close button are guarded by the dialog
    while the request is in flight.
  */
  return (
    <div
      role="group"
      aria-label={`Delete ${projectId}'s folder`}
      onKeyDown={onKeyDown}
      className="flex flex-col gap-4"
    >
      {/* `tabIndex={-1}`: a target for the focus the opening hands over, not a tab stop. No
          ring: inside the modal it was clipped by the body's edge, and the dialog itself
          already says where focus is — the reader hears this sentence first. Inline,
          because the ring in `base.css` is unlayered on purpose and beats any utility. */}
      <p
        ref={setWarning}
        tabIndex={-1}
        style={{ outline: "none" }}
        className="text-sm leading-relaxed text-text"
      >
        This deletes <code className="font-mono text-xs">{named}</code> and everything in it,
        permanently, and takes the project off the roster. Nothing here can undo it.
      </p>

      {folder.isError ? (
        <p className="text-xs text-text-faint">
          the {DAEMON_NAME} did not say what is in that folder: nothing has been deleted
        </p>
      ) : reading === undefined ? (
        <p className="text-xs text-text-faint">reading what is in that folder…</p>
      ) : (
        <Standing reading={reading} />
      )}

      {/*
        The same checkbox the remove panel offers, and the same default. Deleting the folder does
        not imply forgetting what the project did: the two are separate losses, and the owner's
        standing decision is that history stays unless somebody says otherwise.
      */}
      <label className="ws-dialog-option">
        <input
          type="checkbox"
          checked={forget}
          onChange={(event) => setForget(event.target.checked)}
        />
        <span className="flex flex-col gap-0.5">
          <span className="font-medium text-text">Forget its history too</span>
          <span>
            Its runs, proposals and stamps. Left unticked they stay in the {DAEMON_NAME}, which is
            what happens by default.
          </span>
        </span>
      </label>

      {deletion.isError &&
        (isApiRefusal(deletion.error) ? (
          <RefusalNote
            refusal={deletion.error}
            sentences={{
              kill_switch: "the kill switch is engaged: nothing was deleted",
              in_flight: "work is still in flight here: nothing was deleted",
              not_registered: `${projectId} is already off the roster, and its folder was not touched`,
              not_found: `${projectId} is already off the roster, and its folder was not touched`,
            }}
          />
        ) : (
          <p className="text-xs text-text-faint">
            the {DAEMON_NAME} gave no reply: nothing was deleted
          </p>
        ))}

      {/*
        The instruction is drawn here, in the ink the id needs, and the field's own label is
        hidden: `Field` sets its label in capitals, and the id has to be typed exactly.
      */}
      <div className="flex flex-col gap-1.5 text-sm text-text-muted">
        <span aria-hidden="true">
          Type <code className="font-mono text-xs text-text">{projectId}</code> to confirm
        </span>
        <Field label={`Type ${projectId} to confirm`} labelHidden>
          <input
            type="text"
            value={typed}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => setTyped(event.target.value)}
            className="font-mono!"
          />
        </Field>
      </div>

      {/* The modal's answer row: bottom edge, right-aligned, the act last. */}
      <div className="ws-dialog-actions">
        <Button variant="ghost" onClick={close}>
          cancel
        </Button>
        <Button
          variant="danger-solid"
          disabled={!armed || deletion.isPending}
          onClick={() => deletion.mutate({ projectId, forgetHistory: forget })}
        >
          {deletion.isPending ? "deleting…" : "delete the folder"}
        </Button>
      </div>
    </div>
  );
}

/**
 * What is in that folder and nowhere else, in the three answers the daemon can give.
 *
 * Each one is a different sentence because each is a different situation, and the middle one is the
 * one people most need: a folder git knows nothing about has no commits, no remote and nothing
 * anywhere else, so the reassurance the other branches can offer does not apply to it. Printing
 * `0 uncommitted` over that case would be reassuring somebody about the most dangerous one there is.
 */
function Standing({
  reading,
}: {
  reading: NonNullable<ReturnType<typeof useProjectFolder>["data"]>;
}) {
  if (reading.blocked !== null) {
    return <p className="max-w-prose text-sm text-tone-danger-fg">{reading.blocked.detail}</p>;
  }
  if (!reading.exists) {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        That folder is not on this disk any more, so there is nothing here to delete. Removing the
        project from the roster is the thing left to do.
      </p>
    );
  }

  const held = heldBy(reading.holds);
  const only = reading.only_here;

  return (
    <div className="flex flex-col gap-3 text-sm">
      {only === null ? (
        <p className="max-w-prose text-tone-paused-fg">
          That folder is not a git repository, so nothing in it exists anywhere else. Everything
          there is only there.
        </p>
      ) : (
        <p className="max-w-prose text-text-muted">
          {only.uncommitted === 0
            ? "Nothing in that folder is uncommitted."
            : `${only.uncommitted} ${only.uncommitted === 1 ? "file is" : "files are"} not committed.`}{" "}
          {only.unpushed === null
            ? "The repository has no remote, so every commit in it is only there."
            : only.unpushed === 0
              ? "Every commit is on a remote."
              : `${only.unpushed} ${only.unpushed === 1 ? "commit is" : "commits are"} on no remote.`}
        </p>
      )}
      {held !== null && (
        <p className="ws-dialog-note">
          Work is still in flight here: {held}. Nothing is deleted while it runs.
        </p>
      )}
    </div>
  );
}
