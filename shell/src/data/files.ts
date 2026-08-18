import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiBlob, apiFetch, apiText } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The Files pillar — one managed root, seven routes, all verified against
 * `core/src/http.rs` and `core/src/files.rs`.
 *
 * **Every mutation here answers a status this shell's other pillars have
 * already had to name once, never twice.** `POST /files/folder` is 201 with
 * an empty body — the same empty-body class as `data/runs.ts`'s
 * `useCancelRun`, so it goes through {@link apiText}, never `apiFetch`.
 * `POST /files/move` and `DELETE /files` are both 204, so `apiFetch<void>` is
 * safe on them unchanged.
 *
 * **All four mutating routes are Admin/control scope** (`auth.rs:130-135`);
 * the three readers are not. This module does not gate on that itself — the
 * daemon does, and a refusal a person cannot act on is exactly what
 * {@link isApiRefusal} and the page's own copy exist to explain.
 *
 * **The folder listing polls** (`POLL.queue`), unlike Projects' `ls`, which
 * reads once on click. The two look alike and are not: a project's tree
 * changes because a person or a job changed it while looking at *that*
 * project, but this root is shared — mail filing and team workspaces write
 * into it too, from outside this window entirely (`memory.md`, 2026-08-16).
 * A listing that never refetches would miss a file that landed here from
 * somewhere else. Search stays on-demand: a query is something a person
 * asked once, not a fact that changes on its own.
 */

/* ----------------------------------------------------------------- shapes -- */

/**
 * One entry of a folder listing — `GET /files?path=`, `files.rs:33-41`.
 *
 * `size_bytes`, never `size`. Zero for a directory, not a placeholder for an
 * unknown size. `modified` is `null` when the platform will not say, which is
 * a real answer and not a missing one. An entry whose metadata could not be
 * read at all is skipped by the daemon before it ever reaches the wire — this
 * shell never sees the gap and has nothing to render for it.
 */
export interface Entry {
  name: string;
  is_dir: boolean;
  size_bytes: number;
  modified: string | null;
}

/**
 * One search hit — `GET /files/search`, `files.rs:193-258`.
 *
 * The wire object is FLAT (`#[serde(flatten)]` on the daemon side, no nested
 * `entry` key), which is why this is its own shape rather than `Entry & {
 * path }`. `path` is relative to the root with forward slashes, regardless of
 * the platform this window is running on.
 */
export interface Hit {
  path: string;
  name: string;
  is_dir: boolean;
  size_bytes: number;
  modified: string | null;
}

/** What a search answers. `truncated` is true at 500 hits or 20,000 visits — say so, never hide the cut. */
export interface Found {
  hits: Hit[];
  truncated: boolean;
}

/**
 * What a save answers — `POST /files/folder`... no: `POST /files/upload`,
 * `http.rs:1451-1489`.
 *
 * `filename` is the name the upload was ACTUALLY stored under: sanitised and
 * de-collided against whatever is already in the folder
 * (`relatorio.docx` → `relatorio (2).docx`). The name a person typed or
 * dropped is cosmetic the moment this answer arrives; this is the one that is
 * real. Same shape as `data/mail.ts`'s `SavedFile` — same daemon type,
 * `files.rs`, reached through a different route — declared again here rather
 * than imported, because the two pages own their own request shapes and nothing
 * requires them to share a module.
 */
export interface SavedFile {
  filename: string;
  folder: string;
}

/**
 * One file the host handed over from an OS drop — `files://dropped`'s
 * payload, `drop.rs:44-56`.
 *
 * `folder` is where this file goes UNDER the folder being viewed when the
 * drop landed, so a dropped tree keeps its shape; `""` for a lone file with no
 * containing folder of its own. `path` is the REAL filesystem path this
 * window is briefly allowed to read via `invoke("read_dropped", { path })` —
 * the allowed set is replaced whole on the next drop, so this value is only
 * good until then.
 */
export interface DroppedFile {
  path: string;
  folder: string;
  name: string;
  size: number;
}

/** The whole drop, plus whether the host's own walk was cut short. */
export interface Dropped {
  files: DroppedFile[];
  truncated: boolean;
}

/** What `POST /files/move` accepts — `files.rs:411-431`. Never overwrites; a taken `to` is a 409. */
export interface MoveRequest {
  from: string;
  to: string;
}

