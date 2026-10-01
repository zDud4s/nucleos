import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText } from "./client";
import type { RunSearchResult } from "./fleet";
import { keys } from "./keys";
import { POLL, pollWhile } from "./poll";

/**
 * Runs, as hooks: the index, one run in full, its live tail, and the five
 * writes a person can make to one.
 *
 * Every shape below was read off `core/src/runs.rs` and the routes
 * `core/src/http.rs` mounts, field for field. Two of them differ from what the
 * old shell declared, and both differences are load-bearing — `successor_run_id`
 * exists and was never rendered, and the refusal codes are not the ones the old
 * comment claimed. They are documented where they are used rather than here.
 */

/**
 * One row of the index.
 *
 * Re-exported rather than re-declared: `GET /runs` answers this shape whether
 * the caller filtered it or asked for `live=true`, and the fleet's slice
 * declared it first. Two definitions of one row is how a field added to the
 * daemon reaches one page and not the other.
 */
export type { RunSearchResult };

/**
 * One run in full, exactly as `RunStatusResponse` serialises.
 *
 * Note what is *not* here: no `mode`, no `created_at`, no `prompt`. The detail
 * route reads a different set of columns from the index, and a page that shows
 * a run's mode has to have come from a row that carried one. Inventing the
 * fields here would make them `undefined` at runtime under a type that promised
 * otherwise.
 */
export interface RunDetail {
  id: number;
  project_id: string | null;
  status: string;
  /** NULL is **no gate configured** — never a failure. See `ui/state-map.ts`. */
  gate_status: string | null;
  gate_exit_code: number | null;
  gate_output: string | null;
  exit_code: number | null;
  stdout: string | null;
  stderr: string | null;
  session_id: string | null;
  cost_usd: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  num_turns: number | null;
  /**
   * How much of the model's context window this run has filled, in tokens. The
   * daemon hands a run off to a successor at four fifths of it
   * (`core/src/handoff.rs`), so this is the one number about a live run that
   * says what it is about to *do* rather than what it did.
   */
  context_fill: number | null;
  /**
   * Whether this run accepts `POST /runs/{id}/message`. Decided when the run
   * was created and never after, which is why a `false` means the composer is
   * *absent* rather than disabled: nothing a person can do makes this run
   * listen.
   */
  steerable: boolean;
  /**
   * The run that continued this one after a context handoff. Present in the
   * núcleo and absent from the old shell's type, so the link the handoff
   * records was reachable only by reading the database.
   */
  successor_run_id: number | null;
  /**
   * How much of this run's prompt the daemon wrote itself, as an estimated
   * token count: the MCP tool schemas it announced, the standing instructions
   * it appended, the helper definitions it passed, and the prompt.
   *
   * `estimate` and not `tokens`, in the field name and in the label on screen,
   * because it is four characters to the token and nothing finer — there is no
   * tokenizer anywhere in this product. `core/src/prompt_budget.rs` holds the
   * ruler and the argument for it.
   *
   * `null` for a run whose prompt this daemon did not author — the local model,
   * the Codex CLI — and for every run launched before the column existed.
   * Neither of those is a run that wrote nothing.
   */
  authored_prompt_estimate: number | null;
  /**
   * Everything else in the prompt, as an estimate: the CLI's own.
   *
   * **One residual, never a breakdown.** The CLI's system prompt, its built-in
   * tool definitions, whatever it loaded from a CLAUDE.md and the conversation
   * itself are all in this one number, undivided, because nothing the daemon
   * receives separates them — and CLAUDE.md in particular is not something the
   * daemon ever sends, so its size is not a thing this product can know. A
   * field here claiming to split it out would be inventing the number.
   *
   * `null` whenever either side is unknown, which includes every run that
   * reported no token usage at all.
   */
  cli_own_estimate: number | null;
}

/** What a run has written since a byte offset, while it is still writing. */
export interface RunTailChunk {
  text: string;
  /** The offset to send next time, in BYTES, as the daemon counted them. */
  next: number;
  live: boolean;
}

