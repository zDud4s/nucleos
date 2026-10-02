import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL, pollWhile } from "./poll";

/**
 * The council, as hooks: the list, one deliberation in full, convening one and
 * cancelling one.
 *
 * Every shape below was read off `core/src/council.rs` and the three routes
 * `core/src/http.rs` mounts under `/council`, field for field. Two things are
 * worth saying once here rather than at every call site.
 *
 * **The wire key is `ref`, not `model_ref`.** `SeatView` carries
 * `#[serde(rename = "ref")]` on the Rust side — `model_ref` is the field's Rust
 * name, chosen because `ref` is a keyword, and the JSON this file reads never
 * says that. `SeatView.ref` below is deliberately the wire spelling.
 *
 * **`roster` is optional, and absent unless a caller asks for one.**
 * `POST /council` accepts a per-question override of
 * `~/.nucleos/council.yaml`'s roster, and `useCreateCouncil` puts the key on
 * the request only when it is given one. A call with no override sends
 * `{ question }` and nothing else — the same bytes this file sent for the whole
 * time it offered no override at all. That is deliberate and is the property
 * worth protecting: `Option<RosterOverride>` reads an absent key and a `null`
 * the same way, so nothing would have complained if the key had started
 * travelling as `null`, and the request the daemon has always been given would
 * have quietly stopped being the request it gets.
 *
 * *(Corrected 2026-09-06: this said a roster override was "a thing nobody has
 * asked for yet". The owner asked, and `Council.tsx` now offers one.)*
 */

/** One row of the list — `CouncilSummary`. */
export interface CouncilSummary {
  id: string;
  created_at: string;
  question: string;
  status: string;
  /**
   * Critique rounds asked for, and how many have run. Served on the LIST as well
   * as on the detail: the list is the other place a council's progress is drawn,
   * and a round number with no total beside it reads as finished too early.
   */
  rounds: number;
  rounds_run: number;
  /** 0 while the seats answer, then 1 and up for each critique round. */
  current_round: number;
  /** `answer`, `critique`, `revise` or `synthesis` — the phase the council is in now. */
  current_phase: string;
}

/** Where a reviewer stood on one claim of a peer's answer — `formats::Stance`. */
export type Stance = "agree" | "disagree" | "unsure";

/** One claim a reviewer weighed — `formats::Point`. */
export interface Point {
  claim: string;
  stance: Stance;
  why: string;
}

/** One reviewer's reading of one peer, under the peer's anonymous label — `formats::Review`. */
export interface Review {
  label: string;
  points: Point[];
}

/**
 * A critique step's payload — `formats::Critique`.
 *
 * `ranking` is anonymous labels, best first. Empty is a seat that abstained:
 * it critiqued and chose to rank nobody, which is an answer and not a gap.
 * `deanonymise` turns the labels back into seats.
 */
export interface Critique {
  reviews: Review[];
  ranking: string[];
}

/**
 * One step of one seat — `council::StepView`.
 *
 * `answer` is the text an answer or a revision wrote, `null` on a critique and
 * `null` when there is nothing to show. On an `ok` answer step `null` means the
 * transcript that held it has been pruned, which is why a card reads `status`
 * beside it rather than this field alone.
 */
export interface StepView {
  /** 0 for the answer, 1 and up for the critique rounds. */
  round: number;
  /** `answer`, `critique` or `revise`. */
  phase: string;
  run_id: number | null;
  status: string;
  error: string | null;
  answer: string | null;
  /** A critique's reviews and ballot; `null` on any other phase and on an unreadable critique. */
  critique: Critique | null;
  /** A revision's own account of itself: whether it changed its answer, and why. */
  changed: boolean | null;
  why: string | null;
}

/**
 * One seat, as the client sees it — the record plus every step it took, in the
 * order they happened (answer, then each round's critique and revise).
 */
