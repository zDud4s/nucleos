/**
 * The courtroom as numbers: where every chair stands, what state it is in,
 * how far the scales tip and which lines are drawn. PURE, in the manner of
 * `project/atlas-model.ts` — `Courtroom.tsx` only draws what this returns.
 * Every number here is the spec's (2026-10-10, "Decisões" 3-5).
 */

import {
  AGREEMENT_WORDS,
  type Agreement,
  type BordaRow,
  type CouncilView,
  type SeatView,
  type StepView,
  type Synthesis,
} from "../../data/council";
// `court-constants.ts` holds `MAX_COUNCIL_SEATS = 8` (`config::MAX_COUNCIL_SEATS`) on its own,
// because `court-draft.ts` needs it and imports from this file's neighbourhood.
import { MAX_COUNCIL_SEATS } from "./court-constants";

export { MAX_COUNCIL_SEATS };

/** The room's coordinate system. 560×300 is the spec's reference; +20 holds the labels under the lowest chair. */
export const VIEW_W = 560;
export const VIEW_H = 320;

/** The hemicycle: an ellipse arc from 168° to 12°, measured with y growing downwards. */
export const ARC = { cx: 280, cy: 118, rx: 232, ry: 150, from: 168, to: 12 } as const;

/** The full room's step between neighbours: 156°/7. Kept for every n, so a small council sits close. */
export const STEP_DEG = (ARC.from - ARC.to) / (MAX_COUNCIL_SEATS - 1);

/** Chair i of the eight in the setup. */
export function setupAngle(i: number): number {
  return ARC.from - STEP_DEG * i;
}

/** Seat i of n in session: centred on 90°, φ = 90° + step·((n−1)/2 − i). */
export function sessionAngle(i: number, n: number): number {
  return 90 + STEP_DEG * ((n - 1) / 2 - i);
}

export function pointAt(deg: number): { x: number; y: number } {
  const rad = (deg * Math.PI) / 180;
  return { x: ARC.cx + ARC.rx * Math.cos(rad), y: ARC.cy + ARC.ry * Math.sin(rad) };
}

/* ------------------------------------------------------------ scales -- */

/** Mirrors `tally::STRONG_AT` and `tally::SPLIT_AT` (`core/src/council/tally.rs:17-19`). */
export const STRONG_AT = 0.6;
export const SPLIT_AT = 0.2;
/** `tally::MIN_BALLOTS`: below this the daemon does not measure agreement. */
export const MIN_BALLOTS = 3;
/** The tilt at τ ≤ 0, in degrees. Always to the same side: τ is concordance, not direction. */
export const MAX_TILT = 22;

export type Tone = "strong" | "split" | "none" | "insufficient" | "empty";

export interface Balance {
  angle: number;
  tone: Tone;
  label: string;
  sub: string | null;
}

export function balanceAngle(tau: number | null): number {
  if (tau === null) return 0;
  const t = Math.min(1, Math.max(0, tau));
  return MAX_TILT * (1 - t);
}

export function balanceTone(agreement: Agreement): Tone {
  if (agreement.level === "insufficient" || agreement.tau === null) return "insufficient";
  if (agreement.tau >= STRONG_AT) return "strong";
  if (agreement.tau >= SPLIT_AT) return "split";
  return "none";
}

export function balanceOf(agreement: Agreement | null): Balance {
  if (agreement === null) return { angle: 0, tone: "empty", label: "no votes yet", sub: null };
  const tone = balanceTone(agreement);
  if (tone === "insufficient") {
    return { angle: 0, tone, label: AGREEMENT_WORDS.insufficient, sub: `fewer than ${MIN_BALLOTS} ballots` };
  }
  const words = AGREEMENT_WORDS[agreement.level] ?? agreement.level;
  return { angle: balanceAngle(agreement.tau), tone, label: `${words} · τ ${agreement.tau!.toFixed(2)}`, sub: null };
}

