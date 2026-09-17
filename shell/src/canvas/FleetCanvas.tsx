import { createContext, useContext, useEffect, useId, useRef, useState, type ReactNode } from "react";
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
  ViewportPortal,
  getBezierPath,
  useEdgesState,
  useNodesInitialized,
  useNodesState,
  useReactFlow,
  type Connection,
  type EdgeProps,
  type EdgeTypes,
  type FitViewOptions,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN: the Tauri window's CSP is `default-src
// 'self'`, so a stylesheet fetched from anywhere else is a blank canvas in the
// shipped app and a working one in the dev server — the worst pair of outcomes.
import "@xyflow/react/dist/style.css";
import { cancellableOwner, useJob, type JobItem, type SlotOwner } from "../data/fleet";
import { JobProgressGraph, JobProgressLine } from "./JobProgressGraph";
import { Button, ConfirmButton, Field, StateBadge } from "../ui";
import {
  clamped,
  isValidConnection as endsMayJoin,
  prunedLayout,
  slotReading,
  zonesFor,
  ZONE_HEAD,
  ZONE_PAD,
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
 * says *there is no more room*; the surface now says it too, on the labelled
 * region each project's cards sit in.
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

/**
 * What the surface says to a screen reader about its nodes and edges.
 *
 * The library's defaults end in "Press delete to remove it", on a canvas where `deleteKeyCode`
 * is `null` because nothing here may delete anything — the daemon owns both lists. A description
 * that offers a key which does nothing is worse than none: it is the one instruction somebody
 * who cannot see the canvas will follow. Module scope for the reason `nodeTypes` is.
 */
const ARIA_LABELS = {
  "node.a11yDescription.default":
    "Press Enter or Space to pick this card up, the arrow keys to move it, and Escape to put it down.",
  "node.a11yDescription.keyboardDisabled": "Press Enter or Space to select this card.",
  "edge.a11yDescription.default":
    "A rule or a request that two jobs never run at the same time. Press Enter or Space to select it.",
};

/**
 * How close the first view comes.
 *
 * `fitView` alone shrinks the whole fleet into the box, and on a busy fleet that was a zoom of
 * 0.5 — 11px labels drawn at 5.5px, which is not a canvas anyone can read, only a picture of one.
 * A floor of 0.8 keeps every label legible and lets the surface pan to what does not fit, which is
 * what a canvas is for. `maxZoom` stops a fleet of one card arriving blown up to fill the box.
 */
const ZOOM_FLOOR = 0.8;
const FIRST_VIEW: FitViewOptions<SlotNode> = { minZoom: ZOOM_FLOOR, maxZoom: 1, padding: 0.08 };

/** Where the first region's corner lands when the fleet is too big to fit, in screen pixels. */
const FIRST_CORNER = 16;

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
  /** Asks that two jobs never run at the same time — the keyboard's way to the canvas gesture. */
  propose: (jobA: number, jobB: number) => void;
  /**
   * The capacity reading is the last good one, not the daemon's current one.
   *
   * Every card dims, and every control on a card that would act on what it shows goes — Cancel,
   * Lift, the pairing question — for the reason the page's own New job goes: a gesture aimed by
   * a reading nobody can vouch for fails after the click instead of before it.
   */
  stale: boolean;
}

