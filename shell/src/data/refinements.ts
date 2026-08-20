import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * What the agent has been told, as hooks.
 *
 * **This layer is invisible from every other page, by construction.** A
 * refinement waiting for an answer is a `kind = 'refinement'` proposal, and
 * `proposals::list_pending` filters `kind = 'action-approval'`
 * (`core/src/proposals.rs`) — so the Waiting queue, which is the page for
 * things that stopped to ask you something, cannot see one. The listing that
 * can is `GET /refinements`, and it answers every status rather than only the
 * pending ones, because the reviewable history is the whole point of the table.
 *
 * Three routes, read off `core/src/http.rs` rather than inferred from the
 * names:
 *
 * | what                | route                            |
 * |---------------------|----------------------------------|
 * | the layer, all of it| `GET /refinements`               |
 * | one, with its chain | `GET /refinements/{id}`          |
 * | take an active one back | `POST /refinements/{id}/revert` |
 *
 * The two decisions — yes and no — are **not** here: they go through the
 * ordinary proposal doors (`POST /proposals/{id}/approve` and `/reject`), which
 * dispatch on the proposal's kind. That is why the row carries `proposal_id`.
 *
 * `POLL.queue`: a layer only changes when a person decides something or a run
 * declares something, and five seconds is soon enough for a list worked through
 * by hand.
 */

/* ----------------------------------------------------------------- shapes -- */

/** The four kinds, in the order a node reads them — `refine::Kind`. */
export type RefinementKind = "prompt" | "memory" | "skill" | "subagent";

/**
 * A row's status, as `0088_refinements.sql` constrains it.
 *
 * Five and not three: `rejected`, `reverted` and `superseded` are all "not in
 * force" and are three different pieces of news — refused before it ever
 * applied, taken back after it did, and replaced by a later text.
 */
export type RefinementStatus = "proposed" | "active" | "rejected" | "reverted" | "superseded";

/** One refinement, as `refine::Refinement` serialises. */
export interface Refinement {
  id: number;
  /** `null` is machine-wide: a lesson about the house rather than about a repo. */
  project_id: string | null;
  kind: RefinementKind;
  title: string;
  body: string;
  status: RefinementStatus;
  /** The question that let it in. The door a decision is sent through. */
  proposal_id: number | null;
  /** The refinement this one replaces, ended when this one was approved. */
  supersedes: number | null;
  origin_run_id: number | null;
  created_at: string;
  activated_at: string | null;
  ended_at: string | null;
}

/** One decision in a refinement's life — `refine::Event`. */
export interface RefinementEvent {
  id: number;
  from_status: RefinementStatus | null;
  to_status: RefinementStatus;
  note: string | null;
  at: string;
}

/** A refinement, its decisions, and the chain on both sides — `refine::History`. */
export interface RefinementHistory {
  refinement: Refinement;
  events: RefinementEvent[];
  /** Newest first: what this replaced, then what THAT replaced. */
  replaced: Refinement[];
  replaced_by: Refinement | null;
}

/* ------------------------------------------------------------------ reads -- */

/** Every refinement, in every status. */
export function useRefinements() {
  return useQuery({
    queryKey: keys.refinements.all,
    queryFn: () => apiFetch<Refinement[]>("/refinements"),
    refetchInterval: POLL.queue,
  });
}

/**
 * One refinement's chain and decisions, fetched only when somebody opens it.
 *
 * `enabled` rather than a prefetch: the chain is a second request per row, and
 * a page that fetched one for every row would make reading a list of forty cost
 * forty-one requests to answer a question nobody asked yet.
 */
export function useRefinementHistory(id: number | null) {
  return useQuery({
    queryKey: keys.refinements.detail(id ?? 0),
    queryFn: () => apiFetch<RefinementHistory>(`/refinements/${id}`),
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
function useRefinementDecision<Input>(mutationFn: (input: Input) => Promise<unknown>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A decision is settled. A retried approval is a second attempt at letting
    // something into every later prompt that a person allowed once.
    retry: false,
    // `onSettled`, not `onSuccess`: a 409 means somebody decided this while it
    // sat on screen, and the list is wrong either way.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.refinements.all });
      void queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    },
  });
}

/**
 * Yes — and only now does anything reach a prompt.
 *
 * Sent to the proposal, not to the refinement: `post_proposal_approve`
 * dispatches on kind and `refine::approve` activates the row, ends whatever it
 * supersedes and decides the question in one transaction.
 */
export function useApproveRefinement() {
  return useRefinementDecision((proposalId: number) =>
    apiFetch<{ refinement_id: number }>(`/proposals/${proposalId}/approve`, { method: "POST" }),
  );
}

/** No — kept as a refusal rather than as an absence. 204, so no body to read. */
export function useRejectRefinement() {
  return useRefinementDecision((proposalId: number) =>
    apiFetch<void>(`/proposals/${proposalId}/reject`, { method: "POST" }),
  );
}

/**
 * Take back one that is in force.
 *
 * Aimed at the refinement and not at a proposal: the question was answered
 * months ago, and what is being changed now is the layer rather than a
 * decision. 409 when the row is not active — already reverted, or superseded
 * by a later text — which is a different thing for a reader to do about than a
 * 404.
 */
export function useRevertRefinement() {
  return useRefinementDecision((id: number) =>
    apiFetch<void>(`/refinements/${id}/revert`, { method: "POST", body: JSON.stringify({}) }),
  );
}
