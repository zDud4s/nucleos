import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  useAddContextRef,
  useContextRefs,
  useEditContextNote,
  useRemoveContextRef,
  type ContextOwnerKind,
  type ContextRef,
} from "../data/context-refs";
import { isApiRefusal } from "../data/client";
import type { DroppedFile } from "../data/files";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  Field,
  Quiet,
  RefusalNote,
  Row,
  Rows,
} from "../ui";
import "./context.css";

export interface ContextRefsProps {
  ownerKind: ContextOwnerKind;
  ownerId: string;
}

/**
 * The path a drop means for one of the files it resolved to.
 *
 * The host walks a dropped folder into its files and tells each where it sits under that folder
 * (`Docs/sub`); a ref wants the folder itself. So climb back out of the file's own directory,
 * one segment per sub-folder, to the dropped folder. A file dropped on its own has no folder and
 * is its own path.
 */
export function droppedRoot(file: DroppedFile): string {
  if (file.folder === "") return file.path;
  const depth = file.folder.split("/").length;
  const parts = file.path.split(/([\\/])/);
  // parts alternates segment and separator: the file name and its sub-folders go, with their separators.
  return parts.slice(0, parts.length - 2 * depth).join("");
}

/**
 * What one agent or one team may read: the files and folders its runs are given, through the
 * daemon's `read_context`. The daemon decides what is allowed — a path outside its roots is a
 * refusal shown as it came — and says on every listing whether a path is still on disk.
 *
 * A path is typed or dropped on the window; there is no native picker, because the dialog plugin is
 * not part of this shell and a browser file input would hand over a name, never a path.
 */
export function ContextRefs({ ownerKind, ownerId }: ContextRefsProps) {
  const refs = useContextRefs(ownerKind, ownerId);
  const add = useAddContextRef(ownerKind, ownerId);
  const [path, setPath] = useState("");
  const [note, setNote] = useState("");

  // The drop listener is registered once; it reaches the mutation through a ref.
  const addRef = useRef(add.mutate);
  useEffect(() => {
    addRef.current = add.mutate;
  });

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    // Outside the Tauri window there is nothing to listen to, and that is not an error.
    try {
      void listen<{ files: DroppedFile[] }>("files://dropped", (event) => {
        const roots = new Set(event.payload.files.map(droppedRoot));
        for (const dropped of roots) addRef.current({ path: dropped });
      })
        .then((fn) => {
          if (live) unlisten = fn;
          else fn();
        })
        .catch(() => {});
    } catch {
      // not inside the app
    }
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  function submit(event: FormEvent) {
    event.preventDefault();
    const trimmed = path.trim();
    if (trimmed === "") return;
    const text = note.trim();
    add.mutate(
      { path: trimmed, ...(text === "" ? {} : { note: text }) },
      {
        onSuccess: () => {
          setPath("");
          setNote("");
        },
      },
    );
  }

  let body: ReactNode;
  if (refs.isError) {
    body = <Quiet says="the núcleo did not answer — these context files are unknown" />;
  } else if (refs.data === undefined) {
    body = <Quiet says="Loading…" />;
  } else if (refs.data.length === 0) {
    body = <Quiet says="No context files yet." />;
  } else {
    body = (
      <Rows label={`Context of ${ownerKind} ${ownerId}`}>
        {refs.data.map((row) => (
          <RefRow key={row.id} row={row} ownerKind={ownerKind} ownerId={ownerId} />
        ))}
      </Rows>
    );
  }

  return (
    <section className="context-refs" aria-label="Context">
      <h3>Context</h3>
      <p className="context-lede">
        Files and folders this {ownerKind}&rsquo;s runs may read. Type an absolute path, or drop a
        file or folder on the window.
      </p>
      {body}
      <form className="context-add" onSubmit={submit}>
        <Field label="Path">
          <input
            value={path}
            onChange={(event) => setPath(event.target.value)}
            placeholder="C:\files\brief.md"
            autoComplete="off"
            spellCheck={false}
          />
        </Field>
        <Field label="Note">
          <input
            value={note}
            onChange={(event) => setNote(event.target.value)}
            placeholder="what it is for (optional)"
            autoComplete="off"
          />
        </Field>
        <Button type="submit" variant="approve" disabled={path.trim() === "" || add.isPending}>
          Add
        </Button>
      </form>
      {add.isError && <WriteRefusal error={add.error} />}
    </section>
  );
}

function RefRow({
  row,
  ownerKind,
  ownerId,
}: {
  row: ContextRef;
  ownerKind: ContextOwnerKind;
  ownerId: string;
}) {
  const edit = useEditContextNote(ownerKind, ownerId);
  const remove = useRemoveContextRef(ownerKind, ownerId);
  const [draft, setDraft] = useState<string | null>(null);

  function save(event: FormEvent) {
    event.preventDefault();
    const text = (draft ?? "").trim();
    edit.mutate(
      { refId: row.id, note: text === "" ? null : text },
      { onSuccess: () => setDraft(null) },
    );
  }

  return (
    <Row layout="stack">
      <div className="context-head">
        <code className="context-path">{row.path}</code>
        <span className="context-kind">{row.kind === "dir" ? "folder" : "file"}</span>
        {row.state === "missing" && (
          <span className="context-missing" title="nothing exists at this path any more">
            missing
          </span>
        )}
      </div>
      {draft === null ? (
        <div className="context-note">
          <span className={row.note === null ? "context-note-none" : undefined}>
            {row.note ?? "no note"}
          </span>
          <Button variant="quiet" onClick={() => setDraft(row.note ?? "")}>
            Edit note
          </Button>
          <ConfirmButton
            label="Remove"
            confirmLabel={`Remove ${row.path}`}
            variant="danger"
            disabled={remove.isPending}
            onConfirm={() => remove.mutate(row.id)}
          />
        </div>
      ) : (
        <form className="context-note" onSubmit={save}>
          <Field label={`Note for ${row.path}`} labelHidden>
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              autoComplete="off"
              autoFocus
            />
          </Field>
          <Button type="submit" variant="approve" disabled={edit.isPending}>
            Save note
          </Button>
          <Button variant="ghost" onClick={() => setDraft(null)}>
            Cancel
          </Button>
        </form>
      )}
      {edit.isError && <WriteRefusal error={edit.error} />}
      {remove.isError && <WriteRefusal error={remove.error} />}
    </Row>
  );
}

/** The núcleo's own sentence when it refused; otherwise that it did not answer. */
function WriteRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was changed</ErrorNote>;
  const said = error.detail.trim();
  return (
    <RefusalNote
      refusal={error}
      sentences={said === "" ? undefined : { unprocessable: said, conflict: said, not_found: said }}
    />
  );
}