const NO_ACTIONS: FleetActions = {
  openJob: null,
  toggleJob: () => {},
  cancel: () => {},
  lift: () => {},
  propose: () => {},
  stale: false,
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
 *
 * **The kind is said in words and in a badge, and no longer in a stripe.** The
 * card used to carry `fleet-card-${detail.kind}`, which painted a coloured left
 * rule per kind in the state tones — a taxonomy wearing the colours reserved for
 * status, so a job merely queued still got Acting Green. The head names the kind
 * outright (`job 41`, `run 7`) and the two kinds that are themselves states,
 * `unknown` and `orphaned`, render `StateBadge domain="slot"` below. That badge
 * is what the tests read, and it was carrying the distinction all along.
 */
export function SlotCard({ card, connectable = false }: SlotCardProps) {
  const actions = useContext(FleetActionsContext);
  const { detail, slot } = card;
  const jobId = detail.kind === "job" ? detail.job.id : null;
  const open = jobId !== null && actions.openJob === jobId;
  const cancellable = cancellableOwner(slot);
  // An item is named by the job it is a step of and its place in that job's queue. Its own id is
  // `job_items.id`, a number out of a sequence nobody reads, so it goes second and in the mono
  // face, as the handle it is rather than the name it is not.
  const owner =
    detail.kind === "item"
      ? `item ${detail.ordinal + 1} of job ${detail.job.id}`
      : `${slot.owner_kind} ${slot.owner_id}`;

  return (
    <article
      className={actions.stale ? "fleet-card fleet-card-stale" : "fleet-card"}
      aria-label={`slot ${slot.slot} — ${owner}`}
    >
      <header className="fleet-card-head">
        <span className="fleet-card-slot">slot {slot.slot}</span>
        <span className="fleet-card-owner">{owner}</span>
      </header>

      {detail.kind === "job" && (
        <>
          {/* What the job IS, before what it is doing. The live listing carries the rule that
              started it and nothing else — no prompt — so a job somebody asked for by hand says
              so rather than showing an empty line. */}
          <p className="fleet-card-prompt">{detail.job.rule_name ?? "started by hand"}</p>
          <p className="fleet-card-line">
            {/* `slotReading`, the reading the slot rack lights this slot's pip with, so the
                pip and this badge cannot disagree about one slot. The same below for a run,
                an item and an undescribed slot. */}
            <StateBadge {...slotReading(detail)} />
            {detail.job.wait_reason !== null && (
              <StateBadge domain="wait_reason" state={detail.job.wait_reason} />
            )}
          </p>
          <p className="fleet-card-meta">
            round {detail.job.round + 1} of {detail.job.max_rounds}
            {detail.job.team_name !== null && ` · directed by ${detail.job.team_name}`}
          </p>
          {/* An action, not a destination, so it is a quiet button and not the cyan link:
              nothing is navigated to, the card opens in place. */}
          <span className="fleet-card-actions">
            <Button
              variant="quiet"
              aria-expanded={open}
              onClick={() => actions.toggleJob(detail.job.id)}
            >
              {open ? "Hide items" : "Show items"}
            </Button>
          </span>
          {open && <JobItemsPanel jobId={detail.job.id} />}
        </>
      )}

      {detail.kind === "run" && (
        <>
          <p className="fleet-card-prompt">{detail.run.prompt_excerpt}</p>
          <p className="fleet-card-line">
            <StateBadge {...slotReading(detail)} />
            <span className="fleet-card-mode">{detail.run.mode}</span>
          </p>
          {/* A run has no second zoom of its own — its output lives in Runs. */}
          <Link to={`/runs/${detail.run.id}`} className="fleet-card-link">
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
          <p className="fleet-card-prompt">{detail.job.rule_name ?? "started by hand"}</p>
          <p className="fleet-card-line">
            {/* The ITEM's reading and never the job's, though the job's would be
                one field away. A slot is held from the claim until the item is
                terminal, so what a reader of a capacity screen needs from this
                card is whether the slot is busy or stuck — and `conflicted` is
                the answer only the item can give. */}
            <StateBadge {...slotReading(detail)} />
            <span className="fleet-card-of">id {slot.owner_id}</span>
          </p>
          {/* Where the conflict is dealt with. Nobody is asked: `batch_of` in
              `core/src/job.rs` takes a conflicted item as work and starts a
              resolution run in its own tree, and that run is listed in Runs
              under this project. So the door goes there, and the sentence says
              the núcleo is on it rather than implying a person must be. */}
          {detail.status === "conflicted" && (
            <p className="fleet-card-note">
              The núcleo starts a run to resolve it in the item's own tree.{" "}
              <Link
                to="/runs"
                search={{ project: card.project.project_id }}
                className="fleet-card-link"
              >
                Follow it in Runs
              </Link>
            </p>
          )}
        </>
      )}

      {/* The listing said nothing about this owner, and the two reasons for that
          are a different kind of news: one is ordinary, the other is a leaked
          slot nothing is working in. */}
      {(detail.kind === "unknown" || detail.kind === "orphaned") && (
        <p className="fleet-card-line">
          <StateBadge {...slotReading(detail)} />
        </p>
      )}

      {card.badges.map((badge) => (
        <p key={badge.source} className="fleet-collide">
          {/* A word and not colour alone: the two sources have to be told apart
              by anyone, and only one of them is a measurement of the past. The
              word is a badge of its own, so its colour is the map's full triple
              rather than a bare foreground. */}
          <StateBadge
            domain="collision_source"
            state={badge.source === "predicted" ? "declared" : "observed"}
          />
          <StateBadge domain="collision" state={badge.state} />
          <span className="fleet-collide-what">
            also touched by {badge.others.map((other) => `${other.kind} ${other.id}`).join(", ")}:{" "}
            {badge.paths.join(", ")}
          </span>
        </p>
      ))}

      {card.partners.map((partner) => (
        <p key={`${partner.state}-${partner.id}`} className="fleet-edge-note">
          <StateBadge domain="exclusion" state={partner.state} />
          <span className="fleet-edge-text">{partnerLine(partner)}</span>
          {partner.state === "active" ? (
            // Gone rather than disabled while the view is stale, like Cancel below. An
            // interlock and not a click: lifting takes effect at once, and the job it was
            // holding back may start the next tick. Quiet, because it is recoverable — the
            // same pair can be asked about again — which is the grammar's word for it. No
            // `subject` on the armed label: the interlock reserves the wider label's width, and
            // "· job 56" pushed a four-letter Lift half a card to the right of its sentence. The
            // partner is still said to the ear, in `sayAs`, and is already in the sentence. The
            // width still reserved is "Lift the rule"'s, and `fleet.css` starts the resting label
            // at its left edge (`.fleet-edge-note .ui-confirm-stack`) rather than centring it.
            !actions.stale && (
              <ConfirmButton
                label="Lift"
                confirmLabel="Lift the rule"
                sayAs={`lifts the rule, so job ${partner.partner} and this job may run at the same time`}
                variant="quiet"
                onConfirm={() => actions.lift(partner.id)}
              />
            )
          ) : (
            <Link to="/waiting" className="fleet-card-link">
              Answer it in Waiting
            </Link>
          )}
        </p>
      ))}

      {jobId !== null && card.peers.length > 0 && !actions.stale && (
        <KeepApart jobId={jobId} peers={card.peers} onAsk={actions.propose} />
      )}

      {/* Absent on an item's card, and absent rather than disabled: the thing to
          stop is the job, whose own card is in the same column, and a button
          that has to explain why it cannot be pressed is one more thing to read
          on a card that is already dense. Absent while stale too — the slot on
          screen may already have been given back. */}
      {cancellable !== null && !actions.stale && (
        <span className="fleet-card-actions">
          <ConfirmButton
            label="Cancel"
            confirmLabel={`Cancel ${slot.owner_kind} ${slot.owner_id}?`}
            variant="ghost"
            onConfirm={() => actions.cancel(cancellable)}
          />
        </span>
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
 * The pairing question, for somebody who is not dragging a line.
 *
 * The canvas asks it with a gesture that needs a pointer, so without this the question had no
 * keyboard path at all, and no path from the columns either. It calls the same mutation the
 * line does, so the answer is the same proposal in the same queue.
 */
function KeepApart({
  jobId,
  peers,
  onAsk,
}: {
  jobId: number;
  peers: number[];
  onAsk: (jobA: number, jobB: number) => void;
}) {
  const [chosen, setChosen] = useState<number>(peers[0]);
  // A peer can leave between two ticks. Falling back to the first one left is the only answer
  // that never sends a pair the daemon would refuse for naming a job that is gone.
  const partner = peers.includes(chosen) ? chosen : peers[0];
  return (
    <form
      className="fleet-peer"
      onSubmit={(event) => {
        event.preventDefault();
        onAsk(jobId, partner);
      }}
    >
      <Field label="Never at the same time as">
        <select value={partner} onChange={(event) => setChosen(Number(event.target.value))}>
          {peers.map((peer) => (
            <option key={peer} value={peer}>
              job {peer}
            </option>
          ))}
        </select>
      </Field>
      <Button type="submit">Ask</Button>
    </form>
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
 *
 * **Two arrangements of one queue**, the same way the page above it is two
 * arrangements of one fleet. The list answers *what is each item doing* and is
 * the only one of the two that can carry a gate badge and the link out of a
 * skipped item; the graph answers *what is waiting on what*, which is in
 * `depends_on` and which no list can draw. Neither is a better version of the
 * other, so neither replaces it.
 */
function JobItemsPanel({ jobId }: { jobId: number }) {
  const job = useJob(jobId);
  const [drawn, setDrawn] = useState(false);

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
    <div className="fleet-items-panel">
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
      {/* The one-line reading sits with the ARRANGEMENT CONTROL rather than on the closed card,
          and that placement is a constraint and not a preference: the tally counts items, items
          arrive from `useJob`, and `useJob` only runs while a card is open. Putting this on a
          closed card would fetch every job's item list on every tick — the exact multiplication
          this panel's own doc comment exists to prevent. A fleet-wide glance needs the daemon to
          carry a tally on `/jobs?live=true`; until it does, this is as far out as it can go. */}
      <div className="fleet-items-head">
        <JobProgressLine job={detail} items={detail.items} />
        <Button onClick={() => setDrawn((shown) => !shown)} aria-pressed={drawn}>
          {drawn ? "As a list" : "As a graph"}
        </Button>
      </div>
      {/* The line above already says the reading, so the graph does not say it a second time. */}
      {drawn && <JobProgressGraph job={detail} items={detail.items} showReading={false} />}
      {!drawn && (
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
                {/* Two badges, two questions: what the item did, and whether
                    anything measured it. A NULL gate is *no gate configured*. The
                    first is the same map entry the item's own slot card reads, so
                    the list and the card cannot word one state two ways. */}
                <StateBadge domain="job_item" state={item.status} />
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
      )}
    </div>
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
  return (
    <>
      {/* The focus ring, drawn as a wider stroke UNDER the wire. xyflow sets
          `outline: none` on a focused edge, and an outline on an SVG group is
          not drawn reliably anyway — so the ring is a path of its own that only
          takes a colour while the edge has keyboard focus. Under and not over,
          so the wire's own tone still reads through it. */}
      <path d={path} className="fleet-wire-ring" />
      <BaseEdge id={id} path={path} className={`fleet-wire fleet-wire-${state}`} />
    </>
  );
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
        return existing === undefined
          ? node
          : { ...existing, data: node.data, ariaLabel: node.ariaLabel };
      });
    });
  }, [derived, setNodes]);

  useEffect(() => {
    setEdges(derivedEdges);
  }, [derivedEdges, setEdges]);

  /*
    The first view, framed once the cards have been measured.

    Fit when the fleet fits. When it does not, the zoom stops at the floor — and a fit that stops
    at the floor centres the whole fleet, which put the middle of the surface on screen with the
    first region cut off at the top. So at the floor the view starts at the top-left corner
    instead, where the rows begin, and the rest is a pan away. Done here rather than through the
    `fitView` prop so this cannot race it; once, so a view somebody panned is never yanked back.
  */
  const measured = useNodesInitialized();
  const flow = useReactFlow<SlotNode, ExclusionFlowEdge>();
  const framed = useRef(false);
  useEffect(() => {
    if (!measured || framed.current) return;
    framed.current = true;
    void flow.fitView(FIRST_VIEW).then(() => {
      if (flow.getViewport().zoom > ZOOM_FLOOR + 0.001) return;
      const bounds = flow.getNodesBounds(flow.getNodes());
      void flow.setViewport({
        x: FIRST_CORNER - (bounds.x - ZONE_PAD) * ZOOM_FLOOR,
        y: FIRST_CORNER - (bounds.y - ZONE_HEAD) * ZOOM_FLOOR,
        zoom: ZOOM_FLOOR,
      });
    });
  }, [measured, flow]);

  // From the nodes as they are NOW — moved, measured — so a region follows a dragged card and
  // grows with one whose item list is open.
  const zones = zonesFor(nodes);

  return (
    <div className="fleet-canvas">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        ariaLabelConfig={ARIA_LABELS}
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
        // Not `fitView`: the effect above frames the first view. The options still reach the
        // controls' own fit button, which must not zoom past legible either.
        fitViewOptions={FIRST_VIEW}
      >
        {/* Each project's region, drawn in flow coordinates so it pans and zooms with its
            cards. Hidden from the accessibility tree: every card's node already says which
            project it is in, and a region announced separately would be a second name for
            the same fact. */}
        <ViewportPortal>
          {zones.map((zone) => (
            <div
              key={zone.projectId}
              className="fleet-zone"
              aria-hidden="true"
              style={{
                transform: `translate(${zone.x}px, ${zone.y}px)`,
                width: zone.width,
                height: zone.height,
              }}
            >
              <span className="fleet-zone-label">{zone.label}</span>
              {zone.note !== null && <span className="fleet-zone-note">{zone.note}</span>}
            </div>
          ))}
        </ViewportPortal>
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
 *
 * The key under it is the one thing the surface cannot say about itself: that a line dragged
 * between two cards is a question, and where the answer is given.
 */
export function FleetCanvas(props: FleetCanvasProps) {
  const keyId = useId();
  return (
    <ReactFlowProvider>
      <div className="fleet-canvas-frame" aria-describedby={keyId}>
        <FleetSurface {...props} />
        <p id={keyId} className="fleet-canvas-key">
          To ask that two jobs of one project never run at the same time, drag a line from the dot
          on one card's edge to the other card — or use "Never at the same time as" on the card.
          Nothing changes until the request is answered in Waiting.
        </p>
      </div>
    </ReactFlowProvider>
  );
}