/* ------------------------------------------------------------ process line -- */

export type PhaseName = "answer" | "critique" | "revise" | "synthesis";
export interface Moment { round: number; phase: PhaseName }

export interface ProcessPhase extends Moment {
  key: string;
  label: string;
  /** The council has steps for it (or, for the verdict, a synthesis or a synthesis under way). */
  reachable: boolean;
  /** The live moment. */
  current: boolean;
  /** `stopped_early`: the last phase that ran. */
  stoppedHere: boolean;
}

const ROMAN = ["I", "II", "III", "IV", "V"];
export const roman = (n: number) => ROMAN[n - 1] ?? String(n);

/** Opening, Deliberation 1, Rebuttal 1, …, Deliberation R, Verdict — no rebuttal after the last round (`store.rs:12-14`). */
export function processPhases(rounds: number): Moment[] {
  const out: Moment[] = [{ round: 0, phase: "answer" }];
  for (let r = 1; r <= rounds; r += 1) {
    out.push({ round: r, phase: "critique" });
    if (r < rounds) out.push({ round: r, phase: "revise" });
  }
  out.push({ round: rounds, phase: "synthesis" });
  return out;
}

export function phaseLabel(m: Moment): string {
  switch (m.phase) {
    case "answer": return "Opening";
    case "critique": return `Deliberation ${roman(m.round)}`;
    case "revise": return `Rebuttal ${roman(m.round)}`;
    case "synthesis": return "Verdict";
  }
}

export const momentKey = (m: Moment) => (m.phase === "synthesis" ? "synthesis" : `${m.round}:${m.phase}`);

export function sameMoment(a: Moment, b: Moment): boolean {
  return momentKey(a) === momentKey(b);
}

/** Where the council is now. A finished council is at its verdict. */
export function liveMoment(view: CouncilView): Moment {
  if (view.status === "done" || view.current_phase === "synthesis") {
    return { round: view.rounds, phase: "synthesis" };
  }
  return { round: view.current_round, phase: view.current_phase as PhaseName };
}

function hasSteps(view: CouncilView, m: Moment): boolean {
  if (m.phase === "synthesis") {
    return view.synthesis !== null || view.synthesis_structured !== null || view.status === "done"
      || (view.status === "running" && view.current_phase === "synthesis");
  }
  return view.seats.some((s) => s.steps.some((st) => st.round === m.round && st.phase === m.phase));
}

export function processLine(view: CouncilView): ProcessPhase[] {
  const live = liveMoment(view);
  const phases = processPhases(view.rounds).map((m) => ({
    ...m,
    key: momentKey(m),
    label: phaseLabel(m),
    // The live phase stays clickable even before its first step row lands: it is the way back to live.
    reachable: hasSteps(view, m) || (view.status === "running" && sameMoment(m, live)),
    current: view.status === "running" && sameMoment(m, live),
    stoppedHere: false,
  }));
  if (view.stopped_early) {
    const lastRun = [...phases].reverse().find((p) => p.reachable && p.phase !== "synthesis");
    if (lastRun !== undefined) lastRun.stoppedHere = true;
  }
  return phases;
}

/* ------------------------------------------------------------ chair state -- */

export type SeatState =
  | "empty" | "seated"                       // setup
  | "waiting" | "thinking" | "speaking"      // session, not settled
  | "ok" | "failed" | "cancelled" | "skipped";

/**
 * One step as a chair reads it (`council.rs:59-65`, `store::STEP_INVALID`).
 * In progress is `pending` WITH a run in a council still running
 * (`council.rs:1628-1634`); the same row in a council that ended is cancelled.
 * `stalledHere` is true when the moment asked for is where a dead council stopped,
 * so a seat that never got a row there reads as cancelled rather than waiting.
 */
