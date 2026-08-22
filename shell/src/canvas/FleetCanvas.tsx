import { createContext, useContext, useEffect, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  Background,
  BaseEdge,
  ConnectionMode,
  Controls,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getBezierPath,
  useEdgesState,
  useNodesState,
  type Connection,
  type EdgeProps,
  type EdgeTypes,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN: the Tauri window's CSP is `default-src
// 'self'`, so a stylesheet fetched from anywhere else is a blank canvas in the
// shipped app and a working one in the dev server — the worst pair of outcomes.
import "@xyflow/react/dist/style.css";
import { cancellableOwner, useJob, type JobItem, type SlotOwner } from "../data/fleet";
import { Button, ConfirmButton, StateBadge } from "../ui";
import {
  clamped,
  isValidConnection as endsMayJoin,
  prunedLayout,
  slotStateLiteral,
  type ConnectionEnd,
  type ExclusionEdgeData,
  type ExclusionFlowEdge,
  type Layout,
  type Partner,
  type SlotCardModel,
  type SlotNode,
  type SlotNodeData,
} from "./model";

/**
 * The fleet on one surface, a node per **slot**.
 *
 * This is the half of the page the columns cannot be. A column is one project,
 * and the question the surface exists to answer — should these two never run at
 * the same time — is about two jobs that are usually not in the same column.
 * The columns stay because they carry `n/limit`, the only thing on screen that
 * says *there is no more room*.
 *
 * `SlotCard` lives in this module rather than in the page, and that is a
 * dependency decision rather than a filing one: both views draw the same card,
 * and importing it from the page would make a cycle (page → canvas → page). A
 * cycle here is how a screen ends up with two subtly different cards.
 */

/* ------------------------------------------------------------- node types -- */

/**
 * **Module scope, and this is the whole point of the spike.**
 *
 * xyflow compares `nodeTypes` and `edgeTypes` by reference and rebuilds every
 * node in the graph when either changes. Declared inside the component they
 * would be new objects on every render, which is a full remount of the canvas
 * on every 3-second poll — and with the React Compiler memoising the component
 * around them, the failure would be intermittent rather than constant, which is
 * worse. A module constant cannot have that bug: there is one object, for the
 * life of the module.
 */
const nodeTypes: NodeTypes = { slotCard: SlotCardNode };
const edgeTypes: EdgeTypes = { exclusion: ExclusionEdgeLine };

/** Exported so a test can assert the identity the library depends on. */
export const FLEET_NODE_TYPES = nodeTypes;
export const FLEET_EDGE_TYPES = edgeTypes;

/* ----------------------------------------------------------------- actions -- */

/**
 * What a card can do, without every card being handed the page's state.
 *
 * A context rather than props on the node: xyflow builds node components from
 * `nodeTypes` and hands them only `data`, so a callback passed through `data`
 * would be a new function in the node payload on every render — exactly the
 * churn the module-scope `nodeTypes` above exists to avoid. The columns use the
 * same provider, so one card implementation serves both views.
 */
export interface FleetActions {
  /** The job whose item list is open, or `null`. One at a time, page-wide. */
  openJob: number | null;
  toggleJob: (jobId: number) => void;
  cancel: (owner: SlotOwner) => void;
  /** Lifts a rule that is in force. A pending request is answered in Waiting. */
  lift: (exclusionId: number) => void;
}

const NO_ACTIONS: FleetActions = {
  openJob: null,
  toggleJob: () => {},
  cancel: () => {},
  lift: () => {},
};

const FleetActionsContext = createContext<FleetActions>(NO_ACTIONS);

export function FleetActionsProvider({
  actions,
  children,
}: {
  actions: FleetActions;
  children: ReactNode;
}) {
  return <FleetActionsContext.Provider value={actions}>{children}</FleetActionsContext.Provider>;
}

/* -------------------------------------------------------------- slot card -- */

export interface SlotCardProps {
  card: SlotCardModel;
  /** The canvas gives its cards connectable handles; a column has nowhere to connect to. */
  connectable?: boolean;
}

/**
 * One slot.
 *
 * Per slot and not per job, and the difference is not cosmetic: an autonomous
 * worktree run holds a slot too, so a view that counted jobs would say `0/2`
 * about a project that is going to refuse the next one with a 409.
 *
 * `wait_reason` and `round`/`max_rounds` appear only when the owner is a job:
 * they are columns of `jobs`, and a run has no equivalent. Showing them as zero
 * for a run would be inventing a number.
 */
export function SlotCard({ card, connectable = false }: SlotCardProps) {
  const actions = useContext(FleetActionsContext);
  const { detail, slot } = card;
  const jobId = detail.kind === "job" ? detail.job.id : null;
  const open = jobId !== null && actions.openJob === jobId;
  const cancellable = cancellableOwner(slot);

  return (
    <article
      className={`fleet-card fleet-card-${detail.kind}`}
      aria-label={`slot ${slot.slot} — ${slot.owner_kind} ${slot.owner_id}`}
    >
      <header className="fleet-card-head">
        <span className="fleet-card-slot">slot {slot.slot}</span>
        <span className="fleet-card-owner">
          {slot.owner_kind} {slot.owner_id}
        </span>
      </header>

      {detail.kind === "job" && (
        <>
          <p className="fleet-card-line">
            <StateBadge domain="job" state={detail.job.status} />
            {detail.job.wait_reason !== null && (
              <StateBadge domain="wait_reason" state={detail.job.wait_reason} />
            )}
          </p>
          <p className="fleet-card-rounds">
            round {detail.job.round + 1} of {detail.job.max_rounds}
          </p>
          <Button
            variant="link"
            aria-expanded={open}
            onClick={() => actions.toggleJob(detail.job.id)}
          >
            {open ? "Hide items" : "Show items"}
          </Button>
          {open && <JobItemsPanel jobId={detail.job.id} />}
        </>
      )}

      {detail.kind === "run" && (
        <>
          <p className="fleet-card-line">
            <StateBadge domain="run" state={detail.run.status} />
            <span className="fleet-card-mode">{detail.run.mode}</span>
          </p>
          <p className="fleet-card-prompt">{detail.run.prompt_excerpt}</p>
          {/* A run has no second zoom of its own — its output lives in Runs. */}
          <Link to="/runs" className="fleet-card-link">
            Open in Runs
          </Link>
        </>
      )}

      {/* One item of a team's job. It says which job and which item, and stops
          there: the queue belongs to the job, whose own card is in this same
          column with the button that opens it. Two cards drawing one queue would
          be the same list twice, opened and closed independently. */}
      {detail.kind === "item" && (
        <>
          <p className="fleet-card-line">
            {/* The ITEM's reading and never the job's, though the job's would be
                one field away. A slot is held from the claim until the item is
                terminal, so what a reader of a capacity screen needs from this
                card is whether the slot is busy or stuck — and `conflicted` is
                the answer only the item can give. Plain text rather than a
                badge, which is the idiom `itemReading` already sets for an
                item's state one panel over. */}
            <span className="fleet-item-state">{itemReading({ status: detail.status })}</span>
            <span className="fleet-card-of">
              item {detail.ordinal + 1} of job {detail.job.id}
            </span>
          </p>
          {/* Which job's work this is a step of. The item's own description is a
              round trip this card does not need to make — it is one button away
              on the job's card, in this same column. */}
          <p className="fleet-card-prompt">{detail.job.rule_name ?? "started by hand"}</p>
        </>
      )}

      {/* The listing said nothing about this owner, and the two reasons for that
          are a different kind of news: one is ordinary, the other is a leaked
          slot nothing is working in. */}
      {(detail.kind === "unknown" || detail.kind === "orphaned") && (
        <p className="fleet-card-line">
          <StateBadge domain="slot" state={slotStateLiteral(detail)} />
        </p>
      )}

      {card.badges.map((badge) => (
        <p key={badge.source} className={`fleet-collide fleet-collide-${badge.source}`}>
          {/* A word and not colour alone: the two sources have to be told apart
              by anyone, and only one of them is a measurement of the past. */}
          <span className="fleet-collide-source">{badge.source}</span>
          <StateBadge domain="collision" state={badge.state} />
          {badge.state === "collide" && (
            <span className="fleet-collide-what">
              also touched by {badge.others.map((other) => `${other.kind} ${other.id}`).join(", ")}:{" "}
              {badge.paths.join(", ")}
            </span>
          )}
        </p>
      ))}

      {card.partners.map((partner) => (
        <p
          key={`${partner.state}-${partner.id}`}
          className={`fleet-edge-note fleet-edge-${partner.state}`}
        >
          <span className="fleet-edge-text">{partnerLine(partner)}</span>
          {partner.state === "active" ? (
            <Button variant="link" onClick={() => actions.lift(partner.id)}>
              Lift
            </Button>
          ) : (
            <Link to="/waiting" className="fleet-card-link">
              Answer it in Waiting
            </Link>
          )}
        </p>
      ))}

      {/* Absent on an item's card, and absent rather than disabled: the thing to
          stop is the job, whose own card is in the same column, and a button
          that has to explain why it cannot be pressed is one more thing to read
          on a card that is already dense. */}
      {cancellable !== null && (
        <ConfirmButton
          label="Cancel"
          confirmLabel={`Cancel ${slot.owner_kind} ${slot.owner_id}?`}
          onConfirm={() => actions.cancel(cancellable)}
        />
      )}

      {connectable && (
        <>
          {/* Both handles on every card, and `ConnectionMode.Loose` on the flow:
              an exclusion is symmetric, so either end may start the line. */}
          <Handle type="target" position={Position.Left} className="fleet-handle" />
          <Handle type="source" position={Position.Right} className="fleet-handle" />
        </>
      )}
    </article>
  );
}

/**
 * What one edge says from this end of it.
 *
 * A pending edge claims nothing: it has changed nothing about how either job is
 * scheduled, and until somebody answers it in the queue it never will. An
 * active one names which of the two waits rather than saying "held", because
 * the wait only happens while the partner actually holds a slot.
 */
function partnerLine(partner: Partner): string {
  if (partner.state === "pending") {
    return `asked: not at the same time as job ${partner.partner} — waiting for a decision`;
  }
  return partner.waits
    ? `not at the same time as job ${partner.partner} — this one waits`
    : `not at the same time as job ${partner.partner} — that one waits`;
}

/* ------------------------------------------------------------- job detail -- */

/**
 * The second zoom: a job as the chain of items it is running.
 *
 * Mounted only when a card is open, which is what "open" means to the hook
 * underneath — asking for every job's item list on every tick would multiply
 * the poll by the number of cards on screen to show rows nobody opened.
 */
function JobItemsPanel({ jobId }: { jobId: number }) {
  const job = useJob(jobId);

  if (job.data === undefined) {
    return (
      <p className="fleet-items-note">
        {job.isError ? "its item list could not be read" : "reading its item list…"}
      </p>
    );
  }
  if (job.data.items.length === 0) {
    return <p className="fleet-items-note">its planner looked and found no work</p>;
  }

  const detail = job.data;

  return (
    <>
      {/* Said once above the queue rather than on every row, because it is a
          fact about the job. It is also what makes the rows below legible: two
          items running at once is a stuck queue in a job nobody directs, and the
          entire point of one that is directed. */}
      {detail.team_id !== null && (
        <p className="fleet-items-team">
          directed by <Link to={`/teams/${detail.team_id}`}>{detail.team_name ?? detail.team_id}</Link>
          {detail.team_max_parallel !== null && ` — up to ${detail.team_max_parallel} items at once`}
        </p>
      )}
      <ol className="fleet-items" aria-label={`items of job ${jobId}`}>
        {detail.items.map((item, index) => (
          <li key={item.ordinal} className="fleet-item">
            {/* The mark goes on the FIRST item of a new round, which is the
                boundary a reader is looking for. Ordinals carry on across rounds,
                so the number itself says nothing about where one ended. */}
            {index > 0 && item.round !== detail.items[index - 1].round && (
              <p className="fleet-item-round">round {item.round + 1}</p>
            )}
            <div className="fleet-item-row">
              <span className="fleet-item-ordinal">{item.ordinal + 1}</span>
              <span className="fleet-item-what">{item.description}</span>
              <span className="fleet-item-state">{itemReading(item)}</span>
              {/* Two columns, two questions: what the item did, and whether
                  anything measured it. A NULL gate is *no gate configured*. */}
              <StateBadge domain="gate" state={item.gate_status} />
            </div>
            <ItemDirection item={item} />
            {item.status === "skipped" && (
              <p className="fleet-item-why">
                skipped — <Link to="/waiting">the proposal that explains it is in Waiting</Link>
              </p>
            )}
          </li>
        ))}
      </ol>
    </>
  );
}

/**
 * Who was given this item, what it waits for, and what it said it would touch.
 *
 * **The three facts that answer the question a parallel queue provokes and a
 * sequential one never did:** why are *these* items moving and not those. Two of
 * five running used to be a list of statuses with the reasoning taken out — the
 * fold knew about the dependencies and the declared paths, and nothing outside
 * it did.
 *
 * Nothing at all for a job without a team, where all three are empty because
 * nobody was asked. Not "none" and not an empty line: a job that was never
 * directed has no answer to give, which is different from an answer of nothing.
 *
 * Ordinals are shown `+1` here for the same reason the row above shows them that
 * way — a `depends_on` of `[0]` is a reference to the item drawn as `1`, and
 * printing the raw number would name a row that is not on screen.
 */
function ItemDirection({ item }: { item: JobItem }) {
  const parts: string[] = [];
  if (item.depends_on.length > 0) {
    parts.push(`after ${item.depends_on.map((ordinal) => ordinal + 1).join(", ")}`);
  }
  if (item.files.length > 0) parts.push(item.files.join(", "));
  if (item.agent_name === null && parts.length === 0) return null;

  return (
    <p className="fleet-item-direction">
      {item.agent_name !== null && <span className="fleet-item-agent">{item.agent_name}</span>}
      {parts.join(" · ")}
    </p>
  );
}

/**
 * What one item's row says about itself.
 *
 * `passed` is deliberately not the end of the story: an item reading `passed`
 * with no gate status was never measured — the project configures no gate, or
 * this was an intermediate item — and the badge beside this text is what says
 * so. The two are separate because they are separate columns.
 */
function itemReading(item: Pick<JobItem, "status">): string {
  switch (item.status) {
    case "pending":
      return "to do";
    case "running":
      return "running";
    case "implemented":
      return "written";
    case "passed":
      return "done";
    case "gate_failed":
      return "the gate failed";
    case "gate_errored":
      return "the gate could not run";
    case "failed":
      return "failed";
    case "skipped":
      return "skipped";
    case "cancelled":
      return "cancelled";
    // The four states an item of a job a team directs can be in. Without them
    // all four fell to the default below and read as "to do" — a lie about
    // every one of them, and the worst of the four is `conflicted`: an item
    // waiting on a person, shown as work not yet begun.
    case "merging":
      return "merging into the job's branch";
    case "conflicted":
      return "the merge conflicted";
    case "reverted":
      return "taken back off the branch";
    case "orphaned":
      return "never attempted — something it needed did not land";
    default:
      // The core reads an unknown status as still-to-do rather than as done,
      // and so does this. Safe in the core, where erring toward "not finished"
      // costs a repeated item; here it is only ever the last resort, which is
      // why the arms above exist rather than being left to it.
      return "to do";
  }
}

/* ------------------------------------------------------ flow node and edge -- */

function SlotCardNode({ data }: NodeProps<SlotNode>) {
  return <SlotCard card={(data as SlotNodeData).card} connectable />;
}

function ExclusionEdgeLine({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
}: EdgeProps<ExclusionFlowEdge>) {
  const [path] = getBezierPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
  });
  const state = (data as ExclusionEdgeData | undefined)?.exclusion.state ?? "active";
  return <BaseEdge id={id} path={path} className={`fleet-wire fleet-wire-${state}`} />;
}

