import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText, isApiUnavailable } from "./client";
import { keys } from "./keys";
import { POLL, pollWhile } from "./poll";
import type { Proposal } from "./system";

/**
 * The fleet's own slice of the núcleo, as hooks.
 *
 * Everything the Fleet page reads or writes goes through here, and the page
 * never names a route or an interval. The shapes below were read off
 * `core/src/http.rs` and the modules it delegates to rather than inferred from
 * a name — field for field, including the ones the old shell never declared.
 */

/**
 * The ceiling of the live-filtered listings, the same number the daemon uses
 * (`concurrency::LIVE_LIST_LIMIT`).
 *
 * Duplicated on purpose rather than asked of the daemon: the shell needs it to
 * decide whether a list came back whole, and a list arriving exactly at the
 * ceiling may have been cut. If the two numbers ever diverge the worst that
 * happens is the shell saying *detail unavailable* where it could have said
 * *awaiting reconciliation* — the safe direction.
 */
export const LIVE_LIST_LIMIT = 200;

/** How much work the house is holding, against how much it may. */
export interface HouseCapacity {
  limit: number;
  held: number;
}

/** One taken slot, as `GET /concurrency` describes it. */
export interface HeldSlot {
  project_id: string;
  slot: number;
  /**
   * A worktree run holds a slot too — counting jobs alone would over-report the
   * room left — and so does one item of a job a team directs, which gets a
   * checkout of its own and pays for it under the same house rule. And so does
   * each worker of a controller's wave, whose `owner_id` is `wave_workers.id`.
   */
  owner_kind: "run" | "job" | "item" | "wave";
  owner_id: number;
  claimed_at: string;
  /**
   * The job an ITEM belongs to, and `null` for every other kind of owner.
   *
   * Joined by the daemon rather than looked up here, because there is nothing
   * here to look it up in: `owner_id` for an item is `job_items.id`, and no
   * route lists items by id. Nor should one — an item is a step of a job, and
   * the way to reach one is through the job that owns it.
   */
  job_id: number | null;
  /** Which item of that job, counting from zero. `null` for the other kinds. */
  ordinal: number | null;
  /**
   * What that item is doing, and `null` for the other kinds.
   *
   * Not the same question as "does it hold a slot". A slot is held from the
   * claim until the item is terminal, and that window covers `running`,
   * `merging`, `conflicted` and `reverted` — one of which is work in progress
   * and one of which is work waiting on a person. A capacity screen that cannot
   * tell those apart cannot say whether a slot is busy or stuck.
   */
  item_status: string | null;
}

/** Whose tree an overlap belongs to. Job ids and run ids collide, so the pair is the identity. */
export interface OwnerRef {
  kind: string;
  id: number;
}

/** A coincidence between two trees, and the paths where it happens. */
export interface Overlap {
  a: OwnerRef;
  b: OwnerRef;
  paths: string[];
}

/**
 * One of the two sources of the collision warning.
 *
 * `not_measured` is **never** read as `clean`. It is the state that exists so
 * the other is never said in vain: somebody trusting a `clean` nobody computed
 * lets two jobs run at the same file.
 */
export interface CollisionSource {
  state: "collide" | "clean" | "not_measured";
  overlaps: Overlap[];
}

/** Both sources, and never merged: one says "this will collide", the other "this collided". */
export interface Collisions {
  declared: CollisionSource;
  observed: CollisionSource;
}

export interface ProjectConcurrency {
  project_id: string;
  limit: number;
  slots: HeldSlot[];
  collision: Collisions;
}

export interface Concurrency {
  house: HouseCapacity;
  projects: ProjectConcurrency[];
}