/** What `DELETE /files` accepts, as query params — there is no body. */
export interface DeleteRequest {
  path: string;
  /** Default `false` on the wire. A non-empty directory without it is a 409, not a silent partial delete. */
  recursive: boolean;
}

/** What `POST /files/upload` accepts, beyond the bytes themselves. */
export interface UploadRequest {
  /** `""` is the root — `#[serde(default)]` on the daemon's side already means this. */
  folder: string;
  filename: string;
  bytes: ArrayBuffer;
}

/**
 * The daemon's own upload ceiling — `http.rs`'s 104,857,600 bytes (100 MiB).
 *
 * Read here so the page can say so *before* sending a file too large to fit,
 * rather than only after a 413 comes back. The daemon's own check is still the
 * one that is enforced; this is a courtesy, not a second source of truth.
 */
export const MAX_UPLOAD_BYTES = 104_857_600;

/* ------------------------------------------------------------------ reads -- */

/**
 * One folder's contents. The empty path is the managed root itself.
 *
 * `enabled` defaults to on for the main listing; `FolderTree` passes `false`
 * for a node that has not been expanded yet, which is the whole of what makes
 * the tree *lazy* — a collapsed branch asks the daemon nothing.
 */
export function useFolder(path: string, enabled = true) {
  return useQuery({
    queryKey: keys.files.list(path),
    queryFn: () => apiFetch<Entry[]>(`/files?path=${encodeURIComponent(path)}`),
    refetchInterval: POLL.queue,
    enabled,
  });
}

/**
 * A name search under a folder, recursive.
 *
 * `enabled` defaults to on but is gated on `q` being non-blank regardless: the
 * route's own contract is that an empty `q` is never sent (axum 400s a
 * missing one with prose, and a blank one the daemon would happily walk the
 * whole tree for nothing). Debouncing the keystroke into `q` is the page's
 * job — this hook only decides whether to ask.
 */
export function useFileSearch(path: string, q: string, enabled = true) {
  const trimmed = q.trim();
  return useQuery({
    queryKey: keys.files.search(path, trimmed),
    queryFn: () =>
      apiFetch<Found>(`/files/search?path=${encodeURIComponent(path)}&q=${encodeURIComponent(trimmed)}`),
    enabled: enabled && trimmed !== "",
  });
}

/* -------------------------------------------------------------- mutations -- */

/**
 * Make a folder.
 *
 * **`apiText`, not `apiFetch`.** `201 Created` with an EMPTY body — `apiFetch`
 * exempts only 204/205 and would throw trying to parse nothing as JSON. This
 * extends the empty-body class already on record (`memory.md`, 2026-08-18)
 * past 200/202/204 to 201; the idiom is `data/runs.ts`'s `useCancelRun`.
 * Creating a folder that already exists still succeeds (`create_dir_all`) —
 * this is not an upsert failing quietly, it is the operation being what it
 * says: make sure this folder is there.
 */
export function useCreateFolder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (path: string) => {
      await apiText("/files/folder", { method: "POST", body: JSON.stringify({ path }) });
    },
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.files.all });
    },
  });
}

/**
 * Send bytes.
 *
 * RAW BYTES in the body, never `FormData` — the daemon reads the request body
 * directly and was never given a multipart parser. `filename`/`folder` travel
 * in the query string, which is also where `apiFetch`'s own JSON
 * `Content-Type` default would get in the way; this sets its own header so
 * `client.ts`'s `request()` does not stamp one over it.
 */
export function useUpload() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ folder, filename, bytes }: UploadRequest) =>
      apiFetch<SavedFile>(
        `/files/upload?folder=${encodeURIComponent(folder)}&filename=${encodeURIComponent(filename)}`,
        { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: bytes },
      ),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.files.all });
    },
  });
}

/**
 * Move (or rename — the same request, a `to` inside the same folder).
 *
 * `204`, so `apiFetch<void>` is safe unchanged. **Never overwrites**: a taken
 * `to` is `PathError::Exists`, a 409 with an empty body, which this shell
 * names on the page rather than showing the daemon's silence.
 */
export function useMove() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (move: MoveRequest) =>
      apiFetch<void>("/files/move", { method: "POST", body: JSON.stringify(move) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.files.all });
    },
  });
}

/**
 * Remove a path. `recursive` defaults false on the wire; a non-empty
 * directory without it is a 409, and only the recursive retry is ever wrapped
 * in a `ConfirmButton` — the plain delete cannot take more than the one entry
 * a person named.
 */
