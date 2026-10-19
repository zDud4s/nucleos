import { useLayoutEffect, useState } from "react";
import { DAEMON_NAME } from "../../copy";
import { isApiRefusal } from "../../data/client";
import { MAX_UPLOAD_BYTES, type Trashed } from "../../data/files";
import { ErrorNote, RefusalNote } from "../../ui";

/* ------------------------------------------------------------- refusals -- */

/** `GET /files` — a 409 here means the path named a file, not a folder. */
export const LIST_SENTENCES: Record<string, string> = {
  bad_request: `that path leaves the files root, which the ${DAEMON_NAME} will not follow`,
  not_found: "there is nothing at that path",
  conflict: "that path names a file, not a folder — there is nothing to list inside it",
  internal: `the ${DAEMON_NAME} could not read that folder from disk`,
};

export const SEARCH_SENTENCES: Record<string, string> = {
  bad_request: `that path leaves the files root, which the ${DAEMON_NAME} will not follow`,
  internal: `the ${DAEMON_NAME} could not search that folder`,
};

/** `POST /files/folder` — the only refusal it can give beyond 503 is 400. */
export const CREATE_FOLDER_SENTENCES: Record<string, string> = {
  bad_request:
    `that path leaves the files root, is unsafe, or names the root itself — none of which the ${DAEMON_NAME} will create`,
};

export const UPLOAD_SENTENCES: Record<string, string> = {
  bad_request: `that folder leaves the files root, which the ${DAEMON_NAME} will not follow`,
  conflict: "that folder is not actually a folder — there is a file there instead",
  http_413: `that file is larger than the ${String(MAX_UPLOAD_BYTES / (1024 * 1024))} MB the daemon will accept in one upload`,
  internal: `the ${DAEMON_NAME} could not write that file to disk`,
};

/** `POST /files/move` — the 409 a person meets most often on this page. */
export const MOVE_SENTENCES: Record<string, string> = {
  bad_request:
    "that move leaves the files root, is unsafe, targets the root itself, or moves a folder into itself",
  not_found: "the folder the destination would live in does not exist — a typo is never turned into a new folder",
  conflict: "there is already something there — a move never overwrites",
  internal: `the ${DAEMON_NAME} could not make that move on disk`,
};

export const DELETE_SENTENCES: Record<string, string> = {
  bad_request: "the files root itself cannot be deleted",
  conflict: "that folder has something in it — deleting it too means deleting everything inside",
  internal: `the ${DAEMON_NAME} could not move that to the trash, so it is still where it was`,
};

/** `POST /files/restore` — held to the move rule, so its 409 and 404 read like the move's. */
export const RESTORE_SENTENCES: Record<string, string> = {
  bad_request: "that is not something the trash holds",
  not_found: "the folder it came from is gone — a restore never makes one up; recreate it, then restore",
  conflict: "something else now sits where it came from — a restore never overwrites; move that aside first",
  internal: `the ${DAEMON_NAME} could not move it back out of the trash`,
};

/** `GET /files/trash` — the one refusal it can give beyond 503. */
export const TRASH_SENTENCES: Record<string, string> = {
  internal: `the ${DAEMON_NAME} could not read its trash folder`,
};

export const DOWNLOAD_SENTENCES: Record<string, string> = {
  conflict: "that path is a folder, not a file — there is nothing to download",
  not_found: "that file is gone",
};

export function RefusalOrError({
  error,
  sentences,
  what,
}: {
  error: unknown;
  sentences: Record<string, string>;
  what: string;
}) {
  // When it happened, fixed at the first render, and whatever the transport
  // said: "did not answer" alone gave a bug report nothing to go on.
  const [at] = useState(() => new Date());
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={sentences} />;
  const said = error instanceof Error && error.message.trim() !== "" ? ` (${error.message})` : "";
  const clock = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return (
    <ErrorNote>
      The {DAEMON_NAME} could not be reached at {clock}
      {said} — {what}
    </ErrorNote>
  );
}

/**
 * One line for one file that did not arrive, in the words the page uses for
 * the same refusal anywhere else: the upload's own sentence for a daemon
 * refusal, the host's message for a dropped file this window could not read.
 */
export function describeUploadFailure(error: unknown): string {
  if (typeof error === "string" && error.trim() !== "") return error;
  if (isApiRefusal(error)) {
    return UPLOAD_SENTENCES[error.code] ?? (error.detail.trim() !== "" ? error.detail : error.code);
  }
  if (error instanceof Error && error.message.trim() !== "") return error.message;
  return "could not be uploaded";
}

