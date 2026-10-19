import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import { isApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import { MAX_UPLOAD_BYTES, diskPath, downloadFile, dropTargetFolder, fileName, formatBytes, joinPath, openFile, parentPath, readFolder, runsOnOpen, sortEntries, useCreateFolder, useDelete, useFileSearch, useFolder, useMove, useRestore, useTrash, useUpload, type DeleteRequest, type Dropped, type Entry, type Hit, type SortColumn, type SortDirection, type Trashed } from "../data/files";
import { Button, ErrorNote, Field, PageHeader } from "../ui";
import { type ClashChoice, DELETE_SENTENCES, DOWNLOAD_SENTENCES, MOVE_SENTENCES, type MoveDone, type PathFailure, type PendingBatch, RESTORE_SENTENCES, RefusalOrError, UPLOAD_SENTENCES, type Undo, type UploadItem, type UploadProgress, type UploadRefusal, type UploadReport, type UploadSaved, describeUploadFailure, readHintRetired, storeHintRetired, useNarrow } from "./files/shared";
import { DRAG_THRESHOLD_PX, Dismissable, FailureNotes, FilesUnavailableTeach, type FocusAfter, ghostTransform, moveRefusal, pageHeadline, swallowClick } from "./files/pageParts";
import { FolderTree } from "./files/FolderTree";
import { Breadcrumbs } from "./files/Breadcrumbs";
import { ClashNote, UploadButton, UploadReportNote } from "./files/Upload";
import { NewFolderForm } from "./files/NewFolderForm";
import { FileTable } from "./files/FileTable";
import { SearchResults } from "./files/SearchResults";
import { MoveForm } from "./files/MoveForm";
import { NotEmptyNote, RecentlyDeleted, UndoKeys, UndoNote, undoSummary, undoVerb } from "./files/Undo";
import { ContextMenu } from "./files/ContextMenu";
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
        <Field label="Search files by name" labelHidden>
          <input
            ref={searchRef}
            type="search"
            value={qInput}
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
              Drop to upload into {path === "" ? "the files root" : path}
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
                    <div className="fi-sel-actions">
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
                    </div>
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
              <p className="fi-hint">Double-click a file to open it in Windows · drop files here to upload · Ctrl- or Shift-click to select several</p>
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
          Move {drag.label}
          {drag.over !== null && ` to ${drag.over === "" ? "Files" : drag.over}`}
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

