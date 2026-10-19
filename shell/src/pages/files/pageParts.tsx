import { useState, type ReactNode } from "react";
import { DAEMON_NAME } from "../../copy";
import { formatBytes, joinPath, type Entry } from "../../data/files";
import { Button, Teach } from "../../ui";
import { type PathFailure, RefusalOrError } from "./shared";

/** How far the pointer travels with a row held before it is a drag and not a click. */
export const DRAG_THRESHOLD_PX = 6;

/** The label that follows a drag, set just below and right of the pointer. */
export function ghostTransform(x: number, y: number): string {
  return `translate(${String(x + 14)}px, ${String(y + 14)}px)`;
}

export function swallowClick(event: Event) {
  event.stopPropagation();
  event.preventDefault();
}

/** Why moving `entries` from `from` into `destination` cannot go, said before any request — or null when it can. */
export function moveRefusal(from: string, entries: Entry[], destination: string): string | null {
  if (destination === from) return entries.length === 1 ? "that is where it is now" : "that is where they are now";
  const intoItself = entries.some((entry) => {
    if (!entry.is_dir) return false;
    const inner = joinPath(from, entry.name);
    return destination === inner || destination.startsWith(`${inner}/`);
  });
  return intoItself ? "a folder cannot go inside itself" : null;
}

/** A row the keyboard should land on once the rows named in `gone` have left the listing. */
export interface FocusAfter {
  name: string;
  gone: string[];
}

/** A note with a way to put it away, the button outside whatever the note announces. */
export function Dismissable({ label, onDismiss, children }: { label: string; onDismiss: () => void; children: ReactNode }) {
  return (
    <div className="fi-dismissable">
      <div className="fi-dismissable-body">{children}</div>
      <Button variant="quiet" aria-label={label} onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}

/**
 * The refusals from one batch — deletes or restores — above the table. One is
 * said whole, beside its path. Several fold to a line that counts them: thirty
 * notes, each a box of its own, pushed the table off the screen, and the one
 * thing they share is that they did not go through.
 */
export function FailureNotes({
  failures,
  sentences,
  what,
  summary,
  labelFor,
  onDismiss,
  onDismissAll,
}: {
  failures: PathFailure[];
  sentences: Record<string, string>;
  what: string;
  summary: (n: number) => string;
  labelFor: (path: string) => string;
  onDismiss: (failure: PathFailure) => void;
  onDismissAll: () => void;
}) {
  const [first] = failures;
  if (first === undefined) return null;
  const one = (failure: PathFailure) => (
    <div className="fi-path-failure">
      <span className="fi-entry-name">{failure.path}</span>
      <RefusalOrError error={failure.error} sentences={sentences} what={what} />
    </div>
  );
  if (failures.length === 1) {
    return (
      <Dismissable label={labelFor(first.path)} onDismiss={() => onDismiss(first)}>
        {one(first)}
      </Dismissable>
    );
  }
  return (
    <Dismissable label="Dismiss these notes" onDismiss={onDismissAll}>
      <details>
        <summary className="fi-failures-summary">{summary(failures.length)}</summary>
        <ul className="fi-failures-list">
          {failures.map((failure) => (
            <li key={failure.path}>{one(failure)}</li>
          ))}
        </ul>
      </details>
    </Dismissable>
  );
}

export function pageHeadline(rootUnavailable: boolean, entries: Entry[] | undefined, failing: boolean): string | undefined {
  if (rootUnavailable) return "the files folder is unavailable";
  if (entries === undefined) return undefined;
  // The headline is the first thing read, so it may not state as current a
  // count the panel below then calls stale.
  const asOf = failing ? " — as last read" : "";
  if (entries.length === 0) return `this folder is empty${asOf}`;
  const dirs = entries.filter((entry) => entry.is_dir).length;
  const files = entries.length - dirs;
  const totalBytes = entries.reduce((sum, entry) => sum + entry.size_bytes, 0);
  const parts = [
    dirs > 0 ? `${dirs} folder${dirs === 1 ? "" : "s"}` : null,
    // "totalling" ties the bytes to the files: a folder is never measured, and
    // "2 folders, 1 file, 2.0 KB" read as the size of everything here.
    //
    // The value and its unit are joined by a no-break space so "3.4 MB" never
    // wraps between them at narrow widths (testing-library collapses it to a space).
    files > 0 ? `${files} file${files === 1 ? "" : "s"} totalling ${formatBytes(totalBytes).replace(" ", "\u00a0")}` : null,
  ].filter((part): part is string => part !== null);
  return parts.join(", ") + asOf;
}

/* --------------------------------------------------------- the no-root teach -- */

/**
 * The daemon answers 503 here when `files::ensure_root` failed at startup —
 * the folder is not unconfigured so much as unreachable, and the fix is on
 * the disk, not in a setting. The copy says what the folder is for and what
 * to check, and leaves the route count and the pillar vocabulary to the code.
 */
export function FilesUnavailableTeach() {
  const path = "%LOCALAPPDATA%\\nucleos\\NucleOS\\data\\files";
  const [copied, setCopied] = useState(false);
  return (
    <Teach title="The files folder could not be opened">
      <p>
        Check that <code>{path}</code> exists and can be written to, then restart NucleOS. Until then every
        action on this page is refused rather than pointed at somewhere else.
      </p>
      <p>
        <Button
          variant="quiet"
          onClick={() => {
            void navigator.clipboard?.writeText(path).then(
              () => setCopied(true),
              () => setCopied(false),
            );
          }}
        >
          {copied ? "Path copied" : "Copy path"}
        </Button>
      </p>
      <p>
        The {DAEMON_NAME} keeps one folder for everything this page holds — your uploads, drops from your file manager,
        filed mail and team workspaces. It could not create or open that folder when it started; its log names
        the error it hit.
      </p>
    </Teach>
  );
}
