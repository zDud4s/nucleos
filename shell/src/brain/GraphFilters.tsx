import { Chip } from "../ui";
import type { GEdgeType, GFilters, GNodeKind } from "./graph-types";
import { Mark } from "./marks";
import { Segments } from "./Segments";

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

/** Node kinds in the same case as the list's Type segments; link types stay the daemon's words. */
const KIND_LABELS: Record<GNodeKind, string> = {
  note: "Note",
  knowledge: "Knowledge",
  project: "Project",
  contact: "Contact",
  mail: "Mail",
  file: "File",
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
      <div className="brain-bar-line">
        <Segments
          label="Graph state"
          value={filters.state}
          options={STATES}
          onChange={(state) => onFilters({ ...filters, state })}
        />
        <span className="brain-bar-end">
          <Chip
            pressed={filters.showArchived}
            onToggle={() => onFilters({ ...filters, showArchived: !filters.showArchived })}
          >
            Show archived
          </Chip>
          <Chip
            pressed={filters.showOrphans}
            onToggle={() => onFilters({ ...filters, showOrphans: !filters.showOrphans })}
          >
            Show orphans
          </Chip>
        </span>
      </div>
      <div className="brain-bar-line">
        <div role="group" aria-label="Node kinds" className="brain-chips">
          {NODE_KINDS.map((kind) => (
            <Chip
              key={kind}
              pressed={filters.nodeKinds.has(kind)}
              onToggle={() => onFilters({ ...filters, nodeKinds: toggled(filters.nodeKinds, kind) })}
            >
              <Mark hue={kind === "knowledge" ? "k-semantic" : kind} />
              {KIND_LABELS[kind]}
            </Chip>
          ))}
        </div>
      </div>
      <div className="brain-bar-line">
        <div role="group" aria-label="Link types" className="brain-chips brain-chips-link">
          {EDGE_TYPES.map((type) => (
            <Chip
              key={type}
              pressed={filters.edgeTypes.has(type)}
              onToggle={() => onFilters({ ...filters, edgeTypes: toggled(filters.edgeTypes, type) })}
            >
              {EDGE_LABELS[type] ?? type}
            </Chip>
          ))}
        </div>
      </div>
    </div>
  );
}
