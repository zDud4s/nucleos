import type { Budget, ClassTally, ProjectSummary } from "./api";

export interface Readiness {
  ready: boolean;
  rate: number | null;
  samples: number;
}

/**
 * Mirrors `READINESS_MIN_REVIEWED` / `READINESS_MIN_AGREE_PERCENT` in `core/src/shadow.rs`, which is
 * the SINGLE SOURCE OF TRUTH for the promotion bar. These copies drive per-class scoreboard COPY
 * only — the gate on the promote control reads `project.promotable` off the daemon, so a drift here
 * can mislabel a row but can never let a project be promoted on different arithmetic.
 */
export const READINESS_MIN_REVIEWED = 10;
export const READINESS_MIN_RATE = 0.95;

export function promotionReadiness(tally: ClassTally): Readiness {
  const samples = tally.reviewed;
  const rate = samples === 0 ? null : tally.agree / samples;
  const ready =
    samples >= READINESS_MIN_REVIEWED &&
    rate !== null &&
    rate >= READINESS_MIN_RATE;
  return { ready, rate, samples };
}

/** A short reason a class is not yet promotable, or null when it is ready. */
export function readinessGap(tally: ClassTally): string | null {
  const { ready, rate, samples } = promotionReadiness(tally);
  if (ready) return null;
  if (samples < READINESS_MIN_REVIEWED) {
    const missing = READINESS_MIN_REVIEWED - samples;
    return `${missing} more review${missing === 1 ? "" : "s"}`;
  }
  return `${Math.round((rate ?? 0) * 100)}% agreement`;
}

/** How many action classes in a group clear the promotion bar. */
export function scoreboardReadiness(
  tallies: ClassTally[],
): { ready: number; total: number } {
  const ready = tallies.filter((tally) => promotionReadiness(tally).ready).length;
  return { ready, total: tallies.length };
}

export function readinessCriterionLabel(): string {
  return `Ready at ${READINESS_MIN_REVIEWED}+ reviews, ≥${Math.round(READINESS_MIN_RATE * 100)}% agreement`;
}

/**
 * Why the promote-to-active control is locked for a project, or null when it may be promoted.
 *
 * The gate exists so shadow mode has a real exit criterion: promotion stays the human's call, but it
 * can't be made on a hunch after three runs. A project already active is never gated (this only
 * guards the way IN to autonomy), and a project with nothing reviewed yet is blocked for lack of
 * evidence rather than for failing the bar — a different message, because it's a different problem.
 */
export function promotionBlock(project: ProjectSummary): string | null {
  if (project.mode === "active" || project.promotable) return null;
  if (project.classes_total === 0) return "No reviewed shadow decisions yet";
  return `${project.classes_ready}/${project.classes_total} action classes ready`;
}

export function totalPending(projects: ProjectSummary[]): number {
  return projects.reduce((sum, project) => sum + project.pending, 0);
}

export type AutopilotState =
  | "kill"
  | "budget"
  | "first"
  | "swamped"
  | "pending"
  | "quiet";

/** Proposals beyond this count tip the approval queue into its dense layout. */
export const SWAMPED_THRESHOLD = 3;

export interface AutopilotSignals {
  killEngaged: boolean | null;
  budgetPaused: boolean;
  /** True only once the project list has loaded and turned out empty. */
  isFirstProject: boolean;
  proposalCount: number;
  pending: number;
}

/**
 * The single most important thing about autopilot right now, in priority order:
 * a global stop outranks a budget pause, which outranks onboarding, a swamped
 * queue, pending review, and finally calm. Shared by the Autopilot cockpit and
 * the Home digest so the two never tell a different story.
 */
export function autopilotState(signals: AutopilotSignals): AutopilotState {
  if (signals.killEngaged === true) return "kill";
  if (signals.budgetPaused) return "budget";
  if (signals.isFirstProject) return "first";
  if (signals.proposalCount > SWAMPED_THRESHOLD) return "swamped";
  if (signals.pending > 0) return "pending";
  return "quiet";
}

export function agreementRate(tally: ClassTally): number | null {
  if (tally.reviewed === 0) return null;
  return tally.agree / tally.reviewed;
}

export function groupScoreboardByMode(
  tallies: ClassTally[],
): Record<string, ClassTally[]> {
  const grouped: Record<string, ClassTally[]> = {};
  for (const tally of tallies) {
    (grouped[tally.mode] ??= []).push(tally);
  }
  return grouped;
}

export function killSwitchLabel(engaged: boolean): string {
  return engaged ? "Kill switch engaged — autopilot paused" : "Kill switch off";
}

export function formatUsd(amount: number): string {
  return `$${amount.toFixed(2)}`;
}

export function periodLabel(period: Budget["period"]): string {
  switch (period) {
    case "daily":
      return "today";
    case "weekly":
      return "this week";
    case "monthly":
      return "this month";
  }
}

export function budgetStatusLabel(budget: Budget): string {
  if (budget.limit_usd === null) return "No spending limit set";
  const base = `${formatUsd(budget.window_spend_usd)} of ${formatUsd(budget.limit_usd)} ${periodLabel(budget.period)}`;
  return budget.paused ? `Paused — ${base}` : base;
}

/**
 * A compact, human relative time like "12 min ago" for feed/proposal metadata;
 * the exact ISO instant belongs in a `title`. `nowMs` defaults to the current
 * time so callers pass just the ISO string, while tests pin it. Unparseable
 * input falls back to the raw string; timestamps older than ~a month fall back
 * to the calendar date.
 */
export function relativeTime(iso: string, nowMs: number = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const sec = Math.floor((nowMs - then) / 1000);
  if (sec < 60) return "just now";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} min ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} h ago`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day} d ago`;
  const wk = Math.floor(day / 7);
  if (wk < 5) return `${wk} w ago`;
  return iso.slice(0, 10);
}