export interface SeatView {
  seat_idx: number;
  kind: string;
  /** The wire name. See the module header — this is `model_ref` in Rust. */
  ref: string;
  /**
   * The agent that took this seat, when one did. `null` for a seat the roster
   * named by model, which is still the ordinary case and is not a lesser one.
   */
  agent_id: string | null;
  /**
   * The agent's name, read from the catalogue as the view is built rather than
   * copied onto the row. So `agent_id` set with `agent_name` `null` is a real
   * state and not a gap: the agent has been deleted since it answered. The
   * card says so; it does not print an empty title.
   */
  agent_name: string | null;
  /** The role the seat was asked to play (`skeptic`, `fact_checker`, ...), or `null` for a plain seat. */
  role: string | null;
  steps: StepView[];
}

/**
 * One seat's place on a Borda leaderboard — `tally::BordaRow`.
 *
 * `n` travels with the score on purpose: a score over one ballot and a score
 * over five are not the same claim, and a table that dropped it would present
 * them as one.
 */
export interface BordaRow {
  seat_idx: number;
  score: number;
  n: number;
}

/** How far the last critique round's ballots agree — `tally::Agreement`. */
export interface Agreement {
  /** Kendall's tau over the ballots; `null` when there is nothing to compare. */
  tau: number | null;
  level: string;
  ballots: number;
  comparisons: number;
}

/** One side of a disagreement the chairman recorded — `formats::Position`. */
export interface Position {
  seats: number[];
  view: string;
}

export interface Disagreement {
  topic: string;
  positions: Position[];
}

export interface Confidence {
  level: string;
  why: string;
}

/** The chairman's synthesis as its structure — `formats::Synthesis`. */
export interface Synthesis {
  answer: string;
  consensus: string[];
  disagreements: Disagreement[];
  minority: string | null;
  confidence: Confidence;
  open_questions: string[];
  /** Present only when the chairman's structured answer had to be degraded, and why. */
  degraded_reason?: string;
}

/** One deliberation in full — `GET /council/{id}`. */
export interface CouncilView {
  id: string;
  created_at: string;
  question: string;
  status: string;
  /**
   * Critique rounds asked for, rounds that ran, and whether it stopped before
   * the asked number because nothing was left to change. Read from the row and
   * not from the daemon's current configuration, because the file is editable
   * and a council that ran three rounds in March has to keep reading as one.
   */
  rounds: number;
  rounds_run: number;
  stopped_early: boolean;
  current_round: number;
  current_phase: string;
  error: string | null;
  chairman_kind: string;
  chairman_ref: string;
  /** The agent that chaired, when one did — `null` when the roster named a model. */
  chairman_agent_id: string | null;
  /** `null` alongside a set `chairman_agent_id` means that agent is gone, as on a seat. */
  chairman_agent_name: string | null;
  /** How far the last critique round's ballots agree; `null` while no critique round exists. */
  agreement: Agreement | null;
  /**
   * THE leaderboard: the last critique round's Borda scores, best first. Empty
   * while fewer than two seats have a valid answer to rank — that does not stop
   * the synthesis.
   */
  leaderboard: BordaRow[];
  /** Every critique round's leaderboard, in round order. */
  leaderboard_by_round: BordaRow[][];
  /** The synthesis as markdown, once the chairman has produced one. Never HTML. */
  synthesis: string | null;
  synthesis_structured: Synthesis | null;
  synthesis_status: string | null;
  anon_map: Record<string, number>;
  seats: SeatView[];
}

/**
 * One declared seat of the configured roster, in the form the file wrote it —
 * `council::seat_spec_view`. `kind` can be `null` on a seat the file left to
 * its default.
 */
export type ConfiguredSeat = { agent: string } | { kind: string | null; ref: string | null };

/** What the form needs before anyone types — `GET /council/config`, `council::CouncilConfigView`. */
export interface CouncilConfig {
  /** `false` when `~/.nucleos/council.yaml` names no roster. */
  configured: boolean;
  default_rounds: number;
  max_rounds: number;
  /** The closed set of roles a seat may be asked to play, in the daemon's declared order. */
  roles: string[];
  default_roster: { chairman: ConfiguredSeat; members: ConfiguredSeat[] } | null;
}

