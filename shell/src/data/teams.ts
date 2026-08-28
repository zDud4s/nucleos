// §spec teams-consola-e-bancada
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { AgentRequest } from "./agents";
import { keys } from "./keys";
import { POLL, pollWhile } from "./poll";
import type { Proposal } from "./system";

/**
 * The departments pillar — teams, their runs, the actions they ask for and the rules that start
 * them.
 *
 * Every shape below was read off `core/src/team.rs`, `core/src/team_trigger.rs` and the routes
 * `core/src/http.rs:105-160` mounts. Where the design's §6.9 disagrees, the daemon is what this
 * file follows.
 *
 * There are no query parameters anywhere in this pillar. `GET /team-runs` is a hard `LIMIT 100`,
 * newest first, with no paging and no total; `GET /team-actions` is filtered server-side to
 * `pending` and `working` and hides everything finished. Filtering by team, by state or by date
 * happens in the page.
 *
 * `TeamView` and `TeamRunView` are flattened on the wire. There is no nested `team` or `run`
 * object — `members` and `items` are siblings of `id` and `state`.
 *
 * Two routes in this family are unreachable from here. `POST /team-files/read` and
 * `POST /team-actions` require a team run's own credential and answer 403 for the control token
 * the shell holds, so nothing in this file calls them. An item's delivery is read through the
 * Files page, which is what the folder is for.
 */

/* ------------------------------------------------------------------ types -- */

/** One action a department may be granted. `mode` is `propose` or `allow`; there is no `deny`. */
export interface TeamGrant {
  kind: string;
  mode: string;
}

/** A department. `id` is slugged from `name` by the daemon and never changes when the name does. */
export interface Team {
  id: string;
  name: string;
  mission: string;
  director_agent_id: string;
  max_rounds: number;
  max_parallel: number;
  /** `null` means no team ceiling of its own — only the house budget governs. Null is not zero. */
  budget_usd: number | null;
  max_open_actions: number;
  max_live_runs: number;
  created_at: string;
  updated_at: string;
}

/** What `GET /teams` and `GET /teams/{id}` answer — `Team` flattened, plus its roster and grants. */
export interface TeamView extends Team {
  /** Always an array, possibly empty. Never absent, never null. */
  members: string[];
  grants: TeamGrant[];
}

/**
 * The body of `POST /teams` and `PUT /teams/{id}`.
 *
 * `members` and `grants` are REPLACED wholesale, and both are optional on the
 * wire — omitting them wipes them rather than leaving them alone. Every update
 * therefore sends the full roster and the full grant set. There is no `id`
 * field: the daemon slugs one from `name`.
 */
export interface TeamRequest {
  name: string;
  mission: string;
  director_agent_id: string;
  max_rounds: number;
  max_parallel: number;
  budget_usd: number | null;
  max_open_actions: number;
  max_live_runs: number;
  members: string[];
  grants: TeamGrant[];
}

/** One run of a department. */
export interface TeamRun {
  id: string;
  team_id: string;
  request: string;
  /** Relative — `teams/{team_id}/{run_id}`. Deliberately not absolute: it is text, not a link. */
  workspace: string;
  state: string;
  /** Which director node is in flight: `none` | `planning` | `replanning` | `delivering`. */
  director_node: string;
  director_run_id: number | null;
  round: number;
  next_ordinal: number;
  dry_rounds: number;
  plan_retries: number;
  replanned: string;
  /** Equal to the terminal state once it ends; `null` while it is live. */
  outcome: string | null;
  /** The daemon's own sentence about how it ended. Renders verbatim. */
  why: string | null;
  created_at: string;
  updated_at: string;
  finished_at: string | null;
  trigger_id: number | null;
  parent_id: string | null;
  /** NOT NULL, and a run somebody asked for points at ITSELF. Never `COALESCE` it. */
  root_id: string;
  depth: number;
}

/** One item of one round. `run_id` is a daemon run id — an integer, a different id space from `id`. */
export interface TeamItem {
  ordinal: number;
  round: number;
  agent_id: string;
  description: string;
  state: string;
  run_id: number | null;
  output_path: string | null;
}

