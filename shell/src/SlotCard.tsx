import { useState } from "react";

import { type HeldSlot } from "./api";
import { jobIsLive } from "./derive";
import { type PairingRole } from "./fleet-actions";
import { type CollisionBadge, type Partner, type SlotDetail } from "./fleet-derive";
import JobGraph from "./JobGraph";
import { Button, ConfirmButton } from "./ui";

interface SlotCardProps {
  slot: HeldSlot;
  detail: SlotDetail;
  badges: CollisionBadge[];
  /** The exclusions this card's owner is named in, from its own end. */
  partners: Partner[];
  pairing: PairingRole;
  onPair: () => void;
  onLift: (exclusionId: number) => void;
  /** Answers a request: `true` puts the rule in force, `false` refuses it. */
  onDecide: (proposalId: number, yes: boolean) => void;
  /** For the `JobGraph` this card mounts when it opens. */
  token: string;
  onCancel: () => void;
  onOpenRuns: () => void;
}

/**
 * ONE slot.
 *
 * It lives in its own module because two views mount it — the column and the canvas — and importing
 * it from `Fleet.tsx` would make a cycle (`Fleet` → `FleetCanvas` → `Fleet`). A cycle here is how a
 * screen ends up with two subtly different cards.
 *
 * `wait_reason` and `round`/`max_rounds` appear only when the owner is a job: they are columns of
 * `jobs`, and `RunSearchResult` has no equivalent. Showing them as zero for a run would be
 * inventing a number.
 *
 * Pure, **except** for the `JobGraph` it mounts when the card opens — hence the `token`.
 *
 * **It has no "cancelling" state.** Cancelling takes the owner out of the column at the instant of
 * the click, so a card halfway through a cancel does not exist to be drawn.
 */
export function SlotCard({
  slot,
  detail,
  badges,
  partners,
  pairing,
  onPair,
  onLift,
  onDecide,
  token,
  onCancel,
  onOpenRuns,
}: SlotCardProps) {
  // The open state lives here rather than above, as it does in Autopilot's `JobRow`: opening one
  // card says nothing to the others, and lifting it would re-render the whole column on every
  // keystroke elsewhere in it.
  const [open, setOpen] = useState(false);
  const modifier =
    detail.kind === "unknown" ? " is-unknown" : detail.kind === "orphaned" ? " is-orphaned" : "";

  return (
    <article className={`slot-card${modifier}`}>
      <header>
        <span className="slot-number">slot {slot.slot}</span>
        <span className="slot-owner">
          {slot.owner_kind} {slot.owner_id}
        </span>
      </header>
      {detail.kind === "job" && (
        <>
          <p className="slot-status">
            {detail.job.status}
            {detail.job.wait_reason !== null && ` — ${detail.job.wait_reason}`}
          </p>
          <p className="slot-rounds">
            round {detail.job.round + 1} of {detail.job.max_rounds}
          </p>
          <Button size="sm" onClick={() => setOpen((current) => !current)}>
            {open ? "Hide items" : "Show items"}
          </Button>
          {open && (
            <JobGraph token={token} jobId={detail.job.id} live={jobIsLive(detail.job.status)} />
          )}
        </>
      )}
      {detail.kind === "run" && (
        <>
          <p className="slot-status">{detail.run.status}</p>
          <p className="slot-prompt">{detail.run.prompt_excerpt}</p>
          <Button size="sm" onClick={onOpenRuns}>
            Open in Runs
          </Button>
        </>
      )}
      {/* The same words for two different reasons — a listing that failed and a listing that came
          back full — because from here they are the same fact: nothing described this owner. Only
          the orphaned line changes its word, because that one is a sign of a defect. */}
      {detail.kind === "unknown" && <p className="slot-status">detail unavailable</p>}
      {detail.kind === "orphaned" && <p className="slot-status">slot awaiting reconciliation</p>}
      {badges.map((badge) => (
        <p
          key={badge.source}
          className={`collide-badge is-${badge.source}${badge.state === "not_measured" ? " is-unmeasured" : ""}`}
        >
          {/* A label and not colour alone: the two sources have to be distinguishable by anyone. */}
          <span className="collide-source">{badge.source}</span>{" "}
          {badge.state === "not_measured"
            ? "not measured"
            : `also touched by ${badge.others
                .map((other) => `${other.kind} ${other.id}`)
                .join(", ")}: ${badge.paths.join(", ")}`}
        </p>
      ))}
      {partners.map((partner) => (
        <p
          key={`${partner.state}-${partner.id}`}
          className={`exclude-edge is-${partner.state}`}
        >
          {edgeLine(partner)}
          {partner.state === "active" ? (
            <Button size="sm" onClick={() => onLift(partner.id)}>
              Lift
            </Button>
          ) : (
            // The same request is drawn on both cards, so both carry the answer. Whichever is
            // clicked decides the one proposal; the other card's copy leaves on the next tick.
            <>
              <Button size="sm" variant="approve" onClick={() => onDecide(partner.id, true)}>
                Approve
              </Button>
              <Button size="sm" variant="link" onClick={() => onDecide(partner.id, false)}>
                Refuse
              </Button>
            </>
          )}
        </p>
      ))}
      {pairing !== "none" && (
        <Button size="sm" onClick={onPair} disabled={pairing === "picking"}>
          {pairing === "target"
            ? "…as this one"
            : pairing === "picking"
              ? "Picking…"
              : "Not at the same time as…"}
        </Button>
      )}
      <ConfirmButton
        variant="danger"
        size="sm"
        confirmLabel="Confirm cancel?"
        onConfirm={onCancel}
      >
        Cancel
      </ConfirmButton>
    </article>
  );
}

/**
 * What one edge says from this end of it.
 *
 * A pending edge is careful to claim nothing: it has changed nothing about how either job is
 * scheduled, and until somebody approves it on the Autopilot tab it never will.
 *
 * An active one names which of the two waits rather than saying "held" — the wait only happens while
 * the partner actually holds a slot, and this card is drawn whether it does or not.
 */
function edgeLine(partner: Partner): string {
  if (partner.state === "pending") {
    return `asked: not at the same time as job ${partner.partner} — waiting for approval`;
  }
  return partner.waits
    ? `not at the same time as job ${partner.partner} — this one waits`
    : `not at the same time as job ${partner.partner} — that one waits`;
}
