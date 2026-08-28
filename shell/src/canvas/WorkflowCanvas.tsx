// §spec motor-de-workflows
import { motion } from "motion/react";
import {
  BaseEdge,
  EdgeLabelRenderer,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getSmoothStepPath,
  type EdgeProps,
  type EdgeTypes,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN: the Tauri window's CSP is `default-src 'self'`, so a stylesheet
// fetched from anywhere else is a blank canvas in the shipped app and a working one in the dev
// server — the worst pair of outcomes.
import "@xyflow/react/dist/style.css";
import type { GraphEdge, GraphNode } from "../data/workflow-graph";
import {
  buildWorkflow,
  nodeMeaning,
  nodeTone,
  type WorkflowEdgeData,
  type WorkflowFlowEdge,
  type WorkflowFlowNode,
  type WorkflowNodeData,
} from "./workflow-model";

/**
 * A workflow as a picture, with this project's overlay painted on it.
 *
 * §6.4's vocabulary, drawn so that the two questions anybody asks of a workflow are answered before
 * a word is read: **violet spends tokens, amber has a verdict.** A gate is not a fifth shape — it
 * is a command something branches on, drawn in amber because that is where the pipeline stops.
 *
 * **§6.2: the overlay is painted, never applied.** A node this project overrode carries a seal and
 * the inspector shows what the origin said beside it; a node switched off here stays in the graph,
 * dotted, with its edges. Hiding it would make the picture lie about what the workflow is — which
 * is the one thing a picture is for.
 *
 * **The same surface is the editor and the live view.** That is what makes it cheap to justify: a
 * node lights with the same pulse the miniature uses, so there is not a second canvas for watching.
 * Nothing feeds `running` yet — which node a live run is on is execution semantics, and §14 keeps
 * those — so the mechanism is built, tested, and honest about being unfed.
 */

/**
 * **Module scope, and this is the lesson `FleetCanvas` paid for.**
 *
 * xyflow compares `nodeTypes` and `edgeTypes` by reference and rebuilds every node when either
 * changes. Declared inside the component they would be new objects on every render — a full remount
 * of the canvas on every state change, and with the React Compiler memoising around them the
 * failure would be intermittent rather than constant, which is worse.
 *
 * One node type and not four. The four kinds differ in border, shape and tone, which is a `switch`
 * inside one component; four registered types would be four near-identical files and four places
 * for the seal or the dotted border to be forgotten.
 */
const nodeTypes: NodeTypes = { workflowNode: WorkflowNode };
const edgeTypes: EdgeTypes = { workflowEdge: WorkflowEdgeLine };

export const WORKFLOW_NODE_TYPES = nodeTypes;
export const WORKFLOW_EDGE_TYPES = edgeTypes;

/* ------------------------------------------------------------------ node -- */

function WorkflowNode({ data }: NodeProps<WorkflowFlowNode>) {
  const { node, running } = data as WorkflowNodeData;
  const tone = nodeTone(node.type, node.role);

  return (
    <>
      <Handle type="target" position={Position.Left} isConnectable={false} />
      {/*
        `motion.div` for one thing only: the short spring on the node that is running. §11 allows
        motion in three places and calls none of them decorative — this is the second, and its
        argument is that a change that happened while somebody was looking elsewhere is otherwise
        indistinguishable from one that was always there.

        It animates through the Web Animations API rather than by writing a `style=` attribute into
        markup, which is the distinction §11 asked to confirm against the Tauri CSP — and it is
        confirmed by measurement, not by argument. Under the production policy in `tauri.conf.json`
        (`style-src 'self'`, no `unsafe-inline`) served as a header over the built bundle, all three
        of §11's uses ran — a layout transition, a number interpolating, a colour spring — with a
        WAAPI animation live at the sample point and not one refusal. A `<style>` element and a
        `style` attribute injected straight afterwards were both refused, so the policy was in force
        throughout rather than merely absent.

        The trap that made measuring worth it, recorded because it will catch the next person:
        `tauri dev` applies `devCsp`, and `devCsp` carries `style-src 'unsafe-inline'` because Vite
        injects CSS through a `<style>` element it builds at runtime. **Development cannot catch a
        CSP style regression** — only a packaged build can, or a run of the bundle under the real
        header. If one ever appears, the fallback is one keyframe in `ui.css` and this import.
      */}
      <motion.div
        animate={running ? { scale: [1, 1.03, 1] } : { scale: 1 }}
        transition={running ? { duration: 1.6, repeat: Infinity, ease: "easeInOut" } : { duration: 0.2 }}
        aria-label={`${node.label}, ${nodeMeaning(node.type, node.role)}`}
        className={[
          "flex w-[200px] flex-col gap-1 bg-surface px-3 py-2 text-left",
          // Rounded for an agent, square for a command, and a hexagon is beyond a border radius —
          // a decision gets the sharpest corners plus its own colour, which reads as distinct
          // without an SVG clip that would fight the handles.
          node.type === "agent" ? "rounded-xl" : "rounded-sm",
          // A node switched off in this project is dotted and faded — present, and plainly not
          // taking part. §12: this is not the same as a node the bundle does not have, which is not
          // drawn at all.
          node.disabled ? "border-2 border-dotted opacity-50" : "border-2",
          // A fan carries a stacked shadow instead of a fifth colour: its question is how many at
          // once, which is neither of the two the colours answer.
          node.type === "fan" ? "border-dashed shadow-[6px_6px_0_-2px_var(--surface),8px_8px_0_-2px_var(--border)]" : "",
          running ? "shadow-float" : "",
        ].join(" ")}
        style={{ borderColor: `var(--tone-${tone}-border)` }}
      >
        <div className="flex items-baseline gap-2">
          <span
            className={`truncate text-sm ${node.type === "command" || node.role === "gate" ? "font-mono" : "font-display"} text-text`}
          >
            {node.label}
          </span>
          {running ? (
            <span
              aria-label="running"
              className="ml-auto h-1.5 w-1.5 shrink-0 rounded-pill bg-tone-active-fg"
            />
          ) : null}
        </div>
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] uppercase tracking-wide" style={{ color: `var(--tone-${tone}-fg)` }}>
            {node.role === "gate" ? "gate" : node.type}
          </span>
          {/*
            The seal §6.2 makes mandatory. It is on the node and not only in the inspector, because
            the question "what has this project changed" is asked of the whole picture at once.
          */}
          {node.overridden ? (
            <span className="rounded-pill border border-border px-1.5 text-[10px] text-text-muted">
              project
            </span>
          ) : null}
          {node.disabled ? <span className="text-[10px] text-text-faint">off here</span> : null}
        </div>
      </motion.div>
      <Handle type="source" position={Position.Right} isConnectable={false} />
    </>
  );
}

