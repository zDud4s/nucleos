import type { NodeState } from "./types";

export const STATE_TEXT: Record<NodeState, string> = {
  busy: "running",
  idle: "waiting for you",
  done: "done",
  lost: "ended without closing",
};

export function elapsed(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

/** Context size the way the footer says it: `96k`. */
export function tokens(n: number | null): string {
  if (n === null) return "—";
  return n >= 1000 ? `${Math.round(n / 1000)}k` : String(n);
}
