import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import type { AutopilotMode } from "./system";

/**
 * The governance slice of the núcleo, as hooks.
 *
 * Everything the Autopilot cockpit reads or writes that is *not* already served
 * by another module lives here. Four things it deliberately does **not**
 * declare, because declaring them twice would be two cache entries on one
 * truth: `useProjects` / `useBudget` / `useProposals` / `useKillSwitch` come
 * from `data/system.ts`, `useLiveJobs` / `useCreateJob` from `data/fleet.ts`,
 * and the feed from `data/feed.ts`.
 *
 * **`GET /autopilot/state` is not the cockpit's read.** It takes a required
 * `?project_id=` and answers `{project_id, mode}` — one project's setting and
 * nothing else (`core/src/http.rs`, `get_autopilot_state`). The roster with its
 * governance counts is `GET /projects`, which already carries `mode`,
 * `promotable`, `classes_ready` and the WIP fields. So there is no
 * `useAutopilotState` here at all: the page would have had to fire one request
 * per project to learn what one request already told it.
 */

/* ----------------------------------------------------------------- shapes -- */

/** One per-scope brake, exactly as `autopilot::ScopedKill` serialises. */
export interface ScopedKill {
  /** `project` or `trigger` — the daemon answers 400 for anything else. */
  scope_type: string;
  scope_id: string;
  engaged: boolean;
}

/** What `POST /autopilot/kill/scoped` accepts. The body is the whole row. */
export interface ScopedKillChange {
  scope_type: string;
  scope_id: string;
  engaged: boolean;
}

/**
 * One action class's shadow record, as `shadow::ClassTally` serialises.
 *
 * Grouped by `(runs.mode, action_class)`, so a project that has run in more
 * than one mode gets more than one row per class. Only the `shadow` rows are
 * evidence for promotion — see {@link SHADOW_EVIDENCE_MODE}.
 */
export interface ClassTally {
  mode: string;
  action_class: string;
  total: number;
  would_allow: number;
  would_pend: number;
  would_deny: number;
  reviewed: number;
  agree: number;
  disagree: number;
}

/** One decision the classifier made without enforcing it, awaiting a human verdict. */
export interface ShadowDecision {
  id: number;
  run_id: number;
  tool_name: string;
  /** The arguments, as the JSON the classifier saw. Null when nothing was recorded. */
  tool_input: string | null;
  /** What the classifier *would* have done: `allow` | `pending_approval` | `deny`. */
  decision: string;
  reason: string | null;
  action_class: string;
  classifier_version: number;
  /** `approve` | `reject`, once somebody has answered. Null is unreviewed. */
  human_verdict: string | null;
  reviewed_at: string | null;
  created_at: string;
}

/** What `POST /autopilot/state` answers on success — 200 with a body, not a 204. */
export interface AutopilotState {
  project_id: string;
  mode: AutopilotMode;
}

/** What that route accepts. `project_root` is required for `shadow` and `active`. */
export interface ModeChange {
  project_id: string;
  mode: AutopilotMode;
  project_root?: string;
}

export interface ShadowVerdict {
  decisionId: number;
  verdict: "approve" | "reject";
}

/* -------------------------------------------------------------- the bar -- */

/**
 * The mode whose decisions count toward leaving shadow.
 *
 * `shadow::shadow_readiness` reads `WHERE runs.mode = 'shadow'` and nothing
 * else, because promotion out of shadow is earned by evidence gathered *in*
 * shadow: a `worktree`-mode decision was actually enforced, not a hypothetical
 * a human could still overrule. The scoreboard returns every mode, so the page
 * has to say which rows are the evidence and which are history.
 */
export const SHADOW_EVIDENCE_MODE = "shadow";

