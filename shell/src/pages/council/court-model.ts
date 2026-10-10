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
