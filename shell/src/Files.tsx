import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import {
  createFolder, deleteEntry, downloadFile, listFiles, moveEntry, uploadFile,
  type ConnectionState, type FileEntry,
} from "./api";
import { breadcrumbs, formatBytes, joinPath, parentPath, relativeTime, safeDownloadName } from "./derive";
import { Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/** What the daemon takes in one upload — `files::MAX_UPLOAD_BYTES`, said in the shell's units. */
const MAX_UPLOAD_MB = 100;

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

interface RowProps {
  entry: FileEntry;
  busy: boolean;
  onOpen: (entry: FileEntry) => void;
  onDownload: (entry: FileEntry) => void;
  onRename: (entry: FileEntry) => void;
  onDelete: (entry: FileEntry, recursive: boolean) => void;
  /** Set once a delete came back 409: the folder has things in it and the ask is repeated. */
  fullFolder: boolean;
}

function Row({ entry, busy, onOpen, onDownload, onRename, onDelete, fullFolder }: RowProps) {
  return (
    <li className="f-row">
      <button
        type="button"
        className={entry.is_dir ? "t-entry t-dir" : "t-entry"}
        disabled={busy}
        onClick={() => onOpen(entry)}
      >
        <span className="t-icon">{entry.is_dir ? "▸" : "·"}</span>
        {entry.name}
      </button>
      <span className="f-meta">
        {entry.is_dir ? "folder" : formatBytes(entry.size_bytes)}
        {entry.modified !== null && ` · ${relativeTime(entry.modified)}`}
      </span>
      <span className="f-actions">
        {!entry.is_dir && (
          <Button size="sm" disabled={busy} onClick={() => onDownload(entry)}>
            Download
          </Button>
        )}
        <Button size="sm" disabled={busy} onClick={() => onRename(entry)}>
          Rename
        </Button>
        {fullFolder ? (
          // The second ask, and it says what is at stake rather than repeating the first question:
          // the folder was refused a moment ago precisely because it is not empty.
          <ConfirmButton
            variant="danger"
            size="sm"
            confirmLabel="Delete it and everything in it?"
            disabled={busy}
            onConfirm={() => onDelete(entry, true)}
          >
            Delete anyway
          </ConfirmButton>
        ) : (
          <ConfirmButton
            variant="danger"
            size="sm"
            confirmLabel="Delete for good?"
            disabled={busy}
            onConfirm={() => onDelete(entry, false)}
          >
            Delete
          </ConfirmButton>
        )}
      </span>
    </li>
  );
}

/**
 * The folder, browsable.
 *
 * It is one directory on this machine — under the daemon's own data folder, not a window onto the
 * disk around it — holding the user's uploads and the mail they filed. Everything here goes through
 * the daemon rather than the filesystem: the shell never learns the real path, and every name it
 * sends is resolved against the root by `files::resolve_within` before anything is touched.
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
  const [folderName, setFolderName] = useState("");
  /** The entry whose delete came back 409, so the row can ask the harder question. */
  const [fullFolder, setFullFolder] = useState<string | null>(null);
  /** The entry being renamed, and the path being typed for it. */
  const [renaming, setRenaming] = useState<string | null>(null);
  const [destination, setDestination] = useState("");
  const picker = useRef<HTMLInputElement | null>(null);
  /** Counts listings so a slow one landing after a newer one cannot paint the folder you left. */
  const listing = useRef(0);

  const refresh = useCallback(async () => {
    if (token === null || connection !== "connected") return;
    listing.current += 1;
    const mine = listing.current;
    setLoading(true);
    const result = await listFiles(token, path);
    if (listing.current !== mine) return;
    setLoading(false);
    setEntries(result.ok ? result.value : null);
    setListFailed(
      result.ok ? null : fileFailure(result.status, "That is a file, not a folder."),
    );
  }, [connection, path, token]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // A refusal is about the request that earned it, so it goes when the next one starts — otherwise
  // "that name is taken" sits under a listing where it is no longer true.
  function begin() {
    setFailed(null);
    setNote(null);
    setBusy(true);
  }

  function open(entry: FileEntry) {
    if (entry.is_dir) {
      setPath(joinPath(path, entry.name));
      setFullFolder(null);
      setRenaming(null);
      return;
    }
    void download(entry);
  }

  async function download(entry: FileEntry) {
    if (token === null) return;
    begin();
    const blob = await downloadFile(token, joinPath(path, entry.name));
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
    link.download = safeDownloadName(entry.name);
    link.click();
    URL.revokeObjectURL(url);
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
    setFolderName("");
    setNote(`Created ${folderName.trim()}`);
    void refresh();
  }

  async function upload(files: FileList | null) {
    if (token === null || files === null || files.length === 0) return;
    begin();
    const stored: string[] = [];
    for (const file of Array.from(files)) {
      const result = await uploadFile(token, path, file);
      if (!result.ok) {
        setBusy(false);
        setFailed(
          `${file.name}: ${fileFailure(result.status, "That folder is gone, or is a file.")}`,
        );
        // What already landed is said too. A batch that stops halfway is the case where "it
        // failed" alone is most misleading: some of those files ARE in the folder now.
        if (stored.length > 0) setNote(`Uploaded ${stored.join(", ")}`);
        void refresh();
        return;
      }
      stored.push(result.value);
    }
    setBusy(false);
    // The stored names, not the picked ones: a collision is numbered, so what landed can differ
    // from what was chosen, and saying the wrong name here sends someone looking for a file that
    // is not there.
    setNote(`Uploaded ${stored.join(", ")}`);
    void refresh();
  }

  function startRename(entry: FileEntry) {
    setFailed(null);
    setNote(null);
    setFullFolder(null);
    setRenaming(entry.name);
    // Pre-filled with the full path under the root, because this one box does both jobs: change the
    // last part to rename, change a parent to move.
    setDestination(joinPath(path, entry.name));
  }

  async function commitRename() {
    if (token === null || renaming === null) return;
    const from = joinPath(path, renaming);
    const to = destination.trim();
    if (to === "" || to === from) {
      setRenaming(null);
      return;
    }
    begin();
    const result = await moveEntry(token, from, to);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        fileFailure(result.status, "Something with that name is already there. Nothing was replaced."),
      );
      return;
    }
    setRenaming(null);
    setNote(`${renaming} → ${to}`);
    void refresh();
  }

  async function remove(entry: FileEntry, recursive: boolean) {
    if (token === null) return;
    begin();
    const result = await deleteEntry(token, joinPath(path, entry.name), recursive);
    setBusy(false);
    if (!result.ok) {
      if (result.status === 409) {
        // Not an error to read and dismiss: the row now offers the deletion that was refused.
        setFullFolder(entry.name);
        setFailed(`${entry.name} still has things in it.`);
        return;
      }
      setFailed(fileFailure(result.status, "That folder is not empty."));
      return;
    }
    setFullFolder(null);
    setNote(`Deleted ${entry.name}`);
    void refresh();
  }

  if (token === null || connection !== "connected") {
    return (
      <Panel title="Files">
        <p className="a-note">Not connected to the daemon.</p>
      </Panel>
    );
  }

  const trail = breadcrumbs(path);

  return (
    <div className="stack">
      <Panel
        title="Files"
        aside={entries === null ? undefined : `${entries.length} entries`}
      >
        <nav className="crumbs" aria-label="Path">
          {trail.map((crumb, index) => (
            <span key={crumb.path}>
              {index > 0 && <span className="c-sep">/</span>}
              <button
                type="button"
                className="crumb"
                aria-current={crumb.path === path ? "location" : undefined}
                onClick={() => setPath(crumb.path)}
              >
                {index === 0 ? "Files" : crumb.label}
              </button>
            </span>
          ))}
        </nav>

        <div className="f-tools">
          {path !== "" && (
            <Button size="sm" onClick={() => setPath(parentPath(path))}>Up one level</Button>
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
              void upload(event.target.files);
              // Cleared so picking the same file twice in a row is still a change event.
              event.target.value = "";
            }}
          />
          <Button size="sm" disabled={busy} onClick={() => picker.current?.click()}>
            Upload
          </Button>
          <Button size="sm" disabled={busy} onClick={() => void refresh()}>Refresh</Button>
        </div>

        {note !== null && <p className="a-note">{note}</p>}
        {failed !== null && <ErrorNote>{failed}</ErrorNote>}
        {loading && entries === null && <p className="a-note">Loading…</p>}
        {!loading && listFailed !== null && <ErrorNote>{listFailed}</ErrorNote>}
        {entries !== null && entries.length === 0 && (
          <Teach title="Nothing here yet.">
            This folder holds what you upload and the mail attachments you file from the Mail tab. It
            lives under the daemon's own data directory — uploading copies a file into it rather than
            opening a window onto the rest of the disk.
          </Teach>
        )}

        <ul className="f-list">
          {(entries ?? []).map((entry) => (
            <Fragment key={entry.name}>
              <Row
                entry={entry}
                busy={busy}
                fullFolder={fullFolder === entry.name}
                onOpen={open}
                onDownload={(target) => void download(target)}
                onRename={startRename}
                onDelete={(target, recursive) => void remove(target, recursive)}
              />
              {renaming === entry.name && (
                <li className="f-rename">
                  <label>
                    New path, under the files folder
                    <input
                      autoFocus
                      value={destination}
                      disabled={busy}
                      onChange={(event) => setDestination(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") void commitRename();
                        if (event.key === "Escape") setRenaming(null);
                      }}
                    />
                  </label>
                  <Button size="sm" disabled={busy} onClick={() => void commitRename()}>Move</Button>
                  <Button size="sm" disabled={busy} onClick={() => setRenaming(null)}>Cancel</Button>
                </li>
              )}
            </Fragment>
          ))}
        </ul>
      </Panel>
    </div>
  );
}
