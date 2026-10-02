import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import { File as FileIcon, Folder as FolderIcon } from "lucide-react";
import { isApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import {
  MAX_UPLOAD_BYTES,
  TRASH_RETENTION_DAYS,
  diskPath,
  downloadFile,
  dropTargetFolder,
  fileName,
  formatBytes,
  joinPath,
  middleEllipsis,
  openFile,
  parentPath,
  pathSegments,
  pathUpTo,
  readFolder,
  runsOnOpen,
  sortEntries,
  trashDaysLeft,
  useCreateFolder,
  useDelete,
  useFileSearch,
  useFolder,
  useMove,
  useRestore,
  useTrash,
  useUpload,
  type DeleteRequest,
  type Dropped,
  type Entry,
  type Hit,
  type SortColumn,
  type SortDirection,
  type Trashed,
} from "../data/files";
import {
  Button,
  Count,
  ErrorNote,
  Field,
  Inset,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  Teach,
} from "../ui";
import "./files.css";

/**
 * Files — the one managed root, browsed, searched, made, filled, moved and
 * removed from this window, plus whatever a person drags in from Windows.
 *
 * **Everything on this page assumes `files_root` is configured.** When it is
 * not, every route answers `503` with an EMPTY body (`http.rs:1336-1341`)
 * — {@link FilesUnavailableTeach} is the one place that is explained, and
 * nothing else on the page renders until it clears.
 *
 * **Every other refusal on this page is bare-status shell copy, never
 * daemon prose.** `folder_status` (`http.rs:1325-1333`) collapses
 * `NotADirectory`/`NotEmpty`/`Exists` into one 409 for every mutating route —
 * so the same status means "that folder is not empty" on a delete and "there
 * is already something there" on a move, and only the verb tells them apart.
 * Each action below carries its own sentence map for exactly that reason;
 * there is no shared 409 copy on this page.
 *
 * **The OS drop never uploads silently.** `files://dropped`'s payload is
 * REPLACED whole on every drop (`drop.rs:44-56`), `read_dropped` refuses a
 * stale path or an oversized file by name, and this page collects every
 * refusal from every file in the drop and says so in one place — a drop that
 * quietly kept nine of ten files would be worse than one that failed outright.
 * The Upload button is held to the same rule and reports through the same note.
 *
 * **Nothing on this page destroys a file.** A delete moves the entry into the
 * núcleo's trash, beside the root and outside it, where it stays for
 * {@link TRASH_RETENTION_DAYS} days. So every delete — the context menu, the
 * selection bar, the Delete key, one item or many — is the same single action
 * followed by the same undo, and the only extra step left is the one the
 * daemon itself insists on: a folder with something in it.
 *
 * **A file opens where it lives.** Enter, a double-click on its name and the menu's
 * first item hand it to the app Windows gives its kind, by its path on disk
 * (`diskPath`, `openFile`); a download is a copy, and says so in the menu. A
 * program is the one kind never opened from here.
 */

/* ------------------------------------------------------------- refusals -- */

/** `GET /files` — a 409 here means the path named a file, not a folder. */
const LIST_SENTENCES: Record<string, string> = {
  bad_request: "that path leaves the files root, which the núcleo will not follow",
  not_found: "there is nothing at that path",
  conflict: "that path names a file, not a folder — there is nothing to list inside it",
  internal: "the núcleo could not read that folder from disk",
};

const SEARCH_SENTENCES: Record<string, string> = {
  bad_request: "that path leaves the files root, which the núcleo will not follow",
  internal: "the núcleo could not search that folder",
};

/** `POST /files/folder` — the only refusal it can give beyond 503 is 400. */
const CREATE_FOLDER_SENTENCES: Record<string, string> = {
  bad_request:
    "that path leaves the files root, is unsafe, or names the root itself — none of which the núcleo will create",
};

const UPLOAD_SENTENCES: Record<string, string> = {
  bad_request: "that folder leaves the files root, which the núcleo will not follow",
  conflict: "that folder is not actually a folder — there is a file there instead",
  http_413: `that file is larger than the ${String(MAX_UPLOAD_BYTES / (1024 * 1024))} MB the daemon will accept in one upload`,
  internal: "the núcleo could not write that file to disk",
};

/** `POST /files/move` — the 409 a person meets most often on this page. */
const MOVE_SENTENCES: Record<string, string> = {
  bad_request:
    "that move leaves the files root, is unsafe, targets the root itself, or moves a folder into itself",
  not_found: "the folder the destination would live in does not exist — a typo is never turned into a new folder",
  conflict: "there is already something there — a move never overwrites",
  internal: "the núcleo could not make that move on disk",
};

const DELETE_SENTENCES: Record<string, string> = {
  bad_request: "the files root itself cannot be deleted",
  conflict: "that folder has something in it — deleting it too means deleting everything inside",
  internal: "the núcleo could not move that to the trash, so it is still where it was",
};

/** `POST /files/restore` — held to the move rule, so its 409 and 404 read like the move's. */
const RESTORE_SENTENCES: Record<string, string> = {
  bad_request: "that is not something the trash holds",
  not_found: "the folder it came from is gone — a restore never makes one up; recreate it, then restore",
  conflict: "something else now sits where it came from — a restore never overwrites; move that aside first",
  internal: "the núcleo could not move it back out of the trash",
};

/** `GET /files/trash` — the one refusal it can give beyond 503. */
const TRASH_SENTENCES: Record<string, string> = {
  internal: "the núcleo could not read its trash folder",
};

const DOWNLOAD_SENTENCES: Record<string, string> = {
  conflict: "that path is a folder, not a file — there is nothing to download",
  not_found: "that file is gone",
};

function RefusalOrError({
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
      the núcleo did not answer at {clock}
      {said} — {what}
    </ErrorNote>
  );
}

/**
 * One line for one file that did not arrive, in the words the page uses for
 * the same refusal anywhere else: the upload's own sentence for a daemon
 * refusal, the host's message for a dropped file this window could not read.
 */
function describeUploadFailure(error: unknown): string {
  if (typeof error === "string" && error.trim() !== "") return error;
  if (isApiRefusal(error)) {
    return UPLOAD_SENTENCES[error.code] ?? (error.detail.trim() !== "" ? error.detail : error.code);
  }
  if (error instanceof Error && error.message.trim() !== "") return error.message;
  return "could not be uploaded";
}

/** The daemon's ceiling in words, said beside the Upload button before anything is picked. */
const MAX_UPLOAD_LABEL = `${String(MAX_UPLOAD_BYTES / (1024 * 1024))} MB`;

/**
 * One file waiting to go up, whichever door it came through. `read` defers the
 * bytes: a drop's are fetched from the host and a pick's from the browser, and
 * neither is read until the person has said what to do about a name clash.
 */
interface UploadItem {
  folder: string;
  name: string;
  size: number;
  read: () => Promise<ArrayBuffer>;
}

/** A batch waiting on one answer: some of its names are already taken where it is going. */
interface PendingBatch {
  source: "drop" | "picker";
  truncated: boolean;
  items: UploadItem[];
  /** Indexes into `items` whose name already names a file in the target folder. */
  clashes: number[];
  /** Refused before anything was asked — too large to send at all. */
  refusals: UploadRefusal[];
}

/** What to do with a file whose name is taken: the old one to the trash, a numbered copy, or not at all. */
type ClashChoice = "replace" | "keep" | "skip";

interface UploadSaved {
  sent: string;
  stored: string;
}

interface UploadRefusal {
  name: string;
  reason: string;
}

/** What one batch of uploads came to — a drop from Windows, or a pick from the Upload button. */
interface UploadReport {
  source: "drop" | "picker";
  truncated: boolean;
  saved: UploadSaved[];
  refusals: UploadRefusal[];
  skipped: string[];
  /** How many older copies a Replace moved to Recently deleted. */
  replaced: number;
}

/** Where the upload loop is, said with the file it is on. */
interface UploadProgress {
  done: number;
  total: number;
  name: string;
  size: number;
}

/** A delete or a restore that did not go through, kept beside the path it was for. */
interface PathFailure {
  path: string;
  error: unknown;
}

/** One entry a move or a rename carried, with both ends, so it can be carried back. */
interface MoveDone {
  from: string;
  to: string;
  is_dir: boolean;
}

/**
 * The last thing the page can take back. A delete is undone from the trash; a
 * move or a rename by the same move backwards — they had no undo at all, and a
 * move into the wrong folder is the likelier slip of the two.
 */
type Undo = { kind: "trash"; items: Trashed[] } | { kind: "move"; moves: MoveDone[]; renamed: boolean };

/** Where the page remembers that the empty slot's hint has done its job. */
const HINT_KEY = "nucleos.files.hint-retired";

/**
 * Whether the hint under the toolbar is retired: it says how a file opens and
 * that a drop uploads, which is news once. Browser storage, so a private
 * window or cleared data only brings the hint back.
 */
function readHintRetired(): boolean {
  try {
    return window.localStorage.getItem(HINT_KEY) === "1";
  } catch {
    return false;
  }
}

function storeHintRetired() {
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

function remPx(): number {
  return Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
}

/**
 * Whether the body is too narrow for the tree beside the table. The slack is
 * there because folding changes the page's height, and with it the scrollbar,
 * and so the width being measured: without it a body right on the line folds
 * and unfolds on its own. jsdom has no `ResizeObserver` and no layout to fold,
 * so there it counts as wide.
 */
function useNarrow(body: HTMLElement | null): boolean {
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

/* ----------------------------------------------------------------- page -- */

export function Files() {
  const [path, setPath] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [focusIndex, setFocusIndex] = useState<number | null>(null);
  const [sort, setSort] = useState<{ column: SortColumn; direction: SortDirection }>({
    column: "name",
    direction: "asc",
  });
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [renaming, setRenaming] = useState<{ path: string; name: string } | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    entry: Entry;
    /** The folder the entry is in: the page's own, or a search hit's. */
    dir: string;
    /** Opened on a search hit, where there is no selection and no row to rename in. */
    hit: boolean;
    opener: HTMLElement | null;
  } | null>(null);
  // The folder the entries were in when Move opened, kept with them: the form
  // joined the page's *current* folder to each name, so a click in the side
  // tree while it was open aimed the move at a different file.
  const [moving, setMoving] = useState<{ from: string; entries: Entry[] } | null>(null);
  // What Ctrl+X took, waiting for a Ctrl+V in another folder. Kept across
  // navigation, unlike everything the reset below clears: going somewhere else
  // is the whole of the gesture.
  const [cut, setCut] = useState<{ from: string; entries: Entry[] } | null>(null);
  // A drag under way, once it has left the row it started on: what it carries,
  // and the folder under the pointer that would take it.
  const [drag, setDrag] = useState<{ label: string; over: string | null } | null>(null);
  const [moveFailures, setMoveFailures] = useState<PathFailure[]>([]);
  const [notEmptyPaths, setNotEmptyPaths] = useState<string[] | null>(null);
  const [deleteFailures, setDeleteFailures] = useState<PathFailure[]>([]);
  const [undo, setUndo] = useState<Undo | null>(null);
  const [restoreFailures, setPathFailures] = useState<PathFailure[]>([]);
  const [unmoveFailures, setUnmoveFailures] = useState<PathFailure[]>([]);
  const [hintRetired, setHintRetired] = useState(readHintRetired);
  const [dragActive, setDragActive] = useState(false);
  const [pendingBatch, setPendingBatch] = useState<PendingBatch | null>(null);
  const [progress, setProgress] = useState<UploadProgress | null>(null);
  const [uploadReport, setUploadReport] = useState<UploadReport | null>(null);
  const [downloadError, setDownloadError] = useState<unknown>(null);
  const [openError, setOpenError] = useState<{ name: string; reason: string } | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const [focusAfter, setFocusAfter] = useState<FocusAfter | null>(null);
  const [body, setBody] = useState<HTMLDivElement | null>(null);
  const narrow = useNarrow(body);
  const queryClient = useQueryClient();

  const folder = useFolder(path);
  const searching = q.trim() !== "";
  const search = useFileSearch(path, q, searching);
  const create = useCreateFolder();
  const upload = useUpload();
  // Two instances of the same request on purpose. A rename and a move are one
  // route, but a person does them from two places, and one shared mutation
  // meant a failed move surfaced under the table as "that rename did not go
  // through" once its form was closed.
  const move = useMove();
  const rename = useMove();
  const del = useDelete();
  const restore = useRestore();
  const trash = useTrash();

  const tableRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  // The name a "Show in folder" is on its way to. Held across the navigation,
  // because the reset below clears the selection and the focus it would set.
  const revealRef = useRef<string | null>(null);
  const ghostRef = useRef<HTMLDivElement>(null);
  const ghostAt = useRef({ x: 0, y: 0 });
  const ordered = folder.data === undefined ? [] : sortEntries(folder.data, sort.column, sort.direction);

  // Navigating clears what no longer names anything on screen — row-local
  // state, and the notes about actions taken in the folder just left. An error
  // that outlives its folder reads as being about the one you are in now.
  // The upload report stays: it names its own paths. The undo goes with the
  // folder: kept for as long as a page stayed open, a reflex Ctrl+Z hours later
  // quietly took back a delete nobody remembered. The move form goes too: what
  // it was moving is in the table just left.
  const resetRename = rename.reset;
  useEffect(() => {
    setSelected(new Set());
    setMoving(null);
    setUndo(null);
    setFocusIndex(null);
    setRenaming(null);
    setNotEmptyPaths(null);
    setDeleteFailures([]);
    setDownloadError(null);
    setOpenError(null);
    setFocusAfter(null);
    resetRename();
    const reveal = revealRef.current;
    revealRef.current = null;
    if (reveal !== null) {
      setSelected(new Set([reveal]));
      setFocusAfter({ name: reveal, gone: [] });
    }
  }, [path, resetRename]);

  /**
   * A search hit shown where it lives: the search put away, the hit's folder
   * opened, and the file picked with the keyboard on it. It only opened the
   * folder, and a person then read the listing again for the name they had
   * just clicked.
   */
  function reveal(hit: Hit) {
    setQInput("");
    setQ("");
    const dir = parentPath(hit.path);
    if (dir === path) {
      setSelected(new Set([hit.name]));
      setFocusAfter({ name: hit.name, gone: [] });
      return;
    }
    revealRef.current = hit.name;
    setPath(dir);
  }

  // The 200ms debounce the packet asks for, at the page: a search per
  // keystroke would walk the whole tree once per character typed.
  useEffect(() => {
    const timer = window.setTimeout(() => setQ(qInput), 200);
    return () => window.clearTimeout(timer);
  }, [qInput]);

  // A confirmation that needs no answer — a download handed over, a path
  // copied — is said once and then gets out of the way. Six seconds: at four a
  // full path was gone before it had been read to the end.
  useEffect(() => {
    if (flash === null) return;
    const timer = window.setTimeout(() => setFlash(null), 6000);
    return () => window.clearTimeout(timer);
  }, [flash]);

  function retireHint() {
    if (hintRetired) return;
    storeHintRetired();
    setHintRetired(true);
  }

  /* --------------------------------------------------------------- upload -- */

  /**
   * Where every upload starts, from either door. Anything too large is refused
   * by name before a byte is read; then each target folder is read once, and a
   * name that already names a file there stops the batch on one question
   * rather than letting the daemon quietly number the new copy.
   */
  async function beginUpload(source: PendingBatch["source"], truncated: boolean, all: UploadItem[]) {
    const refusals: UploadRefusal[] = [];
    const items: UploadItem[] = [];
    for (const item of all) {
      if (item.size > MAX_UPLOAD_BYTES) refusals.push({ name: item.name, reason: UPLOAD_SENTENCES.http_413 ?? "too large" });
      else items.push(item);
    }

    const taken = new Map<string, Set<string>>();
    for (const target of new Set(items.map((item) => item.folder))) {
      try {
        const listing = await queryClient.fetchQuery({ queryKey: keys.files.list(target), queryFn: () => readFolder(target) });
        // Case-folded: the disk under the root is Windows', where `Report.docx`
        // and `report.docx` are the same file.
        taken.set(target, new Set(listing.filter((entry) => !entry.is_dir).map((entry) => entry.name.toLowerCase())));
      } catch {
        // A folder a drop is about to create has nothing in it to clash with.
        taken.set(target, new Set());
      }
    }
    const clashes = items.flatMap((item, index) =>
      taken.get(item.folder)?.has(item.name.toLowerCase()) === true ? [index] : [],
    );

    const batch: PendingBatch = { source, truncated, items, clashes, refusals };
    if (clashes.length === 0) await runBatch(batch, "keep");
    else setPendingBatch(batch);
  }

  /**
   * Sends a batch one file at a time — fired together, a mutation's per-call
   * `onSuccess` only runs for the last call, and an earlier refusal vanished
   * under a later success. A Replace moves the old file to the trash first;
   * if that is refused, the new one is not sent, so nothing is ever numbered
   * behind a person's back after they asked for it not to be.
   */
  async function runBatch(batch: PendingBatch, choice: ClashChoice) {
    setPendingBatch(null);
    const saved: UploadSaved[] = [];
    const refusals = [...batch.refusals];
    const skipped: string[] = [];
    let replaced = 0;
    for (const [index, item] of batch.items.entries()) {
      const clash = batch.clashes.includes(index);
      if (clash && choice === "skip") {
        skipped.push(item.name);
        continue;
      }
      setProgress({ done: index, total: batch.items.length, name: item.name, size: item.size });
      try {
        if (clash && choice === "replace") {
          try {
            await del.mutateAsync({ path: joinPath(item.folder, item.name), recursive: false });
            replaced += 1;
          } catch {
            refusals.push({ name: item.name, reason: "the file already there could not be moved to the trash, so this one was not sent" });
            continue;
          }
        }
        const bytes = await item.read();
        const result = await upload.mutateAsync({ folder: item.folder, filename: item.name, bytes });
        saved.push({ sent: item.name, stored: result.filename });
      } catch (error) {
        refusals.push({ name: item.name, reason: describeUploadFailure(error) });
      }
    }
    setProgress(null);
    if (saved.length > 0) retireHint();
    // A batch that went up whole, under the names it was sent with, is news
    // for the slot and not a note to dismiss: the files are in the listing, and
    // the report listed every one of them back.
    const clean =
      saved.length > 0 &&
      refusals.length === 0 &&
      skipped.length === 0 &&
      replaced === 0 &&
      !batch.truncated &&
      saved.every((item) => item.stored === item.sent);
    if (clean) {
      setUploadReport(null);
      const [only] = saved;
      setFlash(saved.length === 1 && only !== undefined ? `uploaded ${only.sent}` : `uploaded ${String(saved.length)} files`);
      return;
    }
    setUploadReport({ source: batch.source, truncated: batch.truncated, saved, refusals, skipped, replaced });
  }

  // The OS-drop listeners are registered once (see their effect's own note),
  // so they reach the upload through a ref — through its own closure they
  // would read the path and the mutations this page had when the window opened.
  const pathRef = useRef(path);
  const beginUploadRef = useRef(beginUpload);
  useEffect(() => {
    pathRef.current = path;
    beginUploadRef.current = beginUpload;
  });

  /* -------------------------------------------------------------- OS drop -- */

  useEffect(() => {
    let unlistenEnter: (() => void) | undefined;
    let unlistenLeave: (() => void) | undefined;
    let unlistenDropped: (() => void) | undefined;

    void listen("files://drag-enter", () => setDragActive(true)).then((fn) => {
      unlistenEnter = fn;
    });
    void listen("files://drag-leave", () => setDragActive(false)).then((fn) => {
      unlistenLeave = fn;
    });
    void listen<Dropped>("files://dropped", (event) => {
      setDragActive(false);
      const items = event.payload.files.map((file) => ({
        folder: dropTargetFolder(pathRef.current, file.folder),
        name: file.name,
        size: file.size,
        read: () => invoke<ArrayBuffer>("read_dropped", { path: file.path }),
      }));
      void beginUploadRef.current("drop", event.payload.truncated, items);
    }).then((fn) => {
      unlistenDropped = fn;
    });

    return () => {
      unlistenEnter?.();
      unlistenLeave?.();
      unlistenDropped?.();
    };
  }, []);

  /* --------------------------------------------------------------- delete -- */

  /**
   * Where focus goes once a delete has landed: the row after the last one
   * removed, else the one before the first, else the table itself. Focus left
   * on a button that unmounted falls to `<body>` — the top of the page, for
   * someone halfway down a folder — and the table wrapper was only half an
   * answer, since the arrows then started again from the first row.
   */
  function landFocusAfter(names: string[]) {
    // From the search there is no listing on screen to land in.
    if (searching) return;
    const gone = new Set(names);
    const indexes = ordered.flatMap((entry, index) => (gone.has(entry.name) ? [index] : []));
    const after = ordered.slice((indexes[indexes.length - 1] ?? -1) + 1).find((entry) => !gone.has(entry.name));
    const before = ordered
      .slice(0, indexes[0] ?? 0)
      .reverse()
      .find((entry) => !gone.has(entry.name));
    const neighbour = after ?? before;
    if (neighbour === undefined) {
      window.requestAnimationFrame(() => tableRef.current?.focus());
      return;
    }
    setFocusAfter({ name: neighbour.name, gone: names });
  }

  function focusTable() {
    window.requestAnimationFrame(() => tableRef.current?.focus());
  }

  async function attemptDelete(
    paths: string[],
    recursive: boolean,
  ): Promise<{ trashed: Trashed[]; notEmpty: string[] }> {
    const trashed: Trashed[] = [];
    const notEmpty: string[] = [];
    // Every refusal kept with its path: one slot for the batch meant a third
    // failure replaced the first two, and none of them said which item it was.
    const failures: PathFailure[] = [];
    for (const target of paths) {
      const request: DeleteRequest = { path: target, recursive };
      try {
        trashed.push(await del.mutateAsync(request));
      } catch (error) {
        if (!recursive && isApiRefusal(error) && error.status === 409) {
          notEmpty.push(target);
        } else {
          failures.push({ path: target, error });
        }
      }
    }
    setDeleteFailures(failures);
    return { trashed, notEmpty };
  }

  /** The one delete every control on this page calls. */
  async function deletePaths(paths: string[]) {
    if (paths.length === 0) return;
    const { trashed, notEmpty } = await attemptDelete(paths, false);
    setPathFailures([]);
    setUndo(trashed.length > 0 ? { kind: "trash", items: trashed } : null);
    setNotEmptyPaths(notEmpty.length > 0 ? notEmpty : null);
    setSelected(new Set(notEmpty.map((p) => fileName(p))));
    if (notEmpty.length === 0) landFocusAfter(trashed.map((item) => fileName(item.path)));
  }

  async function deleteRecursive(paths: string[]) {
    const { trashed } = await attemptDelete(paths, true);
    // One undo for the whole gesture: whatever the first pass already moved,
    // and the folders the person then confirmed.
    setUndo((current) => {
      const all = [...(current?.kind === "trash" ? current.items : []), ...trashed];
      return all.length > 0 ? { kind: "trash", items: all } : null;
    });
    setNotEmptyPaths(null);
    setSelected(new Set());
    // Only what this pass moved: the first pass already landed focus for its
    // own, and `undo` read here would be the value from before either awaited.
    landFocusAfter(trashed.map((item) => fileName(item.path)));
  }

  /** Puts entries back, one at a time, and keeps every one that would not go. */
  async function restoreItems(items: Trashed[]) {
    const failures: PathFailure[] = [];
    for (const item of items) {
      try {
        await restore.mutateAsync(item.id);
      } catch (error) {
        failures.push({ path: item.path, error });
      }
    }
    setPathFailures(failures);
    const back = items.filter((item) => !failures.some((failure) => failure.path === item.path));
    const [only] = back;
    if (back.length > 0) {
      setFlash(back.length === 1 && only !== undefined ? `restored ${fileName(only.path)}` : `restored ${String(back.length)} items`);
    }
    setUndo((current) => {
      if (current?.kind !== "trash") return current;
      const left = current.items.filter((item) => !items.some((done) => done.id === item.id));
      return left.length > 0 ? { kind: "trash", items: left } : null;
    });
  }

  /** Takes a move or a rename back: each entry carried from where it went to where it was. */
  async function unmoveItems(moves: MoveDone[], renamed: boolean) {
    const failures: PathFailure[] = [];
    for (const done of moves) {
      try {
        await move.mutateAsync({ from: done.to, to: done.from });
      } catch (error) {
        failures.push({ path: done.to, error });
      }
    }
    setUnmoveFailures(failures);
    // Said, because the only other sign was a row reappearing, and an undo that
    // answers nothing reads as one that did nothing.
    const back = moves.filter((done) => !failures.some((failure) => failure.path === done.to));
    const [only] = back;
    if (back.length > 0) {
      setFlash(
        renamed && only !== undefined
          ? `renamed back to ${fileName(only.from)}`
          : back.length === 1 && only !== undefined
            ? `moved ${fileName(only.from)} back`
            : `moved ${String(back.length)} items back`,
      );
    }
    setUndo((current) => (current?.kind === "move" && current.moves === moves ? null : current));
  }

  function takeBack(target: Undo) {
    if (target.kind === "trash") void restoreItems(target.items);
    else void unmoveItems(target.moves, target.renamed);
  }

  /**
   * The move a drag and a paste both end in: every entry carried into
   * `destination`, whatever went through kept as one undo. The move form keeps
   * its own loop, since it says its refusals inside itself.
   */
  async function moveInto(from: string, entries: Entry[], destination: string) {
    const refusal = moveRefusal(from, entries, destination);
    if (refusal !== null) {
      setFlash(refusal);
      return;
    }
    const failures: PathFailure[] = [];
    const done: MoveDone[] = [];
    for (const entry of entries) {
      const source = joinPath(from, entry.name);
      const to = joinPath(destination, entry.name);
      try {
        await move.mutateAsync({ from: source, to });
        done.push({ from: source, to, is_dir: entry.is_dir });
      } catch (error) {
        failures.push({ path: source, error });
      }
    }
    setMoveFailures(failures);
    if (done.length === 0) return;
    setUndo({ kind: "move", moves: done, renamed: false });
    const where = destination === "" ? "files/" : `files/${destination}/`;
    const [only] = done;
    setFlash(
      done.length === 1 && only !== undefined
        ? `moved ${fileName(only.from)} to ${where}`
        : `moved ${String(done.length)} items to ${where}`,
    );
    if (from === path) {
      setSelected(new Set());
      landFocusAfter(done.map((item) => fileName(item.from)));
    }
  }

  /**
   * Ctrl+X: the entries marked to go, and nothing sent yet. The selection is
   * let go, because the slot it fills is where the cut says what it holds; the
   * rows stay on screen, dimmed, until the paste lands somewhere else.
   */
  function cutEntries(entries: Entry[]) {
    if (entries.length === 0) return;
    setCut({ from: path, entries });
    setSelected(new Set());
  }

  function pasteHere() {
    if (cut === null) return;
    const refusal = moveRefusal(cut.from, cut.entries, path);
    if (refusal !== null) {
      setFlash(refusal);
      return;
    }
    setCut(null);
    void moveInto(cut.from, cut.entries, path);
  }

  /**
   * A row carried onto a folder with the pointer — a folder row, a node of the
   * side tree, or a folder in the path bar. Pointer events and not the
   * browser's drag and drop: the window's own drop is Tauri's, for files from
   * Windows, and the two answering one gesture is how an upload overlay would
   * cover a move. Nothing happens until the pointer has travelled a few pixels,
   * so a click is still a click; the click that ends a drag is swallowed.
   */
  function startDrag(entry: Entry, down: React.PointerEvent) {
    if (down.button !== 0 || down.ctrlKey || down.metaKey || down.shiftKey) return;
    if ((down.target as HTMLElement).closest("input, form") !== null) return;
    const entries = selected.has(entry.name) ? selectedEntries : [entry];
    const from = path;
    const label = entries.length === 1 ? `${entry.name}${entry.is_dir ? "/" : ""}` : `${String(entries.length)} items`;
    const startX = down.clientX;
    const startY = down.clientY;
    let started = false;
    let over: HTMLElement | null = null;
    let shown = false;

    function place(x: number, y: number) {
      ghostAt.current = { x, y };
      if (ghostRef.current !== null) ghostRef.current.style.transform = ghostTransform(x, y);
    }
    function onMove(event: PointerEvent) {
      if (!started) {
        if (Math.hypot(event.clientX - startX, event.clientY - startY) < DRAG_THRESHOLD_PX) return;
        started = true;
        window.getSelection()?.removeAllRanges();
        document.body.classList.add("fi-dragging");
      }
      place(event.clientX, event.clientY);
      const target = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-drop-path]") : null;
      const next =
        target !== null && moveRefusal(from, entries, target.dataset.dropPath ?? "") === null ? target : null;
      if (next === over && shown) return;
      shown = true;
      over?.classList.remove("fi-drop-over");
      next?.classList.add("fi-drop-over");
      over = next;
      setDrag({ label, over: next?.dataset.dropPath ?? null });
    }
    function end(commit: boolean) {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, true);
      if (!started) return;
      const destination = over?.dataset.dropPath;
      over?.classList.remove("fi-drop-over");
      document.body.classList.remove("fi-dragging");
      setDrag(null);
      window.addEventListener("click", swallowClick, { capture: true, once: true });
      window.setTimeout(() => window.removeEventListener("click", swallowClick, { capture: true }), 0);
      if (commit && destination !== undefined) void moveInto(from, entries, destination);
    }
    function onUp() {
      end(true);
    }
    function onCancel() {
      end(false);
    }
    function onKey(event: KeyboardEvent) {
      if (event.key !== "Escape" || !started) return;
      event.preventDefault();
      event.stopPropagation();
      end(false);
    }
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, true);
  }

  const undoBusy = restore.isPending || move.isPending;

  // Ctrl+Z, the undo every file manager has, for as long as the undo note is
  // up. Not inside a field: there it is the field's own undo, and taking it
  // would restore a file when a person meant to take back a typed letter.
  const undoRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    undoRef.current = undo === null || undoBusy ? null : () => takeBack(undo);
  });
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "z" || event.shiftKey) return;
      const target = event.target as HTMLElement | null;
      if (target !== null && (target.tagName === "TEXTAREA" || (target.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox"))) return;
      if (undoRef.current === null) return;
      event.preventDefault();
      undoRef.current();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Ctrl+X and Ctrl+V from anywhere but a field, where they are the field's.
  // The cut takes the selection, else the row the keyboard is on; the paste
  // lands in the folder on screen.
  const clipRef = useRef<{ cut: () => void; paste: () => void } | null>(null);
  useEffect(() => {
    clipRef.current =
      searching || renaming !== null || moving !== null
        ? null
        : {
            cut: () => {
              const inTable = tableRef.current?.contains(document.activeElement) === true;
              const focused = focusIndex === null || !inTable ? undefined : ordered[focusIndex];
              cutEntries(selectedEntries.length > 0 ? selectedEntries : focused !== undefined ? [focused] : []);
            },
            paste: pasteHere,
          };
  });
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey) || event.shiftKey || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key !== "x" && key !== "v") return;
      const target = event.target as HTMLElement | null;
      if (
        target !== null &&
        (target.tagName === "TEXTAREA" ||
          target.isContentEditable ||
          (target.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox"))
      )
        return;
      const clip = clipRef.current;
      if (clip === null) return;
      event.preventDefault();
      if (key === "x") clip.cut();
      else clip.paste();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Ctrl+F from anywhere on the page, "/" from anywhere but a field. The
  // webview's own find bar searched the words on screen, which is not what a
  // person looking for a file means by it.
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      const find = (event.ctrlKey || event.metaKey) && !event.shiftKey && event.key.toLowerCase() === "f";
      const target = event.target as HTMLElement | null;
      const inField =
        target !== null &&
        (target.tagName === "TEXTAREA" ||
          target.isContentEditable ||
          (target.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox"));
      const slash = event.key === "/" && !event.ctrlKey && !event.metaKey && !event.altKey && !inField;
      if (!find && !slash) return;
      const field = searchRef.current;
      if (field === null) return;
      event.preventDefault();
      field.focus();
      field.select();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, []);

  /**
   * The one place `downloadFile` is ever called. It is a plain function, not
   * a mutation, so nothing catches its rejection unless this does — a bare
   * `void downloadFile(...)` at each call site would turn a 409 on a
   * directory or a 404 on a vanished file into a silent no-op with an
   * unhandled rejection behind it. A download that went through says so: the
   * browser's own save happens out of sight, and a click that answered nothing
   * reads as a click that did nothing.
   */
  function handleDownload(target: string) {
    setDownloadError(null);
    downloadFile(target).then(
      () => setFlash(`${fileName(target)} downloaded`),
      (error: unknown) => setDownloadError(error),
    );
  }

  /**
   * A file opened the way Explorer opens it, in whatever app Windows gives its
   * kind. Enter, a double-click on the name and the menu's Open all come here; the
   * download it replaced saved a second copy somewhere else every time a
   * person only wanted to look. A program is the exception, said by name.
   */
  function handleOpen(target: string) {
    const name = fileName(target);
    setOpenError(null);
    if (runsOnOpen(name)) {
      setOpenError({
        name,
        reason: "a program is never run from here, since anything can land in this folder — download it to use it",
      });
      return;
    }
    openFile(target).then(
      () => {
        setFlash(`opened ${name}`);
        retireHint();
      },
      (error: unknown) => {
        // The opener rejects with a bare string; either way the OS's words come after ours.
        const said = error instanceof Error ? error.message : typeof error === "string" ? error : "";
        setOpenError({ name, reason: said === "" ? "Windows could not open it" : `Windows could not open it — ${said}` });
      },
    );
  }

  /**
   * The whole paths on disk, the ones Explorer or a terminal can use — not the
   * part under the root. One per line when the menu was opened on a selection:
   * it copied the clicked row alone while Move and Delete beside it took all.
   */
  function copyPaths(targets: string[]) {
    Promise.all(targets.map((target) => diskPath(target)))
      .then((full) => navigator.clipboard.writeText(full.join("\n")).then(() => full))
      .then(
        (full) => setFlash(full.length === 1 ? `copied ${full[0] ?? ""}` : `copied ${String(full.length)} paths, one per line`),
        () => setFlash(targets.length === 1 ? "that path could not be copied" : "those paths could not be copied"),
      );
  }

  /* --------------------------------------------------------------- render -- */

  const rootUnavailable =
    folder.isError && folder.data === undefined && isApiRefusal(folder.error) && folder.error.status === 503;
  const selectedEntries = ordered.filter((entry) => selected.has(entry.name));
  // What the menu's set verbs act on: the row it was opened on, plus the rest of
  // the selection when that row is part of it — the way a file manager's menu
  // does. Delete used to take the clicked row alone and drop the other picks.
  const menuTargets =
    contextMenu === null
      ? []
      : !contextMenu.hit && selected.has(contextMenu.entry.name) && selectedEntries.length > 1
        ? selectedEntries
        : [contextMenu.entry];

  const tree = <FolderTree currentPath={path} onSelect={setPath} dropTargets />;

  // The slot under the toolbar, and what goes in it besides the selection.
  // Hidden while a question is open, and the news then stands on its own line
  // as it used to: an upload under way is not something to hide.
  const slotShown = !searching && notEmptyPaths === null && moving === null && pendingBatch === null;
  const news =
    progress !== null ? (
      <p className="fi-progress" role="status">
        uploading {progress.done + 1} of {progress.total} —{" "}
        <span className="fi-progress-name">{progress.name}</span>, {formatBytes(progress.size)}…
      </p>
    ) : flash !== null ? (
      <p className="fi-flash" role="status">
        {flash}
      </p>
    ) : null;
  // What a Ctrl+X is holding, said where the selection was: the dimmed rows
  // only show it while their folder is on screen, and the paste is made
  // somewhere else.
  const cutNote =
    cut === null ? null : (
      <span className="fi-cut">
        <span className="fi-cut-what">
          {cut.entries.length === 1 ? `${cut.entries[0]?.name ?? ""} cut` : `${String(cut.entries.length)} items cut`}
          {cut.from === path
            ? " — open the folder it goes to, then Ctrl+V"
            : ` from ${cut.from === "" ? "files/" : `files/${cut.from}/`}`}
        </span>
        {cut.from !== path && (
          <Button variant="quiet" aria-keyshortcuts="Control+V" disabled={move.isPending} onClick={pasteHere}>
            Move here <kbd className="fi-undo-keys" aria-hidden="true">Ctrl+V</kbd>
          </Button>
        )}
        <Button variant="quiet" onClick={() => setCut(null)}>
          Cancel
        </Button>
      </span>
    );
  const undoNote =
    undo === null ? null : (
      <UndoNote undo={undo} busy={undoBusy} onUndo={() => takeBack(undo)} onDismiss={() => setUndo(null)} />
    );

  // Finding and making, on the title's line: the page's verbs sit with its
  // name, and the table starts under the rule. The path went over the tree,
  // which is the other thing that says where you are.
  const toolbar = rootUnavailable ? undefined : (
    <div className="fi-toolbar">
      <div className="fi-search">
        <Field label="Search under this folder" labelHidden>
          <input
            ref={searchRef}
            type="search"
            value={qInput}
            aria-label="Search files by name"
            aria-keyshortcuts="Control+F /"
            placeholder="Search here and below · Ctrl+F"
            onChange={(event) => setQInput(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") setQInput("");
            }}
          />
        </Field>
      </div>
      {/* Making things sits together on the right: the two ways a folder
          gets new contents are one intent. */}
      <div className="fi-make">
        <NewFolderForm key={path} path={path} create={create} />
        <UploadButton
          busy={progress !== null || pendingBatch !== null}
          onFiles={(files) =>
            void beginUpload(
              "picker",
              false,
              files.map((file) => ({ folder: path, name: file.name, size: file.size, read: () => file.arrayBuffer() })),
            )
          }
        />
      </div>
    </div>
  );

  return (
    <>
      <PageHeader
        title="Files"
        headline={pageHeadline(rootUnavailable, folder.data, folder.isError)}
        actions={toolbar}
      />

      {rootUnavailable && <FilesUnavailableTeach />}

      {!rootUnavailable && (
        <div
          ref={setBody}
          className={narrow ? "fi-body fi-body-narrow" : "fi-body"}
          onDragOver={(event) => event.preventDefault()}
        >
          {dragActive && (
            <div className="fi-drop-overlay" role="status">
              drop to upload into {path === "" ? "the files root" : path}
            </div>
          )}

          <div className="fi-side">
            {/* Where you are, heading the column that shows where you can go. */}
            <Breadcrumbs path={path} onGo={setPath} />
            {/* At the degraded width the tree is one line until asked for: stacked
                open above the table, it left one row of the folder above the fold,
                and the path bar already carries the path. The two folds share that
                line, so together they cost the table one row, not two. */}
            {narrow ? (
              <details className="fi-fold">
                <summary className="fi-fold-summary">Folders</summary>
                {tree}
              </details>
            ) : (
              tree
            )}
            <RecentlyDeleted trash={trash} busy={restore.isPending} onRestore={(item) => void restoreItems([item])} />
          </div>

          <div className="fi-main">
            {pendingBatch !== null && (
              <ClashNote
                batch={pendingBatch}
                onChoose={(choice) => void runBatch(pendingBatch, choice)}
                onCancel={() => setPendingBatch(null)}
              />
            )}

            {uploadReport !== null && (
              <UploadReportNote report={uploadReport} onDismiss={() => setUploadReport(null)} />
            )}

            <FailureNotes
              failures={restoreFailures}
              sentences={RESTORE_SENTENCES}
              what="it is still in the trash"
              summary={(n) => `${String(n)} restores did not go through — they are still in the trash`}
              labelFor={(failure) => `Dismiss the note about ${failure}`}
              onDismiss={(failure) => setPathFailures((current) => current.filter((item) => item !== failure))}
              onDismissAll={() => setPathFailures([])}
            />
            <FailureNotes
              failures={unmoveFailures}
              sentences={MOVE_SENTENCES}
              what="it was not moved back"
              summary={(n) => `${String(n)} entries could not be moved back`}
              labelFor={(failure) => `Dismiss the note about ${failure}`}
              onDismiss={(failure) => setUnmoveFailures((current) => current.filter((item) => item !== failure))}
              onDismissAll={() => setUnmoveFailures([])}
            />

            {!slotShown && undoNote}
            {!slotShown && news}

            {/* A slot that is always there, so the first tick does not push every row
                down by a bar's height under the pointer that made it — and the one
                place the page's passing news goes: the upload under way, a path
                copied, the undo after a delete. They were blocks of their own above
                the table, and each one arriving or expiring moved every row under
                the pointer. Empty, it is the band the path shares. It
                steps aside while a question is open — the clash, the not-empty
                note, the move form — and one decision at a time is on screen. */}
            {slotShown && (
              <div
                className={
                  selected.size > 0 || undo !== null || cut !== null
                    ? "fi-selection-bar fi-selection-live"
                    : "fi-selection-bar"
                }
              >
                {/* Only the count is live. The buttons used to sit inside the region
                    too, so every tick of a checkbox re-read the whole bar aloud. */}
                <span role="status">{selected.size > 0 ? `${String(selected.size)} selected` : ""}</span>
                {selected.size > 0 ? (
                  <>
                    {/* Move before Delete: the eye lands on the first verb, and the
                        first used to be the only one in colour and the one that
                        removes. One click, however many are picked: a delete lands
                        in the trash with an undo beside it, which is the protection
                        a second click was standing in for. */}
                    <Button
                      variant="ghost"
                      disabled={selectedEntries.length === 0}
                      onClick={() => setMoving({ from: path, entries: selectedEntries })}
                    >
                      Move…
                    </Button>
                    <Button
                      variant="danger"
                      disabled={del.isPending}
                      onClick={() => void deletePaths([...selected].map((name) => joinPath(path, name)))}
                    >
                      {selected.size === 1 ? "Delete" : `Delete ${String(selected.size)}`}
                    </Button>
                    <Button variant="quiet" onClick={() => setSelected(new Set())}>
                      Clear selection
                    </Button>
                    {/* The undo, kept within reach while something is picked: the
                        selection took the whole slot, and the only way back left
                        was a Ctrl+Z nothing on screen mentioned. */}
                    {undo !== null && (
                      <span className="fi-slot-undo">
                        <Button
                          variant="quiet"
                          disabled={undoBusy}
                          aria-label={`Undo: ${undoSummary(undo)}`}
                          aria-keyshortcuts="Control+Z"
                          title={`Undo: ${undoSummary(undo)}`}
                          onClick={() => takeBack(undo)}
                        >
                          {/* The verb on the button and the whole sentence in its
                              name: a bare "Undo" beside a selection did not say
                              which of the last things it would take back. */}
                          Undo {undoVerb(undo)} <UndoKeys />
                        </Button>
                      </span>
                    )}
                  </>
                ) : (
                  (cutNote ?? undoNote)
                )}
                {news}
              </div>
            )}

            {notEmptyPaths !== null && (
              <NotEmptyNote
                paths={notEmptyPaths}
                busy={del.isPending}
                onConfirm={() => void deleteRecursive(notEmptyPaths)}
                onCancel={() => {
                  setNotEmptyPaths(null);
                  focusTable();
                }}
              />
            )}

            {moving !== null && (
              <MoveForm
                path={moving.from}
                entries={moving.entries}
                move={move}
                onMoved={(moves) => setUndo({ kind: "move", moves, renamed: false })}
                onDone={() => {
                  setMoving(null);
                  setSelected(new Set());
                  move.reset();
                  focusTable();
                }}
                onClose={() => {
                  setMoving(null);
                  move.reset();
                  focusTable();
                }}
              />
            )}

            {/* The notes about a row sit above the rows, not under them: in a long
                folder the space under the table is off-screen, and an answer
                nobody sees is the same as none. Each can be put away — a refusal
                that stays until the next navigation outstays what it was about. */}
            <FailureNotes
              failures={moveFailures}
              sentences={MOVE_SENTENCES}
              what="it was not moved"
              summary={(n) => `${String(n)} entries could not be moved`}
              labelFor={(failure) => `Dismiss the move error for ${failure}`}
              onDismiss={(failure) => setMoveFailures((current) => current.filter((item) => item !== failure))}
              onDismissAll={() => setMoveFailures([])}
            />
            <FailureNotes
              failures={deleteFailures}
              sentences={DELETE_SENTENCES}
              what="that delete did not go through"
              summary={(n) => `${String(n)} deletes did not go through`}
              labelFor={(failure) => `Dismiss the delete error for ${failure}`}
              onDismiss={(failure) => setDeleteFailures((current) => current.filter((item) => item !== failure))}
              onDismissAll={() => setDeleteFailures([])}
            />
            {openError !== null && (
              <Dismissable label="Dismiss the open error" onDismiss={() => setOpenError(null)}>
                <div className="fi-path-failure">
                  <span className="fi-entry-name">{openError.name}</span>
                  <ErrorNote>{openError.reason}</ErrorNote>
                </div>
              </Dismissable>
            )}
            {downloadError !== null && (
              <Dismissable label="Dismiss the download error" onDismiss={() => setDownloadError(null)}>
                <RefusalOrError error={downloadError} sentences={DOWNLOAD_SENTENCES} what="that file was not downloaded" />
              </Dismissable>
            )}

            {searching ? (
              <SearchResults
                result={search}
                onOpenFolder={setPath}
                onOpenFile={handleOpen}
                onReveal={reveal}
                onContextMenu={(hit, x, y, opener) =>
                  setContextMenu({ entry: hit, dir: parentPath(hit.path), hit: true, x, y, opener })
                }
              />
            ) : (
              <FileTable
                tableRef={tableRef}
                path={path}
                folder={folder}
                ordered={ordered}
                sort={sort}
                onSort={(column) =>
                  setSort((current) =>
                    current.column === column
                      ? { column, direction: current.direction === "asc" ? "desc" : "asc" }
                      : { column, direction: "asc" },
                  )
                }
                selected={selected}
                onToggleSelect={(name) =>
                  setSelected((current) => {
                    const next = new Set(current);
                    if (next.has(name)) next.delete(name);
                    else next.add(name);
                    return next;
                  })
                }
                onAddToSelection={(names) =>
                  setSelected((current) => new Set([...current, ...names]))
                }
                onSelectAll={(names) => setSelected(new Set(names))}
                focusIndex={focusIndex}
                onFocusRow={setFocusIndex}
                focusAfter={focusAfter}
                onFocusLanded={() => setFocusAfter(null)}
                renaming={renaming}
                renameError={rename.isError ? rename.error : null}
                onStartRename={(entry) => {
                  rename.reset();
                  setRenaming({ path: joinPath(path, entry.name), name: entry.name });
                }}
                onSubmitRename={(from, name) => {
                  const to = joinPath(path, name);
                  const isDir = ordered.find((entry) => joinPath(path, entry.name) === from)?.is_dir ?? false;
                  rename.mutate(
                    { from, to },
                    {
                      onSuccess: () => {
                        setRenaming(null);
                        setUndo({ kind: "move", moves: [{ from, to, is_dir: isDir }], renamed: true });
                      },
                    },
                  );
                }}
                onCancelRename={() => {
                  setRenaming(null);
                  rename.reset();
                }}
                onOpen={(entry) => {
                  if (entry.is_dir) setPath(joinPath(path, entry.name));
                  else handleOpen(joinPath(path, entry.name));
                }}
                onContextMenu={(entry, x, y, opener) => {
                  // Outside the selection, the row becomes the selection, as in
                  // Explorer: the menu then speaks for exactly what is tinted.
                  if (!selected.has(entry.name)) setSelected(new Set([entry.name]));
                  setContextMenu({ entry, dir: path, hit: false, x, y, opener });
                }}
                onDelete={(names) => void deletePaths(names.map((name) => joinPath(path, name)))}
                onGoUp={() => setPath(parentPath(path))}
                cutNames={cut !== null && cut.from === path ? new Set(cut.entries.map((entry) => entry.name)) : null}
                onCancelCut={() => setCut(null)}
                onDragStart={startDrag}
              />
            )}

            {/* Under the rows, as their footnote: over them it was one more line
                between the rule and the first file. */}
            {!searching && !hintRetired && (
              <p className="fi-hint">Double-click a file to open it in Windows · drop files here to upload</p>
            )}
          </div>
        </div>
      )}

      {drag !== null && (
        <div
          ref={ghostRef}
          className="fi-drag-ghost"
          aria-hidden="true"
          style={{ transform: ghostTransform(ghostAt.current.x, ghostAt.current.y) }}
        >
          move {drag.label}
          {drag.over !== null && ` to ${drag.over === "" ? "files/" : `files/${drag.over}/`}`}
        </div>
      )}

      {contextMenu !== null && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          entry={contextMenu.entry}
          count={menuTargets.length}
          canRename={!contextMenu.hit}
          onClose={(returnFocus) => {
            setContextMenu(null);
            if (returnFocus) contextMenu.opener?.focus();
          }}
          onOpen={() => {
            setContextMenu(null);
            if (contextMenu.entry.is_dir) {
              setQInput("");
              setQ("");
              setPath(joinPath(contextMenu.dir, contextMenu.entry.name));
              return;
            }
            handleOpen(joinPath(contextMenu.dir, contextMenu.entry.name));
            contextMenu.opener?.focus();
          }}
          onDownload={() => {
            handleDownload(joinPath(contextMenu.dir, contextMenu.entry.name));
            setContextMenu(null);
            contextMenu.opener?.focus();
          }}
          onRename={() => {
            rename.reset();
            setRenaming({ path: joinPath(path, contextMenu.entry.name), name: contextMenu.entry.name });
            setContextMenu(null);
          }}
          onMove={() => {
            setMoving({ from: contextMenu.dir, entries: menuTargets });
            setContextMenu(null);
          }}
          onCopyPath={() => {
            copyPaths(menuTargets.map((entry) => joinPath(contextMenu.dir, entry.name)));
            setContextMenu(null);
            contextMenu.opener?.focus();
          }}
          onDelete={() => {
            if (!contextMenu.hit) setSelected(new Set(menuTargets.map((entry) => entry.name)));
            setContextMenu(null);
            void deletePaths(menuTargets.map((entry) => joinPath(contextMenu.dir, entry.name)));
          }}
        />
      )}
    </>
  );
}

/** How far the pointer travels with a row held before it is a drag and not a click. */
const DRAG_THRESHOLD_PX = 6;

/** The label that follows a drag, set just below and right of the pointer. */
function ghostTransform(x: number, y: number): string {
  return `translate(${String(x + 14)}px, ${String(y + 14)}px)`;
}

function swallowClick(event: Event) {
  event.stopPropagation();
  event.preventDefault();
}

/** Why moving `entries` from `from` into `destination` cannot go, said before any request — or null when it can. */
function moveRefusal(from: string, entries: Entry[], destination: string): string | null {
  if (destination === from) return entries.length === 1 ? "that is where it is now" : "that is where they are now";
  const intoItself = entries.some((entry) => {
    if (!entry.is_dir) return false;
    const inner = joinPath(from, entry.name);
    return destination === inner || destination.startsWith(`${inner}/`);
  });
  return intoItself ? "a folder cannot go inside itself" : null;
}

/** A row the keyboard should land on once the rows named in `gone` have left the listing. */
interface FocusAfter {
  name: string;
  gone: string[];
}

/** A note with a way to put it away, the button outside whatever the note announces. */
function Dismissable({ label, onDismiss, children }: { label: string; onDismiss: () => void; children: ReactNode }) {
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
function FailureNotes({
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

function pageHeadline(rootUnavailable: boolean, entries: Entry[] | undefined, failing: boolean): string | undefined {
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
    files > 0 ? `${files} file${files === 1 ? "" : "s"} totalling ${formatBytes(totalBytes)}` : null,
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
function FilesUnavailableTeach() {
  return (
    <Teach title="The files folder could not be opened">
      <p>
        The núcleo keeps one folder for everything this page holds — your uploads, drops from Windows, filed
        mail and team workspaces. When it started, it could not create or open that folder, so every action
        here is refused rather than pointed at somewhere else.
      </p>
      <p>
        The folder belongs at <code>%LOCALAPPDATA%\nucleos\NucleOS\data\files</code>. Check that the path
        exists and can be written to, then restart the núcleo — its log names the error it hit.
      </p>
    </Teach>
  );
}

/* ---------------------------------------------------------------- tree -- */

function FolderTree({
  currentPath,
  onSelect,
  label = "Folders",
  marked = true,
  herePath,
  dropTargets = false,
}: {
  currentPath: string;
  onSelect: (path: string) => void;
  /** Whether a dragged row can be let go on its folders — the side tree's, not the move form's copy. */
  dropTargets?: boolean;
  /** The landmark's name — the side column's tree, or the one the move form picks from. */
  label?: string;
  /** Whether `currentPath` is drawn as chosen. The move form's tree opens on it without it being an answer yet. */
  marked?: boolean;
  /** A folder to say "(here)" beside — where the move form's items already are. */
  herePath?: string;
}) {
  return (
    // The `nav` wraps the `Inset` rather than wearing it: `Inset` renders a
    // `div` or an `li` and is deliberately not widened, and the landmark with
    // its label is what tells a screen reader this column is the folder tree.
    // `--fi-deepest` is how deep the open path runs, so the indent can share
    // the column between that many steps and still leave the names room.
    <nav aria-label={label} style={{ "--fi-deepest": Math.max(3, pathSegments(currentPath).length) } as CSSProperties}>
      <Inset className="fi-tree">
        {/* A tree, walked with the arrows the way Explorer's is, and one Tab
            stop — the folder on screen. It was a column of buttons, two stops
            per folder (the arrow and the name), and twice that again with the
            move form's copy open beside it. */}
        <div role="tree" aria-label={label} onKeyDown={handleTreeKey}>
          <TreeNode
            path=""
            label="files"
            currentPath={currentPath}
            marked={marked}
            herePath={herePath}
            onSelect={onSelect}
            dropTargets={dropTargets}
            depth={0}
          />
        </div>
      </Inset>
    </nav>
  );
}

/**
 * The tree's keys, read off the DOM: the items on screen are the visible ones,
 * since a folded branch renders no children. Up and Down walk them, Right opens
 * a folded folder or steps into an open one, Left folds or steps out, Enter and
 * Space go to the folder. Expanding and choosing go through the same buttons
 * the pointer uses, so there is one path for each.
 */
function handleTreeKey(event: React.KeyboardEvent<HTMLDivElement>) {
  const item = (event.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
  if (item === null) return;
  const items = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('[role="treeitem"]'));
  const at = items.indexOf(item);
  const expanded = item.getAttribute("aria-expanded");
  const toggle = item.querySelector<HTMLElement>(":scope > .fi-tree-row > .fi-tree-toggle");
  let next: HTMLElement | null | undefined;
  switch (event.key) {
    case "ArrowDown":
      next = items[at + 1];
      break;
    case "ArrowUp":
      next = items[at - 1];
      break;
    case "Home":
      next = items[0];
      break;
    case "End":
      next = items[items.length - 1];
      break;
    case "ArrowRight":
      if (expanded === "false") toggle?.click();
      else if (expanded === "true") next = item.querySelector<HTMLElement>(':scope > .fi-tree-children > [role="treeitem"]');
      break;
    case "ArrowLeft":
      if (expanded === "true" && toggle !== null) toggle.click();
      else next = item.parentElement?.closest<HTMLElement>('[role="treeitem"]');
      break;
    case "Enter":
    case " ":
      item.querySelector<HTMLElement>(":scope > .fi-tree-row [data-tree-select]")?.click();
      break;
    default:
      return;
  }
  event.preventDefault();
  event.stopPropagation();
  next?.focus();
}

/** Whether `path` is `currentPath` itself or a folder above it — the branch the tree must show open. */
function isOnTheWay(path: string, currentPath: string): boolean {
  return path === "" || currentPath === path || currentPath.startsWith(`${path}/`);
}

/**
 * One branch of the tree, lazy: a node's own children are asked for only once
 * it is open. The root is open from the start so the first screen is not
 * empty; every folder under it starts collapsed — except the ones on the way
 * to the folder on screen, which open by themselves, so arriving from the
 * table or the breadcrumbs still leaves the tree saying where you are.
 */
function TreeNode({
  path,
  label,
  currentPath,
  marked,
  herePath,
  onSelect,
  dropTargets,
  depth,
}: {
  path: string;
  label: string;
  currentPath: string;
  marked: boolean;
  herePath: string | undefined;
  onSelect: (path: string) => void;
  dropTargets: boolean;
  depth: number;
}) {
  const [open, setOpen] = useState(depth === 0 || (isOnTheWay(path, currentPath) && currentPath !== path));
  const listing = useFolder(path, open);
  const dirs = (listing.data ?? []).filter((entry) => entry.is_dir);
  const active = marked && path === currentPath;
  const here = herePath === path;
  // Known to hold no folders: then it has nothing to expand, and says so by
  // carrying no `aria-expanded` at all.
  const leaf = open && listing.data !== undefined && dirs.length === 0;
  // A listing that failed must not draw as a folder with no subfolders: the
  // tree is on screen the whole time, and an unread branch that looks empty is
  // this page claiming a currency it does not have.
  const fault = !listing.isError ? null : listing.data === undefined ? "unread" : "stale";

  // Opens, never closes: a branch a person folded by hand stays folded unless
  // the folder on screen moves somewhere beneath it.
  useEffect(() => {
    if (isOnTheWay(path, currentPath) && currentPath !== path) setOpen(true);
  }, [path, currentPath]);

  const spoken = [label, here ? "here" : null, fault].filter((part): part is string => part !== null).join(", ");

  return (
    <div
      className="fi-tree-node"
      role="treeitem"
      aria-label={spoken}
      aria-level={depth + 1}
      aria-expanded={leaf ? undefined : open}
      aria-selected={active}
      tabIndex={path === currentPath ? 0 : -1}
    >
      {/* Depth as a custom property and the step as a token, rather than a
          pixel count multiplied inline. */}
      <div
        className={active ? "fi-tree-row ui-current" : "fi-tree-row"}
        style={{ "--fi-depth": depth } as CSSProperties}
        data-drop-path={dropTargets ? path : undefined}
      >
        {/* The root has no toggle and no spacer where one would be: it is always
            open, and the blank 1.5rem left the current-folder rule standing apart
            from its name like a stray bar. */}
        {depth > 0 && (
          <button
            type="button"
            tabIndex={-1}
            className="fi-tree-toggle"
            aria-label={open ? `Collapse ${label}` : `Expand ${label}`}
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {open ? "▾" : "▸"}
          </button>
        )}
        <Button
          variant="link"
          tabIndex={-1}
          data-tree-select
          aria-current={active ? "location" : undefined}
          onClick={() => onSelect(path)}
        >
          <span className="fi-tree-label" title={label}>
            {label}
          </span>
        </Button>
        {here && <span className="fi-tree-here">(here)</span>}
        {fault !== null && (
          <span
            className={fault === "unread" ? "fi-tree-fault fi-tree-fault-unread" : "fi-tree-fault"}
            title={
              fault === "unread"
                ? "the núcleo could not list this folder — open it to see why"
                : "this branch is the last listing that loaded; the latest one failed"
            }
          >
            {fault}
          </span>
        )}
      </div>
      {open && dirs.length > 0 && (
        <div className="fi-tree-children" role="group">
          {dirs.map((dir) => (
            <TreeNode
              key={dir.name}
              path={joinPath(path, dir.name)}
              label={dir.name}
              currentPath={currentPath}
              marked={marked}
              herePath={herePath}
              onSelect={onSelect}
              dropTargets={dropTargets}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/* ---------------------------------------------------------- breadcrumbs -- */

function Breadcrumbs({ path, onGo }: { path: string; onGo: (path: string) => void }) {
  const segments = pathSegments(path);
  const nav = useRef<HTMLElement>(null);
  // How many folders after "files" fold into "…". The path is one line and
  // never two: the far end goes first, since the near end is where you are and
  // the root is the way home. Measured, not guessed from a character count —
  // names are mono but the column is whatever the window leaves.
  const [folded, setFolded] = useState(0);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => setFolded(0), [path, width]);
  useLayoutEffect(() => {
    const element = nav.current;
    if (element === null) return;
    // The folder you are in may shrink to an ellipsis, which hides the overflow
    // from the bar's own measure: it counts as too long while it is cut.
    const here = element.querySelector<HTMLElement>(".fi-crumb-here");
    const over = element.scrollWidth > element.clientWidth || (here !== null && here.scrollWidth > here.clientWidth);
    if (over && folded < segments.length - 1) setFolded(folded + 1);
  }, [folded, segments.length]);
  useLayoutEffect(() => {
    const element = nav.current;
    if (element === null || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry !== undefined) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const hidden = segments.slice(0, folded);
  // The folded folders open from "…" as the deepest of them, and name
  // themselves on hover: a jump one step past what is still written out.
  const lastHidden = pathUpTo(path, folded);

  // The folder you are in is a place, not a control: plain text carrying
  // `aria-current`, where it used to be a disabled button that dropped out of
  // the tab order without ever saying "you are here".
  return (
    // A path bar, not a trail of links floating over the toolbar: at the root
    // the lone "files" read as a stray word, and a bar is where the eye
    // already looks for "where am I" in every file manager.
    <nav ref={nav} className="fi-crumbs" aria-label="Folder path">
      <FolderIcon className="fi-kind" strokeWidth={1.5} aria-hidden="true" />
      {path === "" ? (
        <span className="fi-crumb fi-crumb-here" aria-current="location">
          files
        </span>
      ) : (
        <Button variant="link" data-drop-path="" onClick={() => onGo("")}>
          <span className="fi-crumb">files</span>
        </Button>
      )}
      {folded > 0 && (
        <Button
          variant="link"
          data-drop-path={lastHidden}
          title={hidden.join(" / ")}
          aria-label={`Go to ${hidden.join(" / ")}`}
          onClick={() => onGo(lastHidden)}
        >
          <span className="fi-crumb">/ …</span>
        </Button>
      )}
      {segments.map((segment, index) =>
        index < folded ? null : index === segments.length - 1 ? (
          <span
            key={`${segment}-${String(index)}`}
            className="fi-crumb fi-crumb-here"
            aria-current="location"
            title={segment}
          >
            / {segment}
          </span>
        ) : (
          <Button
            key={`${segment}-${String(index)}`}
            variant="link"
            data-drop-path={pathUpTo(path, index + 1)}
            onClick={() => onGo(pathUpTo(path, index + 1))}
          >
            <span className="fi-crumb">/ {segment}</span>
          </Button>
        ),
      )}
    </nav>
  );
}

/* -------------------------------------------------------------- upload -- */

/**
 * The picker. It only hands the files over: the page runs every upload, from
 * this button and from a drop alike, through one loop and one report.
 *
 * The daemon's ceiling is said beside it, before a file is picked, rather than
 * only in the refusal after one too large was.
 */
function UploadButton({ busy, onFiles }: { busy: boolean; onFiles: (files: File[]) => void }) {
  return (
    <div className="fi-upload">
      <label className="fi-upload-label" title={`Up to ${MAX_UPLOAD_LABEL} each`}>
        <span>Upload</span>
        <input
          type="file"
          className="sr-only"
          multiple
          disabled={busy}
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            // Cleared so picking the same file again is still a change.
            event.target.value = "";
            if (files.length > 0) onFiles(files);
          }}
        />
      </label>
      <span className="fi-upload-limit">up to {MAX_UPLOAD_LABEL} each</span>
    </div>
  );
}

/**
 * The one question an upload asks: some names are already taken where the
 * files are going.
 *
 * It used to be no question at all — the daemon numbers a clash
 * (`report.docx` becomes `report (2).docx`) and the page said so only in the
 * report afterwards, where a person who meant to update the file found a
 * second copy instead. Replace is the answer that means "update": the old file
 * goes to Recently deleted, not away. The pending tone, as the not-empty note:
 * nothing is wrong yet, the page is waiting on a person.
 */
/**
 * How many clashing names the question lists before counting the rest. The
 * answer is one choice for all of them, so the list is there to recognise the
 * batch, not to be read to its end; a drop of forty names pushed the buttons
 * off the screen.
 */
const CLASH_SHOWN = 5;

function ClashNote({
  batch,
  onChoose,
  onCancel,
}: {
  batch: PendingBatch;
  onChoose: (choice: ClashChoice) => void;
  onCancel: () => void;
}) {
  const headingId = useId();
  const noteRef = useRef<HTMLDivElement>(null);
  // Focus on the answer that loses nothing, as the not-empty note lands on
  // Leave: Enter pressed on arrival keeps both files rather than replacing one.
  useEffect(() => {
    noteRef.current?.querySelector<HTMLElement>("[data-safe]")?.focus();
  }, []);
  const names = batch.clashes.flatMap((index) => {
    const item = batch.items[index];
    return item === undefined ? [] : [item];
  });
  const one = names.length === 1;
  const others = batch.items.length - names.length;
  // Written as a path, the way the move form writes its destination: "the
  // files root" in the path's mono face read as a folder named that.
  const folder = names[0]?.folder ?? "";
  const where = folder === "" ? "files/" : `files/${folder}/`;

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
      <p id={headingId}>
        {one ? "A file with this name is already in " : `${String(names.length)} files with these names are already in `}
        <span className="fi-ask-where">{where}</span>:
      </p>
      <ul className="fi-ask-list">
        {names.slice(0, CLASH_SHOWN).map((item, index) => (
          <li key={`${item.folder}/${item.name}-${String(index)}`}>{item.name}</li>
        ))}
        {names.length > CLASH_SHOWN && <li className="fi-ask-more">and {names.length - CLASH_SHOWN} more</li>}
      </ul>
      <div className="fi-ask-actions">
        {/* The answer that loses nothing first, and the filled one — the fill is
            `files.css`'s, on `data-safe`: Replace led, and the pointer met the lossy
            choice before the one focus lands on. */}
        <Button data-safe onClick={() => onChoose("keep")}>
          Keep both
        </Button>
        <Button variant="ghost" onClick={() => onChoose("replace")}>
          Replace
        </Button>
        <Button variant="ghost" onClick={() => onChoose("skip")}>
          {/* Named for what still happens when others are going: "Skip those"
              left a person to work out that the rest went up regardless. */}
          {others > 0 ? "Upload the rest" : `Skip ${one ? "it" : "those"}`}
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          Cancel upload
        </Button>
      </div>
      {/* Under the buttons, as their footnote: above them it was one more
          paragraph to read before the choice it explains came into view. */}
      <p className="fi-ask-then">
        Replace moves the {one ? "old one" : "old ones"} to Recently deleted. Keep both saves the new{" "}
        {one ? "file" : "files"} under a numbered name.
        {others > 0 && ` The other ${String(others)} ${others === 1 ? "file goes" : "files go"} up either way.`}
      </p>
    </div>
  );
}

/**
 * What a batch of uploads came to, whichever door it came through.
 *
 * The names the daemon stored are listed, and a name that differs from the one
 * sent says so beside it — `safe_filename` rewrites what a disk cannot carry,
 * and a Keep both adds a number. Every file that stayed behind is named with
 * its reason, each on its own line even when two reasons read the same.
 */
/**
 * How many lines of one kind the report shows before folding the rest. A drop
 * of two hundred files listed every one back, and pushed the table off the
 * screen until it was dismissed.
 */
const REPORT_SHOWN = 5;

/** A list cut to {@link REPORT_SHOWN}, the rest behind one line that counts them. */
function FoldedList({ children }: { children: ReactNode[] }) {
  const shown = children.slice(0, REPORT_SHOWN);
  const rest = children.slice(REPORT_SHOWN);
  return (
    <>
      <ul className="fi-drop-refusals">{shown}</ul>
      {rest.length > 0 && (
        <details className="fi-drop-more">
          <summary>and {rest.length} more</summary>
          <ul className="fi-drop-refusals">{rest}</ul>
        </details>
      )}
    </>
  );
}

function UploadReportNote({ report, onDismiss }: { report: UploadReport; onDismiss: () => void }) {
  const from = report.source === "drop" ? "that drop" : "that upload";
  const total = report.saved.length + report.refusals.length;
  // A name the daemon changed first: those are the lines the report is for.
  const saved = [...report.saved].sort(
    (a, b) => Number(b.stored !== b.sent) - Number(a.stored !== a.sent),
  );
  const skippedShown = report.skipped.slice(0, REPORT_SHOWN);
  const skippedMore = report.skipped.length - skippedShown.length;
  return (
    <div className="fi-drop-report">
      <div role="status">
        {report.truncated && <p>The drop was cut short by the host before this window saw everything.</p>}
        {total === 0 ? (
          <p>Nothing from {from} was sent.</p>
        ) : report.refusals.length === 0 ? (
          <p>
            {total === 1 ? `The file from ${from} was uploaded` : `Every file from ${from} was uploaded`}
            {report.saved.length > 0 ? " as:" : "."}
          </p>
        ) : (
          <p>
            {report.refusals.length} of {total} file{total === 1 ? "" : "s"} from {from} stayed behind:
          </p>
        )}
        {report.refusals.length > 0 && (
          <FoldedList>
            {report.refusals.map((refusal, index) => (
              <li key={`${refusal.name}-${String(index)}`}>{`${refusal.name} — ${refusal.reason}`}</li>
            ))}
          </FoldedList>
        )}
        {report.refusals.length > 0 && report.saved.length > 0 && <p>Uploaded as:</p>}
        {saved.length > 0 && (
          <FoldedList>
            {saved.map((item, index) => (
              <li key={`${item.stored}-${String(index)}`}>
                <span>{item.stored}</span>
                {item.stored !== item.sent && <span className="fi-drop-renamed"> — renamed from {item.sent}</span>}
              </li>
            ))}
          </FoldedList>
        )}
        {report.replaced > 0 && (
          <p>
            {report.replaced === 1 ? "The older copy is" : `The ${String(report.replaced)} older copies are`} in Recently
            deleted.
          </p>
        )}
        {report.skipped.length > 0 && (
          <p>
            Skipped, already there: {skippedShown.join(", ")}
            {skippedMore > 0 && ` and ${String(skippedMore)} more`}.
          </p>
        )}
      </div>
      <Button variant="quiet" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}

/* ----------------------------------------------------------- new folder -- */

/**
 * A button until it is wanted. Making a folder is occasional, and an open
 * field beside the search took half the toolbar for it; opened, the field
 * takes focus, Escape puts it away, and the refusal lands under the field that
 * caused it rather than at the foot of the page.
 */
function NewFolderForm({ path, create }: { path: string; create: ReturnType<typeof useCreateFolder> }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");

  function close() {
    setOpen(false);
    setName("");
    create.reset();
  }

  if (!open) {
    return (
      <Button
        variant="ghost"
        onClick={() => {
          create.reset();
          setOpen(true);
        }}
      >
        New folder
      </Button>
    );
  }

  return (
    <form
      className="fi-new-folder"
      onSubmit={(event) => {
        event.preventDefault();
        const trimmed = name.trim();
        if (trimmed === "" || create.isPending) return;
        create.mutate(joinPath(path, trimmed), { onSuccess: close });
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") close();
      }}
    >
      <div className="fi-new-folder-row">
        <Field label="New folder">
          <input
            // eslint-disable-next-line jsx-a11y/no-autofocus -- the field was
            // opened by the button that just vanished; focus belongs here.
            autoFocus
            value={name}
            aria-label="New folder name"
            onChange={(event) => setName(event.target.value)}
          />
        </Field>
        <Button type="submit" disabled={name.trim() === "" || create.isPending}>
          Create folder
        </Button>
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
      </div>
      {create.isError && (
        <RefusalOrError error={create.error} sentences={CREATE_FOLDER_SENTENCES} what="that folder was not created" />
      )}
    </form>
  );
}

/* --------------------------------------------------------------- table -- */

interface FileTableProps {
  tableRef: RefObject<HTMLDivElement | null>;
  path: string;
  folder: ReturnType<typeof useFolder>;
  /** The listing in the order on screen — the page sorts it once and reads it too. */
  ordered: Entry[];
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
  selected: Set<string>;
  onToggleSelect: (name: string) => void;
  onAddToSelection: (names: string[]) => void;
  onSelectAll: (names: string[]) => void;
  focusIndex: number | null;
  onFocusRow: (index: number) => void;
  focusAfter: FocusAfter | null;
  onFocusLanded: () => void;
  renaming: { path: string; name: string } | null;
  /** Why the last rename was refused — said under the field it came from, not above the table. */
  renameError: unknown;
  onStartRename: (entry: Entry) => void;
  onSubmitRename: (from: string, name: string) => void;
  onCancelRename: () => void;
  onOpen: (entry: Entry) => void;
  onContextMenu: (entry: Entry, x: number, y: number, opener: HTMLElement | null) => void;
  onDelete: (names: string[]) => void;
  onGoUp: () => void;
  /** The rows a Ctrl+X is holding, when they are in this folder. */
  cutNames: Set<string> | null;
  onCancelCut: () => void;
  onDragStart: (entry: Entry, event: React.PointerEvent) => void;
}

const COLUMN_LABEL: Record<SortColumn, string> = { name: "Name", size: "Size", modified: "Modified" };

/** The name button of row `index` — the element a row's keyboard focus actually lands on. */
function rowButton(container: HTMLElement | null, index: number): HTMLElement | null {
  return container?.querySelector<HTMLElement>(`[data-row-open="${String(index)}"]`) ?? null;
}

function FileTable(props: FileTableProps) {
  const {
    tableRef,
    path,
    folder,
    ordered,
    sort,
    onSort,
    selected,
    onToggleSelect,
    onAddToSelection,
    onSelectAll,
    focusIndex,
    onFocusRow,
    focusAfter,
    onFocusLanded,
    renaming,
    renameError,
    onStartRename,
    onSubmitRename,
    onCancelRename,
    onOpen,
    onContextMenu,
    onDelete,
    onGoUp,
    cutNames,
    onCancelCut,
    onDragStart,
  } = props;

  const [anchor, setAnchor] = useState<number | null>(null);
  const selectAllRef = useRef<HTMLInputElement>(null);
  const keysId = useId();
  const [nameHead, setNameHead] = useState<HTMLTableCellElement | null>(null);
  const nameMax = useNameFit(nameHead);

  const stale = folder.isError && folder.data !== undefined;
  const entries = folder.data;
  // One Tab stop for the whole listing — the row the keyboard was last on, the
  // first before that. Every row's checkbox and name were both in the tab
  // order, so leaving a sixty-row folder took a hundred and twenty presses for
  // what the arrows already do.
  const tabStop = focusIndex !== null && focusIndex < ordered.length ? focusIndex : 0;
  const selectedHere = ordered.filter((entry) => selected.has(entry.name)).length;

  // "Some but not all" is a third state a checkbox can show and a screen
  // reader can say ("mixed"); without it a partial selection read as none.
  useEffect(() => {
    if (selectAllRef.current !== null) {
      selectAllRef.current.indeterminate = selectedHere > 0 && selectedHere < ordered.length;
    }
  }, [selectedHere, ordered.length]);

  /**
   * Moves the keyboard to a row for real. The row used to be only painted as
   * focused while DOM focus stayed on the wrapper, so a screen reader heard
   * nothing as the arrows went down the list; now the row's own name button
   * takes focus, and is read by its name.
   */
  function goTo(index: number) {
    onFocusRow(index);
    rowButton(tableRef.current, index)?.focus();
  }

  // After a delete, the neighbour takes focus once the listing no longer holds
  // what was deleted — before then its index is the old one, and focus would
  // land a row off once the refetch closed the gap.
  useEffect(() => {
    if (focusAfter === null) return;
    // Not read yet: a "Show in folder" arrives before the folder's listing does.
    if (entries === undefined) return;
    if (ordered.some((entry) => focusAfter.gone.includes(entry.name))) return;
    const index = ordered.findIndex((entry) => entry.name === focusAfter.name);
    if (index >= 0) goTo(index);
    else tableRef.current?.focus();
    onFocusLanded();
    // `goTo` and the callbacks are fresh every render; the listing is the trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusAfter, ordered]);

  function selectRange(from: number, to: number) {
    const [low, high] = from <= to ? [from, to] : [to, from];
    onAddToSelection(ordered.slice(low, high + 1).map((entry) => entry.name));
  }

  function openMenuFor(index: number) {
    const entry = ordered[index];
    const button = rowButton(tableRef.current, index);
    if (entry === undefined) return;
    const rect = button?.getBoundingClientRect();
    onContextMenu(entry, rect?.left ?? 0, rect?.bottom ?? 0, button);
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const target = event.target as HTMLElement;
    // A key pressed ON a control inside the table is that control's first. Enter
    // on a name button already opens that row through its own click, and the
    // wrapper opening the focused row as well meant one keypress, two opens.
    const onControl = target !== event.currentTarget && (target.tagName === "BUTTON" || target.tagName === "INPUT");
    const onField = target.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox";

    if (event.key === "Escape") {
      onCancelRename();
      onCancelCut();
      return;
    }
    if (renaming !== null || onField) return;

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
      event.preventDefault();
      onSelectAll(ordered.map((entry) => entry.name));
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Home" || event.key === "End") {
      if (ordered.length === 0) return;
      event.preventDefault();
      const last = ordered.length - 1;
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? last
            : event.key === "ArrowDown"
              ? Math.min(last, (focusIndex ?? -1) + 1)
              : Math.max(0, (focusIndex ?? 0) - 1);
      if (event.shiftKey) selectRange(anchor ?? focusIndex ?? next, next);
      else setAnchor(next);
      goTo(next);
      return;
    }
    if (event.key === " " && target.tagName !== "INPUT") {
      // Space picks the row, the way it does in every file list — never
      // activates the name button under it.
      event.preventDefault();
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) {
        onToggleSelect(entry.name);
        setAnchor(focusIndex);
      }
      return;
    }
    if (event.key === "Enter") {
      if (onControl) return;
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onOpen(entry);
      return;
    }
    if (event.key === "F2") {
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onStartRename(entry);
      return;
    }
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      event.preventDefault();
      if (focusIndex !== null) openMenuFor(focusIndex);
      return;
    }
    if (event.key === "Backspace") {
      if (path !== "") onGoUp();
      return;
    }
    if (event.key === "Delete") {
      // A held key auto-repeats, and each repeat would be one more item gone.
      if (event.repeat) return;
      // The selection if there is one, otherwise the row the keyboard is on.
      // One press: the entry goes to the trash with an undo beside it.
      const focused = focusIndex === null ? undefined : ordered[focusIndex];
      const names = selected.size > 0 ? [...selected] : focused !== undefined ? [focused.name] : [];
      if (names.length > 0) onDelete(names);
    }
  }

  return (
    // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- the
    // shortcuts below (arrows, Enter, F2, Delete) need somewhere to land, and
    // a `<table>` is not itself focusable. Out of the tab order once there are
    // rows: the row's own name is then the stop, and the wrapper was a second
    // one in front of it that did nothing a row does not.
    <div
      ref={tableRef}
      className="fi-table-wrap"
      tabIndex={ordered.length > 0 ? -1 : 0}
      // A group, so the name and the description count: on a bare `div` ARIA
      // allows neither, and both were dropped. The keys are the description,
      // not the label — twenty words of shortcuts read before every row, on
      // every visit. On screen they show while the keyboard is in the listing.
      role="group"
      aria-label="Folder contents"
      aria-describedby={ordered.length > 0 ? keysId : undefined}
      onKeyDown={handleKeyDown}
      onKeyUp={(event) => {
        // A button fires its click on Space's keyup. The keydown above already
        // turned Space into "select", so the keyup must not also open the row.
        if (event.key === " " && (event.target as HTMLElement).tagName === "BUTTON") event.preventDefault();
      }}
    >
      {/* No title: the path bar above already names what this is, and "Contents"
          was one more line between the toolbar and the first row. Flat: the
          card round the rows was padding on every side of a table that is the
          page's whole subject, and a second box beside the tree's. */}
      <Panel variant="flat">
        {stale && <StaleNote dataUpdatedAt={folder.dataUpdatedAt} />}
        {folder.isError && entries === undefined && (
          <RefusalOrError error={folder.error} sentences={LIST_SENTENCES} what="nothing is known about this folder" />
        )}
        {!folder.isError && entries === undefined && <p className="fi-loading">reading the folder…</p>}
        {/* `Teach` is for a place nothing has ever been — the root, empty. A
            subfolder that is empty is an ordinary answer, and says so in one line. */}
        {entries !== undefined && entries.length === 0 && path === "" && (
          <Teach title="Nothing has been filed yet">
            <p>
              Upload a file, make a folder, or drag something in from Windows — a drop lands under the folder you
              are looking at.
            </p>
          </Teach>
        )}
        {entries !== undefined && entries.length === 0 && path !== "" && (
          <Quiet says="nothing in this folder yet — upload, drop or make a folder here." />
        )}

        {/* `fi-table-picking` once anything is picked: until then the row boxes
            stay hidden except under the pointer or the keyboard. A click on the
            row already picks it, and seven empty squares down the edge were the
            loudest thing on a page at rest. */}
        {ordered.length > 0 && (
          <table className={selectedHere > 0 ? "fi-table fi-table-picking" : "fi-table"}>
            <thead>
              <tr>
                <th scope="col" className="fi-col-select">
                  <input
                    ref={selectAllRef}
                    type="checkbox"
                    aria-label="Select all"
                    checked={ordered.length > 0 && selectedHere === ordered.length}
                    onChange={(event) => onSelectAll(event.target.checked ? ordered.map((entry) => entry.name) : [])}
                  />
                </th>
                <SortHeader column="name" sort={sort} onSort={onSort} cellRef={setNameHead} />
                <SortHeader column="size" sort={sort} onSort={onSort} />
                <SortHeader column="modified" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {ordered.map((entry, index) => (
                <FileRow
                  key={entry.name}
                  entry={entry}
                  index={index}
                  tabStop={tabStop === index}
                  focused={focusIndex === index}
                  selected={selected.has(entry.name)}
                  anySelected={selected.size > 0}
                  cut={cutNames?.has(entry.name) === true}
                  dropPath={entry.is_dir ? joinPath(path, entry.name) : undefined}
                  onPointerDown={(event) => onDragStart(entry, event)}
                  renaming={renaming?.path === joinPath(path, entry.name) ? renaming : null}
                  renameError={renameError}
                  nameMax={nameMax}
                  onToggleSelect={(extend) => {
                    onFocusRow(index);
                    if (extend && anchor !== null) {
                      selectRange(anchor, index);
                    } else {
                      onToggleSelect(entry.name);
                      setAnchor(index);
                    }
                  }}
                  onRowClick={({ toggle, extend }) => {
                    goTo(index);
                    // Explorer's three: a click picks this row alone, Ctrl adds or
                    // drops it, Shift takes the run from the last row picked —
                    // instead of the selection, or on top of it with Ctrl.
                    if (extend && anchor !== null) {
                      const [low, high] = anchor <= index ? [anchor, index] : [index, anchor];
                      const run = ordered.slice(low, high + 1).map((item) => item.name);
                      if (toggle) onAddToSelection(run);
                      else onSelectAll(run);
                      return;
                    }
                    if (toggle) onToggleSelect(entry.name);
                    else onSelectAll([entry.name]);
                    setAnchor(index);
                  }}
                  onFocus={() => onFocusRow(index)}
                  onOpen={() => {
                    onFocusRow(index);
                    onOpen(entry);
                  }}
                  onContextMenu={(x, y, opener) => {
                    onFocusRow(index);
                    onContextMenu(entry, x, y, opener);
                  }}
                  onSubmitRename={(name) => onSubmitRename(joinPath(path, entry.name), name)}
                  onCancelRename={onCancelRename}
                />
              ))}
            </tbody>
          </table>
        )}
        {ordered.length > 0 && (
          <p id={keysId} className="fi-keys">
            <kbd>↑</kbd> <kbd>↓</kbd> move · <kbd>Shift</kbd>+<kbd>↑</kbd> <kbd>↓</kbd> extend · <kbd>Space</kbd>{" "}
            select · <kbd>Ctrl</kbd>+<kbd>A</kbd> all · <kbd>Enter</kbd> open in Windows · <kbd>F2</kbd> rename ·{" "}
            <kbd>Delete</kbd> to the trash · <kbd>Ctrl</kbd>+<kbd>X</kbd>, then <kbd>Ctrl</kbd>+<kbd>V</kbd> in another
            folder, move · <kbd>Ctrl</kbd>+<kbd>Z</kbd> undo · <kbd>Backspace</kbd> up ·{" "}
            <kbd>Shift</kbd>+<kbd>F10</kbd> more
          </p>
        )}
      </Panel>
    </div>
  );
}

function SortHeader({
  column,
  sort,
  onSort,
  cellRef,
}: {
  column: SortColumn;
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
  cellRef?: (cell: HTMLTableCellElement | null) => void;
}) {
  const active = sort.column === column;
  const ariaSort: "ascending" | "descending" | "none" = !active ? "none" : sort.direction === "asc" ? "ascending" : "descending";
  // Every head carries a mark, so the ones not sorting still read as
  // controls; the idle one is faint until the pointer is on it.
  const mark = (
    <span aria-hidden="true" className={active ? "fi-sort-mark" : "fi-sort-mark fi-sort-idle"}>
      {active ? (sort.direction === "asc" ? "▲" : "▼") : "↕"}
    </span>
  );
  return (
    <th ref={cellRef} scope="col" aria-sort={ariaSort} className={column === "name" ? "fi-col-name" : "fi-col-num"}>
      <button type="button" className="fi-sort-button" onClick={() => onSort(column)}>
        {/* A number's head keeps its mark on the left, so the label's right
            edge is the column's: with the mark after it, SIZE and MODIFIED
            ended a mark's width short of the figures under them. */}
        {column !== "name" && mark}
        {COLUMN_LABEL[column]}
        {column === "name" && mark}
      </button>
    </th>
  );
}

/** How many characters of a name the row shows before cutting it in the middle. */
const NAME_MAX = 56;

/** Fewer than this and the middle cut leaves too little of either end to recognise. */
const NAME_MIN = 12;

/** The name cell's padding, the kind glyph and the gap before the name, in rem. */
const NAME_CHROME_REM = 2.5;

let glyphCanvas: HTMLCanvasElement | null = null;

/**
 * How many characters of a name fit the name column as it is now drawn. The
 * cut happens in the middle and keeps the extension, but only up to a fixed
 * 56: in a narrower column the CSS end-ellipsis took over, and "roster-ex…"
 * lost the one part that says what kind of file it is. The names are mono, so
 * one glyph's advance measures them all. No `ResizeObserver` (jsdom) keeps 56.
 */
function useNameFit(cell: HTMLElement | null): number {
  const [fit, setFit] = useState(NAME_MAX);
  useLayoutEffect(() => {
    if (cell === null || typeof ResizeObserver === "undefined") return;
    const measure = () => {
      const name = cell.closest("table")?.querySelector<HTMLElement>(".fi-entry-name");
      if (name === null || name === undefined) return;
      glyphCanvas ??= document.createElement("canvas");
      const context = glyphCanvas.getContext("2d");
      if (context === null) return;
      const style = getComputedStyle(name);
      context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
      const glyph = context.measureText("0").width;
      if (glyph <= 0) return;
      const room = cell.clientWidth - NAME_CHROME_REM * remPx() - glyph;
      setFit(Math.max(NAME_MIN, Math.min(NAME_MAX, Math.floor(room / glyph))));
    };
    const observer = new ResizeObserver(measure);
    observer.observe(cell);
    return () => observer.disconnect();
  }, [cell]);
  return fit;
}

/** The tooltip on a name: the whole of it when the row shows it cut, and how a file opens. */
function fileTitle(entry: { name: string; is_dir: boolean }, shown: string): string | undefined {
  const full = shown === entry.name ? null : entry.name;
  if (entry.is_dir) return full ?? undefined;
  return full === null ? "Double-click to open in Windows" : `${full} — double-click to open in Windows`;
}

function FileRow({
  entry,
  index,
  tabStop,
  focused,
  selected,
  anySelected,
  cut,
  dropPath,
  onPointerDown,
  renaming,
  renameError,
  nameMax,
  onToggleSelect,
  onRowClick,
  onFocus,
  onOpen,
  onContextMenu,
  onSubmitRename,
  onCancelRename,
}: {
  entry: Entry;
  index: number;
  /** Whether this row's name is the listing's one Tab stop. */
  tabStop: boolean;
  focused: boolean;
  selected: boolean;
  anySelected: boolean;
  /** Held by a Ctrl+X: drawn faint until the paste lands. */
  cut: boolean;
  /** A folder row's path, which a dragged row can be let go on. */
  dropPath: string | undefined;
  onPointerDown: (event: React.PointerEvent) => void;
  renaming: { path: string; name: string } | null;
  renameError: unknown;
  /** How many characters of a name the column has room for. */
  nameMax: number;
  onToggleSelect: (extend: boolean) => void;
  /** A mouse click on the row outside its controls: `toggle` is Ctrl, `extend` is Shift. */
  onRowClick: (keys: { toggle: boolean; extend: boolean }) => void;
  onFocus: () => void;
  onOpen: () => void;
  onContextMenu: (x: number, y: number, opener: HTMLElement | null) => void;
  onSubmitRename: (name: string) => void;
  onCancelRename: () => void;
}) {
  // A folder's trailing slash takes one of the characters the column has.
  const shown = middleEllipsis(entry.name, nameMax - (entry.is_dir ? 1 : 0));
  const classes = ["fi-row"];
  if (focused) classes.push("fi-row-focus");
  if (selected) classes.push("fi-row-selected");
  if (cut) classes.push("fi-row-cut");
  // The name button is what focus lands on, so it is what says the row's
  // state: its whole name even when the screen shows it cut, whether it is a
  // folder (the eye gets that from the icon and the slash), and whether it is
  // picked, which a checkbox one cell away never told anyone arrowing down.
  // "not selected" only once something is: before that it is noise on every row.
  const spoken = [
    entry.name,
    entry.is_dir ? "folder" : null,
    cut ? "cut" : null,
    selected ? "selected" : anySelected ? "not selected" : null,
  ]
    .filter((part): part is string => part !== null)
    .join(", ");
  return (
    // A row picks itself on a click, as a row in Explorer does: the checkbox was
    // the only way in, a small target a cell away from the name a person aims at.
    // The keyboard has Space for the same thing, so the row needs no role of its own.
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-noninteractive-element-interactions
    <tr
      className={classes.join(" ")}
      data-drop-path={dropPath}
      onPointerDown={onPointerDown}
      onClick={(event) => {
        const target = event.target as HTMLElement;
        // Only a plain single click: the second of a double-click is the open,
        // and a click the keyboard made (`detail` 0) belongs to the control it
        // landed on. The checkbox and the rename field answer for themselves,
        // and a folder's name opens the folder.
        if (event.detail !== 1) return;
        if (target.closest("input, form") !== null) return;
        const toggle = event.ctrlKey || event.metaKey;
        if (entry.is_dir && !toggle && !event.shiftKey && target.closest("[data-row-open]") !== null) return;
        onRowClick({ toggle, extend: event.shiftKey });
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        const opener = event.currentTarget.querySelector<HTMLElement>("[data-row-open]");
        onContextMenu(event.clientX, event.clientY, opener);
      }}
    >
      <td className="fi-col-select">
        {/* Out of the tab order: Space on the row picks it, and a second stop
            per row is what made the listing a wall to Tab through. */}
        <input
          type="checkbox"
          tabIndex={-1}
          aria-label={`Select ${entry.name}`}
          checked={selected}
          // Shift on the click extends from the last row picked, as a file list does.
          onChange={(event) => onToggleSelect((event.nativeEvent as MouseEvent).shiftKey)}
        />
      </td>
      <td className="fi-col-name">
        {renaming !== null ? (
          <RenameForm initialName={renaming.name} error={renameError} onSubmit={onSubmitRename} onCancel={onCancelRename} />
        ) : (
          <span className="fi-name-cell">
            {entry.is_dir ? (
              <FolderIcon className="fi-kind fi-kind-dir" strokeWidth={1.5} aria-hidden="true" />
            ) : (
              <FileIcon className="fi-kind" strokeWidth={1.5} aria-hidden="true" />
            )}
            {/* A folder opens on one click — it stays in this window. A file
                opens on a double-click or Enter, as in Explorer: it hands focus
                to another app, and one click meant to pick a row beside an IDE
                threw Acrobat in front of both; that click picks the row instead.
                `detail` is 0 on a click the keyboard made, so Enter still opens
                through here. */}
            <Button
              variant="link"
              data-row-open={index}
              tabIndex={tabStop ? 0 : -1}
              aria-label={spoken}
              title={fileTitle(entry, shown)}
              onFocus={onFocus}
              onClick={(event) => {
                // Ctrl or Shift on a folder's name adds it to the selection, as
                // everywhere else on the row: it went into the folder, and the
                // way in threw away every row picked so far.
                if (event.ctrlKey || event.metaKey || event.shiftKey) return;
                if (entry.is_dir || event.detail === 0) onOpen();
              }}
              onDoubleClick={() => {
                if (!entry.is_dir) onOpen();
              }}
            >
              <span className={entry.is_dir ? "fi-entry-name fi-entry-dir" : "fi-entry-name"}>
                {shown}
                {entry.is_dir ? "/" : ""}
              </span>
            </Button>
          </span>
        )}
      </td>
      {/* A dash, not a blank: a folder's size is left out on purpose, and an
          empty cell reads as a figure that failed to load. */}
      <td className="fi-col-num">{entry.is_dir ? <span title="not measured for folders">—</span> : formatBytes(entry.size_bytes)}</td>
      <td className="fi-col-num">
        {entry.modified === null ? <span title="the disk did not say when">—</span> : <RelativeTime at={entry.modified} />}
      </td>
    </tr>
  );
}

/**
 * The name field, in the row it renames. A refusal is said under it and tied
 * to it: it used to land above the table, a screen away from the field a
 * person was still typing in, and focus never went to find it.
 */
function RenameForm({
  initialName,
  error,
  onSubmit,
  onCancel,
}: {
  initialName: string;
  error: unknown;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initialName);
  const errorId = useId();
  const refused = error !== null && error !== undefined;
  return (
    <div className="fi-rename-wrap">
      <form
        className="fi-rename"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = value.trim();
          if (trimmed !== "") onSubmit(trimmed);
        }}
      >
        <input
          // eslint-disable-next-line jsx-a11y/no-autofocus -- F2 and the context
          // menu's Rename both open this form to be typed into immediately.
          autoFocus
          value={value}
          aria-label="New name"
          aria-invalid={refused || undefined}
          aria-describedby={refused ? errorId : undefined}
          onChange={(event) => setValue(event.target.value)}
        />
        <Button type="submit">Rename</Button>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </form>
      {refused && (
        <div id={errorId}>
          <RefusalOrError error={error} sentences={MOVE_SENTENCES} what="that rename did not go through" />
        </div>
      )}
    </div>
  );
}

/* -------------------------------------------------------------- search -- */

function SearchResults({
  result,
  onOpenFolder,
  onOpenFile,
  onReveal,
  onContextMenu,
}: {
  result: ReturnType<typeof useFileSearch>;
  onOpenFolder: (path: string) => void;
  /** The table's own open: a hit is the same file, and it used to download instead. */
  onOpenFile: (path: string) => void;
  /** Opens the hit's folder with the hit picked and the keyboard on it. */
  onReveal: (hit: Hit) => void;
  /** The table's row menu, for a hit: the same verbs on the same file. */
  onContextMenu: (hit: Hit, x: number, y: number, opener: HTMLElement | null) => void;
}) {
  const hits = result.data?.hits ?? [];
  return (
    <Panel
      variant="flat"
      title="Search results"
      aside={<Count n={result.data === undefined ? undefined : hits.length} noun="hit" />}
    >
      {/* Hits kept from an earlier search are marked as such, the way the table
          marks a stale listing — they may no longer be where they say. */}
      {result.isError && result.data !== undefined && <StaleNote dataUpdatedAt={result.dataUpdatedAt} />}
      {result.isError && <RefusalOrError error={result.error} sentences={SEARCH_SENTENCES} what="that search did not go through" />}
      {!result.isError && result.data === undefined && <p className="fi-loading">searching…</p>}
      {/* The one-line absence, and `Quiet` rather than `.fi-loading`'s faint:
          here the sentence IS what the panel has to say, and an answer set
          fainter than the labels around it reads as failure rather than as
          emptiness. The wait above keeps the faint rung for the same reason. */}
      {result.data !== undefined && hits.length === 0 && <Quiet says="nothing under this folder matches." />}
      {result.data?.truncated === true && (
        <p className="fi-truncated" role="status">
          stopped early — this search hit the daemon's own ceiling before finishing the whole tree. Narrow it to
          see everything.
        </p>
      )}
      {hits.length > 0 && (
        // The row menu from anywhere on a hit, as on a row of the table: a hit
        // could be opened and nothing else, and "find what the agent wrote and
        // tidy it" ended at a list that could not tidy anything. The keyboard's
        // way in is Shift+F10 on the name, as in the table.
        // eslint-disable-next-line jsx-a11y/no-static-element-interactions
        <div
          onContextMenu={(event) => {
            const opener = (event.target as HTMLElement).closest("li")?.querySelector<HTMLElement>("[data-hit]") ?? null;
            const hit = hits[Number(opener?.dataset.hit ?? -1)];
            if (hit === undefined) return;
            event.preventDefault();
            onContextMenu(hit, event.clientX, event.clientY, opener);
          }}
        >
          <Rows label="Search hits">
            {hits.map((hit, index) => (
              <Row key={hit.path} layout="line">
                {/* The table's rule: a folder on one click, a file on a double-click or Enter. */}
                <Button
                  variant="link"
                  data-hit={index}
                  title={hit.is_dir ? undefined : "Double-click to open in Windows"}
                  onKeyDown={(event) => {
                    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                      event.preventDefault();
                      const rect = event.currentTarget.getBoundingClientRect();
                      onContextMenu(hit, rect.left, rect.bottom, event.currentTarget);
                    }
                  }}
                  onClick={(event) => {
                    if (hit.is_dir) onOpenFolder(hit.path);
                    else if (event.detail === 0) onOpenFile(hit.path);
                  }}
                  onDoubleClick={() => {
                    if (!hit.is_dir) onOpenFile(hit.path);
                  }}
                >
                  <span className="fi-entry-name">
                    {hit.name}
                    {hit.is_dir ? "/" : ""}
                  </span>
                </Button>
                <span className="fi-hit-path">{hit.path}</span>
                <span className="fi-hit-size">{hit.is_dir ? "" : formatBytes(hit.size_bytes)}</span>
                {/* The column the table has and the hits did not: which of them
                    is the one just written is most of why a person searches. */}
                <span className="fi-hit-when">
                  {hit.modified === null ? <span title="the disk did not say when">—</span> : <RelativeTime at={hit.modified} />}
                </span>
                {!hit.is_dir && (
                  <Button variant="ghost" aria-label={`Show ${hit.name} in its folder`} onClick={() => onReveal(hit)}>
                    Show in folder
                  </Button>
                )}
              </Row>
            ))}
          </Rows>
        </div>
      )}
    </Panel>
  );
}

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
function MoveForm({
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
    for (const entry of entries) {
      const from = joinPath(path, entry.name);
      const to = joinPath(destination, entry.name);
      try {
        await move.mutateAsync({ from, to });
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
          onSelect={(next) => {
            // Picking the picked folder again is the go-ahead: Enter twice, or a
            // double-click, moves without the trip down to the button.
            if (picked && next === destination) {
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
              <span className="fi-move-again"> — Enter or click it again to move</span>
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
function NotEmptyNote({
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
function undoSummary(undo: Undo): string {
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
function UndoNote({
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
function undoVerb(undo: Undo): string {
  if (undo.kind === "trash") return "delete";
  return undo.renamed ? "rename" : "move";
}

/**
 * Ctrl+Z, written on the button it stands for. The hint that named it retires
 * after the first open, and nothing else on screen said the key existed.
 * Hidden from the name: `aria-keyshortcuts` on the button says it properly.
 */
function UndoKeys() {
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
function RecentlyDeleted({
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

/* --------------------------------------------------------- context menu -- */

/** How far from the window edge the menu is held, so it never opens half off-screen. */
const MENU_MARGIN = 8;

/** One verb in the row menu, with the key that does the same from the listing, if one does. */
function MenuItem({
  primary = false,
  danger = false,
  keys,
  onClick,
  children,
}: {
  primary?: boolean;
  danger?: boolean;
  keys?: string;
  onClick: () => void;
  children: ReactNode;
}) {
  const classes = ["fi-context-item"];
  if (primary) classes.push("fi-context-item-default");
  if (danger) classes.push("fi-context-item-danger");
  return (
    <button
      type="button"
      role="menuitem"
      tabIndex={-1}
      className={classes.join(" ")}
      aria-keyshortcuts={keys === "Del" ? "Delete" : keys}
      onClick={onClick}
    >
      <span>{children}</span>
      {keys !== undefined && (
        <kbd className="fi-context-keys" aria-hidden="true">
          {keys}
        </kbd>
      )}
    </button>
  );
}

/**
 * The row's actions, reachable from the keyboard as well as the mouse.
 *
 * `role="menu"` carries a contract, and this keeps it: focus moves into the
 * menu when it opens, the arrows and Home/End walk it, Escape closes it and
 * hands focus back to the row it was opened from, and Tab leaves it rather
 * than wandering into the page behind. The Sidebar's switcher chose a
 * disclosure instead to avoid exactly this work; a context menu has no such
 * way out, since a list of verbs on a row is what a menu is.
 */
function ContextMenu({
  x,
  y,
  entry,
  count,
  canRename,
  onClose,
  onOpen,
  onDownload,
  onRename,
  onMove,
  onCopyPath,
  onDelete,
}: {
  x: number;
  y: number;
  entry: Entry;
  /** How many entries Copy path, Move and Delete act on — more than one when the row is part of a selection. */
  count: number;
  /** False on a search hit: the rename field lives in the table's row. */
  canRename: boolean;
  onClose: (returnFocus: boolean) => void;
  onOpen: () => void;
  onDownload: () => void;
  onRename: () => void;
  onMove: () => void;
  onCopyPath: () => void;
  onDelete: () => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const many = count > 1;
  const [position, setPosition] = useState({ left: x, top: y });

  // Measured before paint and pulled back inside the window: at 800x600 a
  // right-click near the bottom opened the menu below the visible edge.
  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (menu === null) return;
    const { width, height } = menu.getBoundingClientRect();
    setPosition({
      left: Math.max(MENU_MARGIN, Math.min(x, window.innerWidth - width - MENU_MARGIN)),
      top: Math.max(MENU_MARGIN, Math.min(y, window.innerHeight - height - MENU_MARGIN)),
    });
  }, [x, y]);

  // Through a ref: the page hands a fresh `onClose` on every render, and the
  // listing polls, so an effect keyed on it re-ran — and re-focused the first
  // item — every few seconds while a person was choosing.
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });
  useEffect(() => {
    menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    function handlePointerDown() {
      onCloseRef.current(false);
    }
    document.addEventListener("mousedown", handlePointerDown);
    return () => document.removeEventListener("mousedown", handlePointerDown);
  }, []);

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    // Kept inside the menu: the table under it listens for the same keys.
    event.stopPropagation();
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    const current = items.indexOf(document.activeElement as HTMLElement);
    const focusAt = (index: number) => items[(index + items.length) % items.length]?.focus();
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(current + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(current - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(items.length - 1);
    } else if (event.key === "Escape" || event.key === "Tab") {
      event.preventDefault();
      onClose(true);
    }
  }

  return (
    <div
      ref={menuRef}
      className="fi-context-menu"
      role="menu"
      aria-label={count > 1 ? `Actions for ${String(count)} selected items` : `Actions for ${entry.name}`}
      style={{ left: position.left, top: position.top }}
      onMouseDown={(event) => event.stopPropagation()}
      onKeyDown={handleKeyDown}
    >
      {/* One scope at a time, as Explorer's menu is. Inside a selection it
          offers only what acts on the whole of it; on a single row, the row's
          verbs and then the rest below a rule. Both scopes at once was six
          items under two captions, Rename two lines above a red "Delete 2". */}
      {many ? (
        <div role="group" aria-label={`${String(count)} selected`} className="fi-context-group">
          <span className="fi-context-caption" aria-hidden="true">
            {count} selected
          </span>
          <MenuItem onClick={onCopyPath}>Copy {count} full paths</MenuItem>
          <MenuItem onClick={onMove}>Move {count}…</MenuItem>
          <MenuItem danger keys="Del" onClick={onDelete}>
            Delete {count}
          </MenuItem>
        </div>
      ) : (
        <>
          <div role="group" className="fi-context-group">
            <MenuItem primary keys="Enter" onClick={onOpen}>
              {entry.is_dir ? "Open" : "Open in Windows"}
            </MenuItem>
            {!entry.is_dir && <MenuItem onClick={onDownload}>Download a copy</MenuItem>}
            {canRename && (
              <MenuItem keys="F2" onClick={onRename}>
                Rename
              </MenuItem>
            )}
          </div>
          <div role="separator" className="fi-context-sep" />
          <div role="group" className="fi-context-group">
            <MenuItem onClick={onCopyPath}>Copy full path</MenuItem>
            <MenuItem onClick={onMove}>Move…</MenuItem>
            <MenuItem danger keys="Del" onClick={onDelete}>
              Delete
            </MenuItem>
          </div>
        </>
      )}
    </div>
  );
}
