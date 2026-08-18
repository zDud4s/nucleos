import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import type { Proposal } from "./system";

/**
 * The single decision queue, as hooks — one per list that actually exists.
 *
 * **`GET /proposals` is not the all-kinds queue the design assumed.**
 * `proposals::list_pending` filters `status = 'pending' AND kind =
 * 'action-approval'` (`core/src/proposals.rs`), so it answers one section of
 * this page and not the page. Every other section is a route of its own, and
 * they were read off `core/src/http.rs` rather than inferred from a name:
 *
 * | § | section            | route                        |
 * |---|--------------------|------------------------------|
 * | 1 | wheel requests     | `GET /browser/sessions`      |
 * | 2 | action approvals   | `GET /proposals`             |
 * | 5 | contact merges     | `GET /contacts/merges`       |
 * | 6 | calendar events    | **no listing route exists**  |
 * | 7 | exclusion requests | `GET /fleet/exclusions/requests` |
 * | 8 | skipped items      | `GET /proposals/skipped-items`   |
 * | 9 | refused actions    | `GET /proposals/refused-actions` |
 * |10 | git queue          | `GET /vcs/requests`          |
 * |11 | parked runs        | `GET /runs/awaiting-approval`|
 *
 * Section 6 has no hook and no route, and the page says so out loud instead of
 * pointing a hook at a path that would 404. The núcleo files `calendar-event`
 * proposals and `POST /proposals/{id}/approve` decides them — what is missing is
 * only the door to read them through, and inventing one here would move the gap
 * from a visible sentence to a request nobody can explain.
 *
 * Reads sit at `POLL.queue`: a queue only moves when something lands in it, and
 * five seconds is soon enough for a list a person works through by hand.
 */

/* ----------------------------------------------------------------- shapes -- */

/** One open browser session, exactly as `browser::SessionRow` serialises. */
export interface BrowserSession {
  id: number;
  sidecar_id: string;
  run_id: number | null;
  /** The project the session was opened FOR — not necessarily whose profile it runs in. */
  project_id: string | null;
  profile_kind: string;
  profile_id: string;
  requested_url: string;
  final_url: string;
  rule: string;
  /** Spec §4.4's state machine. `wheel-requested` is the only one this page reads. */
  mode: "agent" | "wheel-requested" | "human" | "delivery-failed";
  refusal: string | null;
  /** The proposal that asked for the wheel, once one exists. Null means nothing to decide yet. */
  proposal_id: number | null;
  chain: string | null;
  chain_decided_at: string | null;
  opened_at: string;
  closed_at: string | null;
}

/**
 * A session that is actually asking, narrowed to say so in the type.
 *
 * `proposal_id` is nullable on a session and not on one of these: a session in
 * `wheel-requested` with no proposal yet is the window between the mode flipping
 * and the record landing (spec §4.4 rule 3), and there is nothing to answer.
 * Narrowing here rather than at every card means a decision button cannot be
 * rendered for a request that has no id to send.
 */
export interface WheelRequest extends BrowserSession {
  proposal_id: number;
}

/** One row of the git queue, as `vcs::RequestSummary` serialises. */
export interface VcsRequestSummary {
  id: number;
  op: string;
  project_id: string;
  /**
   * What the queue locked on. Carried alongside `project_id` rather than
   * instead of it: the project is the label a reader recognises, the key is the
   * only thing that says whether two differently-labelled rows were competing.
   */
  repo_key: string;
  origin: string;
  status: string;
  created_at: string;
}

/** A worktree run parked on `awaiting_approval`, as `runs::AwaitingRun` serialises. */
export interface AwaitingRun {
  id: number;
  project_id: string | null;
  prompt: string;
  cwd: string | null;
  created_at: string;
}

/**
 * What `POST /proposals/{id}/approve` answers.
 *
 * Every field optional because the route is polymorphic on the proposal's kind
 * and each arm sends a different object (`core/src/http.rs`,
 * `post_proposal_approve`): an action approval resumes a run, a merge joins two
 * people, an exclusion writes a rule, a calendar proposal writes an event. A
 * single required shape here would be a type that is wrong four times out of
 * five.
 *
 * `closed` is the one that needs saying out loud: it is a **success** that
 * changed nothing. The exclusion it authorised named two jobs that have both
 * ended, so no rule was written — the person answered, the answer was recorded,
 * and the question had stopped mattering while it waited. Drawn as a failure it
 * would send somebody looking for a rule that was right not to exist.
 */
