// §spec motor-de-workflows
import { useRef } from "react";
import { motion, useReducedMotion } from "motion/react";
import { Bot, Layers, ShieldCheck, Split, Terminal, type LucideIcon } from "lucide-react";
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
  nodeShape,
  type NodeShape,
  type WorkflowEdgeData,
  type WorkflowFlowEdge,
  type WorkflowFlowNode,
  type WorkflowNodeData,
} from "./workflow-model";

/**
 * A workflow as a picture, with this project's overlay painted on it.
 *
 * §6.4's vocabulary, drawn so that the two questions anybody asks of a workflow are answered before
 * a word is read — **in shape, not in colour.** Every hue this system owns is a state, so the kinds
 * live on the neutral ladder: an agent is rounded, a command is square and mono, a gate is a command
 * with a heavier edge, a decision has a double edge and a fan a dashed one. Colour is left for the
 * one thing here that IS a state — the node a run is on — and `nodeShape` says why at length.
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
 * One node type and not four. The four kinds differ in edge, corner and face, which is a `switch`
 * inside one component; four registered types would be four near-identical files and four places
 * for the seal or the dotted border to be forgotten.
 */
const nodeTypes: NodeTypes = { workflowNode: WorkflowNode };
const edgeTypes: EdgeTypes = { workflowEdge: WorkflowEdgeLine };

export const WORKFLOW_NODE_TYPES = nodeTypes;
export const WORKFLOW_EDGE_TYPES = edgeTypes;

/* ------------------------------------------------------------------ node -- */

/**
 * One drawn icon per shape, from the icon set the rest of the shell uses rather than a row of
 * Unicode glyphs whose weight depends on whichever font happens to carry them.
 */
const SHAPE_ICON: Record<NodeShape, LucideIcon> = {
  agent: Bot,
  command: Terminal,
  gate: ShieldCheck,
  decision: Split,
  fan: Layers,
};

/** The icon for a shape. Exported so the key and the miniature draw the same mark as the node. */
export function ShapeIcon({ shape }: { shape: NodeShape }) {
  const Icon = SHAPE_ICON[shape];
  return <Icon aria-hidden="true" className="size-3 shrink-0" strokeWidth={1.75} />;
}

/**
 * The edge each shape is drawn with. Weight and pattern, never hue — see the module header.
 *
 * A gate is one rung heavier and one rung brighter than a plain command, because it is the node
 * whose outcome decides where the path goes. A decision's double edge is the stand-in for the
 * diamond a border radius cannot draw. A node switched off here is dotted whatever it is: that is the
 * overlay speaking, and it outranks the kind.
 */
function edgeOf(shape: NodeShape, disabled: boolean, running: boolean) {
  return {
    borderColor: running
      ? "var(--tone-active-border)"
      : shape === "gate"
        ? "var(--text-faint)"
        : "var(--border-strong)",
    borderStyle: disabled
      ? "dotted"
      : shape === "fan"
        ? "dashed"
        : shape === "decision"
          ? "double"
          : "solid",
    borderWidth: shape === "decision" ? 3 : shape === "gate" ? 2 : 1,
  } as const;
}

