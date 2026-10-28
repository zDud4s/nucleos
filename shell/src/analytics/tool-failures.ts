// Pure adapters for the devtime tool-failures panel (metric 7, follow-up A). The daemon sends
// programs only, never full arguments, a worktree or a session (spec analytics-devtime §8).
import type { DevtimeToolFailures, ErrorClassFailures, ToolFailureRow } from "../data/devtime";
import type { ShareItem } from "../ui";

/** The daemon's `__other__` row: everything past its own top rows, folded into one. */
export const OTHER_TOOL = "__other__";

/** At most this many tool rows are drawn. */
export const TOP_TOOLS = 8;

/** English labels of every class token the contract can send. */
export const ERROR_CLASS_LABELS: Readonly<Record<string, string>> = {
  exit_nonzero: "Non-zero exit",
  exit_75: "Busy (exit 75)",
  timeout: "Timed out",
  permission_denied: "Permission denied",
  hook_block: "Blocked by a hook",
  edit_not_found: "Edit target not found",
  wrong_shell: "Wrong shell",
  tool_error: "Tool error",
  background_failed: "Background task failed",
  background_killed: "Background task killed",
};

/** The label of a class token; a token a newer daemon added comes back as is. */
export function errorClassLabel(token: string): string {
  return (Object.prototype.hasOwnProperty.call(ERROR_CLASS_LABELS, token) ? ERROR_CLASS_LABELS[token] : undefined) ?? token;
}

export function classShares(rows: ErrorClassFailures[]): ShareItem[] {
  return rows.map((row) => ({ key: row.error_class, label: errorClassLabel(row.error_class), value: row.failures }));
}

/** A tool row ready to draw: `key` is unique per (tool, program), `label` is the tool's text. */
export interface ToolFailureLine extends ToolFailureRow {
  key: string;
  label: string;
}

export function toolText(toolName: string): string {
  return toolName === OTHER_TOOL ? "Other" : toolName;
}

export function programText(program: string | null): string {
  return program ?? "—";
}

/** The first eight rows, in the daemon's order. */
export function topTools(rows: ToolFailureRow[]): ToolFailureLine[] {
  return rows.slice(0, TOP_TOOLS).map((row) => ({
    ...row,
    key: `${row.tool_name}\u0000${row.cmd_program ?? ""}`,
    label: toolText(row.tool_name),
  }));
}

/** Whether there is anything to show: a missing block (older daemon) or an all-zero one has nothing. */
export function hasToolFailures(block: DevtimeToolFailures | undefined): block is DevtimeToolFailures {
  return block !== undefined && (block.attempts > 0 || block.interrupted > 0 || block.unresolved > 0);
}
