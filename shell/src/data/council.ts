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
 * **This page never sends `roster`.** `POST /council` accepts a per-question
 * override of `.ai/council.yaml`'s roster, and nothing here offers one — a
 * roster override is a thing nobody has asked for yet (see the packet's
 * follow-ups), and a control for it would be a decision this slice did not
 * verify against any design.
 */

/** One row of the list — `CouncilSummary`. */
export interface CouncilSummary {
  id: string;
  created_at: string;
  question: string;
  status: string;
  /** 1, then 2, then 3. Rendered as "phase n of 3". */
  stage: number;
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
  stage1_status: string;
  stage1_error: string | null;
  answer: string | null;
  stage2_status: string;
  stage2_error: string | null;
  rankings: Ranking[];
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
  error: string | null;
  chairman_kind: string;
  chairman_ref: string;
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
 * are built here, under that root, the same way `WAITING_KEYS` extends
 * `keys.waiting.all` in `data/waiting.ts`.
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
 * Convene a council. `202 Accepted` with `{ id }` — the record exists and
 * nothing has deliberated yet, which is why this is not a `201`.
 *
 * Refusals arrive as prose, not as a named JSON refusal: `post_council` answers
 * `(StatusCode, String)` on every error arm, so `client.ts` derives the code
 * from the status and carries the daemon's own sentence as `detail` — which is
 * what names the limit and the spend on a budget refusal. `Council.tsx` reads
 * that sentence back out rather than replacing it with generic copy.
 */
export function useCreateCouncil() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (question: string) =>
      apiFetch<{ id: string }>("/council", {
        method: "POST",
        body: JSON.stringify({ question }),
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
