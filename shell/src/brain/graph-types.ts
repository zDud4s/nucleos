import type { KnownLayer } from "../data/knowledge";

/**
 * The Brain graph as plain data: what `graph-model.ts` builds and `ForceGraph.tsx` draws.
 *
 * Node ids: `n:<id>` a note, `k:<id>` a knowledge row, `<kind>:<ref>` any other target
 * (`project:<id>`, `contact:<id>`, `mail:<id>`, `file:<ref>`). One thing is one node however
 * it is reached.
 */

export type GNodeKind = "note" | "knowledge" | "project" | "contact" | "mail" | "file";

/** Which way an item leans for the shared Estado filter; `unknown` is a status this shell has no word for. */
export type StateBucket = "in_force" | "proposed" | "out" | "unknown";

export interface GNode {
  id: string;
  kind: GNodeKind;
  /** The id inside its own kind: the note id, the knowledge id, the project id. */
  ref: string;
  label: string;
  /** Knowledge rows only. */
  layer?: KnownLayer;
  bucket: StateBucket;
  /** The target no longer resolves (deleted note, retained-away mail, unknown knowledge id). */
  missing: boolean;
  /** Edges touching this node after filtering. */
  degree: number;
}

/** A note link carries its `link_type`; `supersedes_k` is knowledge → the row it replaced; `scope` is knowledge → its project. */
export type GEdgeType = "relates" | "supports" | "contradicts" | "details" | "supersedes" | "supersedes_k" | "scope";

export interface GEdge {
  id: string;
  source: string;
  target: string;
  type: GEdgeType;
}

export interface GModel {
  nodes: GNode[];
  edges: GEdge[];
}

export interface GFilters {
  /** `in_force` also keeps `proposed` rows in the graph (drawn hollow). */
  state: "in_force" | "out" | "all";
  nodeKinds: Set<GNodeKind>;
  edgeTypes: Set<GEdgeType>;
  showArchived: boolean;
  showOrphans: boolean;
}