/* ------------------------------------------------------------------ canvas -- */

export interface FleetCanvasProps {
  nodes: SlotNode[];
  edges: ExclusionFlowEdge[];
  /** Node key → end, so a dropped line is judged without walking the graph. */
  ends: Record<string, ConnectionEnd>;
  /** Asks the daemon for the rule. It answers with a *proposal*, not a rule. */
  onPropose: (a: number, b: number) => void;
  /**
   * A gesture finished and the arrangement changed.
   *
   * The canvas does not write to storage itself: the page holds the layout, so
   * an arrangement made here survives a switch to the columns and back. Called
   * on the drop and not on every frame — one write per gesture rather than
   * sixty a second.
   */
  onLayoutChange: (layout: Layout) => void;
}

/**
 * The surface itself.
 *
 * Node positions are held here rather than derived on every tick: a poll lands
 * every three seconds, and a position recomputed from what just arrived is a
 * position that can be yanked out from under a gesture in progress. The effect
 * below refreshes the *data* of nodes that are already on screen and leaves
 * their coordinates alone; only a node that was not there takes the derived
 * position it arrived with.
 */
function FleetSurface({
  nodes: derived,
  edges: derivedEdges,
  ends,
  onPropose,
  onLayoutChange,
}: FleetCanvasProps) {
  const [nodes, setNodes, onNodesChange] = useNodesState<SlotNode>(derived);
  const [edges, setEdges] = useEdgesState<ExclusionFlowEdge>(derivedEdges);

  useEffect(() => {
    setNodes((current) => {
      const placed = new Map(current.map((node) => [node.id, node]));
      return derived.map((node) => {
        const existing = placed.get(node.id);
        return existing === undefined ? node : { ...existing, data: node.data };
      });
    });
  }, [derived, setNodes]);

  useEffect(() => {
    setEdges(derivedEdges);
  }, [derivedEdges, setEdges]);

  return (
    <div className="fleet-canvas">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        // An exclusion is symmetric, so a line may start at either end's handle.
        connectionMode={ConnectionMode.Loose}
        // Positions are a preference of whoever is looking, so they are handed
        // up on the drop — once per gesture rather than sixty times a second.
        onNodeDragStop={(_event, node) => {
          const layout: Layout = {};
          for (const placed of nodes) layout[placed.id] = clamped(placed.position);
          layout[node.id] = clamped(node.position);
          // Pruned on the way out: without it the stored object grows by one
          // entry per job for the life of the machine, and none of the dead
          // ones is ever asked about again.
          onLayoutChange(
            prunedLayout(
              layout,
              nodes.map((placed) => placed.id),
            ),
          );
        }}
        isValidConnection={(connection: Connection | ExclusionFlowEdge) =>
          endsMayJoin(ends[connection.source], ends[connection.target])
        }
        onConnect={(connection: Connection) => {
          const from = ends[connection.source];
          const to = ends[connection.target];
          if (!endsMayJoin(from, to) || from.jobId === null || to.jobId === null) return;
          onPropose(from.jobId, to.jobId);
        }}
        // Nothing here deletes a node or an edge, and a stray Backspace over the
        // canvas must not look as if it did: the daemon owns both lists.
        deleteKeyCode={null}
        nodesDraggable
        nodesConnectable
        elementsSelectable
        fitView
      >
        <Background />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}

/**
 * The canvas, with its own store.
 *
 * `ReactFlowProvider` is mounted here rather than around the page so that
 * switching back to the columns disposes the store instead of leaving a
 * viewport nobody is looking at subscribed to every node.
 */
export function FleetCanvas(props: FleetCanvasProps) {
  return (
    <ReactFlowProvider>
      <FleetSurface {...props} />
    </ReactFlowProvider>
  );
}
