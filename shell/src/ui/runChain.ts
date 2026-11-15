import type { BadgeTone } from "./Badge";
import { gateStage, resultStage, type Stage } from "./RunPipeline";
import { readState } from "./state-map";

/**
 * The chain of runs one piece of work went through, as pure functions. No React and no import from
 * `../data` (`ui/` stays data-free), so the daemon's shapes are declared structurally here.
 * Tones come only from `readState`; this file authors none.
 */

/** Chains up to this many legs are drawn in full. */
export const COLLAPSE_AT = 5;

/** Structural copy of `LineageLeg` in `data/runs.ts`. */
export interface ChainLeg {
  id: number;
  status: string;
  num_turns: number | null;
  started_at: string | null;
  ended_at: string | null;
  ended_because: string | null;
  gate_status: string | null;
  gate_exit_code: number | null;
  exit_code: number | null;
}

export interface ChainRun {
  id: number;
  lineage?: ChainLeg[];
  lineage_truncated?: boolean;
}

export type ChainItem =
  | {
      kind: "leg";
      id: number;
      title: string;
      sub: string;
      tone: BadgeTone | null;
      current: boolean;
      alive: boolean;
    }
  | { kind: "gap"; hiddenIds: number[] };

export interface RunChain {
  items: ChainItem[];
  gate: Stage;
  result: Stage;
  truncated: boolean;
}

/** Structural copy of `PartOf` in `data/runs.ts`. */
export type ChainPartOf =
  | { kind: "tree"; tree_id: string; tree_title: string; node_id: string; node_title: string }
  | {
      kind: "job";
      job_id: number;
      job_title: string;
      item_id: number | null;
      item_ordinal: number | null;
    };

/** Why a leg was followed by the next; `null` for a reason this build does not know. */
export function reasonLabel(reason: string | null): string | null {
  switch (reason) {
    case "context_full":
      return "context full";
    case "approval_approved":
      return "approved";
    case "approval_declined":
      return "declined";
    default:
      return null;
  }
}

/** Where the chain belongs, or `null` for a loose run. A job with no item names the job only. */
export function partOfText(partOf: ChainPartOf | null): string | null {
  if (partOf === null) return null;
  if (partOf.kind === "tree") {
    return `Part of tree "${partOf.tree_title}" > node "${partOf.node_title}"`;
  }
  const job = `Part of job "${partOf.job_title}"`;
  return partOf.item_ordinal === null ? job : `${job} > item ${partOf.item_ordinal}`;
}

/**
 * The legs to draw, with long chains collapsed to the first, the current and the last leg; each
 * maximal run of hidden legs becomes one gap. Gate and result are the LAST leg's: the chain's
 * outcome is where it ended up, not where the viewed run happened to stop.
 * An empty lineage yields no items; the caller falls back to the single-run stages.
 */
export function chainLegs(run: ChainRun, expanded = false): RunChain {
  const legs = run.lineage ?? [];
  const truncated = run.lineage_truncated ?? false;
  if (legs.length === 0) {
    const none = { status: "", gate_status: null, gate_exit_code: null, exit_code: null };
    return { items: [], gate: gateStage(none), result: resultStage(none, null), truncated };
  }
  const last = legs[legs.length - 1];
  const showAll = expanded || legs.length <= COLLAPSE_AT;
  const items: ChainItem[] = [];
  let hidden: number[] = [];
  const flush = () => {
    if (hidden.length > 0) items.push({ kind: "gap", hiddenIds: hidden });
    hidden = [];
  };
  legs.forEach((l, i) => {
    const current = l.id === run.id;
    if (!showAll && i !== 0 && i !== legs.length - 1 && !current) {
      hidden.push(l.id);
      return;
    }
    flush();
    const state = readState("run", l.status);
    const sub = [
      reasonLabel(l.ended_because),
      l.num_turns === null ? "no turns recorded" : `${l.num_turns} turns`,
    ]
      .filter((p): p is string => p !== null)
      .join(" / ");
    items.push({
      kind: "leg",
      id: l.id,
      title: state?.label ?? l.status,
      sub,
      tone: state?.tone ?? null,
      current,
      // Mirrors `runIsAlive` in `data/runs.ts`, which `ui/` must not import.
      alive: l.status === "running",
    });
  });
  flush();
  return { items, gate: gateStage(last), result: resultStage(last, null), truncated };
}
