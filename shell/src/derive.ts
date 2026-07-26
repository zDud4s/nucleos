import type { Budget, ClassTally, ProjectSummary } from "./api";

export interface Readiness {
  ready: boolean;
  rate: number | null;
  samples: number;
}

const READINESS_MIN_REVIEWED = 10;
const READINESS_MIN_RATE = 0.95;

export function promotionReadiness(tally: ClassTally): Readiness {
  const samples = tally.reviewed;
  const rate = samples === 0 ? null : tally.agree / samples;
  const ready =
    samples >= READINESS_MIN_REVIEWED &&
    rate !== null &&
    rate >= READINESS_MIN_RATE;
  return { ready, rate, samples };
}

export function totalPending(projects: ProjectSummary[]): number {
  return projects.reduce((sum, project) => sum + project.pending, 0);
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
