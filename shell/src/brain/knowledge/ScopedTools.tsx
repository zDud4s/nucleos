import { DAEMON_NAME } from "../../copy";
import type { ReactNode } from "react";
import { isApiRefusal } from "../../data/client";
import {
  useLoadoutTools,
  useRevokeLoadoutTool,
  type LoadoutTool,
} from "../../data/loadout-tools";
import { ConfirmButton, ErrorNote, Quiet, RefusalNote, RelativeTime, Row, Rows, SectionHead, Skeleton } from "../../ui";
import { WaitingLink } from "./ScopedMemory";
import "./knowledge.css";

export interface ScopedToolsProps {
  ownerKind: "agent" | "team";
  ownerId: string;
  /** `section`: the compact header row of a panel that holds several sections (the agent page). */
  variant?: "section";
}

/**
 * The tools one agent or one team holds beyond its box's base: the active
 * rows of exactly that owner, with where each came from and a way to take it
 * back. Waiting requests are only counted — deciding them is the Brain's job.
 */
export function ScopedTools({ ownerKind, ownerId, variant }: ScopedToolsProps) {
  const compact = variant === "section";
  const owner = { kind: ownerKind, id: ownerId };
  const active = useLoadoutTools("active", owner);
  const waiting = useLoadoutTools("proposed", owner);
  const revoke = useRevokeLoadoutTool();

  let body: ReactNode;
  let count: ReactNode;
  let waitingTotal = 0;
  if (active.isError) {
    body = (
      <ErrorNote onRetry={() => void active.refetch()}>the {DAEMON_NAME} is not responding: these tools are unknown</ErrorNote>
    );
  } else if (active.data === undefined) {
    body = <Skeleton label="Loading tools…" lines={2} />;
  } else {
    const rows = active.data.filter((row) => row.owner_kind === ownerKind && row.owner_id === ownerId);
    const waitingCount = (waiting.data ?? []).length;
    waitingTotal = waitingCount;
    count = rows.length === 0 ? undefined : rows.length;
    body = (
      <>
        {rows.length === 0 ? (
          compact ? (
            <Quiet compact says="None" />
          ) : (
            <Quiet says="No extra tools">
              <p>No tool beyond the base has been approved here yet.</p>
            </Quiet>
          )
        ) : (
          <Rows label={`Tools of ${ownerKind} ${ownerId}`}>
            {rows.map((row) => (
              <ToolRow
                key={row.id}
                row={row}
                onRevoke={() => revoke.mutate(row.id)}
                deciding={revoke.isPending}
              />
            ))}
          </Rows>
        )}
        {revoke.isError && <ToolRefusal error={revoke.error} />}
        {waitingCount > 0 && !compact && (
          <p className="learned-lede">{waitingCount} more waiting for you in the Brain</p>
        )}
      </>
    );
  }

  if (compact) {
    return (
      <section className="learned-group learned-section" aria-label="Tools">
        <SectionHead
          title="Tools"
          count={count}
          aside={waitingTotal > 0 ? <WaitingLink count={waitingTotal} /> : undefined}
        />
        {body}
      </section>
    );
  }

  return (
    <section className="learned-group learned-memory" aria-label="Tools">
      <h3>Tools</h3>
      {body}
    </section>
  );
}

function ToolRow({
  row,
  onRevoke,
  deciding,
}: {
  row: LoadoutTool;
  onRevoke: () => void;
  deciding: boolean;
}) {
  return (
    <Row className="learned-row">
      <div className="learned-head">
        <span className="learned-scope">{row.tool}</span>
        <span>{originOf(row)}</span>
        <span className="learned-when">
          <RelativeTime at={row.decided_at ?? row.created_at} />
        </span>
      </div>
      <div>
        <ConfirmButton
          label="Revoke"
          confirmLabel="Take this tool back"
          sayAs={`Revoke ${row.tool}`}
          variant="danger"
          disabled={deciding}
          onConfirm={onRevoke}
        />
      </div>
    </Row>
  );
}

/** A grant made here, or a run's request — with the run named, since that is where its reason lives. */
function originOf(row: LoadoutTool): string {
  if (row.source === "owner") return "added by you";
  return row.run_id === null ? "requested by a run" : `requested by run ${row.run_id}`;
}

const TOOL_SENTENCES: Record<string, string> = {
  conflict:
    "this one was already decided, or is no longer active. The list clears it on the next read",
  not_found: "that one is gone; there is nothing left to decide",
  unprocessable: `the ${DAEMON_NAME} found no such team, so nothing was changed`,
  internal: `the ${DAEMON_NAME} failed while carrying the decision out, so nothing was changed`,
};

/** What the tool doors say when they say no; shared by the section and the Brain's queue. */
export function ToolRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the {DAEMON_NAME} is not responding: nothing was decided</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={TOOL_SENTENCES} />;
}