export function useDelete() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ path, recursive }: DeleteRequest) => {
      const params = new URLSearchParams({ path });
      if (recursive) params.set("recursive", "true");
      return apiFetch<void>(`/files?${params.toString()}`, { method: "DELETE" });
    },
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.files.all });
    },
  });
}

/**
 * Fetch one file's bytes and hand them to the browser as a download.
 *
 * A plain function, not a hook — `data/mail.ts`'s `useDownloadAttachment` is a
 * mutation because it needs `isPending`/`isError` wired to one row's button;
 * this is invoked from more places (a row's action, the context menu, a
 * keyboard shortcut) and the caller owns whatever pending/error state it
 * needs, the same way `voice.ts`'s `postCapture` is a function callers await
 * rather than a hook.
 *
 * `apiBlob`, never `apiFetch` or `apiText`: the body is
 * `application/octet-stream`, always, and both of those would either fail to
 * parse it or silently mangle any byte that is not valid UTF-8.
 */
export async function downloadFile(path: string): Promise<void> {
  const blob = await apiBlob(`/files/download?path=${encodeURIComponent(path)}`);
  const url = URL.createObjectURL(blob);
  try {
    const link = document.createElement("a");
    link.href = url;
    link.download = fileName(path);
    document.body.appendChild(link);
    link.click();
    document.body.removeChild(link);
  } finally {
    URL.revokeObjectURL(url);
  }
}

/* ---------------------------------------------------------------- helpers -- */

/** A path's parts, for the breadcrumb trail. The root is the empty path. */
export function pathSegments(path: string): string[] {
  return path.split("/").filter((part) => part !== "");
}

/** The path formed by walking `depth` segments in from the root. */
export function pathUpTo(path: string, depth: number): string {
  return pathSegments(path).slice(0, depth).join("/");
}

/** The folder one level up. The root's parent is the root — there is nowhere above it. */
export function parentPath(path: string): string {
  const segments = pathSegments(path);
  return segments.slice(0, -1).join("/");
}

/** One step deeper, without the leading slash a root join would leave behind. */
export function joinPath(path: string, name: string): string {
  return path === "" ? name : `${path}/${name}`;
}

/** The last segment of a path — what a person would call the file, stripped of where it lives. */
export function fileName(path: string): string {
  const segments = pathSegments(path);
  return segments.length === 0 ? path : segments[segments.length - 1];
}

/**
 * Where a dropped file lands: under the folder being viewed, keeping whatever
 * shape the drop itself had.
 *
 * The pure core of the OS-drop wiring, kept apart from the `invoke`/`listen`
 * plumbing around it so the one rule that matters — a dropped tree keeps its
 * shape, a lone file has no folder of its own — is checkable without a Tauri
 * mock in sight. `droppedFolder` is the payload's own `folder` field, `""` for
 * a file dropped on its own.
 */
export function dropTargetFolder(currentPath: string, droppedFolder: string): string {
  if (droppedFolder === "") return currentPath;
  return joinPath(currentPath, droppedFolder);
}

/** The columns a listing can be sorted by. */
export type SortColumn = "name" | "size" | "modified";
export type SortDirection = "asc" | "desc";

/**
 * One stable order for a folder's rows.
 *
 * Directories sort before files regardless of column or direction — the daemon's
 * own default order does the same (`files.rs`'s `folder_status`), and a table
 * that lost it the moment somebody clicked "Size" would make a deep tree
 * unwalkable the instant it was sorted by anything but name. Within each group,
 * the chosen column decides; a `null` `modified` sorts last in either
 * direction, because "unknown" is not a time before or after any other time.
 */
export function sortEntries(entries: Entry[], column: SortColumn, direction: SortDirection): Entry[] {
  const sign = direction === "asc" ? 1 : -1;
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    if (column === "name") return sign * a.name.localeCompare(b.name);
    if (column === "size") return sign * (a.size_bytes - b.size_bytes);
    // modified
    if (a.modified === null && b.modified === null) return 0;
    if (a.modified === null) return 1;
    if (b.modified === null) return -1;
    return sign * a.modified.localeCompare(b.modified);
  });
}

/** Bytes, for a person — the same three-step scale `MailDetail.tsx` uses for attachments. */
export function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}
