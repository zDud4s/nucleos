import { useEffect, useRef, useState } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { DAEMON_NAME } from "../copy";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import {
  useUpdateNote,
  type NoteDetail,
  type NotesGraph,
  type OwnerNote,
} from "../data/owner-notes";
import { Button, ErrorNote, RefusalNote } from "../ui";
import { NoteText } from "./NoteText";

/** Put an updated note into the detail and graph caches, leaving every other cached shape alone. */
function writeSavedNote(queryClient: QueryClient, saved: OwnerNote) {
  queryClient.setQueryData<NoteDetail>(keys.ownerNotes.detail(saved.id), (old) =>
    old ? { ...old, note: saved } : old,
  );
  for (const archived of [false, true]) {
    queryClient.setQueryData<NotesGraph>(keys.ownerNotes.graph(archived), (old) =>
      old ? { ...old, notes: old.notes.map((n) => (n.id === saved.id ? saved : n)) } : old,
    );
  }
}

/** A refusal as the daemon's own text; anything else means there was no answer. */
function Failure({ error }: { error: unknown }) {
  if (error instanceof ApiRefusal) return <RefusalNote refusal={error} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding</ErrorNote>;
}

/**
 * A note's text with an Edit button. Edit swaps to a textarea; Save PATCHes `{ text }` through
 * `useUpdateNote`, Esc or Cancel restores the view untouched. Replaces only the text paragraph of
 * the note panel header.
 */
export function NoteTextEdit({ id, text, disabled }: { id: number; text: string; disabled?: boolean }) {
  const update = useUpdateNote();
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(text);
  const area = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (editing) area.current?.focus();
  }, [editing]);

  const cancel = () => {
    update.reset();
    setEditing(false);
  };
  const blank = draft.trim() === "";
  const unchanged = draft === text;
  const save = () => {
    if (blank || unchanged || update.isPending) return;
    update.mutate(
      { id, text: draft },
      {
        onSuccess: (saved) => {
          // Write the saved note into the caches before leaving edit mode, so the view never
          // flashes the old text, and keeps the new one if the refetch that follows fails.
          writeSavedNote(queryClient, saved);
          setEditing(false);
        },
      },
    );
  };

  if (!editing) {
    return (
      <div className="brain-note-view">
        <NoteText text={text} />
        <Button
          variant="quiet"
          disabled={disabled}
          onClick={() => {
            setDraft(text);
            update.reset();
            setEditing(true);
          }}
        >
          Edit
        </Button>
      </div>
    );
  }

  return (
    <div
      className="brain-note-edit"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          cancel();
        }
      }}
    >
      <textarea
        ref={area}
        aria-label="Note text"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
      />
      <div className="brain-note-edit-actions">
        <Button variant="ghost" disabled={blank || unchanged || update.isPending} onClick={save}>
          Save
        </Button>
        <Button variant="quiet" onClick={cancel}>
          Cancel
        </Button>
      </div>
      {update.isError && <Failure error={update.error} />}
    </div>
  );
}