/**
 * The shadow-exit bar, as the núcleo states it (`shadow.rs`).
 *
 * Mirrored here **to be shown, never to be applied.** The daemon's own comment
 * is explicit that the shell reads `classes_ready` and `promotable` off
 * `GET /projects` rather than recomputing them, so that the button a person
 * sees and the rule the product enforces cannot gate on different arithmetic.
 *
 * There is a sharper reason than tidiness, and it is the one that makes
 * recomputing here actually *wrong*: the gate counts reviews and agreements
 * **distinct by `(tool_name, tool_input)`** (`AGREE_DISTINCT` in `shadow.rs`),
 * while the scoreboard counts every row. Ten reviews of the same command are
 * ten on this panel and one at the bar. So these two numbers are context for
 * reading a tally — "the bar is ten reviews at 95%" — and the panel says out
 * loud that the enforced count deduplicates.
 */
export const READINESS_MIN_REVIEWED = 10;
export const READINESS_MIN_AGREE_PERCENT = 95;

/* ------------------------------------------------------------------ keys -- */

/**
 * The one key this file has to spell for itself.
 *
 * `keys.autopilot.shadowDecisions` is a bare prefix, and the route it stands
 * for takes a required `?project_id=` — two projects sharing one cache entry
 * would show one project's decisions under the other's name. The project is
 * appended *under* the declared prefix rather than beside it, so an
 * `invalidateQueries({ queryKey: keys.autopilot.shadowDecisions })` after a
 * verdict still reaches every project's list. `data/keys.ts` is not this
 * packet's to edit.
 */
function shadowDecisionsKey(projectId: string) {
  return [...keys.autopilot.shadowDecisions, projectId] as const;
}

/* ----------------------------------------------------------------- reads -- */

/**
 * What the classifier has decided for one project, class by class.
 *
 * `POLL.queue` rather than `POLL.fast`: this only moves when a shadow run
 * records a decision or somebody reviews one, and both are events. `enabled`
 * because the route's `project_id` is required — asking without one is a 400,
 * so "no project chosen" must not become a failed request.
 *
 * **No `keepPreviousData`**, unlike the lists in `system.ts` and `fleet.ts`.
 * The project is part of the key, so carrying the previous answer across a
 * change of key would show one project's record under another project's name —
 * on the page where the reader is deciding whether that project may act on its
 * own. A blank half-second is the cheaper mistake.
 */
