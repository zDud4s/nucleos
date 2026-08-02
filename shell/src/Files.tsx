import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createFolder, deleteEntry, downloadFile, listFiles, moveEntry, searchFiles, uploadFile,
  type ConnectionState, type FileEntry, type FileHit, type FileSearch,
} from "./api";
import {
  breadcrumbs, formatBytes, joinPath, namesBetween, parentPath, relativeTime, safeDownloadName,
  sortFiles, type FileSortKey,
} from "./derive";
import { Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/** What the daemon takes in one upload — `files::MAX_UPLOAD_BYTES`, in the shell's units. */
const MAX_UPLOAD_MB = 100;

/** What one drop resolved to, as `drop.rs` describes it. */
interface DroppedFile {
  path: string;
  folder: string;
  name: string;
  size: number;
}
interface Dropped {
  files: DroppedFile[];
  truncated: boolean;
}

/**
 * Why a request was refused, in words that say what to do next.
 *
 * 409 is the one that needs the caller's help: the daemon answers it for three different conflicts
 * and the page knows which of the three it just asked for, so the sentence is passed in rather than
 * guessed at here.
 */
function fileFailure(status: number, conflict: string): string {
  if (status === 400) {
    return "That name is not one this folder can carry. A path that climbs out of it, a drive letter, or a name Windows will not store is refused rather than rewritten.";
  }
  if (status === 401 || status === 403) return "This token buys nothing here.";
  if (status === 404) return "Not found. It was renamed or removed since this listing.";
  if (status === 409) return conflict;
  if (status === 413) return `Too large. The daemon takes up to ${MAX_UPLOAD_MB} MB in one upload.`;
  if (status === 503) {
    return "The daemon has no files folder — it could not create one at startup. Nothing here can be read or written until that is fixed.";
  }
  if (status === 0) return "The daemon is not answering.";
  return "That did not work.";
}

/** The last segment of a path — the name a moved entry keeps. */
function baseName(path: string): string {
  const cut = path.lastIndexOf("/");
  return cut === -1 ? path : path.slice(cut + 1);
}

interface TreeProps {
  path: string;
  current: string;
  label: string;
  expanded: Set<string>;
  branches: Record<string, FileEntry[]>;
  dropTarget: string | null;
  onToggle: (path: string) => void;
  onOpen: (path: string) => void;
  onDropTarget: (path: string | null) => void;
}

/**
 * One folder in the left pane, and the folders under it.
 *
 * Expanding and opening are separate gestures on separate targets — the chevron unfolds a branch
 * where it is, the name walks into it. Collapsing them into one click means you cannot look inside a
 * folder without leaving the one you are in, which is the thing a tree exists to avoid.
 */
function Tree({
  path, current, label, expanded, branches, dropTarget, onToggle, onOpen, onDropTarget,
}: TreeProps) {
  const open = expanded.has(path);
  const folders = (branches[path] ?? []).filter((entry) => entry.is_dir);

  return (
    <li>
      <div
        className={[
          "f-node",
          path === current ? "f-node--here" : null,
          path === dropTarget ? "f-node--drop" : null,
        ].filter(Boolean).join(" ")}
        onPointerEnter={() => onDropTarget(path)}
        onPointerLeave={() => onDropTarget(null)}
      >
        <button
          type="button"
          className="f-twist"
          aria-label={open ? `Collapse ${label}` : `Expand ${label}`}
          aria-expanded={open}
          onClick={() => onToggle(path)}
        >
          {open ? "▾" : "▸"}
        </button>
        <button
          type="button"
          className="f-branch"
          aria-current={path === current ? "location" : undefined}
          onClick={() => onOpen(path)}
        >
          {label}
        </button>
      </div>
      {open && folders.length > 0 && (
        <ul className="f-tree">
          {folders.map((folder) => (
            <Tree
              key={folder.name}
              path={joinPath(path, folder.name)}
              label={folder.name}
              current={current}
              expanded={expanded}
              branches={branches}
              dropTarget={dropTarget}
              onToggle={onToggle}
              onOpen={onOpen}
              onDropTarget={onDropTarget}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

/**
 * The files folder, as a file manager.
 *
 * One directory on this machine — under the daemon's own data folder, not a window onto the disk
 * around it — holding what you upload and the mail you file. Everything goes through the daemon
 * rather than the filesystem: the shell never learns the real path, and every name it sends is
 * resolved against the root by `files::resolve_within` before anything is touched.
 *
 * The two halves of dragging arrive by different roads, and that is not an accident of style.
 * Dragging a row onto a folder is ours, so it runs on pointer events. Dragging a file in from
 * Windows is not: Tauri takes that drop before the webview sees it, so `drop.rs` resolves it into a
 * manifest, remembers which paths it may read, and tells this page — which then uploads them
 * through the same route the file picker uses.
 */
export default function Files({
  token, connection,
}: {
  token: string | null;
  connection: ConnectionState;
}) {
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [listFailed, setListFailed] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([""]));
  const [branches, setBranches] = useState<Record<string, FileEntry[]>>({});

  const [sortKey, setSortKey] = useState<FileSortKey>("name");
  const [ascending, setAscending] = useState(true);
  /** Full paths under the root, so an action never has to ask which folder a row came from. */
  const [selected, setSelected] = useState<string[]>([]);
  const [anchor, setAnchor] = useState<string | null>(null);

  const [query, setQuery] = useState("");
  const [found, setFound] = useState<FileSearch | null>(null);

  const [menu, setMenu] = useState<{ x: number; y: number; target: string | null } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [folderName, setFolderName] = useState("");

  const [overFromOs, setOverFromOs] = useState(false);
  const [dragging, setDragging] = useState<string[] | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  const picker = useRef<HTMLInputElement | null>(null);
  const table = useRef<HTMLDivElement | null>(null);
  /** Counts listings so a slow one landing after a newer one cannot paint the folder you left. */
  const listing = useRef(0);
  /** The press that may become a drag: where it started, and on what. */
  const press = useRef<{ x: number; y: number; id: string } | null>(null);
  /**
   * The live values the window-level drag handlers need; they are registered once.
   *
   * `refresh` rides along with the rest, and that is not tidiness: it is rebuilt whenever `path`
   * changes, so a handler that captured the first one would reload the ROOT after a move made three
   * folders deep — showing the folder you left as proof the move worked.
   */
  const live = useRef({
    path, selected, dropTarget, dragging, token,
    refresh: async () => {},
  });
  live.current = { ...live.current, path, selected, dropTarget, dragging, token };

  const rows: (FileEntry | FileHit)[] = sortFiles(
    found !== null ? found.hits : (entries ?? []),
    sortKey,
    ascending,
  );
  const idOf = useCallback(
    (row: FileEntry | FileHit) => ("path" in row ? row.path : joinPath(path, row.name)),
    [path],
  );
  const ids = rows.map(idOf);

  const load = useCallback(
    async (folder: string) => {
      if (token === null || connection !== "connected") return null;
      const result = await listFiles(token, folder);
      return result;
    },
    [connection, token],
  );

  const refresh = useCallback(async () => {
    if (token === null || connection !== "connected") return;
    listing.current += 1;
    const mine = listing.current;
    setLoading(true);
    const result = await listFiles(token, path);
    if (listing.current !== mine) return;
    setLoading(false);
    setEntries(result.ok ? result.value : null);
    setListFailed(result.ok ? null : fileFailure(result.status, "That is a file, not a folder."));
    // The tree shows the same folders as the listing, so it is fed from it rather than asked again.
    if (result.ok) setBranches((known) => ({ ...known, [path]: result.value }));
  }, [connection, path, token]);
  live.current.refresh = refresh;

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // The search runs on the daemon because it descends: filtering the listing here would only ever
  // find what is already on screen, which is not what a search box in a file manager means.
  useEffect(() => {
    if (token === null || connection !== "connected") return;
    const needle = query.trim();
    if (needle === "") {
      setFound(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(async () => {
      const result = await searchFiles(token, path, needle);
      if (cancelled) return;
      if (!result.ok) {
        setFound(null);
        setFailed(fileFailure(result.status, "That is a file, not a folder."));
        return;
      }
      setFound(result.value);
    }, 200);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [connection, path, query, token]);

  // A refusal belongs to the request that earned it, so it goes when the next one starts.
  function begin() {
    setFailed(null);
    setNote(null);
    setBusy(true);
  }

  const openFolder = useCallback((folder: string) => {
    setPath(folder);
    setSelected([]);
    setAnchor(null);
    setQuery("");
    setFound(null);
    setRenaming(null);
    setMenu(null);
  }, []);

  async function toggle(folder: string) {
    const next = new Set(expanded);
    if (next.has(folder)) {
      next.delete(folder);
      setExpanded(next);
      return;
    }
    next.add(folder);
    setExpanded(next);
    if (branches[folder] !== undefined) return;
    const result = await load(folder);
    if (result !== null && result.ok) {
      setBranches((known) => ({ ...known, [folder]: result.value }));
    }
  }

  async function download(id: string) {
    if (token === null) return;
    begin();
    const blob = await downloadFile(token, id);
    setBusy(false);
    if (blob === null) {
      setFailed("Could not read that file. It was removed, or the daemon refused the path.");
      return;
    }
    // The browser's own download, from bytes already in hand: the daemon sends every file as an
    // attachment of unknown type, so nothing here is ever rendered in place.
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = safeDownloadName(baseName(id));
    link.click();
    URL.revokeObjectURL(url);
  }

  function open(row: FileEntry | FileHit) {
    if (row.is_dir) {
      openFolder(idOf(row));
      return;
    }
    void download(idOf(row));
  }

  function pick(id: string, event: { ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) {
    setMenu(null);
    if (event.shiftKey && anchor !== null) {
      setSelected(namesBetween(ids, anchor, id));
      return;
    }
    if (event.ctrlKey || event.metaKey) {
      setSelected((current) =>
        current.includes(id) ? current.filter((one) => one !== id) : [...current, id],
      );
      setAnchor(id);
      return;
    }
    setSelected([id]);
    setAnchor(id);
  }

  async function makeFolder() {
    if (token === null || folderName.trim() === "") return;
    begin();
    const result = await createFolder(token, joinPath(path, folderName.trim()));
    setBusy(false);
    if (!result.ok) {
      setFailed(fileFailure(result.status, "Something with that name is already here."));
      return;
    }
    setNote(`Created ${folderName.trim()}`);
    setFolderName("");
    void refresh();
  }

  async function upload(files: FileList | File[] | null, folder = path) {
    if (token === null || files === null) return [] as string[];
    const stored: string[] = [];
    for (const file of Array.from(files)) {
      const result = await uploadFile(token, folder, file);
      if (!result.ok) {
        setFailed(`${file.name}: ${fileFailure(result.status, "That folder is gone, or is a file.")}`);
        break;
      }
      stored.push(result.value);
    }
    return stored;
  }

  async function uploadPicked(files: FileList | null) {
    if (files === null || files.length === 0) return;
    begin();
    const stored = await upload(files);
    setBusy(false);
    // The stored names, not the picked ones: a collision is numbered, so what landed can differ
    // from what was chosen, and naming the wrong one sends someone looking for a file that is not
    // there.
    if (stored.length > 0) setNote(`Uploaded ${stored.join(", ")}`);
    void refresh();
  }

  /**
   * Files dragged in from Windows.
   *
   * A dropped folder arrives as its files plus the shape it had, so the shape is rebuilt here before
   * anything is written — a file whose folder does not exist yet would be refused, and rebuilding it
   * afterwards would put the files in the wrong place first.
   */
  const acceptDropped = useCallback(async (dropped: Dropped) => {
    const { token: key, path: here } = live.current;
    if (key === null) return;
    setOverFromOs(false);
    setFailed(null);
    setNote(null);
    setBusy(true);

    const tooBig = dropped.files.filter((file) => file.size > MAX_UPLOAD_MB * 1024 * 1024);
    const carried = dropped.files.filter((file) => file.size <= MAX_UPLOAD_MB * 1024 * 1024);
    const made = new Set<string>();
    let landed = 0;
    let refused: string | null = null;

    for (const file of carried) {
      const folder = file.folder === "" ? here : joinPath(here, file.folder);
      if (file.folder !== "" && !made.has(folder)) {
        made.add(folder);
        const built = await createFolder(key, folder);
        if (!built.ok) {
          refused = `${file.folder}: ${fileFailure(built.status, "Something with that name is already here.")}`;
          break;
        }
      }
      let bytes: ArrayBuffer;
      try {
        bytes = await invoke<ArrayBuffer>("read_dropped", { path: file.path });
      } catch (error) {
        refused = `${file.name}: ${String(error)}`;
        break;
      }
      const result = await uploadFile(key, folder, new File([bytes], file.name));
      if (!result.ok) {
        refused = `${file.name}: ${fileFailure(result.status, "That folder is gone, or is a file.")}`;
        break;
      }
      landed += 1;
    }

    setBusy(false);
    // Everything the drop did NOT do is said out loud: a ceiling that cut the walk, files too big
    // for one upload, and the one that stopped the rest.
    const left: string[] = [];
    if (dropped.truncated) left.push("more than this drop would carry in one go");
    if (tooBig.length > 0) {
      left.push(`${tooBig.length} over ${MAX_UPLOAD_MB} MB (${tooBig.map((f) => f.name).join(", ")})`);
    }
    if (landed > 0) {
      setNote(`Uploaded ${landed} file${landed === 1 ? "" : "s"}${left.length > 0 ? ` — left behind: ${left.join("; ")}` : ""}`);
    } else if (left.length > 0 && refused === null) {
      setFailed(`Nothing was uploaded — left behind: ${left.join("; ")}`);
    }
    if (refused !== null) setFailed(refused);
    void refresh();
  }, [refresh]);

  // Windows' own drag, which never reaches the webview as an HTML event — see `drop.rs`.
  useEffect(() => {
    const stops: (() => void)[] = [];
    void (async () => {
      try {
        stops.push(await listen("files://drag-enter", () => setOverFromOs(true)));
        stops.push(await listen("files://drag-leave", () => setOverFromOs(false)));
        stops.push(
          await listen<Dropped>("files://dropped", (event) => void acceptDropped(event.payload)),
        );
      } catch {
        // Running outside Tauri (the Vite dev server, or a test): the Upload button is the way in.
      }
    })();
    return () => stops.forEach((stop) => stop());
  }, [acceptDropped]);

  async function renameTo(destination: string) {
    if (token === null || renaming === null) return;
    const to = destination.trim();
    if (to === "" || to === renaming) {
      setRenaming(null);
      return;
    }
    begin();
    const result = await moveEntry(token, renaming, to);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        fileFailure(result.status, "Something with that name is already there. Nothing was replaced."),
      );
      return;
    }
    setNote(`${baseName(renaming)} → ${to}`);
    setRenaming(null);
    setSelected([]);
    void refresh();
  }

  /** Moves everything selected into one folder — the drop half of dragging a row. */
  async function moveInto(folder: string) {
    const { token: key, selected: moving } = live.current;
    if (key === null || moving.length === 0) return;
    begin();
    let moved = 0;
    for (const one of moving) {
      // A folder cannot be dropped into itself, and the daemon says so — but answering it here
      // keeps a pointless request off the wire and the message specific.
      if (folder === one || folder.startsWith(`${one}/`)) {
        setFailed(`${baseName(one)} cannot be moved inside itself.`);
        break;
      }
      const result = await moveEntry(key, one, joinPath(folder, baseName(one)));
      if (!result.ok) {
        setFailed(
          `${baseName(one)}: ${fileFailure(result.status, "Something with that name is already there. Nothing was replaced.")}`,
        );
        break;
      }
      moved += 1;
    }
    setBusy(false);
    if (moved > 0) setNote(`Moved ${moved} to ${folder === "" ? "Files" : folder}`);
    setSelected([]);
    // Through the ref, because this function is reached from a handler registered once: the
    // `refresh` it closed over at mount belongs to the root, not to the folder on screen.
    void live.current.refresh();
  }

  async function remove(targets: string[], recursive: boolean) {
    if (token === null || targets.length === 0) return;
    begin();
    let gone = 0;
    for (const one of targets) {
      const result = await deleteEntry(token, one, recursive);
      if (!result.ok) {
        if (result.status === 409) {
          setFailed(`${baseName(one)} still has things in it. Delete it and its contents?`);
          setSelected([one]);
          break;
        }
        setFailed(`${baseName(one)}: ${fileFailure(result.status, "That folder is not empty.")}`);
        break;
      }
      gone += 1;
    }
    setBusy(false);
    if (gone > 0) setNote(`Deleted ${gone}`);
    if (gone === targets.length) setSelected([]);
    void refresh();
  }

  // Dragging rows: ours, so it runs on pointer events. HTML5 drag-and-drop is not available inside
  // a Tauri window that accepts OS drops, and a file manager without drag-to-move is a list.
  useEffect(() => {
    function move(event: PointerEvent) {
      const start = press.current;
      if (start === null || live.current.dragging !== null) return;
      const far = Math.abs(event.clientX - start.x) + Math.abs(event.clientY - start.y) > 6;
      if (!far) return;
      const carrying = live.current.selected.includes(start.id)
        ? live.current.selected
        : [start.id];
      setSelected(carrying);
      setDragging(carrying);
    }
    function up() {
      const { dragging: carrying, dropTarget: target } = live.current;
      press.current = null;
      if (carrying !== null && target !== null) void moveInto(target);
      setDragging(null);
      setDropTarget(null);
    }
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    // `moveInto` reads everything it needs from `live`, so this registers once and never restacks.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A menu that outlives the click that opened it is a menu in the way.
  useEffect(() => {
    if (menu === null) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [menu]);

  function onKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (renaming !== null) return;
    const at = selected.length > 0 ? ids.indexOf(selected[selected.length - 1] ?? "") : -1;
    const step = (delta: number) => {
      if (ids.length === 0) return;
      const next = ids[Math.min(Math.max(at + delta, 0), ids.length - 1)];
      if (next === undefined) return;
      event.preventDefault();
      if (event.shiftKey && anchor !== null) setSelected(namesBetween(ids, anchor, next));
      else {
        setSelected([next]);
        setAnchor(next);
      }
    };

    if (event.key === "ArrowDown") return step(at === -1 ? 0 : 1);
    if (event.key === "ArrowUp") return step(at === -1 ? 0 : -1);
    if (event.key === "Enter") {
      const row = rows.find((one) => idOf(one) === selected[0]);
      if (row !== undefined) open(row);
      return;
    }
    if (event.key === "Backspace" && path !== "") return openFolder(parentPath(path));
    if (event.key === "F2" && selected.length === 1 && selected[0] !== undefined) {
      setRenaming(selected[0]);
      setDraft(selected[0]);
      return;
    }
    if (event.key === "Delete" && selected.length > 0) {
      // Never straight to the deed: Delete arms the same question the button asks, and the answer
      // is a deliberate second gesture.
      setFailed(
        `Delete ${selected.length === 1 ? baseName(selected[0] ?? "") : `${selected.length} items`}? Use the Delete button to confirm.`,
      );
      return;
    }
    if (event.key === "a" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      setSelected(ids);
      return;
    }
    if (event.key === "Escape") {
      setSelected([]);
      setMenu(null);
    }
  }

  if (token === null || connection !== "connected") {
    return (
      <Panel title="Files">
        <p className="a-note">Not connected to the daemon.</p>
      </Panel>
    );
  }

  const trail = breadcrumbs(path);
  const total = rows.reduce((sum, row) => sum + (row.is_dir ? 0 : row.size_bytes), 0);
  const header = (key: FileSortKey, label: string) => (
    <button
      type="button"
      className="f-col"
      aria-sort={sortKey === key ? (ascending ? "ascending" : "descending") : "none"}
      onClick={() => {
        if (sortKey === key) setAscending(!ascending);
        else {
          setSortKey(key);
          setAscending(true);
        }
      }}
    >
      {label}
      {sortKey === key && <span className="f-arrow">{ascending ? "▲" : "▼"}</span>}
    </button>
  );

  return (
    <div className={overFromOs ? "f-shell f-shell--over" : "f-shell"}>
      <Panel title="Files" flat={false}>
        <div className="f-panes">
          <nav className="f-side" aria-label="Folders">
            <ul className="f-tree">
              <Tree
                path=""
                label="Files"
                current={path}
                expanded={expanded}
                branches={branches}
                dropTarget={dropTarget}
                onToggle={(folder) => void toggle(folder)}
                onOpen={openFolder}
                onDropTarget={(folder) => dragging !== null && setDropTarget(folder)}
              />
            </ul>
          </nav>

          <div className="f-main">
            <div className="f-bar">
              <nav className="crumbs" aria-label="Path">
                {trail.map((crumb, index) => (
                  <span key={crumb.path}>
                    {index > 0 && <span className="c-sep">/</span>}
                    <button
                      type="button"
                      className="crumb"
                      aria-current={crumb.path === path ? "location" : undefined}
                      onClick={() => openFolder(crumb.path)}
                    >
                      {index === 0 ? "Files" : crumb.label}
                    </button>
                  </span>
                ))}
              </nav>
              <input
                className="f-find"
                type="search"
                placeholder="Search this folder and below"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </div>

            <div className="f-tools">
              {path !== "" && (
                <Button size="sm" onClick={() => openFolder(parentPath(path))}>Up</Button>
              )}
              <input
                className="f-name"
                placeholder="New folder"
                value={folderName}
                disabled={busy}
                onChange={(event) => setFolderName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") void makeFolder();
                }}
              />
              <Button size="sm" disabled={busy || folderName.trim() === ""} onClick={() => void makeFolder()}>
                Create
              </Button>
              <input
                ref={picker}
                type="file"
                multiple
                className="f-picker"
                onChange={(event) => {
                  void uploadPicked(event.target.files);
                  event.target.value = "";
                }}
              />
              <Button size="sm" disabled={busy} onClick={() => picker.current?.click()}>Upload</Button>
              {selected.length > 0 && (
                <>
                  <Button
                    size="sm"
                    disabled={busy || selected.length !== 1}
                    onClick={() => {
                      setRenaming(selected[0] ?? null);
                      setDraft(selected[0] ?? "");
                    }}
                  >
                    Rename
                  </Button>
                  <ConfirmButton
                    variant="danger"
                    size="sm"
                    disabled={busy}
                    confirmLabel={`Delete ${selected.length} for good?`}
                    onConfirm={() => void remove(selected, false)}
                  >
                    Delete
                  </ConfirmButton>
                  <ConfirmButton
                    variant="danger"
                    size="sm"
                    disabled={busy}
                    confirmLabel="Delete them and everything in them?"
                    onConfirm={() => void remove(selected, true)}
                  >
                    Delete with contents
                  </ConfirmButton>
                </>
              )}
            </div>

            {note !== null && <p className="a-note">{note}</p>}
            {failed !== null && <ErrorNote>{failed}</ErrorNote>}
            {found?.truncated === true && (
              <p className="a-note">
                More matches than this search will list. Narrow it, or search from a folder further
                in.
              </p>
            )}
            {loading && entries === null && <p className="a-note">Loading…</p>}
            {!loading && listFailed !== null && <ErrorNote>{listFailed}</ErrorNote>}
            {rows.length === 0 && !loading && listFailed === null && (
              <Teach title={found !== null ? "Nothing matches." : "Nothing here yet."}>
                {found !== null
                  ? "No file or folder under here has that in its name."
                  : "This folder holds what you upload and the mail attachments you file from the Mail tab. Drag files onto the window, or use Upload — both copy into the folder rather than opening a window onto the rest of the disk."}
              </Teach>
            )}

            <div
              className="f-rows"
              ref={table}
              role="grid"
              tabIndex={0}
              aria-label="Files in this folder"
              onKeyDown={onKeyDown}
              onContextMenu={(event) => {
                event.preventDefault();
                setMenu({ x: event.clientX, y: event.clientY, target: null });
              }}
            >
              {rows.length > 0 && (
                <div className="f-head" role="row">
                  {header("name", "Name")}
                  {header("size", "Size")}
                  {header("modified", "Modified")}
                </div>
              )}
              {rows.map((row) => {
                const id = idOf(row);
                return (
                  <Fragment key={id}>
                    <div
                      role="row"
                      // The row's identity, in the markup: everything acts on this path, so having
                      // it on the element is what lets a test — or a person reading the DOM — see
                      // which row is which without parsing the text back apart.
                      data-path={id}
                      aria-selected={selected.includes(id)}
                      className={[
                        "f-row",
                        selected.includes(id) ? "f-row--on" : null,
                        dropTarget === id ? "f-row--drop" : null,
                        dragging?.includes(id) === true ? "f-row--lift" : null,
                      ].filter(Boolean).join(" ")}
                      onPointerDown={(event) => {
                        press.current = { x: event.clientX, y: event.clientY, id };
                        pick(id, event);
                      }}
                      onPointerEnter={() => {
                        if (dragging !== null && row.is_dir && !dragging.includes(id)) {
                          setDropTarget(id);
                        }
                      }}
                      onPointerLeave={() => dragging !== null && dropTarget === id && setDropTarget(null)}
                      onDoubleClick={() => open(row)}
                      onContextMenu={(event) => {
                        event.preventDefault();
                        event.stopPropagation();
                        if (!selected.includes(id)) setSelected([id]);
                        setMenu({ x: event.clientX, y: event.clientY, target: id });
                      }}
                    >
                      <span className="f-cell f-cell--name">
                        <span className="t-icon">{row.is_dir ? "▸" : "·"}</span>
                        {row.name}
                        {"path" in row && <span className="f-where">{parentPath(row.path) || "Files"}</span>}
                      </span>
                      <span className="f-cell f-cell--size">
                        {row.is_dir ? "—" : formatBytes(row.size_bytes)}
                      </span>
                      <span className="f-cell f-cell--when">
                        {row.modified === null ? "—" : relativeTime(row.modified)}
                      </span>
                    </div>
                    {renaming === id && (
                      <div className="f-rename">
                        <label>
                          New path, under the files folder
                          <input
                            autoFocus
                            value={draft}
                            disabled={busy}
                            onChange={(event) => setDraft(event.target.value)}
                            onKeyDown={(event) => {
                              if (event.key === "Enter") void renameTo(draft);
                              if (event.key === "Escape") setRenaming(null);
                            }}
                          />
                        </label>
                        <Button size="sm" disabled={busy} onClick={() => void renameTo(draft)}>Move</Button>
                        <Button size="sm" disabled={busy} onClick={() => setRenaming(null)}>Cancel</Button>
                      </div>
                    )}
                  </Fragment>
                );
              })}
            </div>

            <p className="f-status">
              {rows.length} item{rows.length === 1 ? "" : "s"}
              {total > 0 && ` · ${formatBytes(total)}`}
              {selected.length > 0 && ` · ${selected.length} selected`}
              {overFromOs && " · drop to upload here"}
            </p>
          </div>
        </div>
      </Panel>

      {menu !== null && (
        <div className="f-menu" style={{ left: menu.x, top: menu.y }} role="menu">
          {menu.target !== null && (
            <>
              <button type="button" role="menuitem" onClick={() => {
                const row = rows.find((one) => idOf(one) === menu.target);
                if (row !== undefined) open(row);
              }}>
                {rows.find((one) => idOf(one) === menu.target)?.is_dir === true ? "Open" : "Download"}
              </button>
              <button type="button" role="menuitem" onClick={() => {
                setRenaming(menu.target);
                setDraft(menu.target ?? "");
              }}>
                Rename or move
              </button>
              <button type="button" role="menuitem" className="f-menu--danger" onClick={() => void remove(selected, false)}>
                Delete
              </button>
            </>
          )}
          {menu.target === null && (
            <>
              <button type="button" role="menuitem" onClick={() => picker.current?.click()}>Upload here</button>
              <button type="button" role="menuitem" onClick={() => void refresh()}>Refresh</button>
            </>
          )}
        </div>
      )}

      {dragging !== null && (
        <p className="f-ghost" aria-hidden="true">
          Moving {dragging.length} item{dragging.length === 1 ? "" : "s"} — drop on a folder
        </p>
      )}
    </div>
  );
}
