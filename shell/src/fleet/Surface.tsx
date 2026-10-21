import { useEffect, useMemo, useRef } from "react";
import {
  Background,
  Controls,
  ReactFlow,
  ReactFlowProvider,
  ViewportPortal,
  useNodesInitialized,
  useReactFlow,
  useStore,
  type Edge,
  type EdgeTypes,
  Position,
  type FitViewOptions,
  type Node,
  type NodeHandle,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN: the window's CSP is `default-src 'self'`.
import "@xyflow/react/dist/style.css";
import { FleetNode } from "./FleetNode";
import { GroupNode } from "./GroupNode";
import { SpawnEdge } from "./SpawnEdge";
import { useActiveNode, useFleetStore, useLayoutVersion } from "./hooks";
import { layoutForest } from "./layout";
import { useFleetUi } from "./ui-context";
import { selectCard } from "./zoom";

/**
 * Module scope: xyflow compares these by reference and rebuilds every node when they change, so
 * a literal inside the component would remount the canvas on every render.
 */
const nodeTypes: NodeTypes = { agentTerm: FleetNode, doneGroup: GroupNode };
const edgeTypes: EdgeTypes = { spawn: SpawnEdge };
export const FLEET_NODE_TYPES = nodeTypes;
export const FLEET_EDGE_TYPES = edgeTypes;

const FIT: FitViewOptions = { padding: 0.12, maxZoom: 1 };
/** The layout is the only author of nodes, so xyflow's own changes (measure, select) are dropped. */
const noop = (): void => undefined;

/**
 * Where a spawn edge meets a node: top centre in, bottom centre out, from the fixed node size.
 * Declared rather than measured (xyflow reads `handles` before it looks at the DOM), so an edge
 * never waits for a ResizeObserver and a re-layout cannot lose it.
 */
function handlesOf(w: number, h: number, group: boolean): NodeHandle[] {
  const into: NodeHandle = { type: "target", position: Position.Top, x: w / 2 - 0.5, y: 0, width: 1, height: 1 };
  const out: NodeHandle = { type: "source", position: Position.Bottom, x: w / 2 - 0.5, y: h - 1, width: 1, height: 1 };
  return group ? [into] : [into, out];
}

export interface FleetCanvasProps {
  project: string | null;
  onlyActive: boolean;
  expanded: ReadonlySet<string>;
  follow: boolean;
  /** Bumped by the Fit button; 0 is "never pressed". */
  fitSignal: number;
}

export function FleetCanvas(props: FleetCanvasProps) {
  return (
    <ReactFlowProvider>
      <div className="fleet-canvas">
        <Surface {...props} />
      </div>
    </ReactFlowProvider>
  );
}

/**
 * Pan and zoom only: no drag, no connect, no xyflow selection (the page owns `selected`).
 *
 * The layout is recomputed only when the store's structure version moves, or the card/terminal
 * size, the filters or an open group change. The viewport never moves by itself: the first open
 * frames what is alive once, and Follow (off by default) centers the node that spoke last.
 */
function Surface({ project, onlyActive, expanded, follow, fitSignal }: FleetCanvasProps) {
  const store = useFleetStore();
  const ui = useFleetUi();
  const version = useLayoutVersion();
  const card = useStore(selectCard);
  const flow = useReactFlow();
  const viewportReady = flow.viewportInitialized;
  const initialized = useNodesInitialized();
  const active = useActiveNode();

  const layout = useMemo(
    () => layoutForest(store.nodes(), { size: card ? "card" : "terminal", onlyActive, project, expanded }),
    // `version` is the dependency that matters: the store is mutable and structure is the only
    // thing allowed to re-run the layout.
    [store, version, card, onlyActive, project, expanded],
  );

  const nodes = useMemo<Node[]>(
    () =>
      layout.nodes.map((n) => ({
        id: n.id,
        type: n.kind === "group" ? "doneGroup" : "agentTerm",
        position: { x: n.x, y: n.y },
        // Fixed sizes. `measured` is what xyflow's initialized check reads and what it copies
        // forward when this array is rebuilt; without it every re-layout would un-initialize the
        // canvas (hidden nodes, no edges) until the next measurement.
        width: n.w,
        height: n.h,
        measured: { width: n.w, height: n.h },
        handles: handlesOf(n.w, n.h, n.kind === "group"),
        data: n.kind === "group" ? { group: n.group } : { id: n.id },
        draggable: false,
        selectable: false,
      })),
    [layout],
  );
  const edges = useMemo<Edge[]>(
    () =>
      layout.edges.map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        type: "spawn",
        data: { childId: e.target },
        selectable: false,
        focusable: false,
      })),
    [layout],
  );

  const placed = useRef(layout.nodes);
  useEffect(() => {
    placed.current = layout.nodes;
  }, [layout]);

  const framed = useRef(false);
  useEffect(() => {
    // `fitView` is a no-op until the viewport exists, so wait for it as well as for the nodes.
    if (!initialized || !viewportReady || framed.current || layout.nodes.length === 0) return;
    framed.current = true;
    const live = layout.nodes
      .filter((n) => {
        const state = store.getEntry(n.id)?.node.state;
        return state === "busy" || state === "idle";
      })
      .map((n) => ({ id: n.id }));
    void flow.fitView({ ...FIT, nodes: live.length > 0 ? live : undefined, duration: 0 });
  }, [initialized, viewportReady, layout, store, flow]);

  // Only a press that happens while this canvas is mounted: a canvas that remounts with a signal
  // already counted (the page keeps it across an empty state) must not Fit on its own.
  const fitHandled = useRef(fitSignal);
  useEffect(() => {
    if (fitSignal === fitHandled.current) return;
    fitHandled.current = fitSignal;
    void flow.fitView({ ...FIT, duration: 200 });
  }, [fitSignal, flow]);

  useEffect(() => {
    if (!follow || active === null) return;
    const at = placed.current.find((n) => n.id === active);
    if (at === undefined) return;
    void flow.setCenter(at.x + at.w / 2, at.y + at.h / 2, { zoom: flow.getZoom(), duration: 200 });
  }, [follow, active, flow]);

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      minZoom={0.05}
      maxZoom={1.5}
      nodesDraggable={false}
      nodesConnectable={false}
      elementsSelectable={false}
      nodesFocusable={false}
      edgesFocusable={false}
      zoomOnDoubleClick={false}
      deleteKeyCode={null}
      onNodesChange={noop}
      onPaneClick={() => ui.select(null)}
      fitViewOptions={FIT}
    >
      {/* Each project's band, in flow coordinates so it pans and zooms with its trees. Hidden from
          the accessibility tree: every node already says what it is. */}
      <ViewportPortal>
        {layout.bands.map((band) => (
          <div
            key={band.project}
            className="fleet-zone"
            aria-hidden="true"
            style={{ transform: `translate(${band.x}px, ${band.y}px)`, width: band.w, height: band.h }}
          >
            <span className="fleet-zone-label">{band.project}</span>
          </div>
        ))}
      </ViewportPortal>
      <Background />
      <Controls showInteractive={false} />
    </ReactFlow>
  );
}