export function useScoreboard(projectId: string | null) {
  return useQuery({
    queryKey: keys.autopilot.scoreboard(projectId ?? ""),
    queryFn: () => apiFetch<ClassTally[]>(`/scoreboard?project_id=${encodeURIComponent(projectId ?? "")}`),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

/**
 * The decisions nobody has answered yet, for one project.
 *
 * `list_unreviewed` filters `human_verdict IS NULL`, so a row leaves this list
 * the moment a verdict lands — which is why the verdict mutation invalidates it
 * rather than trying to patch it. No `keepPreviousData`, for the reason given
 * on the scoreboard above.
 */
export function useShadowDecisions(projectId: string | null) {
  return useQuery({
    queryKey: shadowDecisionsKey(projectId ?? ""),
    queryFn: () =>
      apiFetch<ShadowDecision[]>(`/shadow-decisions?project_id=${encodeURIComponent(projectId ?? "")}`),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

/**
 * The per-scope brakes.
 *
 * `POLL.fast` and not the queue cadence, for the reason the global switch gives
 * in `data/system.ts`: a brake that reads *released* because nothing refetched
 * is the most dangerous stale value a governance page can show.
 */
export function useScopedKills() {
  return useQuery({
    queryKey: keys.autopilot.scopedKills,
    queryFn: () => apiFetch<ScopedKill[]>("/autopilot/kill/scoped"),
    refetchInterval: POLL.fast,
  });
}

/* ------------------------------------------------------------- mutations -- */

/**
 * Answer one shadow decision.
 *
 * **204, so there is no body** (`post_shadow_verdict`) — `void` is the honest
 * type, and `apiFetch` exempts 204 from its JSON parse. A verdict can also move
 * the project across the promotion bar, which is why the roster is invalidated
 * alongside the list: `promotable` is computed by the daemon on every roster
 * read, and the promote control must unlock on the same tick the last review
 * lands rather than three seconds later.
 */
export function useSetShadowVerdict() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ decisionId, verdict }: ShadowVerdict) =>
      apiFetch<void>(`/shadow-decisions/${decisionId}/verdict`, {
        method: "POST",
        body: JSON.stringify({ verdict }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.shadowDecisions });
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/* ------------------------------------------------------------- the judge -- */

/**
 * Spec A D2: whether a model is asked about this project's tool calls. `observe` asks in
 * parallel and decides nothing; `enforce` lets it decide (D7). Photographed onto each run at
 * launch, so a change here reaches the project's NEXT runs.
 */
export type JudgeMode = "off" | "observe" | "enforce";

export interface JudgeClassReadiness {
  action_class: string;
  reviewed: number;
  agree: number;
}

/** D11, as the núcleo computes it — shown, never recomputed here, for `promotable`'s reason. */
export interface JudgeReadiness {
  reviewed: number;
  agree: number;
  ready: boolean;
  by_class: JudgeClassReadiness[];
}

export interface JudgeStatus {
  project_id: string;
  judge: JudgeMode;
  /** Review item G: why the project's `.ai/autopilot.yaml` cannot be read — the judge is without effect then. */
  rules_error: string | null;
  readiness: JudgeReadiness;
}

/** One entry of the judge's review queue (`judge::JudgeVerdictView`). */
export interface JudgeVerdict {
  id: number;
  run_id: number;
  tool_name: string;
  tool_input: string | null;
  action_class: string;
  classifier_decision: string;
  judge: string;
  model: string;
  p_in_scope: number | null;
  p_safe: number | null;
  p: number | null;
  band: "allow" | "deny";
  capped: boolean;
  final_decision: string;
  enforced: boolean;
  created_at: string;
}

/**
 * Spec A D5, in the words the owner reads before turning `enforce` on. The risk is the spec's,
 * stated as plainly as the spec states it; the page shows it and does not soften it.
 */
export const JUDGE_RESIDUAL_RISK =
  "A judge fooled by text the agent read can still approve a command that runs code the agent " +
  "wrote in the workspace itself — a new build.rs under cargo test, an npm script, a Python " +
  "file — and that code can reach the network. Nothing but an operating-system network sandbox " +
  "closes this, and there is none yet. Turning enforce on accepts this risk for this project.";

/** What TypeSafe is, said once; every panel and dialog that sends it something uses this phrase. */
export const TYPESAFE_DEFINITION = "TypeSafe is the outside service that runs the judge model.";

/** The consequence of asking TypeSafe, in the words both armed labels and both dialogs share. */
export const TYPESAFE_LEAVES = "your commands and their output leave this computer";

/** The two sentences the Enforce dialog leads with; the full risk stays on the panel. */
export const JUDGE_ENFORCE_DIALOG =
  "If the judge is fooled by text the agent read, it can approve a command that runs code the agent " +
  "wrote in the workspace, and that code can reach the network. Nothing but an operating-system " +
  "network sandbox prevents this, and there is none yet.";

export function readJudgeBand(verdict: JudgeVerdict): string {
  if (verdict.band === "deny") return "would refuse";
  return verdict.capped ? "would allow — held back by a guard" : "would allow";
}

/** What the judge said about one decision (`judge::JudgeOpinion`). `band` is null when the call failed. */
export interface JudgeOpinion {
  id: number;
  run_id: number;
  shadow_decision_id: number | null;
  tool_name: string;
  judge: string;
  model: string;
  p_in_scope: number | null;
  p_safe: number | null;
  p: number | null;
  band: "allow" | "middle" | "deny" | null;
  capped: boolean;
  final_decision: string;
  enforced: boolean;
  error: string | null;
  created_at: string;
}

export function useJudgeOpinionsForDecisions(ids: readonly number[]) {
  return useQuery({
    queryKey: keys.autopilot.judgeOpinions(ids),
    queryFn: () => apiFetch<JudgeOpinion[]>(`/judge-verdicts/by-decision?ids=${ids.join(",")}`),
    enabled: ids.length > 0,
    refetchInterval: POLL.queue,
  });
}

export function useRunJudgeOpinions(runId: number) {
  return useQuery({
    queryKey: keys.autopilot.runJudgeOpinions(runId),
    queryFn: () => apiFetch<JudgeOpinion[]>(`/runs/${runId}/judge-verdicts`),
    refetchInterval: POLL.queue,
  });
}

/** One opinion in words: the number and the band, or why there is no number. */
export function readJudgeOpinion(opinion: JudgeOpinion): string {
  if (opinion.band === null) return `no answer — ${opinion.error ?? "not recorded"}`;
  const band =
    opinion.band === "deny"
      ? "would refuse"
      : opinion.band === "middle"
        ? "left it to the classifier"
        : opinion.capped
          ? "would allow — held back by a guard"
          : "would allow";
  return `${formatProbability(opinion.p)} · ${band}${opinion.enforced ? " (applied)" : ""}`;
}

export function formatProbability(p: number | null): string {
  return p === null ? "—" : p.toFixed(2);
}

export function useJudgeStatus(projectId: string | null) {
  return useQuery({
    queryKey: keys.autopilot.judge(projectId ?? ""),
    queryFn: () =>
      apiFetch<JudgeStatus>(`/autopilot/judge?project_id=${encodeURIComponent(projectId ?? "")}`),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

export function useJudgeVerdicts(projectId: string | null) {
  return useQuery({
    queryKey: keys.autopilot.judgeVerdicts(projectId ?? ""),
    queryFn: () =>
      apiFetch<JudgeVerdict[]>(
        `/judge-verdicts/unreviewed?project_id=${encodeURIComponent(projectId ?? "")}`,
      ),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

/** No optimistic write, for `useSetProjectMode`'s reason: whether a model decides must never look settled before it is. */
export function useSetProjectJudge() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: { project_id: string; judge: JudgeMode }) =>
      apiFetch<JudgeStatus>("/autopilot/judge", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
    },
  });
}

/* ------------------------------------------------------------ the resolver -- */

/**
 * Spec B D11 (`.ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md`), in the words
 * the owner reads BEFORE turning it on. The resolver sends TypeSafe what the judge alone never
 * sends; the switch is an opt-in because of this sentence.
 */
export const RESOLVE_DATA_WARNING =
  "Resolving blocks sends TypeSafe what the judge alone never sends: the commands this project's " +
  "runs were refused as destructive, and the last 2000 characters of the project's gate output " +
  "when a finished run fails its gate. Observing changes nothing a run does — it asks, and writes " +
  "the answer down for you to review.";

export type ResolveEvent = "hard_deny" | "park" | "gate_failed";
export type ResolveOutcome = "deny" | "warn" | "stop" | "park" | "explain" | "owner" | "correction";

/** Spec B D3, as `Event::outcomes` in `core/src/judge/resolve.rs` lists them. */
const OUTCOMES: Record<ResolveEvent, ResolveOutcome[]> = {
  hard_deny: ["deny", "warn", "stop"],
  park: ["explain", "park", "stop"],
  gate_failed: ["correction", "owner"],
};

export function outcomesFor(event: ResolveEvent): ResolveOutcome[] {
  return OUTCOMES[event];
}

/** The owner's words for each outcome: what they would have wanted, not the column's name. */
const OUTCOME_WORDS: Record<ResolveOutcome, string> = {
  deny: "refuse, and let it carry on",
  warn: "refuse, and tell me",
  stop: "stop the run",
  park: "ask me",
  explain: "carry on without it",
  owner: "hand it to me",
  correction: "let it fix the gate",
};

export function readOutcome(outcome: ResolveOutcome): string {
  return OUTCOME_WORDS[outcome];
}

/** Spec B D11's bar, as `RESOLVE_MIN_REVIEWED`/`RESOLVE_MIN_AGREE_PERCENT` in `judge/resolve_review.rs`. */
export const RESOLVE_MIN_REVIEWED = 10;
export const RESOLVE_MIN_AGREE_PERCENT = 90;

/** Spec B D11, as the núcleo computes it: shown, never recomputed here. */
export interface ResolveReadiness {
  reviewed: number;
  agree: number;
  less_cautious: number;
  ready: boolean;
}

export function readReadiness(r: ResolveReadiness): string {
  if (r.less_cautious > 0) {
    const reviews = r.less_cautious === 1 ? "review" : "reviews";
    return `${r.less_cautious} ${reviews} where the judge was less careful than you — not ready`;
  }
  if (r.reviewed < RESOLVE_MIN_REVIEWED) return `${r.reviewed} of ${RESOLVE_MIN_REVIEWED} reviews`;
  return r.ready
    ? `${r.reviewed} reviewed, ${r.agree} agree — ready`
    : `${r.agree} of ${r.reviewed} agree — under ${RESOLVE_MIN_AGREE_PERCENT}%`;
}

export interface JudgeResolveStatus {
  project_id: string;
  judge_resolve: JudgeMode;
  readiness: ResolveReadiness;
}

/** One entry of the resolver's review queue (`judge::resolve_review::ResolutionView`). */
export interface Resolution {
  id: number;
  run_id: number;
  lineage_root_id: number;
  event: ResolveEvent;
  tool_name: string | null;
  tool_input: string | null;
  gate_output: string | null;
  p_off_task: number | null;
  p_needed: number | null;
  p_avoidable: number | null;
  p_fixable: number | null;
  default_outcome: ResolveOutcome;
  judge_outcome: ResolveOutcome;
  final_outcome: string;
  enforced: boolean;
  created_at: string;
}

/**
 * Spec B section 5, in the words the owner reads before letting the resolver decide: the four
 * risks as the spec states them, and the one line that never moves.
 */
export const RESOLVE_ENFORCE_RISK =
  "Letting the resolver decide accepts, for this project: a run may be told to carry on without " +
  "an action a person would have approved, and finish with work half done (it is told to say what " +
  "is missing); a run may be stopped when the judge thinks an action is off its task; a finished " +
  "run whose gate failed may be continued once, on its own, in the same conversation — that " +
  "correction may break more than it fixes, and its prompt carries the gate's output, which the " +
  "agent itself may have written; and the judge's questions are measured only by the reviews " +
  "below, never by a test set. A hard refusal is never run, whatever the judge says.";

export function useJudgeResolutions(projectId: string | null) {
  return useQuery({
    queryKey: keys.autopilot.judgeResolutions(projectId ?? ""),
    queryFn: () =>
      apiFetch<Resolution[]>(
        `/judge-resolutions/unreviewed?project_id=${encodeURIComponent(projectId ?? "")}`,
      ),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

/** 204, once. Moves the readiness, so the status is invalidated beside the queue. */
export function useSetResolutionOutcome() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, outcome }: { id: number; outcome: ResolveOutcome }) =>
      apiFetch<void>(`/judge-resolutions/${id}/outcome`, {
        method: "POST",
        body: JSON.stringify({ outcome }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
    },
  });
}

export function useJudgeResolveStatus(projectId: string | null) {
  return useQuery({
    queryKey: keys.autopilot.judgeResolve(projectId ?? ""),
    queryFn: () =>
      apiFetch<JudgeResolveStatus>(
        `/autopilot/judge-resolve?project_id=${encodeURIComponent(projectId ?? "")}`,
      ),
    enabled: projectId !== null,
    refetchInterval: POLL.queue,
  });
}

/** No optimistic write, for `useSetProjectJudge`'s reason. */
export function useSetProjectJudgeResolve() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: { project_id: string; judge_resolve: JudgeMode }) =>
      apiFetch<JudgeResolveStatus>("/autopilot/judge-resolve", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
    },
  });
}

/** 204, like the shadow verdict. Moves the readiness, so the status is invalidated beside the queue. */
export function useSetJudgeVerdict() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ verdictId, verdict }: { verdictId: number; verdict: "approve" | "reject" }) =>
      apiFetch<void>(`/judge-verdicts/${verdictId}/verdict`, {
        method: "POST",
        body: JSON.stringify({ verdict }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
    },
  });
}