/** What `GET /team-runs/{id}` answers — `TeamRun` flattened, plus its items and THIS run's cost. */
export interface TeamRunView extends TeamRun {
  items: TeamItem[];
  /** This run alone. The chain's total is not served by any route — never sum it from the list. */
  cost_usd: number;
}

/** Something a department asked the core to do. Nothing here has happened yet. */
export interface TeamAction {
  id: number;
  team_run_id: string;
  /** `null` means the DIRECTOR asked, rather than one of the specialists. */
  ordinal: number | null;
  kind: string;
  /** A JSON-encoded STRING, not an object — parse it with `parseActionPayload`. */
  payload: string;
  why: string;
  /** `null` means the grant was `allow`: nobody decides, and it happens on the next tick. */
  proposal_id: number | null;
  state: string;
  /** `"rejected"` here with `state === "failed"` means the owner said no — see `teamActionState`. */
  error: string | null;
  created_at: string;
  executed_at: string | null;
}

/** A rule that starts a department. `enabled` is 0 or 1 on the way OUT — it is an integer, not a bool. */
export interface TeamTrigger {
  id: number;
  team_id: string;
  name: string;
  enabled: number;
  source: string;
  cron: string | null;
  timezone: string | null;
  from_team: string | null;
  email_class: string | null;
  request: string;
  created_at: string;
  updated_at: string;
}

/**
 * The body of `POST /team-triggers`. There is deliberately no `enabled` field:
 * a new rule is always written disarmed, and arming it is a second, explicit act.
 */
export interface TriggerRequest {
  team_id: string;
  name: string;
  source: string;
  cron: string | null;
  timezone: string | null;
  from_team: string | null;
  email_class: string | null;
  request: string;
}

/**
 * `GET /team-triggers/{id}/next` — exactly one of the two is non-null, and a
 * rule that fires on something other than a clock answers 200 with
 * `error: "this rule does not fire on a clock"`. That is a value, not a failure.
 */
export interface TriggerNext {
  next: string | null;
  error: string | null;
}

/** `POST /teams/{id}/runs` answers 202 with this and nothing else — no state, no workspace. */
export interface StartedRun {
  id: string;
}

/** `POST /proposals/{id}/approve` for a recruitment. The corrections travel under `hire`. */
export interface HireBody {
  hire: AgentRequest;
}

/* --------------------------------------------------------- constants and helpers -- */

/** `core/src/team.rs:38`. */
export const TEAM_LIVE_STATES = ["planning", "working", "delivering"] as const;

/** `core/src/team.rs:368` — the only three kinds a department may ever be granted. */
export const GRANTABLE_ACTIONS = ["calendar_event", "file_document", "send_email"] as const;

/** `core/src/team.rs:375`. There is no `deny`: a kind with no row is a kind the team may not ask for. */
export const GRANT_MODES = ["propose", "allow"] as const;

/** `core/src/team_trigger.rs:51`. */
export const TRIGGER_SOURCES = ["cron", "team_finished", "email_triaged"] as const;

/** The daemon's own hard cap on `GET /team-runs`, with no way to page past it. */
export const TEAM_RUN_LIST_LIMIT = 100;

export function teamRunIsAlive(state: string): boolean {
  return (TEAM_LIVE_STATES as readonly string[]).includes(state);
}

/**
 * The state to render an action with.
 *
 * A refused action is stored `state = 'failed', error = 'rejected'` — the
 * daemon has no `rejected` state and says why it does not
 * (`core/src/team.rs:1952`). Reading that pair back as `failed` would tell the
 * owner something broke when what happened is that they said no, so this is
 * where the pair becomes the `rejected` key the state map carries.
 */
export function teamActionState(action: TeamAction): string {
  if (action.state === "failed" && action.error === "rejected") return "rejected";
  return action.state;
}

