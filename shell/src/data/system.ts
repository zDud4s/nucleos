// §spec novo-frontend

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
  /**
   * Whether the recorded folder is actually on the disk, answered by the daemon.
   *
   * **Three states, and `data/roster.ts` keeps them three.** `null` means no folder was ever named,
   * which is unfinished; `false` means one was named and is not there, which is broken. The roster
   * used to work this out by asking for a directory listing per project — twenty-five requests on
   * every load to learn one bit each — and its headline counted recorded roots while its rows
   * probed folders, so the two could contradict each other on screen.
   *
   * Optional because the shell can be newer than the daemon: absent reads as `null`, which says
   * "not named" rather than inventing a folder that is there.
   */
  root_exists?: boolean | null;
  /**
   * What this project's gate last said, and when — `passed`, `failed`, `errored`, or absent.
   *
   * Absent is not a fourth failure: a project with no gate command has no definition of green.
   */
  last_gate?: string | null;
  last_gate_at?: string | null;
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
  /**
   * What the turn had read when it reached for this, as a JSON array of
   * `{tool, arguments, at}` — the daemon's `run_untrusted_reads`, copied onto
   * the row so it survives the run being pruned.
   *
   * `null` is a legitimate answer and must never be rendered as "read nothing".
   * A refusal that fired on whose work it is rather than on what was read has no
   * provenance to carry, and neither does one whose recording failed; the two
   * are indistinguishable from here, which is why the card shows nothing rather
   * than a claim.
   */
  read_from: string | null;
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

/** What `POST /autopilot/budget` accepts — `http.rs:697-704`. FIVE fields, all of them, every time. */
export interface BudgetChange {
  limit_usd: number | null;
  period: "daily" | "weekly" | "monthly";
  hourly_limit_usd: number | null;
  per_run_reserve_usd: number;
  time_cost_per_hour_usd: number;
}

/**
 * Replace the budget, whole.
 *
 * `POST /autopilot/budget` answers **200 with the full `BudgetResponse`**
 * (`http.rs:1876-1892`), not 204 — so the mutation is typed against
 * `BudgetView` and its result is written straight into the cache in
 * `onSuccess`, confirmed by an `invalidateQueries` in `onSettled`. No
 * optimistic write, unlike the kill switches above: a ceiling drawn before the
 * daemon accepted it is a ceiling somebody may act on.
 */
export function useSetBudget() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: BudgetChange) =>
      apiFetch<BudgetView>("/autopilot/budget", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onSuccess: (data) => {
      queryClient.setQueryData<BudgetView>(keys.autopilot.budget, data);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.autopilot.budget });
    },
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

/* ------------------------------------------------------------------ health -- */

/**
 * The System pillar's health: the daemon's own subsystem readout and the
 * sidecars' liveness rows, moved here from `data/browser.ts` — this is the
 * ONE health query in the app, and every pillar's own reading (Browser's
 * included) narrows this same cache entry rather than asking again.
 */

/** One sidecar's own liveness row — `sidecar::SidecarState`, `GET /sidecars`. */
export interface SidecarState {
  name: string;
  state: "running" | "down";
  started_at: string | null;
  last_failure: string | null;
  last_failure_at: string | null;
  restarts: number;
  last_line: string | null;
  last_line_at: string | null;
}

/** One subsystem's readiness — `health::SubsystemReadout`, inside `GET /health/readout`. */
export interface SubsystemReadout {
  name: string;
  status: "ok" | "degraded" | "down" | "disabled";
  reason?:
    | "timeout"
    | "not-configured"
    | "unreachable"
    | "permission-denied"
    | "missing"
    | "not-running"
    | "low-disk-space"
    | "unknown";
}

/** `GET /health/readout` — `health::HealthReadout`. */
export interface HealthReadout {
  status: "ok" | "degraded" | "down" | "disabled";
  subsystems: SubsystemReadout[];
}

/**
 * The whole daemon's health, in the daemon's own subsystem order — render it
 * as given, never sorted: `sqlite_pool`, `cli_binary`, `credential_manager`,
 * `worktree_disk`, `echo_sidecar`, `telegram_sidecar`, `email_sidecar`,
 * `web_sidecar`, `browser_sidecar`, `voice_transcriber` (`health.rs:142-183`).
 */
export function useSystemHealth() {
  return useQuery({
    queryKey: keys.system.health,
    queryFn: () => apiFetch<HealthReadout>("/health/readout"),
    refetchInterval: POLL.fast,
  });
}

/** Every sidecar's own liveness row — `GET /sidecars`, a bare array. */
export function useSidecars() {
  return useQuery({
    queryKey: keys.system.sidecars,
    queryFn: () => apiFetch<SidecarState[]>("/sidecars"),
    refetchInterval: POLL.fast,
  });
}

/**
 * Did the whole readout time out?
 *
 * `health.rs` answers a timed-out readout with `status: "down"` and exactly
 * ONE subsystem named `aggregate` — the other nine are not *down*, they were
 * never measured. A page that renders that as nine missing subsystems is
 * inventing an outage.
 */
export function isAggregateTimeout(readout: HealthReadout): boolean {
  return readout.subsystems.length === 1 && readout.subsystems[0].name === "aggregate";
}

/* ----------------------------------------------------------------- backups -- */

/** One stored snapshot — `backup::BackupInfo`. */
export interface BackupInfo {
  name: string;
  /** Absent when the snapshot predates version stamping — absent is not zero. */
  migration_version: number | null;
  size_bytes: number;
}

/** What a restore request answers with — `backup::StagedRestore`. */
export interface StagedRestore {
  name: string;
  migration_version: number;
  /** The daemon's own sentence about when this takes effect. Render it; do not paraphrase. */
  applies: string;
}