/**
 * The daemon has no live tail for this run.
 *
 * A 204, and **not** an error: a finished run — or one a previous daemon
 * started — has its output in `runs.stdout`, which is a different fact from the
 * output being missing. A page that read this as a failure would offer a retry
 * for a state that will never change.
 */
export const RECORDED = "recorded";

export type RunTail = RunTailChunk | typeof RECORDED;

/** What `POST /runs` accepts. */
export interface NewRun {
  prompt: string;
  /** `null` is a run with no project, which is a real thing and not a gap. */
  project_id: string | null;
  cwd: string | null;
  mode: string;
  /**
   * Ask for a run that can be spoken to after it starts. Opt-in on the daemon's
   * side too: a run listens only because somebody asked for one that would.
   */
  steerable: boolean;
}

/**
 * The modes a person may ASK for.
 *
 * `assistant` is deliberately not here. The daemon writes it, so it appears in
 * the status filter below, but an assistant turn is created by talking to the
 * assistant — offering it in this form would be a control that cannot work.
 */
export const RUN_MODES = ["real", "shadow", "worktree"] as const;

/** Every mode a row can carry — the askable three, plus the one only the assistant creates. */
export const RUN_MODE_FILTERS = [...RUN_MODES, "assistant"] as const;

/** Every status a run row can carry, as `runs.rs` writes them. */
export const RUN_STATUSES = [
  "running",
  "awaiting_approval",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "timed_out",
  "superseded",
] as const;

/**
 * The index page's ceiling.
 *
 * The daemon defaults a search to 50 and clamps it at 200
 * (`parse_search_limit`, `SEARCH_LIMIT_MAX`). Sent explicitly rather than left
 * to the default, so that a list arriving at exactly this length is known to
 * have been cut rather than guessed at.
 */
export const RUN_LIST_LIMIT = 50;

/**
 * The filters the index accepts, named as the *route* spells them.
 *
 * `project` here becomes `project_id` in the query string — the shorter name is
 * what a person sees in a search param, and the translation happens once, here,
 * rather than at each caller.
 */
export interface RunFilters {
  project?: string;
  status?: string;
  mode?: string;
  q?: string;
}

/** A filter set as a plain record, which is what the query key is built from. */
export function runFilterFields(filters: RunFilters): Record<string, string | undefined> {
  return {
    project: blankToUndefined(filters.project),
    status: blankToUndefined(filters.status),
    mode: blankToUndefined(filters.mode),
    q: blankToUndefined(filters.q),
  };
}

/**
 * An empty filter is **not** a filter for the empty string.
 *
 * `?status=` would ask the daemon for runs whose status is `""` and get an
 * empty list back, which reads on screen as "there are no runs" rather than as
 * "you filtered everything out".
 */
function blankToUndefined(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
}

function runsQuery(filters: RunFilters): string {
  const fields = runFilterFields(filters);
  const params = new URLSearchParams();
  if (fields.project !== undefined) params.set("project_id", fields.project);
  if (fields.status !== undefined) params.set("status", fields.status);
  if (fields.mode !== undefined) params.set("mode", fields.mode);
  if (fields.q !== undefined) params.set("q", fields.q);
  params.set("limit", String(RUN_LIST_LIMIT));
  return `?${params.toString()}`;
}

/** Whether a run can still change on its own; waiting on a person does not. */
export function runIsAlive(status: string): boolean {
  return status === "running";
}

/**
 * The filtered index.
 *
 * `keepPreviousData` because this is a list: the filters are in the URL and a
 * keystroke in the text box changes the key, and a list that blanks between two
 * keys makes typing feel like the app breaking. The filters are *in* the key on
 * purpose — two filter sets are two answers, and sharing one entry between them
 * is exactly how a list ends up showing the previous filter's rows.
 */