export interface Job {
  id: number;
  project_id: string;
  rule_name: string | null;
  /**
   * `planning` | `implementing` | `gating` | `reviewing` | `waiting` |
   * `awaiting_approval`, then one of the endings: `completed`, `failed`,
   * `gate_failed`, `gate_errored`, `expired`, `stopped`, `cancelled`,
   * `interrupted`.
   */
  status: string;
  /**
   * Why a `waiting` job waits. Budget, slot contention and an exclusion ask
   * three different things of a reader, so `waiting` alone leaves them guessing
   * which.
   */
  wait_reason: string | null;
  max_items: number;
  created_at: string;
  completed_at: string | null;
  /** The slot this job holds, or null once it has given it back. */
  slot: number | null;
  /** The round it is on, counted from zero, and how many it may run. */
  round: number;
  max_rounds: number;
  /**
   * The team directing this job, or null for the sequential job in one shared
   * checkout — which is nearly every job.
   *
   * It changes how everything beside it reads: three items running at once is a
   * stuck queue without a team and the entire point of one with it.
   */
  team_id: string | null;
  /** The team's name. `team_id` is a slug that outlives renames, so the two differ. */
  team_name: string | null;
  /**
   * How many of this job's items the team allows at once.
   *
   * The team's number rather than the job's, read when the job is looked at, so
   * a ceiling raised this morning shows against the job running now.
   */
  team_max_parallel: number | null;
}

export interface JobItem {
  ordinal: number;
  description: string;
  /**
   * `pending` | `running` | `implemented` | `passed` | `failed` | `skipped` |
   * `cancelled` | `gate_*`, and — only in a job a team directs — `merging` |
   * `conflicted` | `reverted` | `orphaned`.
   *
   * Those last four had been left out of this list while `itemReading` already
   * rendered them, which is the wrong way round: the type is what a reader
   * checks before writing the switch.
   */
  status: string;
  /**
   * The round this item was queued in. `ordinal` is no substitute: ordinals
   * carry on across rounds rather than restarting, so nothing in the number
   * marks where one ended.
   */
  round: number;
  run_id: number | null;
  /**
   * Whether anything measured this item, kept apart from `status` because they
   * answer different questions. An item reading `passed` with a null gate
   * status was never measured.
   */
  gate_status: string | null;
  /**
   * Which agent of the team was given this item, and what they are called.
   *
   * Null for every item of every job without a team: nobody was asked. The name
   * travels because the id is a slug, exactly as with `team_name`.
   */
  agent_id: string | null;
  agent_name: string | null;
  /**
   * The ordinals this item may not start before, and the paths its director
   * said it would touch.
   *
   * Both empty for a job without a team. Together they are the whole answer to
   * *why are these two items running and not those two* — until they were on
   * the wire, the queue was a list of statuses with the reasoning removed.
   */
  depends_on: number[];
  files: string[];
}

export interface JobDetail extends Job {
  items: JobItem[];
  /** Where the work is, so a stopped job's partial can be found. Null once the GC took the tree. */
  branch: string | null;
}

/**
 * One "these two must not run at the same time", in force.
 *
 * `job_low` is not decoration: the daemon parks the **higher** id, so the
 * pair's order says which of the two waits.
 */
export interface FleetExclusion {
  id: number;
  project_id: string;
  job_low: number;
  job_high: number;
  /** The request that authorised it. */
  proposal_id: number;
  /** What motivated it, as JSON, when the asker named files. Recorded, not acted on. */
  paths: string | null;
  created_at: string;
}

export interface RunSearchResult {
  id: number;
  project_id: string | null;
  status: string;
  mode: string;
  created_at: string;
  completed_at: string | null;
  cost_usd: number | null;
  prompt_excerpt: string;
}

/**
 * The statuses a job holds its project's slot in.
 *
 * Mirrors `job::LIVE_STATUSES` in the daemon. The shell only reads it, so a
 * drift here is cosmetic rather than dangerous — but a job the shell calls
 * finished while the daemon still drives it is exactly the confusion the cancel
 * button exists to resolve, so it is worth keeping honest.
 */
const JOB_LIVE_STATUSES = [
  "planning",
  "implementing",
  "gating",
  "reviewing",
  "awaiting_approval",
  "waiting",
];

export function jobIsLive(status: string): boolean {
  return JOB_LIVE_STATUSES.includes(status);
}

/** What `POST /jobs` accepts. `max_items` is deliberately absent — it is the daemon's ceiling. */
export interface NewJob {
  project_id: string;
  prompt: string;
  /** `null` is "no job budget", which is a different fact from a budget of zero. */
  budget_usd: number | null;
  max_rounds: number | null;
  /**
   * The team to direct this job, or null for the sequential job in one shared
   * checkout.
   *
   * A team that does not exist is a **422 and never a fallback**. The daemon
   * refuses rather than quietly running the job the old way, because a person
   * who asked for parallel work and silently got a queue would have no way of
   * telling from the outside — which is the whole reason `JobStart::NoTeam`
   * exists on that route.
   */
  team_id: string | null;
}