/** The daemon's ceiling in words, said beside the Upload button before anything is picked. */
export const MAX_UPLOAD_LABEL = `${String(MAX_UPLOAD_BYTES / (1024 * 1024))} MB`;

/**
 * One file waiting to go up, whichever door it came through. `read` defers the
 * bytes: a drop's are fetched from the host and a pick's from the browser, and
 * neither is read until the person has said what to do about a name clash.
 */
export interface UploadItem {
  folder: string;
  name: string;
  size: number;
  read: () => Promise<ArrayBuffer>;
}

/** A batch waiting on one answer: some of its names are already taken where it is going. */
export interface PendingBatch {
  source: "drop" | "picker";
  truncated: boolean;
  items: UploadItem[];
  /** Indexes into `items` whose name already names a file in the target folder. */
  clashes: number[];
  /** Refused before anything was asked — too large to send at all. */
  refusals: UploadRefusal[];
}

/** What to do with a file whose name is taken: the old one to the trash, a numbered copy, or not at all. */
export type ClashChoice = "replace" | "keep" | "skip";

export interface UploadSaved {
  sent: string;
  stored: string;
}

export interface UploadRefusal {
  name: string;
  reason: string;
}

/** What one batch of uploads came to — a drop from the desktop, or a pick from the Upload button. */
export interface UploadReport {
  source: "drop" | "picker";
  truncated: boolean;
  saved: UploadSaved[];
  refusals: UploadRefusal[];
  skipped: string[];
  /** How many older copies a Replace moved to Recently deleted. */
  replaced: number;
}

/** Where the upload loop is, said with the file it is on. */
export interface UploadProgress {
  done: number;
  total: number;
  name: string;
  size: number;
}

/** A delete or a restore that did not go through, kept beside the path it was for. */
export interface PathFailure {
  path: string;
  error: unknown;
}

/** One entry a move or a rename carried, with both ends, so it can be carried back. */
export interface MoveDone {
  from: string;
  to: string;
  is_dir: boolean;
}

/**
 * The last thing the page can take back. A delete is undone from the trash; a
 * move or a rename by the same move backwards — they had no undo at all, and a
 * move into the wrong folder is the likelier slip of the two.
 */
export type Undo = { kind: "trash"; items: Trashed[] } | { kind: "move"; moves: MoveDone[]; renamed: boolean };

/** Where the page remembers that the empty slot's hint has done its job. */
const HINT_KEY = "nucleos.files.hint-retired";

/**
 * Whether the hint under the toolbar is retired: it says how a file opens and
 * that a drop uploads, which is news once. Browser storage, so a private
 * window or cleared data only brings the hint back.
 */
export function readHintRetired(): boolean {
  try {
    return window.localStorage.getItem(HINT_KEY) === "1";
  } catch {
    return false;
  }
}

export function storeHintRetired() {
  try {
    window.localStorage.setItem(HINT_KEY, "1");
  } catch {
    // Not remembered: the hint comes back on the next visit, which is harmless.
  }
}

/**
 * Below this width of the page's own body the tree folds away. The body, not
 * the window: the question was always whether the table has room beside a
 * 14rem tree, and a viewport query could not see the app's rail. With the
 * rail open at 1000px the table got about 450px and cut every name to nine
 * characters, while the tree beside it showed the same names whole. The table
 * has since come out of its card, which gave it back the card's padding, and
 * at 52 a 1000px window folded the tree while the name column stood a third
 * empty: navigation went down to the breadcrumb for room nothing used.
 */
const NARROW_REM = 46;

/** How far past the line a folded body must grow before the tree comes back. */
const NARROW_SLACK_REM = 1.5;

export function remPx(): number {
  return Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
}

/**
 * Whether the body is too narrow for the tree beside the table. The slack is
 * there because folding changes the page's height, and with it the scrollbar,
 * and so the width being measured: without it a body right on the line folds
 * and unfolds on its own. jsdom has no `ResizeObserver` and no layout to fold,
 * so there it counts as wide.
 */
export function useNarrow(body: HTMLElement | null): boolean {
  const [narrow, setNarrow] = useState(false);
  useLayoutEffect(() => {
    if (body === null || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry === undefined) return;
      const width = entry.contentRect.width;
      const rem = remPx();
      setNarrow((was) => width < (was ? NARROW_REM + NARROW_SLACK_REM : NARROW_REM) * rem);
    });
    observer.observe(body);
    return () => observer.disconnect();
  }, [body]);
  return narrow;
}