/* ------------------------------------------------------------------ edge -- */

/**
 * A line, with its condition on it.
 *
 * A verdict edge is amber and says `pass` or `fail`; a conditional edge carries its condition
 * verbatim. Both are labelled rather than only coloured, because a workflow read six months later
 * is read by somebody who does not remember what the colours meant.
 *
 * Smooth-step rather than the fleet's bezier: this is a sequence laid out in columns, and
 * right-angle turns say "then" in a way a curve between scattered cards does not.
 */
function WorkflowEdgeLine({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
}: EdgeProps<WorkflowFlowEdge>) {
  const [path, labelX, labelY] = getSmoothStepPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
  });
  const { edge, dimmed } = (data ?? { edge: { from: "", to: "" }, dimmed: false }) as WorkflowEdgeData;
  const tone = edge.verdict === undefined ? "off" : "pending";
  const label = edge.verdict ?? edge.when;

  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        style={{
          stroke: `var(--tone-${tone}-border)`,
          strokeWidth: 1.5,
          // An edge with a switched-off end is a path nothing takes here, and it fades with the
          // node rather than disappearing — the same reason the node stays.
          opacity: dimmed ? 0.35 : 1,
          strokeDasharray: dimmed ? "3 3" : undefined,
        }}
      />
      {label === undefined ? null : (
        <EdgeLabelRenderer>
          <span
            style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
            className="pointer-events-none absolute rounded-pill border border-border bg-surface px-1.5 py-0.5 text-[10px] text-text-muted"
          >
            {label}
          </span>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

/* ---------------------------------------------------------------- canvas -- */

export interface WorkflowCanvasProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** The node a run is on, when anything knows. Nothing does yet — see the module header. */
  running?: string | null;
  /** The node the inspector is open on. */
  selected: string | null;
  onSelect: (id: string | null) => void;
}

function WorkflowSurface({ nodes, edges, running = null, selected, onSelect }: WorkflowCanvasProps) {
  const model = buildWorkflow(nodes, edges, running);

  return (
    <div className="h-[420px] w-full overflow-hidden rounded-lg border border-border bg-surface-sunken">
      <ReactFlow
        nodes={model.nodes.map((node) => ({ ...node, selected: node.id === selected }))}
        edges={model.edges}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        onNodeClick={(_event, node) => onSelect(node.id === selected ? null : node.id)}
        onPaneClick={() => onSelect(null)}
        /*
          Not draggable, and this is the difference from the fleet canvas rather than an omission.
          A workflow is a sequence and the layout IS the sequence, so a node moved by hand would
          say something false about the graph. There is also nothing to connect: edges come from
          the bundle, and drawing one here would be editing a file this surface does not own.
        */
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable
        // Nothing here deletes anything, and a stray Backspace over the canvas must not look as if
        // it did: the bundle owns both lists.
        deleteKeyCode={null}
        proOptions={{ hideAttribution: false }}
        fitView
      />
    </div>
  );
}

export function WorkflowCanvas(props: WorkflowCanvasProps) {
  return (
    <ReactFlowProvider>
      <WorkflowSurface {...props} />
    </ReactFlowProvider>
  );
}
