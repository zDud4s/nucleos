import type { FleetEvent, FleetNode, NodeMetrics, TermLine } from "./types";

const KNOWN = new Set(["snapshot", "node", "metrics", "lines", "removed", "heartbeat"]);

/**
 * Cuts a byte stream into complete NDJSON lines.
 *
 * One decoder in streaming mode, so a character split across two network chunks is not turned into
 * U+FFFD; the unterminated tail is held until its `\n` arrives.
 */
export class NdjsonSplitter {
  private readonly decoder = new TextDecoder("utf-8");
  private rest = "";

  push(chunk: Uint8Array): string[] {
    this.rest += this.decoder.decode(chunk, { stream: true });
    const parts = this.rest.split("\n");
    this.rest = parts.pop() ?? "";
    return parts.map((part) => part.trim()).filter((part) => part !== "");
  }
}

type Obj = Record<string, unknown>;
const isObj = (value: unknown): value is Obj => typeof value === "object" && value !== null && !Array.isArray(value);
const isStr = (value: unknown): value is string => typeof value === "string";

/** A node with every field the store, the layout and the renderers dereference. */
function validNode(value: unknown): value is FleetNode {
  return (
    isObj(value) &&
    isStr(value.id) &&
    isStr(value.root_id) &&
    isStr(value.label) &&
    isStr(value.state) &&
    isStr(value.started_at) &&
    isObj(value.capabilities)
  );
}
const validMetrics = (value: unknown): value is NodeMetrics => isObj(value) && isStr(value.node_id);
const validLine = (value: unknown): value is TermLine => isObj(value) && isStr(value.kind) && isStr(value.text);

/**
 * The event if its shape is one the store can apply, else `null`. A schema-skewed daemon must
 * degrade to "that event is ignored", never to an exception inside the read loop or a render.
 * A snapshot loses its malformed nodes, metrics and lines; one without `nodes` is dropped.
 */
function checked(event: Obj): FleetEvent | null {
  switch (event.t) {
    case "snapshot": {
      if (!Array.isArray(event.nodes)) return null;
      const lines: Record<string, TermLine[]> = {};
      if (isObj(event.lines)) {
        for (const [id, list] of Object.entries(event.lines)) if (Array.isArray(list)) lines[id] = list.filter(validLine);
      }
      return {
        t: "snapshot",
        nodes: event.nodes.filter(validNode),
        metrics: Array.isArray(event.metrics) ? event.metrics.filter(validMetrics) : [],
        lines,
      };
    }
    case "node":
      return validNode(event.node) ? (event as unknown as FleetEvent) : null;
    case "metrics":
      return Array.isArray(event.metrics) ? { t: "metrics", metrics: event.metrics.filter(validMetrics) } : null;
    case "lines":
      return isStr(event.node_id) && Array.isArray(event.lines)
        ? { t: "lines", node_id: event.node_id, lines: event.lines.filter(validLine) }
        : null;
    case "removed":
      return isStr(event.node_id) ? (event as unknown as FleetEvent) : null;
    default:
      return { t: "heartbeat" };
  }
}

/** One line as an event, or `null` for anything this shell does not understand (forward compatible). */
export function parseEvent(line: string): FleetEvent | null {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    return null;
  }
  if (!isObj(value)) return null;
  const t = value.t;
  return isStr(t) && KNOWN.has(t) ? checked(value) : null;
}
