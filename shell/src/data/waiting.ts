import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import type { Proposal } from "./system";
import { useProposals } from "./system";
import { useWheelRequests } from "./browser";
import { useContactMerges } from "./contacts";
import { useExclusionRequests } from "./fleet";
import { useRecruitProposals, useTeamActionProposals } from "./teams";

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

/**
 * Git outcomes that need a person: escalated is a normal outcome that keeps
 * the queue working, and blocked is terminal without being a failure. Neither
 * is red; `ui/state-map.ts` holds the tones. This serves both the page and the
 * shared waiting count.
 */
export const VCS_WANTS_A_PERSON = ["escalated", "blocked"];

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
  /**
   * `POST /proposals/{id}/approve` on a team action, and the only arm of that
   * door where approving DOES NOTHING but say yes: the núcleo carries the
   * action out on its next tick, about ten seconds later. The sentence is the
   * daemon's — "the department's action will be carried out shortly".
   */
  queued?: string;
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
 * Moved to `data/browser.ts` — Browser data, and now read from two pages —
 * and re-exported here under the idiom this file already uses for
 * `useContactMerges`, so nothing below has to change which door it imports
 * these through.
 */
export { useWheelRequests, isWheelRequest, type BrowserSession, type WheelRequest } from "./browser";

/**
 * §5 — the pairs the núcleo thinks are one person, and the shapes they carry.
 *
 * Moved to `data/contacts.ts` — Contacts data, and now read from two pages —
 * and re-exported here under the idiom this file already uses for
 * `useProposals` and `useExclusionRequests`, so nothing below has to change
 * which door it imports these through.
 */
export { useContactMerges, type MergeSide, type MergeSuggestion } from "./contacts";

/**
 * §3 & §4 — a department's own action approvals and its recruitment requests,
 * plus the join table §3 reads its execution state from.
 *
 * Re-exported for the reason above: `data/teams.ts` is where the Teams pages
 * read these same routes, and one query key is what keeps a decision made
 * here from leaving a stale row there.
 */
export { useOpenTeamActions, useRecruitProposals, useTeamActionProposals } from "./teams";

/**
 * Counts the seven decision lists that own the bare phrase "waiting on you":
 * wheel requests, action approvals, team actions, recruits, contact merges,
 * exclusion requests, and git rows that want a person.
 *
 * A partial count is preferred to a blank: a failed route contributes zero as
 * the existing page arithmetic does, so one broken route cannot hide every
 * answered decision.
 *
 * A run parked on `awaiting_approval` is the run side of an action approval
 * already counted in §2, so `useAwaitingRuns` is not read here.
 * `runs::reconcile_stranded_approvals` sweeps a parked run whose pending
 * `action-approval` proposal is gone; it either has a proposal already counted
 * or is not a decision anybody can take. Every decision is counted once.
 */
export function countWaitingDecisions(lists: {
  wheel: unknown[] | undefined;
  approvals: unknown[] | undefined;
  teamActions: unknown[] | undefined;
  recruits: unknown[] | undefined;
  merges: unknown[] | undefined;
  exclusions: unknown[] | undefined;
  git: { status: string }[] | undefined;
}): number | undefined {
  const values = Object.values(lists);
  if (values.every((list) => list === undefined)) return undefined;
  return (
    (lists.wheel?.length ?? 0) +
    (lists.approvals?.length ?? 0) +
    (lists.teamActions?.length ?? 0) +
    (lists.recruits?.length ?? 0) +
    (lists.merges?.length ?? 0) +
    (lists.exclusions?.length ?? 0) +
    (lists.git?.filter((row) => VCS_WANTS_A_PERSON.includes(row.status)).length ?? 0)
  );
}

/**
 * Reads the seven decision lists for the one shared "waiting on you" arithmetic.
 *
 * The partial count remains more useful than a blank when one route fails;
 * `countWaitingDecisions` is the sole owner of the bare phrase's number.
 */
export function useWaitingCount(): number | undefined {
  const wheel = useWheelRequests();
  const approvals = useProposals();
  const teamActions = useTeamActionProposals();
  const recruits = useRecruitProposals();
  const merges = useContactMerges();
  const exclusions = useExclusionRequests();
  const git = useVcsRequests();
  return countWaitingDecisions({
    wheel: wheel.data,
    approvals: approvals.data,
    teamActions: teamActions.data,
    recruits: recruits.data,
    merges: merges.data,
    exclusions: exclusions.data,
    git: git.data,
  });
}

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
 * anything else (`core/src/proposals.rs`), except for the five kinds
 * `post_proposal_reject` dispatches by hand — browser-wheel, contact-merge,
 * refinement, calendar-event and fleet-exclusion. Every arm answers **204**, so
 * `void` is the honest type: there is no body to read.
 */
export function useRejectProposal() {
  return useDecision((id: number) =>
    apiFetch<void>(`/proposals/${id}/reject`, { method: "POST" }),
  );
}

/**
 * Refuse only the action (spec B D12): the run continues in the same conversation without it.
 *
 * Nothing is granted and nothing is queued: a declined push never reaches the git queue. The
 * same action asked again in this task is refused without asking anybody. A job's node answers
 * 409: inside a job, the job's own policy decides. Answers what the approval answers for a resume.
 */
export function useDeclineAction() {
  return useDecision((id: number) =>
    apiFetch<ApprovalOutcome>(`/proposals/${id}/decline-action`, { method: "POST" }),
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