/**
 * PURE: the config the wire delivered, or `null` when its shape is not one.
 *
 * `apiFetch` casts a `200` body without looking at it, so a daemon answering
 * with the wrong shape would otherwise reach the composer as a config whose
 * bounds are `undefined` — and a sentence built from them. Accepted only when
 * every field is what `council::CouncilConfigView` promises: rounds positive
 * integers with the default inside the ceiling, roles a list of strings, and a
 * roster that is `null` or a chairman plus a list of members.
 */
export function readCouncilConfig(raw: unknown): CouncilConfig | null {
  if (typeof raw !== "object" || raw === null) return null;
  const config = raw as Record<string, unknown>;
  if (typeof config.configured !== "boolean") return null;
  const positive = (value: unknown): value is number =>
    typeof value === "number" && Number.isInteger(value) && value > 0;
  if (!positive(config.default_rounds) || !positive(config.max_rounds)) return null;
  if (config.default_rounds > config.max_rounds) return null;
  if (!Array.isArray(config.roles) || !config.roles.every((role) => typeof role === "string")) {
    return null;
  }
  const roster = config.default_roster;
  if (roster !== null) {
    if (typeof roster !== "object" || roster === undefined) return null;
    const { chairman, members } = roster as Record<string, unknown>;
    if (typeof chairman !== "object" || chairman === null || !Array.isArray(members)) return null;
  }
  return raw as CouncilConfig;
}

/**
 * The steps a council takes one after another: the answers, each round's
 * critique, every revision but the last round's, and the synthesis. The calls
 * inside one step run side by side, so this is the length of the wait.
 */
export function stepsInSequence(rounds: number): number {
  return 2 * rounds + 1;
}

/* ---------------------------------------------------------------- helpers -- */

/**
 * The most model calls one council can make: N answers, R*N critiques,
 * (R-1)*N revisions — the last round's critique is followed by the synthesis,
 * not by another revision — and the chairman's two calls.
 *
 * A ceiling and not an estimate: a council that stops early spends less.
 */
export function ceilingCalls(members: number, rounds: number): number {
  return members + rounds * members + Math.max(rounds - 1, 0) * members + 2;
}

/**
 * The seats an anonymous ballot stood for, position for position.
 *
 * A label that names no seat answers `null` IN PLACE rather than being
 * dropped: dropping it would shift every later position up one, and a ballot
 * that named a stray label would then read as ranking the wrong seats.
 */
export function deanonymise(
  labels: string[],
  anonMap: Record<string, number>,
): (number | null)[] {
  return labels.map((label) =>
    Object.prototype.hasOwnProperty.call(anonMap, label) ? anonMap[label] : null,
  );
}

/**
 * What a seat is called wherever it is named away from its own card.
 *
 * The agent's name when an agent sat and still exists, else the model that
 * answered — never "seat 2", a number the reader then has to carry back to the
 * seat grid to decode. The role is appended because two seats on the same
 * model are told apart by it.
 */
export function seatName(seat: SeatView): string {
  const name = seat.agent_name ?? seat.ref;
  if (seat.role === null || seat.role.trim() === "") return name;
  return `${name} · ${seat.role.replace(/_/g, " ")}`;
}

/* ------------------------------------------------------------------ keys -- */

/**
 * The two keys this file has to spell for itself.
 *
 * `keys.council` landed as a minimal `{ all }` root — this packet's roster of
 * allowed files does not include `data/keys.ts`, so the list and detail keys
 * are built here, under that root. It used to say `WAITING_KEYS` in
 * `data/waiting.ts` does the same; that constant was retired into the Pillars
 * namespace (`keys.ts:431` records the reform) and is no longer an example of
 * anything.
 */
const COUNCIL_KEYS = {
  list: [...keys.council.all, "list"] as const,
  detail: (id: string) => [...keys.council.all, "detail", id] as const,
  config: [...keys.council.all, "config"] as const,
} as const;

/* ------------------------------------------------------------------ reads -- */

/**
 * The listing — `GET /council`, newest first, capped at the daemon's default
 * of 50. Never polled: a council that is deliberating is watched from its own
 * detail page, and a list of past questions does not change on a timer.
 */
