/**
 * The fleet wire format, mirrored from `.ai/plans/2026-10-20-fleet-arvore-viva/contract.md` §4
 * (serde snake_case on the daemon side). Hand-written like every other `data/*.ts` shape.
 */
export type NodeState = "busy" | "idle" | "done" | "lost";
export type TermKind = "prompt" | "text" | "tool" | "result" | "error" | "status" | "final";
export interface TermLine { kind: TermKind; text: string }
export interface Capabilities { stop: boolean; open_run: boolean; full_transcript: boolean; pin: boolean }
export interface FleetNode {
  id: string;
  parent_id: string | null;
  root_id: string;
  depth: number;
  harness: string;
  model: string | null;
  role: string | null;
  label: string;
  state: NodeState;
  started_at: string;
  ended_at: string | null;
  pinned: boolean;
  done_omitted: number;
  project: string | null;
  run_id: number | null;
  capabilities: Capabilities;
}
export interface NodeMetrics {
  node_id: string;
  last_activity_at: string;
  context_tokens: number | null;
  cost_usd: number | null;
}
export type FleetEvent =
  | { t: "snapshot"; nodes: FleetNode[]; metrics: NodeMetrics[]; lines: Record<string, TermLine[]> }
  | { t: "node"; node: FleetNode }
  | { t: "metrics"; metrics: NodeMetrics[] }
  | { t: "lines"; node_id: string; lines: TermLine[] }
  | { t: "removed"; node_id: string }
  | { t: "heartbeat" };
export interface TranscriptEntry { kind: TermKind; text: string; detail: string | null }
export interface TranscriptPage { entries: TranscriptEntry[]; before: string | null }

/** What the top bar shows: `● live`, `◌ reconnecting…`; `connecting` until the first snapshot. */
export type ConnectionState = "connecting" | "live" | "reconnecting";
