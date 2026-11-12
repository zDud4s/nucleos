// Pure adapters for the devtime agent-effectiveness panel (metric 10, follow-up C). The daemon sends an
// agent type, its role and counts, never a session, an agent id, a worktree or a prompt (spec analytics-devtime §8).
import type { AgentTypeRow, AgentVerification, DevtimeAgents } from "../data/devtime";
import { percentText } from "../lib/format";
import { plural } from "../lib/plural";
import { perCallText } from "./turn-efficiency";

/** The daemon's `__other__` row: every type past its own top rows, folded into one. */
export const OTHER_AGENT = "__other__";

/** The daemon's name for a run whose agent type was not recorded. */
export const UNKNOWN_AGENT = "unknown";

/** At most this many type rows are drawn. */
export const TOP_AGENT_TYPES = 8;

const ROLE_LABELS: Readonly<Record<string, string>> = {
  implementer: "Implementer",
  reviewer: "Reviewer",
};

export function agentTypeText(agentType: string): string {
  if (agentType === OTHER_AGENT) return "Other";
  if (agentType === UNKNOWN_AGENT) return "Unknown type";
  return agentType;
}

/** The label of a role; a dash for none, and a token a newer daemon added comes back as is. */
export function roleText(role: string | null): string {
  if (role === null) return "—";
  return (Object.prototype.hasOwnProperty.call(ROLE_LABELS, role) ? ROLE_LABELS[role] : undefined) ?? role;
}

/** "71% (10 of 14)": checks passed out of the checked runs; a dash when no run was checked. */
export function passText(block: { pass_rate: number | null; verified: AgentVerification }): string {
  if (block.pass_rate === null) return "—";
  const { passed, failed } = block.verified;
  return `${percentText(block.pass_rate)} (${passed} of ${passed + failed})`;
}

/** A type's median run: seconds under a minute, then the page's minutes/hours; a dash when unknown. */
export function medianText(row: AgentTypeRow): string {
  return perCallText(row.median_ms);
}

/** What "checked" means, plus the runs whose sessions no rule has read yet. */
export function checkedNote(agents: DevtimeAgents): string {
  const note = "Implementer runs are checked by the next test or build in the main session; others show as not measured.";
  const unruled = agents.verified.unruled;
  return unruled > 0 ? `${note} · ${plural(unruled, "run")} in sessions not ruled yet` : note;
}

/** The first `max` type rows, in the daemon's order. */
export function topAgentTypes(rows: AgentTypeRow[], max = TOP_AGENT_TYPES): AgentTypeRow[] {
  return rows.slice(0, max);
}

/** Whether there is anything to show: a missing block (older daemon) or one with no runs has nothing. */
export function hasAgents(block: DevtimeAgents | undefined): block is DevtimeAgents {
  return block !== undefined && block.attempts > 0;
}
