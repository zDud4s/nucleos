import type { RunSearchResult } from "../data/fleet";
import { NOW } from "./daemon";

/**
 * A runs list that changes over time, for watching the Runs page animate in the preview: a row
 * appears, a waiting row moves from "Needs you" to "Moving", a running row lands in the history.
 *
 * Opt-in with `?runs=live` in the preview URL. Without it the list stays the static fixture, so
 * `scripts/preview-shots.mjs` keeps producing the same picture twice.
 *
 * The list is a pure function of a step number; a step lasts `STEP_MS` of real time. One cycle is
 * four steps and then it starts over from the static list (the new run leaves, the others return).
 */

export const STEP_MS = 4_000;
const CYCLE = 4;
const FRESH_ID = 100;

/** The step the preview is on, or `null` when the live cycle is not switched on. */
export function liveRunsStep(search: string, elapsedMs: number): number | null {
  if (new URLSearchParams(search).get("runs") !== "live") return null;
  return Math.floor(elapsedMs / STEP_MS);
}

const started = typeof performance === "undefined" ? 0 : performance.now();

export function currentRunsStep(): number | null {
  if (typeof window === "undefined") return null;
  return liveRunsStep(window.location.search, performance.now() - started);
}

/** `base` as it stands at `step`; `base` itself when `step` is `null` or on phase 0. */
export function liveRuns(base: RunSearchResult[], step: number | null): RunSearchResult[] {
  if (step === null) return base;
  const phase = step % CYCLE;
  if (phase === 0) return base;
  const at = NOW + phase * STEP_MS;
  let rows = base;
  // Phase 1: a new run arrives, with an id no earlier cycle used.
  rows = [
    {
      id: FRESH_ID + Math.floor(step / CYCLE),
      project_id: "alpha",
      status: "running",
      mode: "real",
      created_at: new Date(at).toISOString(),
      completed_at: null,
      cost_usd: null,
      prompt_excerpt: "Draft the changelog for the release and check it against the merged pull requests.",
    },
    ...rows,
  ];
  // Phase 2: the run awaiting approval is approved and starts moving.
  if (phase >= 2) rows = rows.map((r) => (r.status === "awaiting_approval" ? { ...r, status: "running" } : r));
  // Phase 3: the run that was already running completes and drops into the history.
  if (phase >= 3) {
    rows = rows.map((r) =>
      r.id === 2 ? { ...r, status: "completed", completed_at: new Date(at).toISOString(), cost_usd: 0.0213 } : r,
    );
  }
  return rows;
}