export function useCouncils() {
  return useQuery({
    queryKey: COUNCIL_KEYS.list,
    queryFn: () => apiFetch<CouncilSummary[]>("/council"),
    placeholderData: keepPreviousData,
  });
}

/**
 * What the convene form may offer — `GET /council/config`: the round bounds,
 * the closed set of roles, and the roster an override would stand in for.
 *
 * Never polled. The file it reflects is edited by hand and read by the daemon
 * at start, so the answer does not move while the page is open.
 */
export function useCouncilConfig() {
  return useQuery({
    queryKey: COUNCIL_KEYS.config,
    queryFn: () => apiFetch<CouncilConfig>("/council/config"),
  });
}

/** Whether a council can still change on its own. */
export function councilIsAlive(status: string): boolean {
  return status === "running";
}

/**
 * One council in full. Polls at `POLL.council` only while it is still
 * `running` — a settled, errored or cancelled council answers the same bytes
 * forever, and the poll switches itself off on the tick that lands the
 * terminal state.
 *
 * No `keepPreviousData`: a detail that kept the previous council's seats on
 * screen under the current council's title would not be a stale view, it
 * would be the wrong one.
 */
export function useCouncil(id: string) {
  return useQuery({
    queryKey: COUNCIL_KEYS.detail(id),
    queryFn: () => apiFetch<CouncilView>(`/council/${encodeURIComponent(id)}`),
    refetchInterval: pollWhile<CouncilView>(POLL.council, (view) => councilIsAlive(view.status)),
  });
}

/**
 * How a settled council ended, for its list row — the same read as
 * `useCouncil` under the same key, so opening a council warms its row and the
 * reverse. Never polled and never stale: a settled council answers the same
 * bytes forever. `enabled` is the caller's bound — `CouncilSummary` carries no
 * error, synthesis or agreement, so every row that shows one costs a read.
 */
export function useCouncilOutcome(id: string, enabled: boolean) {
  return useQuery({
    queryKey: COUNCIL_KEYS.detail(id),
    queryFn: () => apiFetch<CouncilView>(`/council/${encodeURIComponent(id)}`),
    enabled,
    staleTime: Infinity,
  });
}

/**
 * How far the ballots agreed, in the words a reader uses.
 *
 * The daemon's `tally::Agreement` level is a code ("strong", "none"); "none"
 * printed bare reads as "no data", which is the opposite of what it says — the
 * seats were compared and did not agree. A level this table does not know is
 * printed as the daemon sent it rather than hidden: an unknown word is still
 * the fact, and a missing badge would claim there was nothing to report.
 */
export const AGREEMENT_WORDS: Record<string, string> = {
  strong: "strong consensus",
  split: "split",
  none: "no consensus",
  insufficient: "too few votes",
};

/** "τ 0.42 · split", or the words alone when there was nothing to compare. */
export function agreementText(agreement: Agreement): string {
  const words = AGREEMENT_WORDS[agreement.level] ?? agreement.level;
  return agreement.tau === null ? words : `τ ${agreement.tau.toFixed(2)} · ${words}`;
}

/**
 * How a council ended, in one line: why it failed, that it was cancelled, or
 * the chairman's confidence and the ballots' agreement.
 */
export function outcomeOf(view: CouncilView): string {
  if (view.status === "error") return view.error ?? "failed — no reason recorded";
  if (view.status === "cancelled") return "cancelled";
  const structured = view.synthesis_structured;
  if (structured === null) return "answered";
  const confidence = `${structured.confidence.level} confidence`;
  return view.agreement === null ? confidence : `${confidence} · ${agreementText(view.agreement)}`;
}

/* --------------------------------------------------------------- writes -- */

/**
 * One seat of a roster, in the shape `config::SeatSpec` accepts.
 *
 * A union, and not one object with three optional fields, because the daemon's
 * rule is exclusive: `council::resolve_seat` refuses a seat naming both an
 * agent and a model, and refuses one naming neither, and both refusals are a
 * `400` written in prose. Encoding that as the type means the request cannot be
 * built wrong rather than being checked afterwards.
 *
 * `SeatSpec` also carries `#[serde(deny_unknown_fields)]`, so a fourth key
 * invented on this side is not ignored — it is a refusal. There are three keys
 * and this union names all of them.
 *
 * `ref` is the wire spelling for the same reason `SeatView.ref` is; see the
 * module header. It holds a `ModelChoice.id`, which is what the daemon hands to
 * `--model`, never the label a person reads.
 */
