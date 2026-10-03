import { useEffect, useMemo, useRef } from "react";
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  MiniMap,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useNodesInitialized,
  useReactFlow,
  type Edge,
  type FitViewOptions,
  type Node,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN: the Tauri window's CSP is `default-src 'self'`.
import "@xyflow/react/dist/style.css";
import { LINK_TYPES, TARGET_KINDS } from "../data/owner-notes";
import type { BrainFilters, BrainMeta } from "../data/brain-graph";
import { Button } from "../ui";

/**
 * The map of the owner's notes: notes, the knowledge they were taught into, and whatever else
 * they point at. Selection is the page's business — a click only reports the node id.
 */

/** The same floor the fleet canvas uses: `fitView` alone shrinks a busy graph into illegibility. */
const ZOOM_FLOOR = 0.8;
const FIRST_VIEW: FitViewOptions = { minZoom: ZOOM_FLOOR, maxZoom: 1, padding: 0.08 };

/** Link types that read as a direction, so they get an arrow. */
const DIRECTED = new Set(["supersedes", "details"]);

function Handles() {
  return (
    <>
      <Handle type="target" position={Position.Left} isConnectable={false} />
      <Handle type="source" position={Position.Right} isConnectable={false} />
    </>
  );
}

function NoteNode({ data, selected }: NodeProps) {
  const note = data.note as { text: string; state: string } | null;
  const missing = data.missing === true;
  return (
    <div
      className={`brain-node brain-node-note${selected ? " brain-node-selected" : ""}${
        missing ? " brain-node-missing" : ""
      }`}
    >
      <Handles />
      {note === null ? (
        missing ? (
          <span>note — gone</span>
        ) : (
          <>
            <p className="brain-node-text">{typeof data.label === "string" ? data.label : "note"}</p>
            <span className="brain-node-tag">archived</span>
          </>
        )
      ) : (
        <>
          <p className="brain-node-text">{note.text}</p>
          {note.state === "archived" && <span className="brain-node-tag">archived</span>}
        </>
      )}
    </div>
  );
}

function KnowledgeNode({ data, selected }: NodeProps) {
  const known = data.known as { title: string; status: string } | null;
  const missing = data.missing === true;
  return (
    <div
      className={`brain-node brain-node-knowledge${selected ? " brain-node-selected" : ""}${
        missing ? " brain-node-missing" : ""
      }`}
    >
      <Handles />
      <span>{known === null ? "knowledge — gone" : known.title}</span>
    </div>
  );
}

function EntityNode({ data, selected }: NodeProps) {
  const missing = data.missing === true;
  return (
    <div
      className={`brain-node brain-node-entity${selected ? " brain-node-selected" : ""}${
        missing ? " brain-node-missing" : ""
      }`}
    >
      <Handles />
      <span>
        {String(data.kind)}: {String(data.label)}
      </span>
      {missing && <span className="brain-node-tag">gone</span>}
    </div>
  );
}

// Module scope: xyflow compares `nodeTypes` by reference and rebuilds every node when it changes.
const nodeTypes: NodeTypes = { note: NoteNode, knowledge: KnowledgeNode, entity: EntityNode };
export const BRAIN_NODE_TYPES = nodeTypes;

function toggled(set: Set<string>, value: string): Set<string> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

const KNOWLEDGE_MODES: readonly (readonly ["linked" | "all" | "none", string])[] = [
  ["linked", "Linked"],
  ["all", "All"],
  ["none", "None"],
];

/** Filters and legend: a bar above the canvas, never laid over the nodes. */
function BrainBar({
  filters,
  onFilters,
  meta,
}: {
  filters: BrainFilters;
  onFilters: (next: BrainFilters) => void;
  meta: BrainMeta;
}) {
  return (
    <div className="brain-bar" data-testid="brain-bar">
      <div role="group" aria-label="Knowledge">
        {KNOWLEDGE_MODES.map(([value, label]) => (
          <Button
            key={value}
            variant="quiet"
            aria-pressed={filters.knowledge === value}
            onClick={() => onFilters({ ...filters, knowledge: value })}
          >
            Knowledge: {label}
          </Button>
        ))}
      </div>
      <div role="group" aria-label="Link types">
        {LINK_TYPES.map((type) => (
          <Button
            key={type}
            variant="quiet"
            aria-pressed={filters.linkTypes.has(type)}
            onClick={() => onFilters({ ...filters, linkTypes: toggled(filters.linkTypes, type) })}
          >
            {type}
          </Button>
        ))}
      </div>
      <div role="group" aria-label="Target kinds">
        {TARGET_KINDS.map((kind) => (
          <Button
            key={kind}
            variant="quiet"
            aria-pressed={filters.kinds.has(kind)}
            onClick={() => onFilters({ ...filters, kinds: toggled(filters.kinds, kind) })}
          >
            {kind}
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
      <ul className="brain-legend" aria-label="Legend">
        <li>{meta.notes} notes</li>
        <li>{meta.knowledge} knowledge</li>
        <li>{meta.entities} other</li>
        <li>{meta.edges} links</li>
        <li>arrow: supersedes, details</li>
      </ul>
    </div>
  );
}

export interface BrainCanvasProps {
  nodes: Node[];
  edges: Edge[];
  filters: BrainFilters;
  onFilters: (next: BrainFilters) => void;
  meta: BrainMeta;
  selectedId: string | null;
  onSelect: (nodeId: string) => void;
}

function Inner({ nodes, edges, selectedId, onSelect }: BrainCanvasProps) {
  const shownNodes = useMemo(
    () => nodes.map((node) => ({ ...node, selected: node.id === selectedId })),
    [nodes, selectedId],
  );
  const shownEdges = useMemo(
    () =>
      edges.map((edge) =>
        DIRECTED.has(String(edge.data?.linkType))
          ? { ...edge, markerEnd: { type: MarkerType.ArrowClosed } }
          : edge,
      ),
    [edges],
  );

  // Framed once, here, and not through the `fitView` prop: that one has no floor of its own.
  const measured = useNodesInitialized();
  const flow = useReactFlow();
  const framed = useRef(false);
  useEffect(() => {
    if (!measured || framed.current) return;
    framed.current = true;
    void flow.fitView(FIRST_VIEW);
  }, [measured, flow]);

  return (
    <ReactFlow
      nodes={shownNodes}
      edges={shownEdges}
      nodeTypes={nodeTypes}
      nodesConnectable={false}
      nodesDraggable={false}
      deleteKeyCode={null}
      proOptions={{ hideAttribution: true }}
      fitViewOptions={FIRST_VIEW}
      onNodeClick={(_event, node) => onSelect(node.id)}
    >
      <Background />
      <Controls showInteractive={false} />
      <MiniMap pannable zoomable />
    </ReactFlow>
  );
}

export function BrainCanvas(props: BrainCanvasProps) {
  return (
    <div className="brain-canvas-wrap">
      <BrainBar filters={props.filters} onFilters={props.onFilters} meta={props.meta} />
      <div className="brain-canvas" data-testid="brain-canvas">
        <ReactFlowProvider>
          <Inner {...props} />
        </ReactFlowProvider>
      </div>
    </div>
  );
}
