import { Handle, Position, type Node, type NodeProps } from "@xyflow/react";
import { useNodeLabel } from "./hooks";
import type { GroupInfo } from "./layout";
import { useFleetUi } from "./ui-context";

export type FlowGroup = Node<{ group: GroupInfo }, "doneGroup">;

/**
 * "✓ N finished": a parent with more than six done children folds them here. Expanding lists the
 * `done` nodes the snapshot actually carries; `omitted` ones are only a count (no route fetches
 * them — see spec §5).
 */
export function GroupNode({ data }: NodeProps<FlowGroup>) {
  const ui = useFleetUi();
  const { group } = data;
  const parentLabel = useNodeLabel(group.parentId) ?? "this node";
  return (
    <article className="fleet-node fleet-group" aria-label={`${group.count} finished`}>
      <Handle type="target" position={Position.Top} isConnectable={false} className="fleet-handle" />
      <span>✓ {group.count} finished</span>
      <button
        type="button"
        className="fleet-group-toggle nodrag nopan"
        aria-expanded={group.open}
        aria-label={`${group.open ? "Collapse" : "Expand"} the finished nodes under ${parentLabel}`}
        onClick={(event) => {
          event.stopPropagation();
          ui.toggleGroup(group.parentId);
        }}
      >
        {group.open ? "Collapse" : "Expand"}
      </button>
      {group.open && group.omitted > 0 && <span className="fleet-group-more">+{group.omitted} older not loaded</span>}
    </article>
  );
}