/** Who holds a slot, for the one action that takes it back. */
export interface SlotOwner {
  kind: "run" | "job";
  id: number;
}

/**
 * The owner of a slot, when the gesture that takes it back has a route to call.
 *
 * `null` for an item. There is no `/items/<id>/cancel`, and the kind is not a
 * detail of the URL the way a run and a job are: an item is one step of a job,
 * so the thing to stop is the job, and the card for that is the one beside it.
 * Offering the button anyway would send `POST /runs/<item id>/cancel` — a
 * destructive gesture aimed by a number that means something else, which is the
 * defect `ownerKey` exists to prevent, arriving through the door nobody was
 * watching.
 *
 * Nor for a wave. Its workers are a controller's processes, which the daemon
 * cannot stop, and `owner_id` is a `wave_workers.id` — the same wrong-number
 * gesture. A whitelist and not an exclusion, so the next kind of owner is not
 * cancellable until somebody gives it a route.
 */
export function cancellableOwner(slot: HeldSlot): SlotOwner | null {
  return slot.owner_kind === "run" || slot.owner_kind === "job"
    ? { kind: slot.owner_kind, id: slot.owner_id }
    : null;
}

/**
 * How much room there is, and who is in it.
 *
 * The authority of this page: the columns, their `n/limit` headers and the
 * collision warnings all come from here. Everything else on the screen is a
 * description of what this reading already said was there.
 */
export function useConcurrency() {
  return useQuery({
    queryKey: keys.concurrency,
    queryFn: () => apiFetch<Concurrency>("/concurrency"),
    refetchInterval: POLL.fast,
  });
}

/**
 * The jobs in flight.
 *
 * `keepPreviousData` here and on every list below: a refetch that blanks the
 * cards makes the fleet flicker once every three seconds, and a stale card with
 * a `StaleNote` over it beats an empty column that reads as *there is room*.
 */