export function useRuns(filters: RunFilters) {
  const path = `/runs${runsQuery(filters)}`;
  return useQuery({
    queryKey: keys.runs.search(runFilterFields(filters)),
    queryFn: () => apiFetch<RunSearchResult[]>(path),
    refetchInterval: POLL.fast,
    placeholderData: keepPreviousData,
  });
}

/**
 * One run in full.
 *
 * **No `keepPreviousData`, and that is the whole point of the exception.** On a
 * list it means a stale view; on a detail it means the previous run's output
 * under the current run's title, which is not a stale view but the wrong one.
 *
 * The poll stops on the tick that lands a terminal status — a finished run
 * answers the same bytes forever, and a window left open on one would cost a
 * request every three seconds for a page that is over.
 */
export function useRun(id: number) {
  return useQuery({
    queryKey: keys.runs.detail(id),
    queryFn: () => apiFetch<RunDetail>(`/runs/${id}`),
    refetchInterval: pollWhile<RunDetail>(POLL.fast, (run) => runIsAlive(run.status)),
  });
}

/**
 * What the run has written since `since`.
 *
 * `since` is the daemon's own `next`, in BYTES, handed straight back. Measuring
 * the received string in JavaScript characters instead drifts on the first
 * non-ASCII byte and then redraws text already on screen.
 *
 * The cursor is **not** in the query key: one run has one tail, and a key per
 * offset would leave a cache entry per chunk for a log that is read once. The
 * key separates the tails of different runs and nothing else; the offset rides
 * in the closure, so the next scheduled fetch picks it up.
 *
 * `alive` is the run's own vitality rather than the tail's. A `pending` run has
 * no tail yet and answers 204 — reading that as "recorded" and stopping would
 * mean a run that never showed its output.
 */
export function useRunTail(id: number, since: number, alive: boolean) {
  return useQuery({
    queryKey: keys.runs.tail(id),
    queryFn: async (): Promise<RunTail> => {
      // A 204 comes back through `apiFetch` as `undefined`, and react-query
      // refuses `undefined` as data — so the absence is named here instead.
      const chunk = await apiFetch<RunTailChunk | undefined>(`/runs/${id}/tail?since=${since}`);
      return chunk ?? RECORDED;
    },
    refetchInterval: alive ? POLL.fast : false,
  });
}

/**
 * One decision the tool gate made, as `GET /runs/{id}/stop` reports it.
 *
 * `tool_input` is a PREVIEW and `tool_input_truncated` says whether it is the
 * whole thing. The flag is a sibling field rather than an ellipsis inside the
 * text on purpose: a marker in the string is indistinguishable from a command
 * that happens to end in one.
 */
export interface StopDecision {
  tool_name: string;
  action_class: string;
  decision: string;
  reason: string | null;
  classifier_version: number;
  policy_digest: string | null;
  tool_input: string | null;
  tool_input_truncated: boolean;
  created_at: string;
}

/**
 * Why a run stopped.
 *
 * Every kind-specific field is present and `null` for a kind that does not use
 * it, never omitted — so a missing `timeout` means "this was not a timeout" and
 * never "the daemon stopped sending this field".
 *
 * `verdict` is only ever `"silence"` or `"undetermined"`. There is deliberately
 * no `"wall"`: affirming the wall clock fired would need the moment the run
 * actually started, and `elapsed_seconds` is measured from `created_at`, which
 * includes any time the run waited before it began.
 */
export interface RunStop {
  run_id: number;
  status: string;
  kind:
    | "gate"
    | "timeout"
    | "failed"
    | "cancelled"
    | "interrupted"
    | "superseded"
    | "completed"
    | "running";
  summary: string;
  /** False for `real` mode, which is governed by the person at the keyboard and records nothing. */
  decisions_recorded: boolean;
  gate: StopDecision | null;
  timeout: {
    elapsed_seconds: number;
    wall_ceiling_seconds: number;
    silence_ceiling_seconds: number;
    measured_from: string;
    verdict: "silence" | "undetermined";
  } | null;
  leading_up: StopDecision[] | null;
  exit_code: number | null;
  stderr_tail: string | null;
  successor_run_id: number | null;
}

