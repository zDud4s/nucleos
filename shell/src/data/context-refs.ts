import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";

/**
 * The context files an agent or a team carries — `core/src/context_refs.rs`.
 *
 * | what          | route                                              |
 * |---------------|----------------------------------------------------|
 * | list          | `GET /context-refs/{owner_kind}/{owner_id}`        |
 * | add           | `POST /context-refs/{owner_kind}/{owner_id}`       |
 * | edit the note | `PUT /context-refs/{owner_kind}/{owner_id}/{id}`   |
 * | remove        | `DELETE /context-refs/{owner_kind}/{owner_id}/{id}`|
 *
 * The daemon validates a path on `POST` (absolute, under the managed files root or, for an agent,
 * a project root, and present on disk) and answers a refusal with a 4xx text the section shows
 * as it is.
 */

export type ContextOwnerKind = "agent" | "team";

/** What a path is on disk right now — `context_refs::PathState`'s wire spelling. */
export type ContextPathState = "file" | "dir" | "missing";

/** One row, as `context_refs::ContextRef` serialises. */
export interface ContextRef {
  id: number;
  owner_kind: ContextOwnerKind;
  owner_id: string;
  path: string;
  /** What it was when the ref was saved. */
  kind: "file" | "dir";
  note: string | null;
  created_at: string;
  /**
   * What it is on disk now. Absent from an answer that did not look, which reads as "present":
   * only an explicit `missing` raises the indicator.
   */
  state?: ContextPathState;
}

// Local rather than in `keys.ts`: one root per owner, so a write refetches that owner's list only.
const contextKey = (kind: ContextOwnerKind, id: string) => ["context-refs", kind, id] as const;

const route = (kind: ContextOwnerKind, id: string) =>
  `/context-refs/${kind}/${encodeURIComponent(id)}`;

export function useContextRefs(kind: ContextOwnerKind, id: string) {
  return useQuery({
    queryKey: contextKey(kind, id),
    queryFn: () => apiFetch<ContextRef[]>(route(kind, id)),
  });
}

function useContextMutation<Input, Result>(
  kind: ContextOwnerKind,
  id: string,
  mutationFn: (input: Input) => Promise<Result>,
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A write is settled; a retried add would be a second 409.
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: contextKey(kind, id) });
    },
  });
}

export function useAddContextRef(kind: ContextOwnerKind, id: string) {
  return useContextMutation(kind, id, (input: { path: string; note?: string }) =>
    apiFetch<ContextRef>(route(kind, id), { method: "POST", body: JSON.stringify(input) }),
  );
}

export function useEditContextNote(kind: ContextOwnerKind, id: string) {
  return useContextMutation(kind, id, ({ refId, note }: { refId: number; note: string | null }) =>
    apiFetch<ContextRef>(`${route(kind, id)}/${refId}`, {
      method: "PUT",
      body: JSON.stringify({ note }),
    }),
  );
}

/** 204, so no body to read. */
export function useRemoveContextRef(kind: ContextOwnerKind, id: string) {
  return useContextMutation(kind, id, (refId: number) =>
    apiFetch<void>(`${route(kind, id)}/${refId}`, { method: "DELETE" }),
  );
}
