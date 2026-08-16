import { useState } from "react";

import type { HeldSlot, Job, ProjectConcurrency, RunSearchResult } from "./api";
import { useExclusionActions } from "./fleet-actions";
import {
  collisionBadges,
  ownerKey,
  partnersOf,
  slotDetail,
  type ExclusionEdge,
} from "./fleet-derive";
import { edgeGeometry } from "./fleet-edges";
import {
  clamped,
  positionsFor,
  pruned,
  readLayout,
  withMoved,
  writeLayout,
  type Layout,
  type Point,
} from "./fleet-layout";
import { SlotCard } from "./SlotCard";
import { Button, ErrorNote } from "./ui";

/**
 * The nominal size of a node.
 *
 * Nominal because a card's real height depends on what it has to say — an open item graph is much
 * taller than a waiting job — and nothing here needs the real one. It is used to work out how far
 * the surface has to reach so a node dragged into the corner can still be scrolled to.
 *
 * Generous on purpose, and the first guess was not: at 170 the surface ended above the bottom of
 * the lowest card and clipped it, because a job card carrying an exclusion request is nearly twice
 * that. Under-reaching cuts a card in half; over-reaching costs some empty surface to scroll past.
 */
const NODE = { width: 280, height: 320 };

/** Never smaller than this, so an empty or nearly empty canvas still looks like a surface. */
const FLOOR = { width: 640, height: 520 };

/**
 * A gesture in progress.
 *
 * `from` is where the node was when the hand closed on it and `origin` is where the pointer was;
 * the node is drawn at `from + (pointer − origin)`. Both are frozen at the press on purpose — the
 * position is computed from the GESTURE and never from the state a poll updates, which is what makes
 * a tick landing mid-drag harmless.
 */
interface Drag {
  key: string;
  from: Point;
  origin: Point;
  at: Point;
}

interface FleetCanvasProps {
  /** Every project, because an exclusion is about two jobs and this is where both are visible. */
  projects: ProjectConcurrency[];
  jobs: Job[] | null;
  runs: RunSearchResult[] | null;
  edges: ExclusionEdge[];
  token: string;
  cancelled: Set<string>;
  onCancel: (slot: HeldSlot) => Promise<void>;
  onOpenRuns: () => void;
  refresh: () => Promise<void>;
}

/**
 * The whole house on one surface, a node per SLOT.
 *
 * This is the half of the fleet view the columns cannot be. A column is one project, and the
 * question this screen exists to answer — should these two never run at the same time — is about
 * two jobs that are usually not in the same column. The columns stay because they carry `n/limit`,
 * the only thing on screen that says *there is no more room*, and a free surface has nowhere to put
 * that number without inventing a frame per project.
 *
 * Positioned with `transform: translate(...)` rather than `left`/`top`: of the two, only the
 * transform moves a node without making the browser lay the page out again on every frame of a drag.
 *
 * The saved layout is read ONCE, at mount. It is not derived from the props on purpose — a poll
 * lands every three seconds, and a position recomputed from what just arrived is a position that can
 * be yanked out from under a gesture in progress.
 */