export interface RunBriefing {
  run_id: number;
  mode: string | null;
  traced: boolean;
  reason: "no_trace_context" | "past_retention" | "nothing_offered" | null;
  items: BriefingItem[];
}

export interface BriefingItem {
  knowledge_id: number;
  shown: boolean;
  s_fts: number;
  s_scope: number;
  s_structure: number;
  s_recency: number;
  s_use: number;
  at: string;
  layer: string;
  kind: string;
  scope_kind: string;
  scope_id: string | null;
  source: string;
  status: string;
  observations: number | null;
  title: string;
  body: string;
}

/**
 * What this run was told, and the selection trace behind it.
 *
 * No poll: the briefing is written once before the run starts and never
 * changes afterwards.
 */
export function useRunBriefing(id: number) {
  return useQuery({
    queryKey: keys.runs.briefing(id),
    queryFn: () => apiFetch<RunBriefing>(`/runs/${id}/knowledge`),
  });
}

/**
 * Why this run stopped, for the block on the run page.
 *
 * **No poll of its own.** It rides the cadence the page is already keeping —
 * the same `alive` the tail is given — rather than choosing an interval here.
 * A second cadence on one page is two answers about one run arriving at
 * different moments, which is how a header and a panel end up disagreeing.
 *
 * A live run is still asked, and answers `kind: "running"`: "it has not
 * stopped" is a real answer to this question, and the block says so. Once the
 * run is over the report is immutable, so the poll stops with the page's.
 */
export function useRunStop(id: number, alive: boolean) {
  return useQuery({
    queryKey: keys.runs.stop(id),
    queryFn: () => apiFetch<RunStop>(`/runs/${id}/stop`),
    refetchInterval: alive ? POLL.fast : false,
  });
}

/**
 * Ask for a run.
 *
 * No optimistic row: a run does not exist until the daemon says it does, and
 * every refusal this route makes is one an optimistically drawn row would have
 * to be taken back. The invalidation covers capacity too — a worktree run takes
 * a project slot, and the fleet's `n/limit` is the number that must not lag.
 */
export function useCreateRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (run: NewRun) =>
      apiFetch<{ id: number }>("/runs", { method: "POST", body: JSON.stringify(run) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.all });
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
    },
  });
}

/**
 * Stop a run.
 *
 * `apiText` and not `apiFetch`: `cancel_run` answers a bare `StatusCode::OK`,
 * which is a 200 with an **empty body**, and `apiFetch` would try to parse it as
 * JSON and turn a success into "the daemon answered with a body that is not
 * JSON". The text call treats an empty body as the empty string, which is what
 * it is. Same reasoning for the steering write below.
 */
export function useCancelRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: number) => {
      await apiText(`/runs/${id}/cancel`, { method: "POST" });
    },
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.all });
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
    },
  });
}

/** One more turn, said to a run that is already working. `202 Accepted`, empty body. */
export function useSteerRun(id: number) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (message: string) => {
      await apiText(`/runs/${id}/message`, { method: "POST", body: JSON.stringify({ message }) });
    },
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.detail(id) });
    },
  });
}

/**
 * Say that no more turns are coming.
 *
 * Not a nicety: a steerable run reads until its stdin closes, and the daemon
 * holds that stdin open for as long as it holds the run's channel. Without
 * this, a conversation can only end by going quiet long enough to trip the
 * progress deadline — and is then recorded `timed_out`, a failure status, for
 * having waited. Idempotent by design, so it is never a race to send.
 */
export function useEndTurns(id: number) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<void>(`/runs/${id}/message`, { method: "DELETE" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.detail(id) });
    },
  });
}

/**
 * Give a paused worktree run's tree back.
 *
 * Refuses with 409 when the run is not `awaiting_approval` — the tree is still
 * being worked in, and releasing it would take the work with it.
 */
export function useReleaseWorktree(id: number) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<void>(`/worktrees/${id}/release`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.all });
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
    },
  });
}
