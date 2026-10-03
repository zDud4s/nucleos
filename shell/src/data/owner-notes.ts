import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The owner's notes, as hooks — `core/src/owner_notes.rs` over `/owner-notes`.
 *
 * Routes, read off `core/src/http.rs`:
 *
 * | what                 | route                                 |
 * |----------------------|---------------------------------------|
 * | list by state        | `GET /owner-notes?state=`             |
 * | create               | `POST /owner-notes`                   |
 * | search               | `GET /owner-notes/search?q=`          |
 * | one, with its links  | `GET /owner-notes/{id}`               |
 * | edit text or state   | `PATCH /owner-notes/{id}`             |
 * | add a link           | `POST /owner-notes/{id}/links`        |
 * | remove a link        | `DELETE /owner-notes/links/{link_id}` |
 * | the whole graph      | `GET /owner-notes/graph`              |
 * | teach to the agent   | `POST /owner-notes/{id}/teach`        |
 */

/* ----------------------------------------------------------------- shapes -- */

export type NoteState = "active" | "archived";
export type NoteOrigin = "shell" | "telegram";

export const LINK_TYPES = ["relates", "supports", "contradicts", "details", "supersedes"] as const;
export type LinkType = (typeof LINK_TYPES)[number];

export const TARGET_KINDS = ["note", "knowledge", "project", "contact", "mail", "file"] as const;
export type TargetKind = (typeof TARGET_KINDS)[number];

export interface OwnerNote {
  id: number;
  text: string;
  origin: NoteOrigin;
  state: NoteState;
  created_at: string;
  updated_at: string;
}

export interface NoteLink {
  id: number;
  note_id: number;
  link_type: LinkType;
  target_kind: TargetKind;
  target_ref: string;
  created_at: string;
}

export interface NoteEvent {
  id: number;
  note_id: number;
  kind: string;
  detail: string | null;
  at: string;
}

export interface NoteDetail {
  note: OwnerNote;
  links_out: NoteLink[];
  links_in: NoteLink[];
  events: NoteEvent[];
}

/** What a link points at, with a label and whether it still resolves — `owner_notes::Target`. */
export interface NoteTarget {
  kind: TargetKind;
  ref: string;
  label: string | null;
  missing: boolean;
}

export interface NotesGraph {
  notes: OwnerNote[];
  links: NoteLink[];
  targets: NoteTarget[];
}

/* ------------------------------------------------------------------ reads -- */

export function useOwnerNotes(state: NoteState | "all") {
  return useQuery({
    queryKey: keys.ownerNotes.list(state),
    queryFn: () => apiFetch<OwnerNote[]>(`/owner-notes?state=${state}`),
  });
}

/** One note's links and events, fetched only when somebody opens it. */
export function useOwnerNote(id: number | null) {
  return useQuery({
    queryKey: keys.ownerNotes.detail(id ?? 0),
    queryFn: () => apiFetch<NoteDetail>(`/owner-notes/${id}`),
    enabled: id !== null,
  });
}

/** Never sent for a blank query: the daemon has nothing useful to say to it. */
export function useSearchOwnerNotes(q: string) {
  return useQuery({
    queryKey: keys.ownerNotes.search(q),
    queryFn: () => apiFetch<OwnerNote[]>(`/owner-notes/search?q=${encodeURIComponent(q)}`),
    enabled: q.trim() !== "",
  });
}

export function useNotesGraph(includeArchived: boolean) {
  return useQuery({
    queryKey: keys.ownerNotes.graph(includeArchived),
    queryFn: () => apiFetch<NotesGraph>(`/owner-notes/graph?include_archived=${includeArchived}`),
  });
}

/* ---------------------------------------------------------------- writes -- */

function useNoteMutation<Input, Result>(mutationFn: (input: Input) => Promise<Result>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A write is settled; a retried create would be a second note.
    retry: false,
    // `onSettled`: a failed write still leaves the screen's copy suspect.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.ownerNotes.all });
    },
  });
}

export function useCreateNote() {
  return useNoteMutation((input: { text: string; origin: NoteOrigin }) =>
    apiFetch<{ id: number }>("/owner-notes", { method: "POST", body: JSON.stringify(input) }),
  );
}

export function useUpdateNote() {
  return useNoteMutation(({ id, ...patch }: { id: number; text?: string; state?: NoteState }) =>
    apiFetch<OwnerNote>(`/owner-notes/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  );
}

export function useAddLink() {
  return useNoteMutation(
    ({
      noteId,
      ...link
    }: {
      noteId: number;
      link_type: LinkType;
      target_kind: TargetKind;
      target_ref: string;
    }) =>
      apiFetch<{ id: number }>(`/owner-notes/${noteId}/links`, {
        method: "POST",
        body: JSON.stringify(link),
      }),
  );
}

export type TeachKind = "memory" | "prompt" | "skill" | "subagent";
export const TEACH_KINDS: readonly TeachKind[] = ["memory", "prompt", "skill", "subagent"];

/**
 * Teach a note to the agent: it becomes a pending knowledge proposal, answered in Learned.
 * Refusals come back as 409 `already_taught` / `archived` and 400 `unknown_kind`.
 * Invalidates the knowledge list too, since a new row now exists there.
 */
export function useTeachNote() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: { id: number; kind: TeachKind; title?: string }) =>
      apiFetch<{ knowledge_id: number; proposal_id: number; link_id: number }>(
        `/owner-notes/${id}/teach`,
        { method: "POST", body: JSON.stringify(body) },
      ),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.ownerNotes.all });
      void queryClient.invalidateQueries({ queryKey: keys.knowledge.all });
    },
  });
}

/** 204, so no body to read. */
export function useRemoveLink() {
  return useNoteMutation((linkId: number) =>
    apiFetch<void>(`/owner-notes/links/${linkId}`, { method: "DELETE" }),
  );
}
