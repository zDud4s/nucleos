import { BaseEdge, getBezierPath, type Edge, type EdgeProps } from "@xyflow/react";
import { useNodeEntry } from "./hooks";
import { useFleetUi } from "./ui-context";

export type FlowSpawn = Edge<{ childId: string }, "spawn">;

/**
 * A curve from parent to child: dashed and animated while the child is busy, solid and quiet
 * after. The child's state is read here, from the store, so a state-only change redraws this edge
 * without touching the layout. An edge between harnesses looks like any other (spec §6).
 */
export function SpawnEdge({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, data }: EdgeProps<FlowSpawn>) {
  const ui = useFleetUi();
  const childId = data?.childId ?? null;
  const busy = useNodeEntry(childId)?.node.state === "busy";
  const [path] = getBezierPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });
  const classes = ["fleet-edge", busy ? "fleet-edge-busy" : "", childId !== null && ui.path.has(childId) ? "fleet-edge-path" : ""]
    .filter(Boolean)
    .join(" ");
  return <BaseEdge id={id} path={path} className={classes} />;
}