/**
 * Move a project between `off`, `shadow` and `active`.
 *
 * No optimistic write, and that is the whole point of this mutation: the daemon
 * checks the activation prerequisites and can answer **422 with an empty body**
 * (`activation_status` maps four different `ActivationError`s onto one bare
 * status). A mode drawn optimistically would have to be un-drawn, and the one
 * thing a person must never be unsure about is whether a project is acting on
 * its own.
 *
 * The refusal carries **no detail at all** — `client.ts` fills it from
 * `statusText`, so it arrives as the words "Unprocessable Entity". The page
 * therefore says the generic prerequisite sentence and offers the root input;
 * inventing a specific cause here would be a guess between four.
 */
export function useSetProjectMode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: ModeChange) =>
      apiFetch<AutopilotState>("/autopilot/state", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.all });
    },
  });
}

/**
 * Engage or release one scope's brake.
 *
 * 204 again, so `void`. Optimistic, like the global switch and for the same
 * reason: engaging a brake is a stopping gesture and has to look like it took.
 * `cancelQueries` is what stops a 3-second tick already in flight from landing
 * after the optimistic write and putting the switch back.
 *
 * A scope the daemon has never been told about is simply absent from the
 * listing, so the optimistic update has to be able to *add* a row rather than
 * only patch one — the first engagement of a scope is the common case, not the
 * edge one.
 */
