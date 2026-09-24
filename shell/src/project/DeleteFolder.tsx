import { useEffect, useRef, useState, type KeyboardEvent } from "react";

import { isApiRefusal } from "../data/client";
import { useDeleteProjectFolder, useProjectFolder } from "../data/projects";
import { heldBy } from "../data/roster";
import { Button } from "../ui/Button";
import { RefusalNote } from "../ui/RefusalNote";

/**
 * Deleting a project's folder from the disk.
 *
 * **The only irreversible thing this app does, and everything about where it sits says so.** It is
 * not on the roster: that page compares projects, and a control that destroys one has no business
 * sitting in a column beside three that describe it. It is at the foot of the project's own State
 * page, reached only by somebody who is already inside the project they mean — which is the same
 * argument the roster's `remove` makes in reverse, and the reason the two are not one control with
 * a checkbox.
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
}

export function DeleteFolder({ projectId }: DeleteFolderProps) {
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [forget, setForget] = useState(false);
  // Fetched only once the control is open: two git subprocesses and a `stat` are not something a
  // page runs to draw a button nobody pressed.
  const folder = useProjectFolder(projectId, open);
  const deletion = useDeleteProjectFolder();

  /*
    Where focus goes when the control opens and when it closes.

    Both gestures unmount the element that had focus: "delete this folder…" is replaced by the
    panel, and "cancel" takes the panel away. Left alone, focus fell to `<body>` in the middle of
    the one irreversible thing this app does, and a keyboard user had to find their place again
    from the top of the page. So opening hands focus to the warning — the reading comes first, and
    it is the first thing a screen reader should say — and closing hands it back to the button that
    opened it, which is the standard the project switcher already keeps.

    `settle` says which of the two just happened. Neither fires on first paint: a page that
    stole focus into its last section on arrival would be the opposite bug.
  */
  const warning = useRef<HTMLParagraphElement>(null);
  const trigger = useRef<HTMLParagraphElement>(null);
  const [settle, setSettle] = useState<"opened" | "closed" | null>(null);

  useEffect(() => {
    if (settle === "opened") warning.current?.focus();
    if (settle === "closed") trigger.current?.querySelector("button")?.focus();
    if (settle !== null) setSettle(null);
  }, [settle]);

  function show() {
    setOpen(true);
    setSettle("opened");
  }

  function close() {
    setOpen(false);
    setTyped("");
    setForget(false);
    deletion.reset();
    setSettle("closed");
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

  if (!open) {
    return (
      <p ref={trigger} className="flex flex-wrap items-baseline gap-3 text-sm text-text-muted">
        <span>
          Removing this project from NucleOS leaves its folder alone. Deleting the folder is a
          separate thing, and it is permanent.
        </span>
        <Button variant="quiet" onClick={show}>
          delete this folder…
        </Button>
      </p>
    );
  }

  const reading = folder.data;
  const held = reading === undefined ? null : heldBy(reading.holds);
  const named = reading?.root ?? projectId;
  const stopped = reading?.blocked ?? null;
  const gone = reading !== undefined && !reading.exists;
  const armed = typed === projectId && stopped === null && held === null && !gone;

  return (
    <div
      role="group"
      aria-label={`Delete ${projectId}'s folder`}
      onKeyDown={onKeyDown}
      className="flex flex-col gap-3 rounded-md border border-tone-danger-border bg-surface-sunken p-3"
    >
      {/* `tabIndex={-1}`: a target for the focus the opening hands over, not a tab stop. The
          global ring's 2px offset sat flush on the glyphs of a block of prose, so this one
          stands further off — same colour and width, only the offset, as the house overrides do.
          Inline, because the ring in `base.css` is unlayered on purpose and beats any utility;
          inline still wins over it, and changes nothing but the distance. */}
      <p
        ref={warning}
        tabIndex={-1}
        style={{ outlineOffset: "4px" }}
        className="max-w-prose text-sm text-text"
      >
        This deletes <code className="font-mono text-xs">{named}</code> and everything in it,
        permanently, and takes the project off the roster. Nothing here can undo it.
      </p>

      {folder.isError ? (
        <p className="text-xs text-text-faint">
          the núcleo did not say what is in that folder — nothing has been deleted
        </p>
      ) : reading === undefined ? (
        <p className="text-xs text-text-faint">reading what is in that folder…</p>
      ) : (
        <Standing reading={reading} />
      )}

      {/*
        The same checkbox the roster's remove control offers, and the same default. Deleting the
        folder does not imply forgetting what the project did: the two are separate losses, and the
        owner's standing decision is that history stays unless somebody says otherwise.
      */}
      <label className="flex max-w-prose items-baseline gap-2 text-sm text-text-muted">
        <input
          type="checkbox"
          checked={forget}
          onChange={(event) => setForget(event.target.checked)}
        />
        <span>
          Forget what this project did as well — its runs, proposals and stamps. Left unticked they
          stay in the núcleo, which is what happens by default.
        </span>
      </label>

      {deletion.isError &&
        (isApiRefusal(deletion.error) ? (
          <RefusalNote
            refusal={deletion.error}
            sentences={{
              kill_switch: "the kill switch is engaged — nothing was deleted",
              in_flight: "work is still in flight here — nothing was deleted",
              not_registered: `${projectId} is already off the roster, and its folder was not touched`,
              not_found: `${projectId} is already off the roster, and its folder was not touched`,
            }}
          />
        ) : (
          <p className="text-xs text-text-faint">the núcleo did not answer — nothing was deleted</p>
        ))}

      <label className="flex flex-wrap items-baseline gap-2 text-sm text-text-muted">
        <span>
          Type <code className="font-mono text-xs text-text">{projectId}</code> to confirm:
        </span>
        <input
          type="text"
          value={typed}
          spellCheck={false}
          autoComplete="off"
          aria-label={`Type ${projectId} to confirm`}
          onChange={(event) => setTyped(event.target.value)}
          className="rounded-md border border-border bg-surface px-2 py-1 font-mono text-xs text-text"
        />
      </label>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="danger-solid"
          disabled={!armed || deletion.isPending}
          onClick={() => deletion.mutate({ projectId, forgetHistory: forget })}
        >
          {deletion.isPending ? "deleting…" : "delete the folder"}
        </Button>
        <Button variant="quiet" onClick={close}>
          cancel
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
function Standing({ reading }: { reading: NonNullable<ReturnType<typeof useProjectFolder>["data"]> }) {
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
    <div className="flex flex-col gap-1 text-sm">
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
        <p className="max-w-prose text-tone-paused-fg">
          Work is still in flight here — {held}. Nothing is deleted while it runs.
        </p>
      )}
    </div>
  );
}