/** The stored snapshots — `GET /backups`. */
export function useBackups() {
  return useQuery({
    queryKey: keys.system.backups,
    queryFn: () => apiFetch<BackupInfo[]>("/backups"),
    refetchInterval: POLL.queue,
  });
}

/**
 * Take a snapshot now.
 *
 * `POST /backup` — SINGULAR, one letter apart from the listing route above and
 * answered by a different handler (`http.rs:48-49`). No body; the daemon
 * decides the name and retention.
 */
export function useTakeBackup() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<BackupInfo>("/backup", { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.backups });
    },
  });
}

/**
 * Stage a restore, by snapshot name.
 *
 * Nothing changes immediately — `StagedRestore.applies` is the daemon's own
 * sentence about when the swap actually happens (its next start), and pages
 * render it verbatim rather than paraphrasing.
 */
export function useStageRestore() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      apiFetch<StagedRestore>(`/backups/${encodeURIComponent(name)}/restore`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.backups });
    },
  });
}

/* ------------------------------------------------------------------ tokens -- */

/** The three durable key levels — `auth::ApiTokenLevel`, kebab-case on the wire. */
export type ApiTokenLevel = "read-only" | "run-creating" | "admin";

/** A token as the LISTING gives it — no secret here, ever. */
export interface ApiTokenSummary {
  name: string;
  level: ApiTokenLevel;
  created_at: string;
}

/**
 * What minting answers with — the ONLY place `token` exists.
 *
 * `ApiTokenSummary` above never carries it: the daemon does not keep the plain
 * value around to answer a second read with, so this one response is the
 * token's entire lifetime as a readable string. `ui/CopyOnce` exists because
 * of this shape.
 */
export interface CreatedApiToken {
  name: string;
  level: ApiTokenLevel;
  created_at: string;
  token: string;
}

export interface MintToken {
  name: string;
  level: ApiTokenLevel;
}

/** The minted tokens — `GET /api-tokens`. No poll: the list does not change while the window is open. */
export function useApiTokens() {
  return useQuery({
    queryKey: keys.system.tokens,
    queryFn: () => apiFetch<ApiTokenSummary[]>("/api-tokens"),
  });
}

/**
 * Mint a token.
 *
 * `retry: false` like every mutation in this layer — a refusal is settled, not
 * a glitch to retry past. The answer carries the only copy of the secret the
 * daemon will ever give out; the caller holds it in component state and shows
 * it through `CopyOnce`, never writing it into the query cache — a cache entry
 * is a thing that can be refetched, and this value cannot be.
 */
export function useMintToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (mint: MintToken) =>
      apiFetch<CreatedApiToken>("/api-tokens", {
        method: "POST",
        body: JSON.stringify(mint),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.tokens });
    },
  });
}

/**
 * Revoke a token by name.
 *
 * `DELETE /api-tokens/{name}` answers **204**, typed `void` — `apiFetch`
 * already exempts 204/205 from its JSON parse. A 404 means the token is
 * already gone, which the page treats as a success in substance rather than a
 * failure; invalidating here either way is what makes the listing catch up
 * with that reading without a special case in the mutation itself.
 */
export function useRevokeToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      apiFetch<void>(`/api-tokens/${encodeURIComponent(name)}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.tokens });
    },
  });
}

/* ------------------------------------------------------------------- config -- */

/** `GET /config/email` — `EmailConfigView`. No SMTP field exists; there is no send pre-flight. */
export interface EmailConfig {
  enabled: boolean;
  armed: boolean;
  host: string;
  username: string;
  mailbox: string;
  sent_mailbox: string | null;
  poll_interval_secs: number;
  notify_classes: string[];
  digest_hour_utc: number;
  retain_bodies_days: number;
  local_triage_disabled: string | null;
}

/** `GET /voice/config` — note the path is NOT under `/config/`. */
export interface VoiceConfig {
  armed: boolean;
  hints: string[];
  cleanup_prompt: string;
  cleanup_model: string | null;
  retain_dictations_days: number;
  hotkey: string;
  memo_hotkey: string;
  max_capture_seconds: number;
  max_body_bytes: number;
}

/** `GET /calendar/config` — also NOT under `/config/`. */
export interface CalendarConfig {
  default_tz: string;
  working_hours_start: string;
  working_hours_end: string;
  working_weekdays: string[];
}

/** The e-mail pillar's own configuration. No poll: config does not change while the window is open. */
export function useEmailConfig() {
  return useQuery({
    queryKey: keys.system.config("email"),
    queryFn: () => apiFetch<EmailConfig>("/config/email"),
  });
}

/** The voice pillar's own configuration — `GET /voice/config`, asymmetric with the other two. */
export function useVoiceConfig() {
  return useQuery({
    queryKey: keys.system.config("voice"),
    queryFn: () => apiFetch<VoiceConfig>("/voice/config"),
  });
}

/** The calendar pillar's own configuration — `GET /calendar/config`, asymmetric with the other two. */
export function useCalendarConfig() {
  return useQuery({
    queryKey: keys.system.config("calendar"),
    queryFn: () => apiFetch<CalendarConfig>("/calendar/config"),
  });
}

/* -------------------------------------------------------------------- pii -- */

/** One row of the PII tally — `GET /pii/observations` is an aggregate, not a list of observations. */
export interface PiiTallyRow {
  column: string;
  class: string;
  count: number;
}

/** The PII tally. No `refetchInterval` — read once per open, per design §6.20. */
export function usePiiTally() {
  return useQuery({
    queryKey: keys.system.pii,
    queryFn: () => apiFetch<PiiTallyRow[]>("/pii/observations"),
  });
}
