import type { GNode } from "./graph-types";

/**
 * The shape key of the Brain: one silhouette per kind of thing, shared by the list rows, the graph
 * legend and the canvas nodes. The shapes carry identity; colour does not — every mark is one
 * neutral tone, so the state tones (ember, amber, green) stay free to mean state only.
 *
 * Each path lives in a -1..1 box centred on the origin, so the SVG uses it as the viewBox and the
 * canvas scales it by the node radius with the same data.
 */
export type MarkKey =
  | "note"
  | "k-semantic"
  | "k-episodic"
  | "k-procedural"
  | "k-working"
  | "project"
  | "contact"
  | "mail"
  | "file"
  | "job";

export const MARK_PATHS: Record<MarkKey, string> = {
  note: "M0 -0.9A0.9 0.9 0 1 1 0 0.9A0.9 0.9 0 1 1 0 -0.9Z",
  "k-semantic": "M-0.8 -0.8H0.8V0.8H-0.8Z",
  "k-episodic": "M0 -1L1 0L0 1L-1 0Z",
  "k-procedural": "M0 -1L0.95 0.8H-0.95Z",
  "k-working": "M-0.95 -0.8H0.95L0 1Z",
  project: "M1 0L0.5 0.87L-0.5 0.87L-1 0L-0.5 -0.87L0.5 -0.87Z",
  contact: "M-0.35 -1H0.35V-0.35H1V0.35H0.35V1H-0.35V0.35H-1V-0.35H-0.35Z",
  mail: "M-1 -0.65H1V0.65H-1Z",
  file: "M-0.7 -1H0.3L0.8 -0.5V1H-0.7Z",
  // A pentagon, point up: the one regular shape no other kind takes.
  job: "M0 -1L0.95 -0.31L0.59 0.81H-0.59L-0.95 -0.31Z",
};

/** The mark a graph node takes: a knowledge row by its layer, anything else by its kind. */
export function markKey(node: Pick<GNode, "kind" | "layer">): MarkKey {
  if (node.kind === "knowledge") return `k-${node.layer ?? "semantic"}` as MarkKey;
  return node.kind;
}

/** The mark of a list row or legend chip. A capture has no shape: it stays the hollow "asking" dot. */
export function Mark({ hue }: { hue: string }) {
  const d = (MARK_PATHS as Record<string, string | undefined>)[hue];
  if (!d) return <span className={`unified-mark unified-mark-${hue}`} aria-hidden="true" />;
  return (
    <svg className="unified-mark unified-mark-shape" viewBox="-1.1 -1.1 2.2 2.2" aria-hidden="true">
      <path d={d} />
    </svg>
  );
}