/**
 * A team action's payload, parsed.
 *
 * The daemon stores the canonical re-serialisation as a string, so this is a
 * second parse and it can fail on data that is not ours to fix. It answers
 * `null` rather than throwing: a page that cannot read a payload still shows
 * the raw text, which is a worse view and not a broken one.
 */
export function parseActionPayload(payload: string): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(payload);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/* ------------------------------------------------------------------ reads -- */

/** Every department. Not polled: a roster changes when somebody changes it. */
export function useTeams() {
  return useQuery({
    queryKey: keys.teams.list,
    queryFn: () => apiFetch<TeamView[]>("/teams"),
    placeholderData: keepPreviousData,
  });
}

export function useTeam(id: string) {
  return useQuery({
    queryKey: keys.teams.detail(id),
    queryFn: () => apiFetch<TeamView>(`/teams/${encodeURIComponent(id)}`),
  });
}

/**
 * The newest hundred runs, across every department.
 *
 * The limit is the daemon's and there is no way past it — the page has to say
 * so rather than let a hundred rows read as a whole history.
 */
export function useTeamRuns() {
  return useQuery({
    queryKey: keys.teams.runs,
    queryFn: () => apiFetch<TeamRun[]>("/team-runs"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** One run in full, polled only while it can still change on its own. */
export function useTeamRun(id: string) {
  return useQuery({
    queryKey: keys.teams.run(id),
    queryFn: () => apiFetch<TeamRunView>(`/team-runs/${encodeURIComponent(id)}`),
    refetchInterval: pollWhile<TeamRunView>(POLL.queue, (run) => teamRunIsAlive(run.state)),
  });
}

/**
 * What one run asked for, in every state including the finished ones.
 *
 * `alive` is passed in rather than derived, because this query cannot see the
 * run's state and a finished run's actions are the same bytes forever. An
 * unknown run id answers `[]` with a 200 here, not a 404.
 */
export function useTeamRunActions(id: string, alive: boolean) {
  return useQuery({
    queryKey: keys.teams.runActions(id),
    queryFn: () => apiFetch<TeamAction[]>(`/team-runs/${encodeURIComponent(id)}/actions`),
    refetchInterval: alive ? POLL.queue : false,
  });
}

/**
 * Everything still waiting on somebody, across every department.
 *
 * Filtered server-side to `pending` and `working`, so a finished action is
 * invisible here by design — the per-run route is where history lives.
 */
export function useOpenTeamActions() {
  return useQuery({
    queryKey: keys.teams.openActions,
    queryFn: () => apiFetch<TeamAction[]>("/team-actions"),
    refetchInterval: POLL.queue,
  });
}

/** Every rule, armed or not. There is no route that reads one rule — this list is how. */
export function useTeamTriggers() {
  return useQuery({
    queryKey: keys.teams.triggers,
    queryFn: () => apiFetch<TeamTrigger[]>("/team-triggers"),
  });
}

/** When one rule fires next, or the daemon's sentence about why that question has no answer. */
export function useTriggerNext(id: number) {
  return useQuery({
    queryKey: keys.teams.triggerNext(id),
    queryFn: () => apiFetch<TriggerNext>(`/team-triggers/${id}/next`),
  });
}

/** The team actions waiting on a person — `GET /proposals/team-actions`, pending only. */
export function useTeamActionProposals() {
  return useQuery({
    queryKey: keys.teams.proposedActions,
    queryFn: () => apiFetch<Proposal[]>("/proposals/team-actions"),
    refetchInterval: POLL.queue,
  });
}

/** The specialists directors asked for — `GET /proposals/recruits`, pending only. */
export function useRecruitProposals() {
  return useQuery({
    queryKey: keys.teams.recruits,
    queryFn: () => apiFetch<Proposal[]>("/proposals/recruits"),
    refetchInterval: POLL.queue,
  });
}

/* ----------------------------------------------------------------- writes -- */

export function useCreateTeam() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: TeamRequest) =>
      apiFetch<TeamView>("/teams", { method: "POST", body: JSON.stringify(body) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.all });
    },
  });
}

