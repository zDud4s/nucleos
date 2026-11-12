import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The owner decisions inbox: what parks a tree branch until somebody answers.
 *
 * Not the same table as `useMapDecisions` in `project-map.ts` -- that is the project map's
 * extraction pile. This reads `GET /decisions?status=open`.
 */

/** `"decision"` can be answered; `"notice"` (the `pending-*` ids) is information only. */
export type OwnerDecisionKind = "decision" | "notice";

export interface OwnerDecision {
  id: string;
  /** `null` on rows that do not come from a tree (origin `owner_decisions`). */
  tree_id: string | null;
  node_id: string | null;
  /** The full text, which may be a very long gate log. */
  question: string;
  /** ISO timestamp the decision was opened. */
  at: string;
  status: string;
  answer: string | null;
  kind: OwnerDecisionKind;
}

/** The wire row: everything but `id` and `question` may be absent, and unknown fields are ignored. */
interface WireDecision {
  id: string;
  tree_id?: string | null;
  node_id?: string | null;
  question?: string | null;
  at?: string | null;
  status?: string | null;
  answer?: string | null;
  kind?: string | null;
}

/** A missing or unrecognised `kind` is a decision: the safe reading is the one the owner can act on. */
export function parseOwnerDecision(row: WireDecision): OwnerDecision {
  return {
    id: row.id,
    tree_id: row.tree_id ?? null,
    node_id: row.node_id ?? null,
    question: row.question ?? "",
    at: row.at ?? "",
    status: row.status ?? "open",
    answer: row.answer ?? null,
    kind: row.kind === "notice" ? "notice" : "decision",
  };
}

export function isAnswerable(decision: OwnerDecision): boolean {
  return decision.kind === "decision";
}

/** The badge count: answerable decisions only. `undefined` until the list has answered once. */
export function answerableCount(decisions: OwnerDecision[] | undefined): number | undefined {
  return decisions === undefined ? undefined : decisions.filter(isAnswerable).length;
}

export function useOwnerDecisions() {
  return useQuery({
    queryKey: keys.ownerDecisions.open,
    queryFn: async () => {
      const rows = await apiFetch<WireDecision[]>("/decisions?status=open");
      return rows.map(parseOwnerDecision);
    },
    refetchInterval: POLL.queue,
  });
}

export interface AnswerOwnerDecisionInput {
  id: string;
  answer: string;
}

/** Answer one decision. Not retried: a refused answer does not become a different one. */
export function useAnswerOwnerDecision() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, answer }: AnswerOwnerDecisionInput) =>
      apiFetch<void>(`/decisions/${encodeURIComponent(id)}`, {
        method: "POST",
        body: JSON.stringify({ answer }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.ownerDecisions.open });
    },
  });
}