export function useLiveJobs() {
  return useQuery({
    queryKey: keys.jobs.live,
    queryFn: () => apiFetch<Job[]>("/jobs?live=true"),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * One job with its item list — the second zoom.
 *
 * Polls only while the job can still change. A finished job answers the same
 * bytes forever, and a card left open on one would cost a request every three
 * seconds for a list that is over. `enabled` rather than a conditional hook:
 * the panel unmounts when it closes, and `null` is how the caller says *not
 * open*.
 */
export function useJob(id: number | null) {
  return useQuery({
    queryKey: keys.jobs.detail(id ?? -1),
    queryFn: () => apiFetch<JobDetail>(`/jobs/${id ?? -1}`),
    enabled: id !== null,
    refetchInterval: pollWhile<JobDetail>(POLL.fast, (job) => jobIsLive(job.status)),
  });
}

/**
 * The runs still holding a slot.
 *
 * `limit` is sent explicitly even though the daemon defaults a live listing to
 * the same number: the shell compares the length against `LIVE_LIST_LIMIT` to
 * decide whether the list may have been cut, and a comparison against a ceiling
 * we did not ask for is a guess.
 */
export function useLiveRuns() {
  return useQuery({
    queryKey: keys.runs.live,
    queryFn: () => apiFetch<RunSearchResult[]>(`/runs?live=true&limit=${LIVE_LIST_LIMIT}`),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/** The exclusions in force. */
export function useExclusions() {
  return useQuery({
    queryKey: keys.fleet.exclusions,
    queryFn: () => apiFetch<FleetExclusion[]>("/fleet/exclusions"),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * The exclusion requests still waiting on an answer.
 *
 * A separate route from `/proposals` in the daemon and a separate query here,
 * for the reason the núcleo gives: approving an action-approval resumes a
 * paused run and approving one of these resumes nothing.
 */
export function useExclusionRequests() {
  return useQuery({
    queryKey: keys.fleet.exclusionRequests,
    queryFn: () => apiFetch<Proposal[]>("/fleet/exclusions/requests"),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * Ask for a job.
 *
 * No optimistic write: a job does not exist until the daemon says it does, and
 * both of this route's refusals — the kill switch and a full project — are
 * answers a card drawn optimistically would have to un-draw. The invalidation
 * covers capacity as well as the listing, because a started job takes a slot
 * and the `n/limit` header is the number that must not lag.
 */
export function useCreateJob() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (job: NewJob) =>
      apiFetch<{ job_id: number }>("/jobs", { method: "POST", body: JSON.stringify(job) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
      void queryClient.invalidateQueries({ queryKey: keys.jobs.all });
    },
  });
}

/**
 * Take a slot back, whoever is holding it.
 *
 * One hook for both kinds because it is one gesture: the card says *cancel* and
 * the route it takes is a detail of what happens to be in the slot. The
 * optimistic write drops the slot out of the capacity reading, and
 * `cancelQueries` is what stops a 3-second tick already in flight from landing
 * after it and putting the card back.
 *
 * **A refusal does not restore the card.** A 409 is the owner having ended
 * between the render and the click — the slot really is gone. Only an
 * `ApiUnavailable` puts it back, because then nothing is known about whether
 * the cancel happened at all.
 *
 * **`apiText`, never `apiFetch`.** The two routes answer success differently and
 * neither answers with a document: `cancel_job` returns `204 No Content`, while
 * `cancel_run` returns a bare `StatusCode::OK` — a **200 with an empty body**.
 * `apiFetch` exempts only 204/205 from parsing, so the run half turned every
 * successful cancel into "the daemon answered with a body that is not JSON" and
 * the card reported a failure for work that really had stopped. `apiText` reads
 * both as the empty string, which is what they are. Same idiom, same reason, as
 * `useCancelRun` in `data/runs.ts`.
 */
export function useCancelSlotOwner() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (owner: SlotOwner) => {
      await apiText(`/${owner.kind === "job" ? "jobs" : "runs"}/${owner.id}/cancel`, {
        method: "POST",
      });
    },
    retry: false,
    onMutate: async (owner: SlotOwner) => {
      await queryClient.cancelQueries({ queryKey: keys.concurrency });
      const previous = queryClient.getQueryData<Concurrency>(keys.concurrency);
      if (previous !== undefined) {
        queryClient.setQueryData<Concurrency>(keys.concurrency, {
          ...previous,
          projects: previous.projects.map((project) => ({
            ...project,
            slots: project.slots.filter(
              (slot) => !(slot.owner_kind === owner.kind && slot.owner_id === owner.id),
            ),
          })),
        });
      }
      return { previous };
    },
    onError: (error, _owner, context) => {
      if (context !== undefined && isApiUnavailable(error)) {
        queryClient.setQueryData<Concurrency | undefined>(keys.concurrency, context.previous);
      }
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
      void queryClient.invalidateQueries({ queryKey: keys.jobs.all });
      void queryClient.invalidateQueries({ queryKey: keys.runs.all });
    },
  });
}

/** The two jobs an exclusion is about, and the paths that motivated asking. */
export interface ExclusionRequest {
  job_a: number;
  job_b: number;
  paths: string[];
}

/**
 * Ask that two jobs of one project never run at the same time.
 *
 * It answers with a **proposal** id and not a rule id, and that is the design
 * rather than an implementation detail: drawing this edge changes nothing about
 * how the fleet schedules until somebody answers it in the same queue every
 * other decision passes through.
 */
export function useProposeExclusion() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (request: ExclusionRequest) =>
      apiFetch<{ proposal_id: number }>("/fleet/exclusions", {
        method: "POST",
        body: JSON.stringify(request),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.fleet.all });
      void queryClient.invalidateQueries({ queryKey: keys.proposals.all });
    },
  });
}

/**
 * Lift a rule that is in force.
 *
 * Unlike proposing, this takes effect immediately — the rule was already
 * approved once, and withdrawing consent is not a decision that needs a second
 * pair of eyes.
 */
export function useRevokeExclusion() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (exclusionId: number) =>
      apiFetch<void>(`/fleet/exclusions/${exclusionId}`, { method: "DELETE" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.fleet.all });
    },
  });
}
