import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isApiRefusal } from "../data/client";
import {
  MAX_UPLOAD_BYTES,
  downloadFile,
  dropTargetFolder,
  fileName,
  formatBytes,
  joinPath,
  parentPath,
  pathSegments,
  pathUpTo,
  sortEntries,
  useCreateFolder,
  useDelete,
  useFileSearch,
  useFolder,
  useMove,
  useUpload,
  type DeleteRequest,
  type Dropped,
  type Entry,
  type SortColumn,
  type SortDirection,
} from "../data/files";
import { Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, RelativeTime, StaleNote, Teach } from "../ui";
import "./files.css";

/**
 * Files — the one managed root, browsed, searched, made, filled, moved and
 * removed from this window, plus whatever a person drags in from Windows.
 *
 * **Everything on this page assumes `files_root` is configured.** When it is
 * not, all seven routes answer `503` with an EMPTY body (`http.rs:1336-1341`)
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
  internal: "the núcleo could not delete that from disk",
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
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={sentences} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

/* ----------------------------------------------------------------- page -- */

/** How long an armed keyboard delete stays armed — same window `ConfirmButton` uses. */
const KEYBOARD_ARM_MS = 4000;

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
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; entry: Entry } | null>(null);
  const [movingEntry, setMovingEntry] = useState<Entry | null>(null);
  const [notEmptyPaths, setNotEmptyPaths] = useState<string[] | null>(null);
  const [deleteError, setDeleteError] = useState<unknown>(null);
  const [keyboardArmed, setKeyboardArmed] = useState(false);
  const [dragActive, setDragActive] = useState(false);
  const [dropReport, setDropReport] = useState<{ truncated: boolean; refusals: string[] } | null>(null);
  const [downloadError, setDownloadError] = useState<unknown>(null);

  const folder = useFolder(path);
  const searching = q.trim() !== "";
  const search = useFileSearch(path, q, searching);
  const create = useCreateFolder();
  const upload = useUpload();
  const move = useMove();
  const del = useDelete();

  // Every hook above is read by the effects below through a ref, never through
  // its own closure — the OS-drop listeners are registered once (see their
  // effect's own note) and would otherwise call the mutation and read the path
  // this page had at the moment the window opened, not the one on screen now.
  const pathRef = useRef(path);
  const uploadRef = useRef(upload);
  useEffect(() => {
    pathRef.current = path;
  }, [path]);
  useEffect(() => {
    uploadRef.current = upload;
  });

  // Navigating clears row-local state that no longer names anything on screen.
  useEffect(() => {
    setSelected(new Set());
    setFocusIndex(null);
    setRenaming(null);
    setKeyboardArmed(false);
    setNotEmptyPaths(null);
  }, [path]);

  // The 200ms debounce the packet asks for, at the page: a search per
  // keystroke would walk the whole tree once per character typed.
  useEffect(() => {
    const timer = window.setTimeout(() => setQ(qInput), 200);
    return () => window.clearTimeout(timer);
  }, [qInput]);

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
      void handleDropped(event.payload);
    }).then((fn) => {
      unlistenDropped = fn;
    });

    async function handleDropped(payload: Dropped) {
      const refusals: string[] = [];
      for (const file of payload.files) {
        const folderPath = dropTargetFolder(pathRef.current, file.folder);
        try {
          const bytes = await invoke<ArrayBuffer>("read_dropped", { path: file.path });
          await uploadRef.current.mutateAsync({ folder: folderPath, filename: file.name, bytes });
        } catch (error) {
          refusals.push(`${file.name} — ${describeDropFailure(error)}`);
        }
      }
      setDropReport({ truncated: payload.truncated, refusals });
    }

    return () => {
      unlistenEnter?.();
      unlistenLeave?.();
      unlistenDropped?.();
    };
    // Registered once: the handler reaches the current path and mutation
    // through refs, exactly as `Voice.tsx`'s real-hotkey effect does.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /* --------------------------------------------------------------- delete -- */

  async function attemptDelete(paths: string[], recursive: boolean): Promise<{ notEmpty: string[] }> {
    const notEmpty: string[] = [];
    setDeleteError(null);
    for (const target of paths) {
      const request: DeleteRequest = { path: target, recursive };
      try {
        await del.mutateAsync(request);
      } catch (error) {
        if (!recursive && isApiRefusal(error) && error.status === 409) {
          notEmpty.push(target);
        } else {
          setDeleteError(error);
        }
      }
    }
    return { notEmpty };
  }

  async function deleteSelected() {
    const paths = [...selected].map((name) => joinPath(path, name));
    if (paths.length === 0) return;
    const { notEmpty } = await attemptDelete(paths, false);
    setNotEmptyPaths(notEmpty.length > 0 ? notEmpty : null);
    setSelected(new Set(notEmpty.map((p) => fileName(p))));
  }

  async function deleteRecursive(paths: string[]) {
    await attemptDelete(paths, true);
    setNotEmptyPaths(null);
    setSelected(new Set());
  }

  /**
   * The one place `downloadFile` is ever called. It is a plain function, not
   * a mutation, so nothing catches its rejection unless this does — a bare
   * `void downloadFile(...)` at each call site would turn a 409 on a
   * directory or a 404 on a vanished file into a silent no-op with an
   * unhandled rejection behind it.
   */
  function handleDownload(target: string) {
    setDownloadError(null);
    downloadFile(target).catch((error: unknown) => setDownloadError(error));
  }

  /* --------------------------------------------------------------- render -- */

  const rootUnavailable =
    folder.isError && folder.data === undefined && isApiRefusal(folder.error) && folder.error.status === 503;

  return (
    <>
      <PageHeader
        title="Files"
        headline={pageHeadline(rootUnavailable, folder.data)}
        actions={rootUnavailable ? undefined : <UploadButton path={path} upload={upload} />}
      />

      {rootUnavailable && <FilesUnavailableTeach />}

      {!rootUnavailable && (
        <div className="fi-body" onDragOver={(event) => event.preventDefault()}>
          {dragActive && (
            <div className="fi-drop-overlay" role="status">
              drop to upload into {path === "" ? "the files root" : path}
            </div>
          )}

          <div className="fi-side">
            <FolderTree currentPath={path} onSelect={setPath} />
          </div>

          <div className="fi-main">
            <Breadcrumbs path={path} onGo={setPath} />

            <div className="fi-toolbar">
              <label className="fi-search">
                <span>Search under this folder</span>
                <input
                  value={qInput}
                  aria-label="Search files by name"
                  placeholder="find by name…"
                  onChange={(event) => setQInput(event.target.value)}
                />
              </label>
              <NewFolderForm path={path} create={create} />
            </div>

            {dropReport !== null && (
              <DropReportNote report={dropReport} onDismiss={() => setDropReport(null)} />
            )}

            {selected.size > 0 && (
              <div className="fi-selection-bar" role="status">
                <span>
                  {selected.size} selected
                  {keyboardArmed ? " — press Delete again to remove them" : ""}
                </span>
                <Button variant="danger" disabled={del.isPending} onClick={() => void deleteSelected()}>
                  Delete
                </Button>
                <Button variant="ghost" onClick={() => setSelected(new Set())}>
                  Clear selection
                </Button>
              </div>
            )}

            {notEmptyPaths !== null && (
              <NotEmptyNote
                paths={notEmptyPaths}
                busy={del.isPending}
                onConfirm={() => void deleteRecursive(notEmptyPaths)}
                onCancel={() => setNotEmptyPaths(null)}
              />
            )}

            {deleteError !== null && (
              <RefusalOrError error={deleteError} sentences={DELETE_SENTENCES} what="that delete did not go through" />
            )}

            {movingEntry !== null && (
              <MoveForm
                path={path}
                entry={movingEntry}
                move={move}
                onClose={() => setMovingEntry(null)}
              />
            )}

            {searching ? (
              <SearchResults result={search} onOpenFolder={setPath} onDownload={handleDownload} />
            ) : (
              <FileTable
                path={path}
                folder={folder}
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
                onSelectAll={(names) => setSelected(new Set(names))}
                focusIndex={focusIndex}
                onFocusRow={setFocusIndex}
                renaming={renaming}
                onStartRename={(entry) => setRenaming({ path: joinPath(path, entry.name), name: entry.name })}
                onSubmitRename={(from, name) =>
                  move.mutate({ from, to: joinPath(path, name) }, { onSuccess: () => setRenaming(null) })
                }
                onCancelRename={() => setRenaming(null)}
                onOpen={(entry) => {
                  if (entry.is_dir) setPath(joinPath(path, entry.name));
                  else handleDownload(joinPath(path, entry.name));
                }}
                onContextMenu={(entry, x, y) => setContextMenu({ entry, x, y })}
                keyboardArmed={keyboardArmed}
                onKeyboardArm={setKeyboardArmed}
                onDeleteSelected={() => void deleteSelected()}
                onGoUp={() => setPath(parentPath(path))}
              />
            )}

            {downloadError !== null && (
              <RefusalOrError error={downloadError} sentences={DOWNLOAD_SENTENCES} what="that file was not downloaded" />
            )}

            {move.isError && movingEntry === null && (
              <RefusalOrError error={move.error} sentences={MOVE_SENTENCES} what="that rename did not go through" />
            )}
            {upload.isError && (
              <RefusalOrError error={upload.error} sentences={UPLOAD_SENTENCES} what="that upload did not go through" />
            )}
            {create.isError && (
              <RefusalOrError error={create.error} sentences={CREATE_FOLDER_SENTENCES} what="that folder was not created" />
            )}
          </div>
        </div>
      )}

      {contextMenu !== null && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          entry={contextMenu.entry}
          onClose={() => setContextMenu(null)}
          onDownload={() => {
            handleDownload(joinPath(path, contextMenu.entry.name));
            setContextMenu(null);
          }}
          onRename={() => {
            setRenaming({ path: joinPath(path, contextMenu.entry.name), name: contextMenu.entry.name });
            setContextMenu(null);
          }}
          onMove={() => {
            setMovingEntry(contextMenu.entry);
            setContextMenu(null);
          }}
          onDelete={() => {
            setSelected(new Set([contextMenu.entry.name]));
            setContextMenu(null);
            void attemptDelete([joinPath(path, contextMenu.entry.name)], false).then(({ notEmpty }) => {
              setNotEmptyPaths(notEmpty.length > 0 ? notEmpty : null);
              if (notEmpty.length === 0) setSelected(new Set());
            });
          }}
        />
      )}
    </>
  );
}