export default function FleetCanvas({
  projects,
  jobs,
  runs,
  edges,
  token,
  cancelled,
  onCancel,
  onOpenRuns,
  refresh,
}: FleetCanvasProps) {
  const { pairing, failed, closed, roleFor, pick, neverMind, lift, decide } = useExclusionActions(
    token,
    refresh,
  );
  const [saved, setSaved] = useState<Layout>(() => readLayout(localStorage));
  const [drag, setDrag] = useState<Drag | null>(null);

  const nodes = projects.flatMap((project) =>
    project.slots
      .filter((slot) => !cancelled.has(ownerKey(slot)))
      .map((slot) => ({
        project,
        slot,
        key: ownerKey(slot),
        detail: slotDetail(slot, jobs, runs),
      })),
  );

  const keys = nodes.map((node) => node.key);
  const positions = positionsFor(keys, saved);
  // The node under the hand is drawn from the gesture, and everything else from the layout. The
  // gesture's own position is kept RAW in state and clamped only here: clamping the state would
  // make a node dragged past the left edge and back again lag the pointer by however far it went.
  const positionOf = (key: string): Point =>
    drag !== null && drag.key === key ? clamped(drag.at) : positions[key];

  function grab(key: string, event: React.PointerEvent<HTMLElement>) {
    // Left button only. A right-click opens a menu, and a node that follows the pointer afterwards
    // is a node nobody can let go of.
    if (event.button !== 0) return;
    // `?.()` because jsdom has no pointer capture: without it the tests fail in a place that has
    // nothing to do with what they are testing. In a browser this is what makes the release arrive
    // even when the cursor has left the surface.
    event.currentTarget.setPointerCapture?.(event.pointerId);
    const from = positionOf(key);
    setDrag({ key, from, origin: { x: event.clientX, y: event.clientY }, at: from });
  }

  function move(event: React.PointerEvent<HTMLElement>) {
    if (drag === null) return;
    setDrag({
      ...drag,
      at: {
        x: drag.from.x + (event.clientX - drag.origin.x),
        y: drag.from.y + (event.clientY - drag.origin.y),
      },
    });
  }

  /**
   * Ends the gesture, and writes ONCE — on the drop and not on every frame, which is one write per
   * gesture instead of sixty a second.
   *
   * A press that never moved writes nothing at all. Writing anyway would freeze the derived fallback
   * into storage the first time anybody touched a card, trading a position that improves whenever
   * `fallbackPosition` does for one that is stuck, in exchange for a gesture nobody made.
   */
  function drop() {
    if (drag === null) return;
    const moved = drag.at.x !== drag.from.x || drag.at.y !== drag.from.y;
    setDrag(null);
    if (!moved) return;
    const next = withMoved(saved, drag.key, drag.at);
    setSaved(next);
    // Pruned on the write: without it the stored object grows by one entry per job for the life of
    // the machine, and none of the dead ones is ever asked about again.
    writeLayout(localStorage, pruned(next, keys));
  }

  // Who a job's neighbours are, PER PROJECT. The daemon refuses an exclusion across two projects, so
  // a canvas that treated the whole surface as one neighbourhood would offer the action on cards
  // where its only possible outcome is a refusal.
  const neighbours = new Map<string, number[]>();
  for (const node of nodes) {
    if (node.detail.kind !== "job") continue;
    const found = neighbours.get(node.project.project_id) ?? [];
    found.push(node.detail.job.id);
    neighbours.set(node.project.project_id, found);
  }

  // Where every node is being drawn RIGHT NOW, including the one under the hand. The lines are
  // built from this rather than from the saved layout, so an edge stays attached to the card being
  // dragged instead of to where the card used to be.
  const drawn: Layout = {};
  for (const key of keys) drawn[key] = positionOf(key);
  const lines = edgeGeometry(edges, drawn, NODE);

  // How far the surface has to reach. Derived from where the nodes actually are, so a node dragged
  // into the far corner stays reachable instead of being clipped out of the world.
  const extent = nodes.reduce(
    (far, node) => ({
      width: Math.max(far.width, positionOf(node.key).x + NODE.width),
      height: Math.max(far.height, positionOf(node.key).y + NODE.height),
    }),
    FLOOR,
  );

  return (
    <div className="fleet-canvas">
      {/* The banner and not a state on the card: the card being picked FROM can leave the surface
          mid-pick — its job ends — and the way out has to survive that. */}
      {pairing !== null && (
        <p className="fleet-pairing">
          Pick the job that must not run at the same time as job {pairing}.{" "}
          <Button size="sm" onClick={neverMind}>
            Never mind
          </Button>
        </p>
      )}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      {closed !== null && <p className="fleet-closed">{closed}</p>}
      <div
        className="fleet-surface"
        style={{ minWidth: extent.width, minHeight: extent.height }}
        // The move and the release listen HERE rather than on the node: with the pointer captured
        // the events are delivered to the header that took the capture, and they bubble to this.
        // A cancel — the system taking the gesture away — puts the node back rather than saving a
        // move nobody finished making.
        onPointerMove={move}
        onPointerUp={drop}
        onPointerCancel={() => setDrag(null)}
      >
        {/* OVER the cards, and deaf to the pointer.
            Under them it read wrong: with a third card sitting between the two ends, the line went
            in one side of the innocent card and out the other, and what a person saw was an
            exclusion between the wrong pair — on the one feature whose entire subject is WHICH TWO.
            `pointer-events: none` is what keeps the buttons underneath clickable, and it is the
            whole of the argument for putting the layer below; crossing a card is legible, ending on
            one that is not yours is not.
            The dot at each end says where the line belongs, so a card it merely crosses is never
            mistaken for one it joins. */}
        <svg
          className="fleet-wires"
          width={extent.width}
          height={extent.height}
          aria-hidden="true"
        >
          {lines.map((line) => (
            <g key={line.id} data-edge={line.id} className={`fleet-wire is-${line.state}`}>
              <line x1={line.from.x} y1={line.from.y} x2={line.to.x} y2={line.to.y} />
              <circle cx={line.from.x} cy={line.from.y} r={4} />
              <circle cx={line.to.x} cy={line.to.y} r={4} />
            </g>
          ))}
        </svg>
        {nodes.map(({ project, slot, key, detail }) => {
          const jobId = detail.kind === "job" ? detail.job.id : null;
          const partners = jobId === null ? [] : partnersOf(edges, jobId);
          const at = positionOf(key);
          return (
            <div
              key={key}
              className={`fleet-node${drag !== null && drag.key === key ? " is-dragging" : ""}`}
              data-node={key}
              style={{ transform: `translate(${at.x}px, ${at.y}px)` }}
            >
              <SlotCard
                slot={slot}
                detail={detail}
                badges={collisionBadges(project, { kind: slot.owner_kind, id: slot.owner_id })}
                partners={partners}
                pairing={roleFor(jobId, partners, neighbours.get(project.project_id) ?? [])}
                onPair={() => {
                  if (jobId !== null) pick(jobId);
                }}
                onLift={(id) => void lift(id)}
                onDecide={(proposalId, yes) => void decide(proposalId, yes)}
                token={token}
                onCancel={() => void onCancel(slot)}
                onOpenRuns={onOpenRuns}
                onGrab={(event) => grab(key, event)}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
