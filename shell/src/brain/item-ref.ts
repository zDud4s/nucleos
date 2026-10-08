/**
 * The address of one Brain item, as it travels in `?item=`.
 *
 * Three kinds: a note, a knowledge row and a capture request (keyed by its job id). Graph entity nodes (projects,
 * files) are not items - they have no side panel.
 */
export type ItemRef = { kind: "note"; id: number } | { kind: "knowledge"; id: number }
  | { kind: "capture"; id: number };

export type BrainView = "list" | "graph";

const ITEM = /^(note|knowledge|capture):([1-9][0-9]*)$/;

/** `"note:45"` | `"knowledge:123"` -> a ref; anything else, including a non-positive id, -> null. */
export function parseItem(raw: unknown): ItemRef | null {
  if (typeof raw !== "string") return null;
  const match = ITEM.exec(raw);
  if (!match) return null;
  const id = Number(match[2]);
  if (!Number.isSafeInteger(id)) return null;
  return { kind: match[1] as ItemRef["kind"], id };
}

export function formatItem(ref: ItemRef): string {
  return `${ref.kind}:${ref.id}`;
}

/**
 * The graph node id of an item: `n:<id>` for a note, `k:<id>` for a knowledge row. A capture
 * request is no graph node, so it has none.
 */
export function nodeIdOf(ref: ItemRef): string | null {
  if (ref.kind === "capture") return null;
  return `${ref.kind === "note" ? "n" : "k"}:${ref.id}`;
}

/** Inverse of `nodeIdOf`; an entity node (or a malformed id) is not an item. */
export function itemOfNode(nodeId: string): ItemRef | null {
  const match = /^([nk]):([1-9][0-9]*)$/.exec(nodeId);
  if (!match) return null;
  const id = Number(match[2]);
  if (!Number.isSafeInteger(id)) return null;
  return { kind: match[1] === "n" ? "note" : "knowledge", id };
}

export function parseView(raw: unknown): BrainView {
  return raw === "graph" ? "graph" : "list";
}