export function stepState(
  step: StepView | undefined,
  councilStatus: string,
  stalledHere = false,
): { state: SeatState; reason: string | null } {
  if (step === undefined) {
    return { state: stalledHere && councilStatus !== "running" ? "cancelled" : "waiting", reason: null };
  }
  switch (step.status) {
    case "pending":
      if (councilStatus !== "running") return { state: "cancelled", reason: null };
      return { state: step.run_id === null ? "thinking" : "speaking", reason: null };
    case "ok": return { state: "ok", reason: null };
    case "invalid": return { state: "failed", reason: "unreadable answer" };
    case "error":
    case "timeout": return { state: "failed", reason: step.error ?? step.status };
    case "cancelled": return { state: "cancelled", reason: null };
    case "skipped": return { state: "skipped", reason: null };
    default: return { state: "failed", reason: step.status };
  }
}

/* ------------------------------------------------------------ tally -- */

export const R_MIN = 14;
export const R_MAX = 24;
export const R_EVEN = 18;

const INSUFFICIENT: Agreement = { tau: null, level: "insufficient", ballots: 0, comparisons: 0 };

/**
 * The tally a moment shows. `*_by_round` are dense — only rounds with a
 * readable ballot (`council.rs:2671`) — so round r is found through
 * `ballot_rounds`, never by index. A round with no entry is even sizes and
 * insufficient; the opening has no votes; the verdict is the last of each.
 */
export function tallyAt(view: CouncilView, m: Moment): { board: BordaRow[] | null; agreement: Agreement | null } {
  if (m.phase === "answer") return { board: null, agreement: null };
  if (m.phase === "synthesis") {
    return { board: view.leaderboard.length > 0 ? view.leaderboard : null, agreement: view.agreement };
  }
  const i = view.ballot_rounds.indexOf(m.round);
  if (i < 0) return { board: null, agreement: INSUFFICIENT };
  return { board: view.leaderboard_by_round[i] ?? null, agreement: view.agreement_by_round[i] ?? INSUFFICIENT };
}

export function radiusFor(board: BordaRow[] | null, seatIdx: number): number {
  if (board === null || board.length === 0) return R_EVEN;
  const top = Math.max(1, ...board.map((row) => row.score));
  const row = board.find((candidate) => candidate.seat_idx === seatIdx);
  if (row === undefined) return R_MIN;
  return R_MIN + (R_MAX - R_MIN) * Math.min(1, Math.max(0, row.score / top));
}

/** Competition ranking: ties share a place and the next place skips. `null` for a seat not on the board. */
export function rankIn(board: BordaRow[], seatIdx: number): number | null {
  const row = board.find((candidate) => candidate.seat_idx === seatIdx);
  if (row === undefined) return null;
  return 1 + board.filter((other) => other.score > row.score + 1e-9).length;
}

/* ------------------------------------------------------------ verdict lines -- */

export interface CourtLink { kind: "top" | "dissent"; from: string; to: string; title: string }

export const seatKey = (seatIdx: number) => `seat-${seatIdx}`;

/**
 * Judge → the seat(s) with the highest Borda. Read as "most voted", never as
 * "the seat the verdict followed": `consensus` and `answer` carry no seat
 * references (`formats.rs:124-135`), so that line would be invented.
 */
export function topLinks(board: BordaRow[] | null): CourtLink[] {
  if (board === null || board.length === 0) return [];
  const best = Math.max(...board.map((row) => row.score));
  return board
    .filter((row) => row.score >= best - 1e-9)
    .map((row) => row.seat_idx)
    .sort((a, b) => a - b)
    .map((seatIdx) => ({ kind: "top" as const, from: "judge", to: seatKey(seatIdx), title: "most voted" }));
}

