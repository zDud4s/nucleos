import type { ReactNode } from "react";
import { isApiRefusal } from "../../data/client";
import {
  useLoadoutTools,
  useRevokeLoadoutTool,
  type LoadoutTool,
} from "../../data/loadout-tools";
import { Button, ErrorNote, Quiet, RefusalNote, RelativeTime, Row, Rows } from "../../ui";
import "./knowledge.css";

export interface ScopedToolsProps {
  ownerKind: "agent" | "team";
  ownerId: string;
}

/**
 * The tools one agent or one team holds beyond its box's base: the active
 * rows of exactly that owner, with where each came from and a way to take it
 * back. Waiting requests are only counted — deciding them is the Brain's job.
 */
export function ScopedTools({ ownerKind, ownerId }: ScopedToolsProps) {
  const owner = { kind: ownerKind, id: ownerId };
  const active = useLoadoutTools("active", owner);
  const waiting = useLoadoutTools("proposed", owner);
  const revoke = useRevokeLoadoutTool();

  let body: ReactNode;
  if (active.isError) {
    body = <Quiet says="the núcleo did not answer — these tools are unknown" />;
  } else if (active.data === undefined) {
    body = <Quiet says="Loading…" />;
  } else {
    const rows = active.data.filter((row) => row.owner_kind === ownerKind && row.owner_id === ownerId);
    const waitingCount = (waiting.data ?? []).length;
    body = (
      <>
        {rows.length === 0 ? (
          <Quiet says="No tool beyond the base has been approved here yet." />
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
        {waitingCount > 0 && (
          <p className="learned-lede">{waitingCount} more waiting for you in the Brain</p>
        )}
      </>
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
        <Button
          aria-label={`Revoke ${row.tool}`}
          onClick={onRevoke}
          disabled={deciding}
        >
          Revoke
        </Button>
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
    "this one was already decided, or is no longer active — the list clears it on the next read",
  not_found: "that one is gone; there is nothing left to decide",
  unprocessable: "the núcleo found no such team — nothing was changed",
  internal: "the núcleo failed while carrying the decision out — nothing was changed",
};

/** What the tool doors say when they say no; shared by the section and the Brain's queue. */
export function ToolRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={TOOL_SENTENCES} />;
}