export interface ApprovalOutcome {
  resume_run_id?: number;
  closed?: string;
  exclusion_id?: number | null;
  merged?: boolean;
  event_id?: number;
}

/* ------------------------------------------------------------------ reads -- */

/**
 * §2 — the approvals, which is what `GET /proposals` actually answers.
 *
 * Re-exported from `data/system.ts` rather than declared again. Two hooks on one
 * route is two cache entries on one truth, and this particular route is also the
 * sidebar's pending badge: a second definition would leave the rail and the page
 * disagreeing about how many decisions are waiting. It keeps that hook's
 * `POLL.fast` cadence for the same reason — the badge is on screen everywhere,
 * and it is the thing that tells you to come and look.
 */
export { useProposals as useActionApprovals } from "./system";

/**
 * §7 — the exclusion requests, from the fleet's slice.
 *
 * Re-exported for the reason above: the Fleet page draws these as edges on its
 * canvas and this page decides them, and one query key is what keeps an approval
 * here from leaving a phantom edge there.
 */
export { useExclusionRequests } from "./fleet";

/**
 * §1 — the sessions where an agent has asked for the wheel.
 *
 * The route answers every open session and the filter happens in `select`, so
 * the cache holds what the daemon said and only this page's view is narrowed. A
 * session in `wheel-requested` with no `proposal_id` is the window between the
 * mode flipping and the proposal landing (spec §4.4 rule 3) — there is nothing
 * to decide yet, and offering buttons that would 404 is worse than waiting a
 * tick.
 */