function WorkflowNode({ data, selected }: NodeProps<WorkflowFlowNode>) {
  const { node, running } = data as WorkflowNodeData;
  const shape = nodeShape(node.type, node.role);
  // The pulse repeats forever, and motion animates through the Web Animations API — which the
  // global `prefers-reduced-motion` clamp in `base.css` does not reach. So the component asks
  // itself. The still alternative is not a frozen pulse: the dot and the green edge stay, and both
  // say "running" without moving.
  const still = useReducedMotion() === true;

  /**
   * One `box-shadow` and two claimants, resolved here instead of by whichever utility Tailwind
   * happens to emit last. `shadow-*` utilities all write the same property, so a selected fan would
   * otherwise have shown one of its two marks and silently dropped the other.
   *
   * **The selected node had no mark at all, and finding that is the whole of this pass here.**
   * xyflow's own stylesheet paints `--xy-node-boxshadow-selected` on `.react-flow__node-default`,
   * `-input`, `-output` and `-group` — the four built-in types, and nothing else. Every node on this
   * surface is the custom `workflowNode`, so the rule never matched and clicking one changed only
   * the inspector beside the canvas. Pointing the library variable at a token would not have fixed
   * it either, which is worth knowing before somebody tries.
   *
   * So the mark is drawn on the card, and it is `.ui-current`'s own recipe written out:
   * `inset 2px 0 0 var(--text)`, the 2px inset rule on the leading edge in the top rung of the
   * neutral ladder. The class itself cannot be used because it sets `box-shadow` outright and would
   * take the fan's stack with it; the *geometry* fits here where it did not on the fleet's canvas,
   * because this card is the thing being marked rather than a wrapper around an opaque one, and an
   * inset shadow is clipped to the padding box, so it lands just inboard of the node's edge rather
   * than under it.
   *
   * `shadow-float` on a running node is gone. DESIGN.md records `--shadow-md` as defined and
   * applied to nothing, to be treated as unused rather than as an available middle tier, and this
   * was the line making that false: a lift expressing state, in a system whose depth is a rung.
   * Nothing is lost — running already says so three ways, in the spring, in the dot and its green
   * edge, and in the node's accessible name.
   *
   * `aria-current` below is the other half of the same finding: a mark that exists only as a shadow
   * is a mark for whoever can see it, and the inspector it opens is a separate region of the page.
   */
  const shadow = [
    // A fan carries a stacked shadow behind its dashed edge: its question is how many at once, and
    // a stack is what "many" looks like without a word or a colour.
    node.type === "fan" ? "6px 6px 0 -2px var(--surface), 8px 8px 0 -2px var(--border)" : "",
    selected ? "inset 2px 0 0 var(--text)" : "",
  ]
    .filter((part) => part !== "")
    .join(", ");

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
      {/*
        No `aria-label` here any more: focus lands on xyflow's wrapper, and the name is handed to it
        through the node object in `buildWorkflow`. Two names — one on the wrapper and one on this —
        would be read twice by anything that walks the tree.
      */}
      <motion.div
        animate={running && !still ? { scale: [1, 1.03, 1] } : { scale: 1 }}
        transition={
          running && !still
            ? { duration: 1.6, repeat: Infinity, ease: "easeInOut" }
            : { duration: still ? 0 : 0.2 }
        }
        aria-current={selected ? true : undefined}
        className={[
          "flex w-[200px] flex-col gap-1 bg-surface px-3 py-2 text-left",
          // Rounded for an agent, square for everything that runs a program. `rounded-lg` and not
          // the `rounded-xl` this was: `tailwind.css` clears the radius namespace and redefines only
          // the four rungs of the ladder, so `xl` compiled to nothing and the documented "rounded for
          // an agent" never reached the screen — agents and commands were both square.
          shape === "agent" ? "rounded-lg" : "rounded-sm",
          // A node switched off in this project is dotted and faded — present, and plainly not
          // taking part. §12: this is not the same as a node the bundle does not have, which is not
          // drawn at all. The dotted edge itself comes from `edgeOf`, where it can outrank the kind.
          node.disabled ? "opacity-50" : "",
        ].join(" ")}
        style={{
          ...edgeOf(shape, node.disabled, running),
          boxShadow: shadow === "" ? undefined : shadow,
        }}
      >
        <div className="flex items-baseline gap-2">
          <span
            className={`truncate text-sm ${shape === "command" || shape === "gate" ? "font-mono" : "font-display"} text-text`}
          >
            {node.label}
          </span>
          {running ? (
            // The wrapper's name already says "running"; this is the mark for the eye.
            <span
              aria-hidden="true"
              className="ml-auto h-1.5 w-1.5 shrink-0 rounded-pill bg-tone-active-fg"
            />
          ) : null}
        </div>
        <div className="flex items-center gap-1.5">
          <span className="flex items-center gap-1 text-xs uppercase tracking-wide text-text-muted">
            <ShapeIcon shape={shape} />
            {shape}
          </span>
          {/*
            The seal §6.2 makes mandatory. It is on the node and not only in the inspector, because
            the question "what has this project changed" is asked of the whole picture at once. An
            inline mark at the 3px rung, not a pill: pills are badges, and this is not a state.
          */}
          {node.overridden ? (
            <span className="rounded-sm border border-border px-1 text-xs text-text-muted">
              project
            </span>
          ) : null}
          {node.disabled ? <span className="text-xs text-text-faint">off here</span> : null}
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
 * A verdict edge says `pass` or `fail`, and the `fail` line is dashed; a conditional edge carries
 * its condition verbatim. All of them are neutral. The verdict edge was amber, which put the tone
 * that means "this needs you" on every gate's outgoing line whether or not anything was waiting —
 * and the label was already doing the work, because a workflow read six months later is read by
 * somebody who does not remember what the colours meant.
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
  const label = edge.verdict ?? edge.when;

  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        style={{
          stroke: "var(--border-strong)",
          strokeWidth: 1.5,
          // An edge with a switched-off end is a path nothing takes here, and it fades with the
          // node rather than disappearing — the same reason the node stays. A `fail` line is dashed
          // at a longer period, so the two stay apart even where they meet.
          opacity: dimmed ? 0.35 : 1,
          strokeDasharray: dimmed ? "3 3" : edge.verdict === "fail" ? "6 4" : undefined,
        }}
      />
      {label === undefined ? null : (
        <EdgeLabelRenderer>
          {/*
            Mono, because the words are the bundle's rather than this app's; the 3px inline-mark
            rung and no border, because a bordered pill is what a badge looks like and this is not
            one. The surface fill stays — it is what stops the line running through the word.
          */}
          <span
            style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
            className="pointer-events-none absolute rounded-sm bg-surface px-1 py-0.5 font-mono text-xs text-text-muted"
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
  /**
   * `via` says whether a key or the pointer did it, so the page can move focus to the inspector for
   * somebody on the keyboard without yanking it away from somebody clicking around the graph.
   */
  onSelect: (id: string | null, via?: SelectedBy) => void;
}

export type SelectedBy = "keyboard" | "pointer";

/**
 * What xyflow says to a screen reader on a focused node. Its default promises "press delete to
 * remove it", and nothing on this surface deletes anything — `deleteKeyCode` is off on purpose.
 */
const ARIA_LABELS = {
  "node.a11yDescription.default":
    "Press enter or space to open this node in the inspector, and escape to close it.",
  "node.a11yDescription.keyboardDisabled":
    "Press enter or space to open this node in the inspector, and escape to close it.",
};

function WorkflowSurface({ nodes, edges, running = null, selected, onSelect }: WorkflowCanvasProps) {
  const model = buildWorkflow(nodes, edges, running);
  // Whether the selection about to arrive was asked for by a key. Set in the capture phase of the
  // keydown, read by `onNodesChange` in the same event, and cleared by any pointer.
  const byKey = useRef(false);

  return (
    <div
      className="h-[420px] w-full overflow-hidden rounded-lg border border-border bg-surface-sunken"
      onKeyDownCapture={(event) => {
        if (event.key === "Escape") {
          // Ours, entirely. xyflow's own Escape on a node that is NOT selected selects it —
          // `handleNodeClick` only honours `unselect` on a node that already is — so letting the
          // key through would make "close" open things.
          event.stopPropagation();
          if (selected !== null) onSelect(null, "keyboard");
          return;
        }
        byKey.current = event.key === "Enter" || event.key === " ";
      }}
      onPointerDownCapture={() => {
        byKey.current = false;
      }}
    >
      <ReactFlow
        nodes={model.nodes.map((node) => ({ ...node, selected: node.id === selected }))}
        edges={model.edges}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        /*
          Selection is read from xyflow's change stream and not only from `onNodeClick`, and that is
          the whole keyboard fix. Enter or Space on a focused node calls xyflow's internal
          `handleNodeClick`, which emits a `select` change and never calls `onNodeClick` — so a
          surface wired only to the click could be tabbed to and never opened. The nodes are
          controlled and nothing else here needs applying: positions are derived, dimensions live in
          xyflow's own lookup.
        */
        onNodesChange={(changes) => {
          const via: SelectedBy = byKey.current ? "keyboard" : "pointer";
          byKey.current = false;
          for (const change of changes) {
            if (change.type === "select" && change.selected) {
              onSelect(change.id, via);
              return;
            }
          }
        }}
        // A click on the node that is already open closes it. xyflow emits no change for that — the
        // node is selected already — so the click is the only place it can be heard.
        onNodeClick={(_event, node) => {
          if (node.id === selected) onSelect(null, "pointer");
        }}
        onPaneClick={() => onSelect(null, "pointer")}
        // Edges carry nothing to open, and as tab stops they doubled the walk through the graph.
        edgesFocusable={false}
        ariaLabelConfig={ARIA_LABELS}
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