export type RosterSeat = { agent: string } | { kind: "cloud" | "local"; ref: string };

/**
 * The roster for ONE question — `council::RosterOverride`.
 *
 * Writes no configuration. `~/.nucleos/council.yaml` is untouched, and the next
 * council convened without an override reads it exactly as before — which is
 * the whole difference between this and editing the file.
 *
 * The chairman is held apart from the members because the daemon holds it
 * apart: `config::MAX_COUNCIL_SEATS` is compared against `members.len()` alone
 * (`council::start`), so eight members plus a chairman is a roster the daemon
 * accepts and nine members is not.
 */
export interface RosterOverride {
  chairman: RosterSeat;
  members: RosterSeat[];
}

/** What convening takes — `council::CreateCouncilRequest`. */
export interface CreateCouncilRequest {
  question: string;
  /** Omitted, never `null`, when nothing is being overridden. See the module header. */
  roster?: RosterOverride;
  /** Critique rounds for this question. Omitted, never `null`, to take the file's default. */
  rounds?: number;
  /** A role per seat, keyed by the seat's index as a string. Omitted when no seat plays one. */
  roles?: Record<string, string>;
}

/**
 * The request body, one key at a time: `question` always, and each override
 * only when it was given. With none of them this is `{ question }` and nothing
 * else — the module header's property, kept as the request grows.
 * `JSON.stringify` would drop an `undefined` field anyway; what it would not
 * do is make it obvious that dropping it is the point.
 */
function createBody(request: CreateCouncilRequest): Record<string, unknown> {
  const body: Record<string, unknown> = { question: request.question };
  if (request.roster !== undefined) body.roster = request.roster;
  if (request.rounds !== undefined) body.rounds = request.rounds;
  if (request.roles !== undefined) body.roles = request.roles;
  return body;
}

/**
 * Convene a council. `202 Accepted` with `{ id }` — the record exists and
 * nothing has deliberated yet, which is why this is not a `201`.
 *
 * Refusals arrive as prose, not as a named JSON refusal: `post_council` answers
 * `(StatusCode, String)` on every error arm, so `client.ts` derives the code
 * from the status and carries the daemon's own sentence as `detail` — which is
 * what names the limit and the spend on a budget refusal. `Council.tsx` reads
 * that sentence back out rather than replacing it with generic copy. That
 * applies to every refusal an override earns as well: the daemon says which
 * seat was wrong and why, in `config::seat_name`'s own vocabulary, and there is
 * no copy on this side that could say it better.
 */
export function useCreateCouncil() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (request: CreateCouncilRequest) =>
      apiFetch<{ id: string }>("/council", {
        method: "POST",
        // Built key by key rather than stringified whole — see `createBody`.
        body: JSON.stringify(createBody(request)),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: COUNCIL_KEYS.list });
    },
  });
}

/** What `POST /council/{id}/cancel` answers. */
export interface CancelCouncilResult {
  /** Whether THIS call is what settled it. `false` means it had already ended — not an error. */
  cancelled: boolean;
}

/**
 * Ask a running council to stop.
 *
 * `onSettled`, not `onSuccess`: a `404` here means the council ended and was
 * swept between the button rendering and the click landing, and the detail
 * query has to be asked again either way to find out what actually happened.
 */
export function useCancelCouncil() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      apiFetch<CancelCouncilResult>(`/council/${encodeURIComponent(id)}/cancel`, { method: "POST" }),
    retry: false,
    onSettled: (_data, _error, id) => {
      void queryClient.invalidateQueries({ queryKey: COUNCIL_KEYS.detail(id) });
      void queryClient.invalidateQueries({ queryKey: COUNCIL_KEYS.list });
    },
  });
}
