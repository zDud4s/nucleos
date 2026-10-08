import type { GEdgeType, GFilters, GNodeKind } from "./graph-types";
import { Button } from "../ui";

/**
 * The bar above the Brain graph: Estado, which kinds of node, which kinds of link, archived notes
 * and orphans. A bar, never laid over the nodes.
 */

export const NODE_KINDS: readonly GNodeKind[] = ["note", "knowledge", "project", "contact", "mail", "file"];

export const EDGE_TYPES: readonly GEdgeType[] = [
  "relates",
  "supports",
  "contradicts",
  "details",
  "supersedes",
  "supersedes_k",
  "scope",
];

/** The two edge types that are not a note's `link_type` get words of their own. */
const EDGE_LABELS: Partial<Record<GEdgeType, string>> = {
  supersedes_k: "replaces (knowledge)",
  scope: "project scope",
};

const STATES: readonly (readonly [GFilters["state"], string])[] = [
  ["in_force", "In force"],
  ["out", "Out"],
  ["all", "All"],
];

/** In force, every kind, every link type, archived and orphans off (spec §3.3). Fresh sets each call. */
export function defaultGraphFilters(): GFilters {
  return {
    state: "in_force",
    nodeKinds: new Set(NODE_KINDS),
    edgeTypes: new Set(EDGE_TYPES),
    showArchived: false,
    showOrphans: false,
  };
}

function toggled<T>(set: Set<T>, value: T): Set<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

export function GraphFilters({
  filters,
  onFilters,
}: {
  filters: GFilters;
  onFilters: (next: GFilters) => void;
}) {
  return (
    <div className="brain-bar" data-testid="brain-bar">
      <div role="group" aria-label="Graph state">
        {STATES.map(([value, label]) => (
          <Button
            key={value}
            variant="quiet"
            aria-pressed={filters.state === value}
            onClick={() => onFilters({ ...filters, state: value })}
          >
            {label}
          </Button>
        ))}
      </div>
      <div role="group" aria-label="Node kinds">
        {NODE_KINDS.map((kind) => (
          <Button
            key={kind}
            variant="quiet"
            aria-pressed={filters.nodeKinds.has(kind)}
            onClick={() => onFilters({ ...filters, nodeKinds: toggled(filters.nodeKinds, kind) })}
          >
            {kind}
          </Button>
        ))}
      </div>
      <div role="group" aria-label="Link types">
        {EDGE_TYPES.map((type) => (
          <Button
            key={type}
            variant="quiet"
            aria-pressed={filters.edgeTypes.has(type)}
            onClick={() => onFilters({ ...filters, edgeTypes: toggled(filters.edgeTypes, type) })}
          >
            {EDGE_LABELS[type] ?? type}
          </Button>
        ))}
      </div>
      <Button
        variant="quiet"
        aria-pressed={filters.showArchived}
        onClick={() => onFilters({ ...filters, showArchived: !filters.showArchived })}
      >
        Show archived
      </Button>
      <Button
        variant="quiet"
        aria-pressed={filters.showOrphans}
        onClick={() => onFilters({ ...filters, showOrphans: !filters.showOrphans })}
      >
        Show orphans
      </Button>
    </div>
  );
}