/** Every pair of seats on different positions of one disagreement (`formats.rs:106-115`), once each. */
export function dissentLinks(synthesis: Synthesis | null): CourtLink[] {
  if (synthesis === null) return [];
  const seen = new Set<string>();
  const out: CourtLink[] = [];
  for (const d of synthesis.disagreements) {
    d.positions.forEach((p, i) => {
      for (const q of d.positions.slice(i + 1)) {
        for (const a of p.seats) for (const b of q.seats) {
          if (a === b) continue;
          const [lo, hi] = a < b ? [a, b] : [b, a];
          const key = `${d.topic}|${lo}|${hi}`;
          if (seen.has(key)) continue;
          seen.add(key);
          out.push({ kind: "dissent", from: seatKey(lo), to: seatKey(hi), title: d.topic });
        }
      }
    });
  }
  return out;
}

/** Seats marked dissent: only with a `minority`, only the position with STRICTLY fewer seats. */
export function dissenters(synthesis: Synthesis | null): Set<number> {
  const out = new Set<number>();
  if (synthesis === null || synthesis.minority === null) return out;
  for (const d of synthesis.disagreements) {
    if (d.positions.length < 2) continue;
    const sizes = d.positions.map((p) => p.seats.length);
    const least = Math.min(...sizes);
    if (sizes.filter((size) => size === least).length !== 1) continue;
    d.positions[sizes.indexOf(least)].seats.forEach((s) => out.add(s));
  }
  return out;
}

/* ------------------------------------------------------------ session court -- */

export interface CourtSeat {
  key: string;            // seatKey(seat_idx) in session, `chair-${i}` in setup
  index: number;          // seat_idx in session, chair index in setup
  x: number; y: number; r: number;
  /** What the chair shows: the anonymous letter in a critique, else the name; "+" when empty. */
  label: string;
  /** The real name, revealed on hover/focus when masked. */
  name: string;
  sublabel: string | null; // the role, drawn in lilac
  state: SeatState;
  reason: string | null;
  masked: boolean;
  rank: number | null;
  dissent: boolean;
  /** The step's run, for the dossier's tail while speaking. */
  runId: number | null;
  /** The step this seat took at the moment shown (session only): the dossier reads its text from here. */
  step: StepView | undefined;
  /** The setup position this seat entered from (for the settle animation). */
  from: { x: number; y: number };
}

export type JudgeState = "empty" | "seated" | "waiting" | "speaking" | "verdict" | "failed" | "cancelled";
export interface CourtJudge { name: string; label: string; sub: string | null; state: JudgeState }

export interface Court {
  seats: CourtSeat[];
  judge: CourtJudge;
  balance: Balance;
  wing: { title: string; sub: string | null };
  lectern: { text: string; full: string };
  /** `from`: one voter-chair position per ballot, in `seat_idx` order, for the fly-in animation. */
  ballots: { got: number; total: number; from: { x: number; y: number }[] } | null;
  links: CourtLink[];
  dimmed: boolean;
}

/** How many characters of the question the lectern can hold. */
export const LECTERN_CHARS = 34;
export function truncate(text: string, room = LECTERN_CHARS): string {
  const t = text.trim();
  return t.length > room ? `${t.slice(0, room - 1)}…` : t;
}

const PHASE_WORDS: Record<PhaseName, string> = {
  answer: "opening", critique: "deliberation", revise: "rebuttal", synthesis: "verdict",
};

function stepAt(seat: SeatView, m: Moment): StepView | undefined {
  if (m.phase === "synthesis") return seat.steps[seat.steps.length - 1];
  return seat.steps.find((s) => s.round === m.round && s.phase === m.phase);
}

