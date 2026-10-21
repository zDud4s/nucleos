import type { FleetEvent, FleetNode, NodeMetrics, TermKind, TermLine } from "./types";

const T0 = Date.parse("2026-10-20T10:00:00Z");
/** An RFC 3339 timestamp `min` minutes after a fixed origin. */
export const at = (min: number): string => new Date(T0 + min * 60_000).toISOString();

export function node(id: string, over: Partial<FleetNode> = {}): FleetNode {
  return {
    id,
    parent_id: null,
    root_id: id,
    depth: 0,
    harness: "claude-code",
    model: "sonnet-5.5",
    role: null,
    label: id,
    state: "busy",
    started_at: at(0),
    ended_at: null,
    pinned: false,
    done_omitted: 0,
    project: "nucleos",
    run_id: null,
    capabilities: { stop: false, open_run: false, full_transcript: false, pin: false },
    ...over,
  };
}

/** A child of `parent`: same root and project, one level deeper. */
export function child(id: string, parent: FleetNode, over: Partial<FleetNode> = {}): FleetNode {
  return node(id, {
    parent_id: parent.id,
    root_id: parent.root_id,
    depth: parent.depth + 1,
    project: parent.project,
    ...over,
  });
}

export const line = (kind: TermKind, text: string): TermLine => ({ kind, text });

export function metricsOf(node_id: string, over: Partial<NodeMetrics> = {}): NodeMetrics {
  return { node_id, last_activity_at: at(1), context_tokens: 96_000, cost_usd: 0.71, ...over };
}

export function snapshot(
  nodes: FleetNode[],
  lines: Record<string, TermLine[]> = {},
  metrics: NodeMetrics[] = [],
): Extract<FleetEvent, { t: "snapshot" }> {
  return { t: "snapshot", nodes, metrics, lines };
}

/** An NDJSON body from events, `\n`-terminated like the daemon sends it. */
export function ndjson(...events: FleetEvent[]): string {
  return events.map((event) => JSON.stringify(event)).join("\n") + "\n";
}
