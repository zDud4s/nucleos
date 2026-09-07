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
  /** 1, then 2, then 3 — and 4 on a council configured for a second round. */
  stage: number;
  /**
   * Three, or four with a second round. Served on the LIST as well as on the
   * detail, because this is the other place a phase number is drawn: a row
   * reading "phase 3" with no total reads as finished on a council that still
   * has a fourth phase to run.
   */
  stages_total: number;
}

/** One peer's vote for one seat, in the anonymised alphabet stage 2 ranks under. */
export interface Ranking {
  anon: string;
  rank: number;
}

/**
 * One seat, as the client sees it — record plus the answer read out of its run.
 *
 * `answer` is `null` for two different reasons the daemon does not distinguish
 * in this field alone: nothing has been written yet, or the transcript that
 * held it has since been pruned. A card tells them apart by also reading
 * `stage1_status` — `ok` with a `null` answer is the pruned case; anything else
 * is simply not there yet.
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
  stage1_status: string;
  stage1_error: string | null;
  answer: string | null;
  stage2_status: string;
  stage2_error: string | null;
  rankings: Ranking[];
  /**
   * The second round, when there was one.
   *
   * `pending` on every seat of a one-round council, and it stays that way
   * forever — the daemon does not write `skipped` across a phase that was never
   * part of the council. So this field alone cannot say whether a revision is
   * still coming; `CouncilView.stages_total` is what answers that, and is why a
   * card is told the total rather than inferring it from here.
   */
  revision_status: string;
  revision_error: string | null;
  /**
   * What the seat wrote the second time. `null` until there is one, and `null`
   * forever on a council of one round.
   *
   * Beside `answer`, never instead of it: the ranking was cast over the FIRST
   * answers, so a card that showed only the revision would be showing a
   * leaderboard of text it never displayed.
   */
  revised_answer: string | null;
}

/** One seat's place in the leaderboard. `n` travels with the average on purpose — see below. */
export interface LeaderboardEntry {
  seat_idx: number;
  avg_rank: number;
  /**
   * How many peers ranked this seat. Shown beside `avg_rank` always: an average
   * over one vote and an average over five are not the same claim, and a table
   * that dropped this would present them as one.
   */
  n: number;
}

/** One deliberation in full — `GET /council/{id}`. */
export interface CouncilView {
  id: string;
  created_at: string;
  question: string;
  status: string;
  stage: number;
  /**
   * How many phases THIS council runs: three, or four when a second round was
   * configured for it.
   *
   * Read from the row and not from the daemon's current configuration, because
   * the file is editable and a council that ran four phases in March has to
   * keep reading as one. It is also the only thing that tells a page whether
   * `stage: 3` is the last phase or the second to last.
   */
  stages_total: number;
  error: string | null;
  chairman_kind: string;
  chairman_ref: string;
  /** The agent that chaired, when one did — `null` when the roster named a model. */
  chairman_agent_id: string | null;
  /** `null` alongside a set `chairman_agent_id` means that agent is gone, as on a seat. */
  chairman_agent_name: string | null;
  /** The chairman's synthesis, once phase 3 has produced one. Plain text, never markup. */
  synthesis: string | null;
  anon_map: Record<string, number>;
  /** Empty while fewer than two seats have a valid answer to rank — that does not stop phase 3. */
  leaderboard: LeaderboardEntry[];
  seats: SeatView[];
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
        // Built key by key rather than stringified whole, so that the no-override
        // case is one branch a reader can check by eye. `JSON.stringify` would
        // drop an `undefined` field anyway; what it would not do is make it
        // obvious that dropping it is the point.
        body: JSON.stringify(
          request.roster === undefined
            ? { question: request.question }
            : { question: request.question, roster: request.roster },
        ),
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
