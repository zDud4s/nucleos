import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, probeHealth } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/** Autopilot's three settings for a project. `off` is not "broken" and `shadow` is not "on". */
export type AutopilotMode = "off" | "shadow" | "active";

export interface KillSwitchState {
  engaged: boolean;
}

export interface BudgetView {
  /** `null` is "no ceiling", which is a different fact from a ceiling of zero. */
  limit_usd: number | null;
  period: "daily" | "weekly" | "monthly";
  hourly_limit_usd: number | null;
  per_run_reserve_usd: number;
  time_cost_per_hour_usd: number;
  window_spend_usd: number;
  hourly_spend_usd: number;
  /** Whether autonomous work is currently held. `reason` is why, and is non-null exactly when this is true. */
  paused: boolean;
  reason: string | null;
}

export interface ProjectSummary {
  project_id: string;
  mode: AutopilotMode;
  /** `null` when the project has no root on disk yet — the promote flow asks for one. */
  project_root: string | null;
  pending: number;
  /** Action classes clearing the shadow-exit bar, out of those the project has exercised. */
  classes_ready: number;
  classes_total: number;
  /**
   * Optional because the shell can be newer than the daemon it talks to. Absent
   * reads as zero, which keeps the promote control locked rather than
   * unlocking it — the safe direction for a field that is not there.
   */
  withheld_classes_ready?: number;
  promotable: boolean;
  open_proposals: number;
  wip_limit: number | null;
  queue_full: boolean;
}

export interface Proposal {
  id: number;
  /** What kind of decision this is. The card is polymorphic on it; there is no generic proposal. */
  kind: string;
  status: string;
  run_id: number | null;
  session_id: string | null;
  project_id: string | null;
  /**
   * Not derivable from `project_id`: an errand has no project, so an errand's
   * proposal and a machine-wide one both carry a null `project_id`.
   */
  errand_id: number | null;
  /** Joined in only by the queries whose readers need it; null means this query did not ask. */
  errand_name: string | null;
  tool_name: string | null;
  reasoning: string;
  tool_input: string | null;
  created_at: string;
  decided_at: string | null;
}

/**
 * Is the daemon answering at all?
 *
 * Keeps polling with the window hidden, unlike almost everything else here: it
 * is the global handshake, and a shell that comes back from the tray showing a
 * connection state it stopped checking ten minutes ago is lying. It also never
 * rejects — `false` is the answer for "not there" — so react-query's retry and
 * error states stay out of a question that has only two outcomes.
 */
export function useHealth() {
  return useQuery({
    queryKey: keys.health,
    queryFn: probeHealth,
    refetchInterval: POLL.fast,
    refetchIntervalInBackground: true,
  });
}

/**
 * The kill switch, as the daemon has it.
 *
 * Also polled in the background, and for a sharper reason than health: this
 * control is visible in every view of the app, and a switch that reads
 * *disengaged* because the window was hidden while something engaged it is the
 * single most dangerous stale value in the shell.
 */
export function useKillSwitch() {
  return useQuery({
    queryKey: keys.autopilot.kill,
    queryFn: () => apiFetch<KillSwitchState>("/autopilot/kill"),
    refetchInterval: POLL.fast,
    refetchIntervalInBackground: true,
  });
}

/**
 * Engage or release the kill switch.
 *
 * The optimistic update is not cosmetic here — engaging is a panic gesture and
 * has to look like it took immediately. The `cancelQueries` in `onMutate` is
 * what stops a 3-second tick already in flight from landing *after* the
 * optimistic write and resurrecting the old value; `onSettled` then invalidates
 * so the daemon, not this cache, has the last word.
 *
 * `retry: false` is deliberate and applies to every mutation in this layer: a
 * refusal is settled, and a retried write is a second attempt at an action a
 * person asked for once.
 */
export function useSetKillSwitch() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (engaged: boolean) =>
      apiFetch<void>("/autopilot/kill", {
        method: "POST",
        body: JSON.stringify({ engaged }),
      }),
    retry: false,
    onMutate: async (engaged: boolean) => {
      await queryClient.cancelQueries({ queryKey: keys.autopilot.kill });
      const previous = queryClient.getQueryData<KillSwitchState>(keys.autopilot.kill);
      queryClient.setQueryData<KillSwitchState>(keys.autopilot.kill, { engaged });
      return { previous };
    },
    onError: (_error, _engaged, context) => {
      // Put back exactly what was there, including "there was nothing" — writing
      // a guessed value on a failed write would invent a state the daemon never had.
      if (context !== undefined) {
        queryClient.setQueryData<KillSwitchState | undefined>(keys.autopilot.kill, context.previous);
      }
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.kill });
    },
  });
}

/** Spend against the ceilings, and whether either one is currently holding work. */
export function useBudget() {
  return useQuery({
    queryKey: keys.autopilot.budget,
    queryFn: () => apiFetch<BudgetView>("/autopilot/budget"),
    refetchInterval: POLL.fast,
  });
}

/**
 * The project roster with its governance counts.
 *
 * `keepPreviousData` on this and on the proposals list below: these feed page
 * headers and badges, and a list that blanks on every refetch makes the whole
 * shell flicker once every three seconds. A stale view with a timestamp beats
 * an empty one — the pages say so, with `StaleNote`.
 */
export function useProjects() {
  return useQuery({
    queryKey: keys.projects.all,
    queryFn: () => apiFetch<ProjectSummary[]>("/projects"),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * Everything waiting on a human decision.
 *
 * Fixed at the fast cadence rather than the queue cadence because this list is
 * also the sidebar's pending badge — it is on screen on every page, not only
 * on the queue, and a badge is the thing that tells you to go and look.
 */
export function useProposals() {
  return useQuery({
    queryKey: keys.proposals.all,
    queryFn: () => apiFetch<Proposal[]>("/proposals"),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * Tell the daemon someone is here.
 *
 * A plain function and not a hook on purpose: this has no cache entry, nothing
 * renders from it, and its caller is a visibility listener rather than a
 * component tree. Making it a mutation would give it a status nobody reads.
 *
 * An empty `project_id` is a 400 from the daemon — a blank string is not a
 * scope — so a blank collapses to the global heartbeat here rather than
 * travelling to the núcleo to be rejected.
 */
export async function postAttention(projectId?: string): Promise<void> {
  const scoped = projectId !== undefined && projectId.trim() !== "";
  await apiFetch<void>("/autopilot/attention", {
    method: "POST",
    body: JSON.stringify(scoped ? { project_id: projectId } : {}),
  });
}
