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
import { positionsFor, readLayout, type Layout } from "./fleet-layout";
import { SlotCard } from "./SlotCard";
import { Button, ErrorNote } from "./ui";

/**
 * The nominal size of a node.
 *
 * Nominal because a card's real height depends on what it has to say — an open item graph is much
 * taller than a waiting job — and nothing here needs the real one. It is used to work out how far
 * the surface has to reach so a node dragged into the corner can still be scrolled to.
 */
const NODE = { width: 260, height: 170 };

/** Never smaller than this, so an empty or nearly empty canvas still looks like a surface. */
const FLOOR = { width: 640, height: 520 };

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
  const [saved] = useState<Layout>(() => readLayout(localStorage));

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

  const positions = positionsFor(
    nodes.map((node) => node.key),
    saved,
  );

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

  // How far the surface has to reach. Derived from where the nodes actually are, so a node dragged
  // into the far corner stays reachable instead of being clipped out of the world.
  const extent = nodes.reduce(
    (far, node) => ({
      width: Math.max(far.width, positions[node.key].x + NODE.width),
      height: Math.max(far.height, positions[node.key].y + NODE.height),
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
      >
        {nodes.map(({ project, slot, key, detail }) => {
          const jobId = detail.kind === "job" ? detail.job.id : null;
          const partners = jobId === null ? [] : partnersOf(edges, jobId);
          const at = positions[key];
          return (
            <div
              key={key}
              className="fleet-node"
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
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
