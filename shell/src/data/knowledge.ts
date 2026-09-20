import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * What the agent knows, as hooks.
 *
 * **This store is invisible from every other page, by construction.** Something
 * waiting for an answer is a `kind = 'refinement'` proposal, and
 * `proposals::list_pending` filters `kind = 'action-approval'`
 * (`core/src/proposals.rs`) — so the Waiting queue, which is the page for
 * things that stopped to ask you something, cannot see one. The listing that
 * can is `GET /knowledge`, and it answers every status rather than only the
 * pending ones, because the reviewable history is the whole point of the table.
 *
 * **`refinement` is still the proposal's kind, and that is not an oversight.**
 * It is a value already written to rows on disk that `0143_knowledge.sql` does
 * not rewrite; renaming it would orphan every question still waiting for an
 * answer. The store is `knowledge`; the question about it kept its old word.
 *
 * Three routes, read off `core/src/http.rs` rather than inferred from the
 * names:
 *
 * | what                | route                          |
 * |---------------------|--------------------------------|
 * | the store, all of it| `GET /knowledge`               |
 * | one, with its chain | `GET /knowledge/{id}`          |
 * | take an active one back | `POST /knowledge/{id}/revert` |
 *
 * The two decisions — yes and no — are **not** here: they go through the
 * ordinary proposal doors (`POST /proposals/{id}/approve` and `/reject`), which
 * dispatch on the proposal's kind. That is why the row carries `proposal_id`.
 *
 * `POLL.queue`: a store only changes when a person decides something or a run
 * declares something, and five seconds is soon enough for a list worked through
 * by hand.
 */

/* ----------------------------------------------------------------- shapes -- */

/** The four kinds, in the order a node reads them — `knowledge::Kind`. */
export type KnownKind = "prompt" | "memory" | "skill" | "subagent";

/**
 * The nature of what is known — `knowledge::Layer`.
 *
 * The column `0143_knowledge.sql` added, and the one that makes this one store
 * rather than four: a fact about the project, a measurement of what happened,
 * how work is done here, and what one job knows while it runs.
 */
export type KnownLayer = "semantic" | "episodic" | "procedural" | "working";

/**
 * Whose it is. Two columns rather than one, because `errand` and `job` are not
 * projects and a single nullable `project_id` could not say so.
 */
export type KnownScope = "machine" | "project" | "errand" | "job";

/**
 * A row's status.
 *
 * Nine and not five: `0088`'s five said everything a person decides, and the
 * four `0143` adds say what the store does to itself — merged into a lesson,
 * closed with its job, stopped being confirmed, or live inside one job's run.
 * `active` still means exactly one thing, which is why `live` exists at all.
 */
export type KnownStatus =
  | "proposed"
  | "active"
  | "rejected"
  | "reverted"
  | "superseded"
  | "archived"
  | "closed"
  | "expired"
  | "live";

/** One thing the agent knows, as `knowledge::Known` serialises. */
export interface Known {
  id: number;
  layer: KnownLayer;
  scope_kind: KnownScope;
  /** `null` only for `machine`: a lesson about the house rather than about a repo. */
  scope_id: string | null;
  /** Who knocked at the door — never read from the body. */
  source: "owner" | "run" | "consolidator";
  kind: KnownKind;
  title: string;
  body: string;
  status: KnownStatus;
  /** The question that let it in. The door a decision is sent through. */
  proposal_id: number | null;
  /** The row this one replaces, ended when this one was approved. */
  supersedes: number | null;
  origin_run_id: number | null;
  created_at: string;
  activated_at: string | null;
  ended_at: string | null;
}

/** One decision in a row's life — `knowledge::Event`. */
export interface KnowledgeEvent {
  id: number;
  from_status: KnownStatus | null;
  to_status: KnownStatus;
  note: string | null;
  at: string;
}

/** One row, its decisions, and the chain on both sides — `knowledge::History`. */
export interface KnowledgeHistory {
  known: Known;
  events: KnowledgeEvent[];
  /** Newest first: what this replaced, then what THAT replaced. */
  replaced: Known[];
  replaced_by: Known | null;
}

/* ------------------------------------------------------------------ reads -- */

/** Everything the store holds, in every status. */
export function useKnowledge() {
  return useQuery({
    queryKey: keys.knowledge.all,
    queryFn: () => apiFetch<Known[]>("/knowledge"),
    refetchInterval: POLL.queue,
  });
}

/**
 * One row's chain and decisions, fetched only when somebody opens it.
 *
 * `enabled` rather than a prefetch: the chain is a second request per row, and
 * a page that fetched one for every row would make reading a list of forty cost
 * forty-one requests to answer a question nobody asked yet.
 */
export function useKnowledgeHistory(id: number | null) {
  return useQuery({
    queryKey: keys.knowledge.detail(id ?? 0),
    queryFn: () => apiFetch<KnowledgeHistory>(`/knowledge/${id}`),
    enabled: id !== null,
  });
}

/* -------------------------------------------------------------- decisions -- */

/**
 * Every list a decision can move, invalidated together.
 *
 * The detail cache comes too: approving a successor changes the *predecessor's*
 * row — it ends it — so a chain left on screen from before the decision would
 * show a superseded text as still in force.
 */
function useKnowledgeDecision<Input>(mutationFn: (input: Input) => Promise<unknown>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A decision is settled. A retried approval is a second attempt at letting
    // something into every later prompt that a person allowed once.
    retry: false,
    // `onSettled`, not `onSuccess`: a 409 means somebody decided this while it
    // sat on screen, and the list is wrong either way.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.knowledge.all });
      void queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    },
  });
}

/**
 * Yes — and only now does anything reach a prompt.
 *
 * Sent to the proposal, not to the row: `post_proposal_approve` dispatches on
 * kind and `knowledge::approve` activates the row, ends whatever it supersedes
 * and decides the question in one transaction.
 */
export function useApproveKnowledge() {
  return useKnowledgeDecision((proposalId: number) =>
    apiFetch<{ refinement_id: number }>(`/proposals/${proposalId}/approve`, { method: "POST" }),
  );
}

/** No — kept as a refusal rather than as an absence. 204, so no body to read. */
export function useRejectKnowledge() {
  return useKnowledgeDecision((proposalId: number) =>
    apiFetch<void>(`/proposals/${proposalId}/reject`, { method: "POST" }),
  );
}

/**
 * Take back one that is in force.
 *
 * Aimed at the row and not at a proposal: the question was answered months ago,
 * and what is being changed now is the store rather than a decision. 409 when
 * the row is not active — already reverted, or superseded by a later text —
 * which is a different thing for a reader to do about than a 404.
 */
export function useRevertKnowledge() {
  return useKnowledgeDecision((id: number) =>
    apiFetch<void>(`/knowledge/${id}/revert`, { method: "POST", body: JSON.stringify({}) }),
  );
}
