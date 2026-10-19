import { useEffect, useId, useRef, useState } from "react";
import { joinPath, useMove, type Entry } from "../../data/files";
import { Button, Inset } from "../../ui";
import { MOVE_SENTENCES, type MoveDone, RefusalOrError } from "./shared";
import { FolderTree } from "./FolderTree";

/* ---------------------------------------------------------------- move -- */

/** How many names the move form's heading spells out before it says "and N more". */
const MOVE_NAMES_SHOWN = 3;

/**
 * Where the selection goes, picked from the tree rather than typed.
 *
 * This was one field holding a full destination path, for one entry at a time:
 * a person had to recall the folder's exact spelling, and a typo was a 404.
 * Now the destination is a folder chosen from the same lazy tree the side
 * column draws, the whole selection moves at once, and the line under the tree
 * says where each thing will end up before anything is sent. The folder the
 * entries are already in is not a destination — the Move button says so by
 * staying off, rather than letting the daemon answer a 409 for nothing.
 *
 * Dragging rows onto the side tree was the other half of the ask, and is not
 * here: the host takes every drag over the window for its own OS drop
 * (`drop.rs`), and on Windows a webview under a native drop handler never
 * sees an HTML drop. The picker is the path that works.
 */
export function MoveForm({
  path,
  entries,
  move,
  onMoved,
  onDone,
  onClose,
}: {
  /** The folder the entries are in — the one Move was opened from, not wherever the page is now. */
  path: string;
  entries: Entry[];
  move: ReturnType<typeof useMove>;
  /** Whatever went through, even when some did not: that much can be taken back. */
  onMoved: (moves: MoveDone[]) => void;
  onDone: () => void;
  onClose: () => void;
}) {
  const [destination, setDestination] = useState(path);
  // Whether a folder has been picked yet. The form opened on the folder the
  // items are already in, and so opened on a refusal — "that is where they are
  // now" — before a person had done anything to be refused.
  const [picked, setPicked] = useState(false);
  const [failures, setFailures] = useState<{ name: string; error: unknown }[]>([]);
  const [busy, setBusy] = useState(false);
  // Read by the second pick, which can come before the render `busy` would.
  const busyRef = useRef(false);
  const movedNames = useRef(new Set<string>());
  const headingId = useId();
  const formRef = useRef<HTMLFormElement>(null);
  useEffect(() => {
    formRef.current?.focus();
  }, []);

  const one = entries.length === 1 ? entries[0] : undefined;
  const same = destination === path;
  // A folder cannot go inside itself or anything beneath it; said before the
  // daemon has to refuse it.
  const intoItself = entries.some((entry) => {
    if (!entry.is_dir) return false;
    const from = joinPath(path, entry.name);
    return destination === from || destination.startsWith(`${from}/`);
  });
  const where = destination === "" ? "files" : `files/${destination}`;
  const here = path === "" ? "files/" : `files/${path}/`;
  // Named, not counted: "Move 3 items" left a person to remember which three.
  // The first few, and how many more, keeps a long selection to one line.
  const shownNames = entries.slice(0, MOVE_NAMES_SHOWN).map((entry) => `${entry.name}${entry.is_dir ? "/" : ""}`);
  const moreCount = entries.length - shownNames.length;

  async function submit() {
    if (same || intoItself || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    const failed: { name: string; error: unknown }[] = [];
    const done: MoveDone[] = [];
    // After a partial failure only what is still where it was is tried again:
    // the ones already moved would fail with "not found" on the second Move.
    for (const entry of entries.filter((item) => !movedNames.current.has(item.name))) {
      const from = joinPath(path, entry.name);
      const to = joinPath(destination, entry.name);
      try {
        await move.mutateAsync({ from, to });
        movedNames.current.add(entry.name);
        done.push({ from, to, is_dir: entry.is_dir });
      } catch (error) {
        failed.push({ name: entry.name, error });
      }
    }
    busyRef.current = false;
    setBusy(false);
    setFailures(failed);
    if (done.length > 0) onMoved(done);
    if (failed.length === 0) onDone();
  }

  return (
    // The `form` wraps the `Inset` for the reason the tree's `nav` does:
    // submit-on-Enter is behaviour, and `Inset` renders a `div` or an `li`.
    <form
      ref={formRef}
      tabIndex={-1}
      className="fi-move-form"
      aria-labelledby={headingId}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
    >
      <Inset className="fi-move">
        <p id={headingId} className="fi-move-head">
          Move <span className="fi-move-what">{shownNames.join(", ")}</span>
          {moreCount > 0 && ` and ${String(moreCount)} more`} to a folder:
        </p>
        {/* Nothing drawn as chosen until something is: the tree opened with the
            items' own folder marked, which read as an answer already given. That
            folder says "(here)" instead. */}
        <FolderTree
          currentPath={destination}
          marked={picked}
          herePath={path}
          onSelect={(next, byKey) => {
            // Clicking the picked folder again is the go-ahead, without the trip
            // down to the button. Enter only picks: two Enters in a row are how
            // a person walks and expands the tree, and must not move files.
            if (picked && next === destination && !byKey) {
              void submit();
              return;
            }
            setDestination(next);
            setPicked(true);
          }}
          label="Destination folder"
        />
        <p className="fi-move-target" role="status">
          {same && !picked ? (
            <>
              Pick the folder to move {one !== undefined ? "it" : "them"} into — {one !== undefined ? "it is" : "they are"} in{" "}
              <span className="fi-move-what">{here}</span> now.
            </>
          ) : same ? (
            `That is where ${one !== undefined ? "it is" : "they are"} now — pick another folder.`
          ) : intoItself ? (
            "A folder cannot go inside itself — pick one outside it."
          ) : (
            <>
              To <span className="fi-move-what">{where}/</span>
              {one !== undefined && (
                <>
                  , as <span className="fi-move-what">{joinPath(where, one.name)}</span>
                </>
              )}
              <span className="fi-move-again"> — click it again, or press Move</span>
            </>
          )}
        </p>
        {failures.map((failure) => (
          <div key={failure.name} className="fi-path-failure">
            <span className="fi-entry-name">{failure.name}</span>
            <RefusalOrError error={failure.error} sentences={MOVE_SENTENCES} what="that move did not go through" />
          </div>
        ))}
        <div className="fi-move-actions">
          <Button type="submit" disabled={same || intoItself || busy}>
            Move
          </Button>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
        </div>
      </Inset>
    </form>
  );
}