export function useSetScopedKill() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: ScopedKillChange) =>
      apiFetch<void>("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onMutate: async (change: ScopedKillChange) => {
      await queryClient.cancelQueries({ queryKey: keys.autopilot.scopedKills });
      const previous = queryClient.getQueryData<ScopedKill[]>(keys.autopilot.scopedKills);
      if (previous !== undefined) {
        queryClient.setQueryData<ScopedKill[]>(
          keys.autopilot.scopedKills,
          withScope(previous, change),
        );
      }
      return { previous };
    },
    onError: (_error, _change, context) => {
      // Put back exactly what was there, including "there was nothing": writing a
      // guessed value onto a failed brake would invent a state the daemon never had.
      if (context !== undefined) {
        queryClient.setQueryData<ScopedKill[] | undefined>(
          keys.autopilot.scopedKills,
          context.previous,
        );
      }
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.scopedKills });
    },
  });
}

/** The listing with one scope's state replaced, or appended when it was not in it. */
function withScope(rows: ScopedKill[], change: ScopedKillChange): ScopedKill[] {
  const known = rows.some(
    (row) => row.scope_type === change.scope_type && row.scope_id === change.scope_id,
  );
  if (!known) return [...rows, { ...change }];
  return rows.map((row) =>
    row.scope_type === change.scope_type && row.scope_id === change.scope_id
      ? { ...row, engaged: change.engaged }
      : row,
  );
}

/* --------------------------------------------------------------- readings -- */

/** Is this scope's brake on? An absent row means the daemon was never told, which is off. */
export function scopeEngaged(rows: ScopedKill[] | undefined, type: string, id: string): boolean {
  return rows?.some((row) => row.scope_type === type && row.scope_id === id && row.engaged) ?? false;
}

/**
 * What a classifier decision *would* have done, in a word.
 *
 * `pending_approval` is the one worth spelling out: it is neither an allow nor a
 * deny, it is the classifier declining to decide — and in shadow it is also the
 * evidence of restraint that `promotable` requires at least one ready class to
 * be made of.
 */
export function readShadowDecision(decision: string): string {
  switch (decision.trim()) {
    case "allow":
      return "would have allowed";
    case "deny":
      return "would have denied";
    case "pending_approval":
      return "would have asked first";
    default:
      return decision.trim();
  }
}
