import { useEffect, useState } from "react";

import { getJob, getSkippedItems, type JobDetail, type JobItem, type Proposal } from "./api";

interface JobGraphProps {
  token: string;
  jobId: number;
  /** A finished job never changes again; polling it would be a request per tick for nothing. */
  live: boolean;
}

/**
 * The second zoom: a job as the chain of items it is running.
 *
 * It fetches while it is open AND the job is live — the same rule Autopilot's `JobRow` keeps, and
 * for the same reason: asking for every job's queue on every tick would multiply the poll by the
 * number of cards on screen in order to show rows nobody opened. This component is only mounted
 * when a card opens, so "open" is the mounting.
 */
export default function JobGraph({ token, jobId, live }: JobGraphProps) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [skipped, setSkipped] = useState<Proposal[] | null>(null);

  useEffect(() => {
    let current = true;
    const read = () => {
      void getJob(token, jobId).then((next) => {
        if (current) setDetail(next);
      });
    };
    read();
    if (!live) {
      return () => {
        current = false;
      };
    }
    const id = setInterval(read, 3000);
    return () => {
      current = false;
      clearInterval(id);
    };
  }, [jobId, live, token]);

  // Only when there is a skipped item. The route is global and takes no per-job filter, so asking
  // for it always would pull the whole approval queue in order to show nothing.
  const hasSkipped = detail?.items.some((entry) => entry.status === "skipped") ?? false;
  useEffect(() => {
    if (!hasSkipped) return;
    let current = true;
    void getSkippedItems(token).then((next) => {
      if (current) setSkipped(next);
    });
    return () => {
      current = false;
    };
  }, [hasSkipped, token]);

  if (detail === null) return <p className="item-note">Loading its list…</p>;
  if (detail.items.length === 0) {
    return <p className="item-note">Its planner looked and found no work.</p>;
  }

  return (
    <ol className="item-chain">
      {detail.items.map((entry, index) => (
        <li key={entry.ordinal}>
          {/* The mark goes on the FIRST item of a new round, which is the boundary a reader is
              looking for. Ordinals carry on across rounds, so the number itself says nothing. */}
          {index > 0 && entry.round !== detail.items[index - 1].round && (
            <p className="item-round">round {entry.round + 1}</p>
          )}
          <div className="item-row">
            <span className="item-row__ordinal">{entry.ordinal + 1}</span>
            <span className="item-row__what">{entry.description}</span>
            <span className="item-row__reading">{reading(entry)}</span>
            {entry.run_id !== null && <span className="item-row__run">run {entry.run_id}</span>}
          </div>
          {entry.status === "skipped" && <SkippedNote item={entry} skipped={skipped} />}
        </li>
      ))}
    </ol>
  );
}

/**
 * What one item's row says.
 *
 * The nine states `item_state_from` distinguishes yield TEN readings. The tenth does not come from
 * `status`: it comes from `gate_status`, a second column that function never sees. An item reading
 * `passed` with no gate status was never measured — the project has no gate command, or it was an
 * intermediate item under `gate_after_each_item: false` — and collapsing it into `passed` is the
 * same class of error as saying `clean` without having measured.
 *
 * The three gate literals are `"passed"`, `"failed"` and `"errored"` — deliberately NOT the same
 * strings as the item's own `status`.
 */
function reading(item: JobItem): string {
  switch (item.status) {
    case "pending":
      return "to do";
    case "running":
      return "running";
    case "implemented":
      return "written, not yet measured";
    case "passed":
      return item.gate_status === "passed" ? "passed" : "not measured";
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
    default:
      // The core reads an unknown status as still-to-do rather than as done, and so does this.
      return "to do";
  }
}

/**
 * The proposal that explains a skipped item, or what can be said in its absence.
 *
 * **Three states, not two.** `skipped === null` means *not read yet* **or** *the read failed* —
 * `getSkippedItems` answers `null` in both cases and the effect does not retry. Collapsing that
 * with "the list arrived and had nothing in it" would make the first paint, and a dead daemon,
 * both claim the proposal had been put away. It is the same class of error the collision warning's
 * `not_measured` exists not to commit.
 */
function explanationFor(
  item: JobItem,
  skipped: Proposal[] | null,
): { kind: "unread" } | { kind: "gone" } | { kind: "proposal"; proposal: Proposal } {
  if (skipped === null) return { kind: "unread" };
  // The run id has to exist on BOTH sides: `Proposal.run_id` and `JobItem.run_id` are each
  // `number | null`, and without this guard a proposal with no run would match an item that never
  // ran.
  const found =
    item.run_id === null
      ? undefined
      : skipped.find((candidate) => candidate.run_id === item.run_id);
  return found === undefined ? { kind: "gone" } : { kind: "proposal", proposal: found };
}

function SkippedNote({ item, skipped }: { item: JobItem; skipped: Proposal[] | null }) {
  const explanation = explanationFor(item, skipped);
  if (explanation.kind === "unread") return null;
  return (
    <p className="item-why">
      {explanation.kind === "gone"
        ? "the proposal that explained it has been put away"
        : explanation.proposal.reasoning}
    </p>
  );
}