export function useWheelRequests() {
  return useQuery({
    queryKey: keys.browser.sessions,
    queryFn: () => apiFetch<BrowserSession[]>("/browser/sessions"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
    select: (sessions: BrowserSession[]) => sessions.filter(isWheelRequest),
  });
}

/** Whether a session is asking for a person, and has a proposal to answer with. */
export function isWheelRequest(session: BrowserSession): session is WheelRequest {
  return session.mode === "wheel-requested" && session.proposal_id !== null;
}

/**
 * §5 — the pairs the núcleo thinks are one person, and the shapes they carry.
 *
 * Moved to `data/contacts.ts` — Contacts data, and now read from two pages —
 * and re-exported here under the idiom this file already uses for
 * `useProposals` and `useExclusionRequests`, so nothing below has to change
 * which door it imports these through.
 */
export { useContactMerges, type MergeSide, type MergeSuggestion } from "./contacts";

/** §8 — what the night put down without doing. A record to read, not a queue to work. */
export function useSkippedItems() {
  return useQuery({
    queryKey: keys.waiting.skippedItems,
    queryFn: () => apiFetch<Proposal[]>("/proposals/skipped-items"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/**
 * §9 — what the injection barrier refused.
 *
 * The one listing that joins an errand's name in (`proposals::list_refused_actions`),
 * because "send_email" without the errand is the verb with the subject missing.
 */
export function useRefusedActions() {
  return useQuery({
    queryKey: keys.waiting.refusedActions,
    queryFn: () => apiFetch<Proposal[]>("/proposals/refused-actions"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/**
 * §10 — the git queue.
 *
 * Nothing prunes `vcs_requests`, so this is the permanent history of every
 * operation the daemon has queued, capped at 200 rows. A listing that arrives at
 * the cap means the daemon has been running a while: it is **not** a finding,
 * and the panel must not draw it as one.
 */
export function useVcsRequests() {
  return useQuery({
    queryKey: keys.waiting.vcsRequests,
    queryFn: () => apiFetch<VcsRequestSummary[]>("/vcs/requests"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** How many rows `GET /vcs/requests` returns at most — `vcs::LIST_LIMIT`. */
export const VCS_LIST_LIMIT = 200;

/** §11 — the worktree runs holding a tree, parked until somebody answers. */
export function useAwaitingRuns() {
  return useQuery({
    queryKey: keys.runs.awaitingApproval,
    queryFn: () => apiFetch<AwaitingRun[]>("/runs/awaiting-approval"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/* -------------------------------------------------------------- decisions -- */

/**
 * Everything a decision can move, invalidated together.
 *
 * One helper rather than a list per mutation, because the lists on this page are
 * not independent: approving a wheel request closes a browser session AND
 * decides a proposal, approving an exclusion writes a fleet rule AND clears a
 * request, approving an action approval starts a run. A mutation that
 * invalidated only its own list would leave the page showing the consequence of
 * a decision in one section and the cause of it in another.
 */
function decidedKeys() {
  return [keys.waiting.all, keys.proposals.all, keys.fleet.all, keys.runs.all];
}

function useDecision<Answer, Input>(mutationFn: (input: Input) => Promise<Answer>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    /**
     * `retry: false` here as everywhere below this line, and it matters more
     * here than on a read: a refusal is settled, and a retried approval is a
     * second attempt at letting an action through that a person allowed once.
     */
    retry: false,
    /**
     * `onSettled`, not `onSuccess`. A 409 means somebody else decided this while
     * it sat on screen — the list is wrong either way, and refetching after a
     * refusal is how the row that is no longer there goes away.
     */
    onSettled: () => {
      for (const key of decidedKeys()) {
        void queryClient.invalidateQueries({ queryKey: key });
      }
    },
  });
}

/**
 * Let it through.
 *
 * No optimistic removal. Approving is the one gesture here with a *consequence*
 * beyond the row — a run resumes, a browser window opens, a rule starts parking
 * jobs — and a card drawn as gone before the daemon agreed would have to come
 * back if the daemon refused, which is the one moment a person must not be
 * confused about whether the thing happened.
 */
export function useApproveProposal() {
  return useDecision((id: number) =>
    apiFetch<ApprovalOutcome>(`/proposals/${id}/approve`, { method: "POST" }),
  );
}

/**
 * Refuse it.
 *
 * `reject_proposal` guards on `kind = 'action-approval'` and answers 409 for
 * anything else (`core/src/proposals.rs`), except for the four kinds
 * `post_proposal_reject` dispatches by hand — browser-wheel, contact-merge,
 * calendar-event and fleet-exclusion. Every arm answers **204**, so `void` is
 * the honest type: there is no body to read.
 */
export function useRejectProposal() {
  return useDecision((id: number) =>
    apiFetch<void>(`/proposals/${id}/reject`, { method: "POST" }),
  );
}

/**
 * Put a read record away.
 *
 * **Never pointed at `/reject`.** `DISMISSABLE_KINDS` is `["skipped-item",
 * "refused-action"]` and `reject_proposal` guards on `action-approval`, so a
 * dismiss sent to the rejection door answers 409 every time. Nothing is being
 * refused here and nothing is released: the job let go of the item and its
 * worktree hours before anybody read this. 204, so again no body.
 */
export function useDismissProposal() {
  return useDecision((id: number) =>
    apiFetch<void>(`/proposals/${id}/dismiss`, { method: "POST" }),
  );
}

export interface MergeDecision {
  proposalId: number;
  verdict: "approve" | "reject";
}

/**
 * Answer a suggested merge.
 *
 * **`POST /contacts/verdict` is not this route.** That one records a standing
 * decision about a sender; the merge is decided through the ordinary proposal
 * doors, and the two are exactly what the 409 here is about — it names two
 * standing decisions that disagree, in a body, which is why the page lets the
 * daemon's own sentence through rather than replacing it.
 *
 * Rejecting is not a no-op: `reject_merge` records the refused pair in the same
 * transaction as the status, which is what stops the heuristic from asking the
 * identical question on every sweep.
 */
export function useDecideContactMerge() {
  return useDecision(async (decision: MergeDecision) => {
    if (decision.verdict === "reject") {
      await apiFetch<void>(`/proposals/${decision.proposalId}/reject`, { method: "POST" });
      return {} as ApprovalOutcome;
    }
    return await apiFetch<ApprovalOutcome>(`/proposals/${decision.proposalId}/approve`, {
      method: "POST",
    });
  });
}
