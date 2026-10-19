import { useEffect, useId, useRef } from "react";
import { TRASH_RETENTION_DAYS, fileName, parentPath, trashDaysLeft, useTrash, type Trashed } from "../../data/files";
import { Button, Count, Quiet, RelativeTime, Row, Rows, StaleNote } from "../../ui";
import { RefusalOrError, TRASH_SENTENCES, type Undo } from "./shared";

/* -------------------------------------------------------------- delete -- */

/**
 * A folder that is not empty, said before it goes.
 *
 * The pending tone and not the danger one — nothing has gone wrong, and since
 * the trash nothing here is final either. The folders are named, because with
 * the selection bar stepped aside nothing else on screen says which ones they
 * are, and the note says where their contents go. One click: this used to arm
 * a second "Really delete" button, which was three steps for something the
 * undo beside it takes back in one — the named note is the pause, and the
 * trash is the safety. It takes focus when it opens — on Leave, the answer that
 * keeps everything — because it is the next thing to decide.
 */
export function NotEmptyNote({
  paths,
  busy,
  onConfirm,
  onCancel,
}: {
  paths: string[];
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const headingId = useId();
  const noteRef = useRef<HTMLDivElement>(null);
  // On the answer that keeps everything: the group's name is still read on the
  // way in, and an Enter pressed on arrival no longer deletes a whole folder.
  useEffect(() => {
    noteRef.current?.querySelector<HTMLElement>("[data-safe]")?.focus();
  }, []);
  const one = paths.length === 1;

  return (
    <div
      ref={noteRef}
      className="fi-ask"
      role="group"
      aria-labelledby={headingId}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <p id={headingId}>{one ? "This folder still has things inside:" : "These folders still have things inside:"}</p>
      <ul className="fi-ask-list">
        {paths.map((target) => (
          <li key={target}>{fileName(target)}/</li>
        ))}
      </ul>
      <p className="fi-ask-then">
        Deleting {one ? "it" : "them"} moves everything inside to Recently deleted as well, where it can be restored
        for {TRASH_RETENTION_DAYS} days.
      </p>
      <div className="fi-ask-actions">
        <Button variant="ghost" data-safe onClick={onCancel}>
          Keep {one ? "it" : "them"}
        </Button>
        <Button variant="danger" disabled={busy} onClick={onConfirm}>
          Delete with everything inside
        </Button>
      </div>
    </div>
  );
}

/** A path as the page writes a destination: from the root, with a trailing slash. */
function folderLabel(folder: string): string {
  return folder === "" ? "files/" : `files/${folder}/`;
}

/** The undo's sentence as plain words, for the button that stands in for the note. */
export function undoSummary(undo: Undo): string {
  if (undo.kind === "trash") {
    const [first] = undo.items;
    return undo.items.length === 1 && first !== undefined
      ? `moved ${fileName(first.path)}${first.is_dir ? "/" : ""} to Recently deleted`
      : `moved ${String(undo.items.length)} items to Recently deleted`;
  }
  const [first] = undo.moves;
  if (first === undefined) return "";
  if (undo.renamed) return `renamed ${fileName(first.from)} to ${fileName(first.to)}`;
  const into = folderLabel(parentPath(first.to));
  return undo.moves.length === 1
    ? `moved ${fileName(first.from)}${first.is_dir ? "/" : ""} to ${into}`
    : `moved ${String(undo.moves.length)} items to ${into}`;
}

/**
 * What a delete, a move or a rename just did, and the way back from it.
 *
 * Only the sentence is live; the buttons sit outside the region so a screen
 * reader hears what happened once rather than the controls read along with it.
 */
export function UndoNote({
  undo,
  busy,
  onUndo,
  onDismiss,
}: {
  undo: Undo;
  busy: boolean;
  onUndo: () => void;
  onDismiss: () => void;
}) {
  const trashed = undo.kind === "trash" ? undo.items : [];
  const [first] = trashed;
  const [moved] = undo.kind === "move" ? undo.moves : [];
  return (
    <div className="fi-undo">
      <p role="status">
        {undo.kind === "move" && moved !== undefined ? (
          undo.renamed ? (
            <>
              renamed <span className="fi-undo-name">{fileName(moved.from)}</span> to{" "}
              <span className="fi-undo-name">{fileName(moved.to)}</span>
            </>
          ) : undo.moves.length === 1 ? (
            <>
              moved <span className="fi-undo-name">{fileName(moved.from)}{moved.is_dir ? "/" : ""}</span> to{" "}
              <span className="fi-undo-name">{folderLabel(parentPath(moved.to))}</span>
            </>
          ) : (
            <>
              moved {undo.moves.length} items to{" "}
              <span className="fi-undo-name">{folderLabel(parentPath(moved.to))}</span>
            </>
          )
        ) : trashed.length === 1 && first !== undefined ? (
          <>
            moved <span className="fi-undo-name">{fileName(first.path)}{first.is_dir ? "/" : ""}</span> to Recently
            deleted
          </>
        ) : (
          <>moved {trashed.length} items to Recently deleted</>
        )}
      </p>
      <Button variant="ghost" disabled={busy} aria-keyshortcuts="Control+Z" onClick={onUndo}>
        Undo <UndoKeys />
      </Button>
      <Button variant="quiet" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}

/** The undo's verb, for a button with room for one word after "Undo". */
export function undoVerb(undo: Undo): string {
  if (undo.kind === "trash") return "delete";
  return undo.renamed ? "rename" : "move";
}

/**
 * Ctrl+Z, written on the button it stands for. The hint that named it retires
 * after the first open, and nothing else on screen said the key existed.
 * Hidden from the name: `aria-keyshortcuts` on the button says it properly.
 */
export function UndoKeys() {
  return (
    <kbd className="fi-undo-keys" aria-hidden="true">
      Ctrl+Z
    </kbd>
  );
}

/**
 * The trash, folded to one line under the tree until it is wanted.
 *
 * A disclosure rather than a panel of its own: it is the place a person goes
 * after the undo has gone, not something to read on every visit, and in normal
 * operation it should cost the page one line.
 */
export function RecentlyDeleted({
  trash,
  busy,
  onRestore,
}: {
  trash: ReturnType<typeof useTrash>;
  busy: boolean;
  onRestore: (item: Trashed) => void;
}) {
  const items = trash.data;
  return (
    <details className="fi-fold">
      <summary className="fi-fold-summary">
        Recently deleted <Count n={items?.length} />
      </summary>
      <p className="fi-trash-note">kept {TRASH_RETENTION_DAYS} days, then removed for good.</p>
      {trash.isError && items !== undefined && <StaleNote dataUpdatedAt={trash.dataUpdatedAt} />}
      {trash.isError && items === undefined && (
        <RefusalOrError error={trash.error} sentences={TRASH_SENTENCES} what="nothing is known about the trash" />
      )}
      {!trash.isError && items === undefined && <p className="fi-loading">reading the trash…</p>}
      {items !== undefined && items.length === 0 && <Quiet says="nothing deleted recently." />}
      {items !== undefined && items.length > 0 && (
        <Rows label="Recently deleted">
          {items.map((item) => (
            <Row key={item.id} layout="line" dense>
              {/* The daemon's own purge is the term; this only says how near it is. */}
              <span className="fi-trash-path">
                {item.path}
                {item.is_dir ? "/" : ""}
              </span>
              <span className="fi-trash-when">
                <RelativeTime at={item.deleted_at} /> · {daysLeft(item.deleted_at)}
              </span>
              <Button variant="ghost" disabled={busy} onClick={() => onRestore(item)} aria-label={`Restore ${item.path}`}>
                Restore
              </Button>
            </Row>
          ))}
        </Rows>
      )}
    </details>
  );
}

/** A trashed row's own term, so the one that goes tomorrow does not read like the one that just arrived. */
function daysLeft(deletedAt: string): string {
  const days = trashDaysLeft(deletedAt);
  return days <= 1 ? "goes within a day" : `${String(days)} days left`;
}
