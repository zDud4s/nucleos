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
  /** Why the daemon marked it `obsolete`; `null` otherwise. */
  obsolete_reason?: string | null;
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
  /** Set only when `status` is `obsolete`. */
  obsolete_reason?: string | null;
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
    obsolete_reason: row.obsolete_reason ?? null,
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
      // The daemon already returns live rows only; filter again so an `obsolete` (or any
      // unknown) status is never counted or shown as waiting.
      return rows.map(parseOwnerDecision).filter((d) => d.status === "open");
    },
    refetchInterval: POLL.queue,
  });
}

const FENCE_PARK =
  /changed files outside its declared paths \[[^\]]*\]:\s*([\s\S]+?)\.\s*Move that change to the branch that owns those paths/;

/**
 * The files a fence-park decision names after the colon, or `null` when the question is not a
 * fence park (or names no file). The wire row carries no structured list, so the text is parsed.
 */
export function fenceParkFiles(question: string): string[] | null {
  const match = FENCE_PARK.exec(question);
  if (match === null) return null;
  const files = match[1]
    .split(",")
    .map((file) => file.trim().replace(/^[`'"]+|[`'"]+$/g, ""))
    .filter((file) => file !== "");
  return files.length === 0 ? null : files;
}

export interface AnswerOwnerDecisionInput {
  id: string;
  answer: string;
  /** Files to add to the branch's declared paths; sent only when present. */
  widen_paths?: string[];
}

/** Answer one decision. Not retried: a refused answer does not become a different one. */
export function useAnswerOwnerDecision() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, answer, widen_paths }: AnswerOwnerDecisionInput) =>
      apiFetch<void>(`/decisions/${encodeURIComponent(id)}`, {
        method: "POST",
        body: JSON.stringify(widen_paths === undefined ? { answer } : { answer, widen_paths }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.ownerDecisions.open });
    },
  });
}