function describeDropFailure(error: unknown): string {
  if (typeof error === "string" && error.trim() !== "") return error;
  if (isApiRefusal(error)) return error.detail.trim() !== "" ? error.detail : error.code;
  if (error instanceof Error && error.message.trim() !== "") return error.message;
  return "could not be uploaded";
}

function pageHeadline(rootUnavailable: boolean, entries: Entry[] | undefined): string | undefined {
  if (rootUnavailable) return "no files root is configured";
  if (entries === undefined) return undefined;
  if (entries.length === 0) return "this folder is empty";
  const dirs = entries.filter((entry) => entry.is_dir).length;
  const files = entries.length - dirs;
  const totalBytes = entries.reduce((sum, entry) => sum + entry.size_bytes, 0);
  const parts = [
    dirs > 0 ? `${dirs} folder${dirs === 1 ? "" : "s"}` : null,
    files > 0 ? `${files} file${files === 1 ? "" : "s"}, ${formatBytes(totalBytes)}` : null,
  ].filter((part): part is string => part !== null);
  return parts.join(", ");
}

/* --------------------------------------------------------- the no-root teach -- */

function FilesUnavailableTeach() {
  return (
    <Teach title="No files root is configured">
      <p>
        This pillar keeps one managed folder for everything it holds — uploads, drops from Windows, and
        the copies mail filing and team workspaces write into the same root. The núcleo has not been told
        where that folder is, so all seven of its routes refuse with the same empty answer rather than one
        of them guessing.
      </p>
      <p>
        The daemon's own location for it is <code>%LOCALAPPDATA%\nucleos\NucleOS\data\files</code>.
        Nothing on this page will work until a folder is set there — there is nothing to retry towards in
        the meantime.
      </p>
    </Teach>
  );
}

