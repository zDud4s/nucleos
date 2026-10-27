import type { GModel, GNodeKind } from "./graph-types";
import { KIND_LABELS, NODE_KINDS } from "./GraphFilters";

/** The colour key over the canvas: only the kinds the graph is drawing right now. */
export function GraphLegend({ model }: { model: GModel }) {
  const present = new Set<GNodeKind>(model.nodes.map((n) => n.kind));
  const kinds = NODE_KINDS.filter((kind) => present.has(kind));
  if (kinds.length === 0) return null;
  return (
    <ul className="brain-legend" aria-label="Node colours">
      {kinds.map((kind) => (
        <li key={kind}>
          <span className="brain-legend-swatch" data-kind={kind} aria-hidden="true" />
          {KIND_LABELS[kind]}
        </li>
      ))}
    </ul>
  );
}