export function buildSessionCourt(view: CouncilView, at: Moment | "live"): Court {
  const live = liveMoment(view);
  const m = at === "live" ? live : at;
  const stalledHere = sameMoment(m, live);
  const seats = [...view.seats].sort((a, b) => a.seat_idx - b.seat_idx);
  const n = seats.length;
  const letters: Record<number, string> = Object.fromEntries(
    Object.entries(view.anon_map).map(([letter, idx]) => [idx, letter]),
  );
  const { board, agreement } = tallyAt(view, m);
  const verdict = m.phase === "synthesis";
  const structured = view.synthesis_status === "degraded" ? null : view.synthesis_structured;
  const dissent = verdict ? dissenters(structured) : new Set<number>();

  const courtSeats = seats.map((seat, i): CourtSeat => {
    const { x, y } = pointAt(sessionAngle(i, n));
    const step = stepAt(seat, m);
    const { state, reason } = stepState(step, view.status, stalledHere);
    const name = seat.agent_name ?? seat.ref;
    const letter = letters[seat.seat_idx];
    const masked = m.phase === "critique" && letter !== undefined;
    return {
      key: seatKey(seat.seat_idx), index: seat.seat_idx, x, y,
      r: radiusFor(board, seat.seat_idx),
      label: masked ? letter : name, name,
      sublabel: seat.role !== null && seat.role !== "" ? seat.role.replace(/_/g, " ") : null,
      state, reason, masked,
      rank: verdict && board !== null ? rankIn(board, seat.seat_idx) : null,
      dissent: dissent.has(seat.seat_idx),
      runId: step?.run_id ?? null,
      step,
      from: pointAt(setupAngle(i)),
    };
  });

  const ballotRound = m.phase === "synthesis" ? view.rounds_run : m.round;
  const voters = courtSeats.filter((_, i) =>
    seats[i].steps.some((st) => st.round === ballotRound && st.phase === "critique" && st.status === "ok" && st.critique !== null),
  );
  const ballots = m.phase === "answer" ? null : {
    got: voters.length,
    total: n,
    from: voters.map((v) => ({ x: v.x, y: v.y })),
  };

  return {
    seats: courtSeats,
    judge: judgeOf(view, m),
    balance: balanceOf(agreement),
    wing: wingOf(view, m, at === "live", courtSeats),
    lectern: { text: truncate(view.question), full: view.question },
    ballots,
    links: verdict ? [...topLinks(board), ...dissentLinks(structured)] : [],
    dimmed: view.status === "cancelled",
  };
}

function judgeOf(view: CouncilView, m: Moment): CourtJudge {
  const name = view.chairman_agent_name ?? view.chairman_agent_id ?? view.chairman_ref;
  if (view.status === "error") return { name, label: "FAILED", sub: view.error, state: "failed" };
  if (view.status === "cancelled") return { name, label: "CANCELLED", sub: null, state: "cancelled" };
  if (view.status === "done" && m.phase === "synthesis") {
    const level = view.synthesis_structured?.confidence.level;
    return { name, label: "VERDICT", sub: level !== undefined ? `confidence ${level}` : null, state: "verdict" };
  }
  if (view.status === "running" && view.current_phase === "synthesis") {
    return { name, label: name, sub: "writing the verdict", state: "speaking" };
  }
  return { name, label: name, sub: "waiting", state: "waiting" };
}

function wingOf(view: CouncilView, m: Moment, live: boolean, seats: CourtSeat[]): Court["wing"] {
  if (view.status === "cancelled") return { title: "COUNCIL CANCELLED", sub: null };
  if (view.status === "done" && m.phase === "synthesis") {
    const parts = [`${view.rounds_run} ${view.rounds_run === 1 ? "ROUND" : "ROUNDS"}`];
    if (view.agreement !== null) parts.push(AGREEMENT_WORDS[view.agreement.level] ?? view.agreement.level);
    if (view.stopped_early) parts.push("stopped early");
    return { title: parts.join(" · "), sub: null };
  }
  const title = m.phase === "synthesis" ? "VERDICT · the chair is writing" : `ROUND ${roman(Math.max(m.round, 1))} · ${PHASE_WORDS[m.phase]}`;
  const done = seats.filter((s) => s.state === "ok").length;
  return { title: m.phase === "answer" ? "OPENING" : title, sub: live ? `${done} of ${seats.length} seats` : "from the record" };
}