/* ---------------------------------------------------------------- tree -- */

function FolderTree({ currentPath, onSelect }: { currentPath: string; onSelect: (path: string) => void }) {
  return (
    <nav className="fi-tree" aria-label="Folders">
      <TreeNode path="" label="files" currentPath={currentPath} onSelect={onSelect} depth={0} />
    </nav>
  );
}

/**
 * One branch of the tree, lazy: a node's own children are asked for only once
 * it is open. The root is open from the start so the first screen is not
 * empty; every folder under it starts collapsed.
 */
function TreeNode({
  path,
  label,
  currentPath,
  onSelect,
  depth,
}: {
  path: string;
  label: string;
  currentPath: string;
  onSelect: (path: string) => void;
  depth: number;
}) {
  const [open, setOpen] = useState(depth === 0);
  const listing = useFolder(path, open);
  const dirs = (listing.data ?? []).filter((entry) => entry.is_dir);
  const active = path === currentPath;

  return (
    <div className="fi-tree-node">
      <div className={active ? "fi-tree-row fi-tree-row-active" : "fi-tree-row"} style={{ paddingLeft: depth * 12 }}>
        {depth > 0 ? (
          <button
            type="button"
            className="fi-tree-toggle"
            aria-label={open ? `Collapse ${label}` : `Expand ${label}`}
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {open ? "▾" : "▸"}
          </button>
        ) : (
          <span className="fi-tree-toggle-spacer" />
        )}
        <Button variant="link" onClick={() => onSelect(path)}>
          <span className="fi-tree-label">{label}</span>
        </Button>
      </div>
      {open && dirs.length > 0 && (
        <div className="fi-tree-children">
          {dirs.map((dir) => (
            <TreeNode
              key={dir.name}
              path={joinPath(path, dir.name)}
              label={dir.name}
              currentPath={currentPath}
              onSelect={onSelect}
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
  return (
    <nav className="fi-crumbs" aria-label="Folder path">
      <Button variant="link" disabled={path === ""} onClick={() => onGo("")}>
        <span className="fi-crumb">files</span>
      </Button>
      {segments.map((segment, index) => (
        <Button
          key={`${segment}-${String(index)}`}
          variant="link"
          disabled={index === segments.length - 1}
          onClick={() => onGo(pathUpTo(path, index + 1))}
        >
          <span className="fi-crumb">/ {segment}</span>
        </Button>
      ))}
    </nav>
  );
}

/* -------------------------------------------------------------- upload -- */

function UploadButton({ path, upload }: { path: string; upload: ReturnType<typeof useUpload> }) {
  const [oversized, setOversized] = useState<string[]>([]);
  const [savedAs, setSavedAs] = useState<string[]>([]);

  async function handleFiles(fileList: FileList | null) {
    if (fileList === null) return;
    const files = Array.from(fileList);
    const tooBig = files.filter((file) => file.size > MAX_UPLOAD_BYTES).map((file) => file.name);
    setOversized(tooBig);
    setSavedAs([]);
    for (const file of files) {
      if (file.size > MAX_UPLOAD_BYTES) continue;
      const bytes = await file.arrayBuffer();
      upload.mutate(
        { folder: path, filename: file.name, bytes },
        { onSuccess: (saved) => setSavedAs((current) => [...current, saved.filename]) },
      );
    }
  }

  return (
    <div className="fi-upload">
      <label className="fi-upload-label">
        <span>Upload</span>
        <input
          type="file"
          multiple
          aria-label="Upload files"
          onChange={(event) => void handleFiles(event.target.files)}
        />
      </label>
      {oversized.length > 0 && (
        <p className="fi-upload-note" role="alert">
          not sent — larger than the {String(MAX_UPLOAD_BYTES / (1024 * 1024))} MB the daemon will accept in one
          upload: {oversized.join(", ")}
        </p>
      )}
      {savedAs.length > 0 && (
        <p className="fi-upload-note" role="status">
          saved as: {savedAs.join(", ")}
        </p>
      )}
    </div>
  );
}

/* ----------------------------------------------------------- new folder -- */

function NewFolderForm({ path, create }: { path: string; create: ReturnType<typeof useCreateFolder> }) {
  const [name, setName] = useState("");
  return (
    <form
      className="fi-new-folder"
      onSubmit={(event) => {
        event.preventDefault();
        const trimmed = name.trim();
        if (trimmed === "" || create.isPending) return;
        create.mutate(joinPath(path, trimmed), { onSuccess: () => setName("") });
      }}
    >
      <label className="fi-field">
        <span>New folder</span>
        <input value={name} aria-label="New folder name" onChange={(event) => setName(event.target.value)} />
      </label>
      <Button type="submit" disabled={name.trim() === "" || create.isPending}>
        Create folder
      </Button>
    </form>
  );
}

/* --------------------------------------------------------------- table -- */

interface FileTableProps {
  path: string;
  folder: ReturnType<typeof useFolder>;
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
  selected: Set<string>;
  onToggleSelect: (name: string) => void;
  onSelectAll: (names: string[]) => void;
  focusIndex: number | null;
  onFocusRow: (index: number) => void;
  renaming: { path: string; name: string } | null;
  onStartRename: (entry: Entry) => void;
  onSubmitRename: (from: string, name: string) => void;
  onCancelRename: () => void;
  onOpen: (entry: Entry) => void;
  onContextMenu: (entry: Entry, x: number, y: number) => void;
  keyboardArmed: boolean;
  onKeyboardArm: (armed: boolean) => void;
  onDeleteSelected: () => void;
  onGoUp: () => void;
}

const COLUMN_LABEL: Record<SortColumn, string> = { name: "Name", size: "Size", modified: "Modified" };

function FileTable(props: FileTableProps) {
  const {
    path,
    folder,
    sort,
    onSort,
    selected,
    onToggleSelect,
    onSelectAll,
    focusIndex,
    onFocusRow,
    renaming,
    onStartRename,
    onSubmitRename,
    onCancelRename,
    onOpen,
    onContextMenu,
    keyboardArmed,
    onKeyboardArm,
    onDeleteSelected,
    onGoUp,
  } = props;

  const stale = folder.isError && folder.data !== undefined;
  const entries = folder.data;
  const ordered = entries === undefined ? [] : sortEntries(entries, sort.column, sort.direction);

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      onKeyboardArm(false);
      onCancelRename();
      return;
    }
    if (renaming !== null) return;

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
      event.preventDefault();
      onSelectAll(ordered.map((entry) => entry.name));
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      onFocusRow(Math.min(ordered.length - 1, (focusIndex ?? -1) + 1));
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      onFocusRow(Math.max(0, (focusIndex ?? 0) - 1));
      return;
    }
    if (event.key === "Enter") {
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onOpen(entry);
      return;
    }
    if (event.key === "F2") {
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onStartRename(entry);
      return;
    }
    if (event.key === "Backspace") {
      if (path !== "") onGoUp();
      return;
    }
    if (event.key === "Delete") {
      // A held key auto-repeats; only a genuine second press may confirm.
      if (event.repeat || selected.size === 0) return;
      if (!keyboardArmed) {
        onKeyboardArm(true);
        window.setTimeout(() => onKeyboardArm(false), KEYBOARD_ARM_MS);
      } else {
        onKeyboardArm(false);
        onDeleteSelected();
      }
    }
  }

  return (
    // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- the
    // shortcuts below (arrows, Enter, F2, Delete) need somewhere to land, and
    // a `<table>` is not itself focusable.
    <div className="fi-table-wrap" tabIndex={0} onKeyDown={handleKeyDown}>
      <Panel
        title="Contents"
        aside={<Count entries={entries} />}
      >
        {stale && <StaleNote dataUpdatedAt={folder.dataUpdatedAt} />}
        {folder.isError && entries === undefined && <RefusalOrError error={folder.error} sentences={LIST_SENTENCES} what="nothing is known about this folder" />}
        {!folder.isError && entries === undefined && <p className="fi-loading">reading the folder…</p>}
        {entries !== undefined && entries.length === 0 && (
          <Teach title="This folder is empty">
            <p>
              Upload a file, make a folder, or drag something in from Windows — a drop lands under the
              folder you are looking at.
            </p>
          </Teach>
        )}

        {ordered.length > 0 && (
          <table className="fi-table">
            <thead>
              <tr>
                <th scope="col" className="fi-col-select">
                  <input
                    type="checkbox"
                    aria-label="Select all"
                    checked={ordered.length > 0 && ordered.every((entry) => selected.has(entry.name))}
                    onChange={(event) => onSelectAll(event.target.checked ? ordered.map((entry) => entry.name) : [])}
                  />
                </th>
                <SortHeader column="name" sort={sort} onSort={onSort} />
                <SortHeader column="size" sort={sort} onSort={onSort} />
                <SortHeader column="modified" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {ordered.map((entry, index) => (
                <FileRow
                  key={entry.name}
                  path={path}
                  entry={entry}
                  index={index}
                  focused={focusIndex === index}
                  selected={selected.has(entry.name)}
                  renaming={renaming?.path === joinPath(path, entry.name) ? renaming : null}
                  onToggleSelect={() => {
                    onFocusRow(index);
                    onToggleSelect(entry.name);
                  }}
                  onOpen={() => {
                    onFocusRow(index);
                    onOpen(entry);
                  }}
                  onContextMenu={(x, y) => {
                    onFocusRow(index);
                    onContextMenu(entry, x, y);
                  }}
                  onSubmitRename={(name) => onSubmitRename(joinPath(path, entry.name), name)}
                  onCancelRename={onCancelRename}
                />
              ))}
            </tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}

function SortHeader({
  column,
  sort,
  onSort,
}: {
  column: SortColumn;
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
}) {
  const active = sort.column === column;
  const ariaSort: "ascending" | "descending" | "none" = !active ? "none" : sort.direction === "asc" ? "ascending" : "descending";
  return (
    <th scope="col" aria-sort={ariaSort} className={column === "name" ? "fi-col-name" : "fi-col-num"}>
      <button type="button" className="fi-sort-button" onClick={() => onSort(column)}>
        {COLUMN_LABEL[column]}
        {active ? <span aria-hidden="true">{sort.direction === "asc" ? " ▲" : " ▼"}</span> : null}
      </button>
    </th>
  );
}

function FileRow({
  path,
  entry,
  index,
  focused,
  selected,
  renaming,
  onToggleSelect,
  onOpen,
  onContextMenu,
  onSubmitRename,
  onCancelRename,
}: {
  path: string;
  entry: Entry;
  index: number;
  focused: boolean;
  selected: boolean;
  renaming: { path: string; name: string } | null;
  onToggleSelect: () => void;
  onOpen: () => void;
  onContextMenu: (x: number, y: number) => void;
  onSubmitRename: (name: string) => void;
  onCancelRename: () => void;
}) {
  void path;
  void index;
  return (
    <tr
      className={focused ? "fi-row fi-row-focus" : "fi-row"}
      onContextMenu={(event) => {
        event.preventDefault();
        onContextMenu(event.clientX, event.clientY);
      }}
    >
      <td>
        <input type="checkbox" aria-label={`Select ${entry.name}`} checked={selected} onChange={onToggleSelect} />
      </td>
      <td className="fi-col-name">
        {renaming !== null ? (
          <RenameForm initialName={renaming.name} onSubmit={onSubmitRename} onCancel={onCancelRename} />
        ) : (
          <Button variant="link" onClick={onOpen}>
            <span className="fi-entry-name">
              {entry.name}
              {entry.is_dir ? "/" : ""}
            </span>
          </Button>
        )}
      </td>
      <td className="fi-col-num">{entry.is_dir ? "" : formatBytes(entry.size_bytes)}</td>
      <td className="fi-col-num">{entry.modified === null ? "unknown" : <RelativeTime at={entry.modified} />}</td>
    </tr>
  );
}

function RenameForm({
  initialName,
  onSubmit,
  onCancel,
}: {
  initialName: string;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initialName);
  return (
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
        onChange={(event) => setValue(event.target.value)}
      />
      <Button type="submit">Rename</Button>
      <Button variant="ghost" onClick={onCancel}>
        Cancel
      </Button>
    </form>
  );
}

function Count({ entries }: { entries: Entry[] | undefined }) {
  if (entries === undefined) return null;
  const totalBytes = entries.reduce((sum, entry) => sum + entry.size_bytes, 0);
  return (
    <span className="fi-count">
      {entries.length} item{entries.length === 1 ? "" : "s"}, {formatBytes(totalBytes)}
    </span>
  );
}

/* -------------------------------------------------------------- search -- */

function SearchResults({
  result,
  onOpenFolder,
  onDownload,
}: {
  result: ReturnType<typeof useFileSearch>;
  onOpenFolder: (path: string) => void;
  onDownload: (path: string) => void;
}) {
  const hits = result.data?.hits ?? [];
  return (
    <Panel title="Search results" aside={result.data === undefined ? undefined : <span className="fi-count">{hits.length} hit{hits.length === 1 ? "" : "s"}</span>}>
      {result.isError && <RefusalOrError error={result.error} sentences={SEARCH_SENTENCES} what="that search did not go through" />}
      {!result.isError && result.data === undefined && <p className="fi-loading">searching…</p>}
      {result.data !== undefined && hits.length === 0 && <p className="fi-empty">nothing under this folder matches.</p>}
      {result.data?.truncated === true && (
        <p className="fi-truncated" role="status">
          stopped early — this search hit the daemon's own ceiling before finishing the whole tree. Narrow it to
          see everything.
        </p>
      )}
      {hits.length > 0 && (
        <ul className="fi-hits" aria-label="Search hits">
          {hits.map((hit) => (
            <li className="fi-hit" key={hit.path}>
              <Button
                variant="link"
                onClick={() => {
                  if (hit.is_dir) onOpenFolder(hit.path);
                  else onDownload(hit.path);
                }}
              >
                <span className="fi-entry-name">
                  {hit.name}
                  {hit.is_dir ? "/" : ""}
                </span>
              </Button>
              <span className="fi-hit-path">{hit.path}</span>
              <span className="fi-hit-size">{hit.is_dir ? "" : formatBytes(hit.size_bytes)}</span>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

/* ---------------------------------------------------------------- move -- */

function MoveForm({
  path,
  entry,
  move,
  onClose,
}: {
  path: string;
  entry: Entry;
  move: ReturnType<typeof useMove>;
  onClose: () => void;
}) {
  const from = joinPath(path, entry.name);
  const [to, setTo] = useState(from);
  return (
    <form
      className="fi-move"
      onSubmit={(event) => {
        event.preventDefault();
        if (to.trim() === "" || move.isPending) return;
        move.mutate({ from, to: to.trim() }, { onSuccess: onClose });
      }}
    >
      <label className="fi-field">
        <span>Move {entry.name} to</span>
        <input value={to} aria-label="Destination path" onChange={(event) => setTo(event.target.value)} />
      </label>
      <Button type="submit" disabled={move.isPending}>
        Move
      </Button>
      <Button variant="ghost" onClick={onClose}>
        Cancel
      </Button>
      {move.isError && <RefusalOrError error={move.error} sentences={MOVE_SENTENCES} what="that move did not go through" />}
    </form>
  );
}

/* -------------------------------------------------------------- delete -- */

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
  return (
    <div className="fi-not-empty" role="status">
      <p>
        {paths.length === 1 ? "that folder has" : "those folders have"} something inside — the núcleo will not
        remove {paths.length === 1 ? "it" : "them"} without saying so first.
      </p>
      <div className="fi-not-empty-actions">
        {/* The one delete on this page wrapped in the two-click interlock: this
            is the irreversible one, taking a whole subtree with it. */}
        <ConfirmButton
          label="Delete with everything inside"
          confirmLabel="Really delete everything inside"
          disabled={busy}
          onConfirm={onConfirm}
        />
        <Button variant="ghost" onClick={onCancel}>
          Leave it
        </Button>
      </div>
    </div>
  );
}

/* --------------------------------------------------------- context menu -- */

function ContextMenu({
  x,
  y,
  entry,
  onClose,
  onDownload,
  onRename,
  onMove,
  onDelete,
}: {
  x: number;
  y: number;
  entry: Entry;
  onClose: () => void;
  onDownload: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
}) {
  useEffect(() => {
    function handlePointerDown() {
      onClose();
    }
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  return (
    <div
      className="fi-context-menu"
      role="menu"
      aria-label={`Actions for ${entry.name}`}
      style={{ left: x, top: y }}
      onMouseDown={(event) => event.stopPropagation()}
    >
      {!entry.is_dir && (
        <button type="button" role="menuitem" className="fi-context-item" onClick={onDownload}>
          Download
        </button>
      )}
      <button type="button" role="menuitem" className="fi-context-item" onClick={onRename}>
        Rename
      </button>
      <button type="button" role="menuitem" className="fi-context-item" onClick={onMove}>
        Move…
      </button>
      <button type="button" role="menuitem" className="fi-context-item fi-context-item-danger" onClick={onDelete}>
        Delete
      </button>
    </div>
  );
}

/* -------------------------------------------------------------- os drop -- */

function DropReportNote({
  report,
  onDismiss,
}: {
  report: { truncated: boolean; refusals: string[] };
  onDismiss: () => void;
}) {
  return (
    <div className="fi-drop-report" role="status">
      {report.truncated && <p>the drop was cut short by the host before this window saw everything.</p>}
      {report.refusals.length === 0 ? (
        <p>every file from that drop was uploaded.</p>
      ) : (
        <>
          <p>{report.refusals.length} file{report.refusals.length === 1 ? "" : "s"} from that drop stayed behind:</p>
          <ul className="fi-drop-refusals">
            {report.refusals.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </>
      )}
      <Button variant="ghost" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}