/** A full replace, including the roster and the grants — an omitted list is a wiped one. */
export function useUpdateTeam() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: TeamRequest }) =>
      apiFetch<TeamView>(`/teams/${encodeURIComponent(id)}`, {
        method: "PUT",
        body: JSON.stringify(body),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.all });
    },
  });
}

/** 204. Refused with 400 and a sentence when the team has a run in flight or runs on record. */
export function useDeleteTeam() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      apiFetch<void>(`/teams/${encodeURIComponent(id)}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.all });
    },
  });
}

/** 202 with `{id}` and nothing else — the run has no items until the tick picks it up. */
export function useStartTeamRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, request }: { id: string; request: string }) =>
      apiFetch<StartedRun>(`/teams/${encodeURIComponent(id)}/runs`, {
        method: "POST",
        body: JSON.stringify({ request }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.runs });
    },
  });
}

/**
 * Ask a live run to stop. 204 when this call is what stopped it.
 *
 * A run that had already ended answers **404**, and its body says "team not
 * found" about a run — so neither the status nor the sentence may be shown as
 * given. `onSettled`, not `onSuccess`: either way the run has to be read again
 * to find out what actually happened.
 */
export function useCancelTeamRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      apiFetch<void>(`/team-runs/${encodeURIComponent(id)}/cancel`, { method: "POST" }),
    retry: false,
    onSettled: (_data, _error, id) => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.run(id) });
      void queryClient.invalidateQueries({ queryKey: keys.teams.runs });
    },
  });
}

/** 204, and it deletes the run's folder on disk too. Refused 400 while the run is still going. */
export function useDeleteTeamRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      apiFetch<void>(`/team-runs/${encodeURIComponent(id)}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.runs });
    },
  });
}

/** 201 with the rule. It is written disarmed whatever else is sent. */
export function useCreateTrigger() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: TriggerRequest) =>
      apiFetch<TeamTrigger>("/team-triggers", { method: "POST", body: JSON.stringify(body) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.triggers });
    },
  });
}

/** 204. There is no update route, so changing a rule is deleting it and writing another. */
export function useDeleteTrigger() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<void>(`/team-triggers/${id}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.triggers });
    },
  });
}

/**
 * Arm or disarm a rule.
 *
 * The body carries a real boolean and the answer carries `enabled` as 0 or 1 —
 * the type flips direction across this one call, which is why nothing anywhere
 * compares a trigger's `enabled` with `true`.
 */
export function useSetTriggerEnabled() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      apiFetch<TeamTrigger>(`/team-triggers/${id}/enable`, {
        method: "POST",
        body: JSON.stringify({ enabled }),
      }),
    retry: false,
    onSettled: (_data, _error, variables) => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.triggers });
      void queryClient.invalidateQueries({ queryKey: keys.teams.triggerNext(variables.id) });
    },
  });
}

/**
 * Say yes to a team action.
 *
 * Approving does nothing but say yes: the núcleo carries the action out on its
 * next tick, about ten seconds later, which is why the answer is `queued` and
 * not a result. The action's own state is the second fact and arrives after.
 */
export function useApproveTeamAction() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (proposalId: number) =>
      apiFetch<{ queued?: string }>(`/proposals/${proposalId}/approve`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.proposedActions });
      void queryClient.invalidateQueries({ queryKey: keys.teams.openActions });
    },
  });
}

/**
 * Hire the specialist a director asked for, over whatever was edited.
 *
 * The only editable approval in the house: the corrections travel under `hire`,
 * and `agent::validate` runs over what was approved rather than over what was
 * proposed. Answers `{agent_id}`; refuses 409 with a sentence when the team
 * went away or the name now collides.
 */
export function useHireRecruit() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ proposalId, hire }: { proposalId: number; hire: AgentRequest }) =>
      apiFetch<{ agent_id: string }>(`/proposals/${proposalId}/approve`, {
        method: "POST",
        body: JSON.stringify({ hire } satisfies HireBody),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.teams.recruits });
      void queryClient.invalidateQueries({ queryKey: keys.agents.all });
    },
  });
}
