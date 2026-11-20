import { DAEMON_NAME } from "../../copy";
import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { useScopedKnowledge } from "../../data/knowledge";
import { ErrorNote, Quiet, Rows, SectionHead, Skeleton } from "../../ui";
import { KnownRow } from "./KnownRow";
import "./knowledge.css";

export interface ScopedMemoryProps {
  scopeKind: "agent" | "team";
  scopeId: string;
  /** `section`: the compact header row of a panel that holds several sections (the agent page). */
  variant?: "section";
}

/**
 * What one agent or one team knows on its own: the active knowledge rows of
 * exactly that scope. Waiting rows are only counted, never listed — deciding
 * them is the Brain's job, and a second place to decide would be a second
 * place to get the decision wrong.
 */
export function ScopedMemory({ scopeKind, scopeId, variant }: ScopedMemoryProps) {
  const compact = variant === "section";
  const knowledge = useScopedKnowledge(scopeKind, scopeId);

  let body: ReactNode;
  let count: ReactNode;
  let waitingCount = 0;
  if (knowledge.isError) {
    body = (
      <ErrorNote onRetry={() => void knowledge.refetch()}>the {DAEMON_NAME} is not responding: this memory is unknown</ErrorNote>
    );
  } else if (knowledge.data === undefined) {
    body = <Skeleton label="Loading memory…" lines={2} />;
  } else {
    const inScope = knowledge.data.filter(
      (row) => row.scope_kind === scopeKind && row.scope_id === scopeId,
    );
    const active = inScope.filter((row) => row.status === "active");
    const waiting = inScope.filter((row) => row.status === "proposed").length;
    waitingCount = waiting;
    count = active.length === 0 ? undefined : active.length;
    body = (
      <>
        {active.length === 0 ? (
          <Quiet compact={compact} says={compact ? "None" : "Nothing is known in this scope yet."} />
        ) : (
          <Rows label={`Memory of ${scopeKind} ${scopeId}`}>
            {active.map((row) => (
              <KnownRow key={row.id} row={row} />
            ))}
          </Rows>
        )}
        {waiting > 0 && !compact && <p className="learned-lede">{waiting} more waiting for you in the Brain</p>}
      </>
    );
  }

  if (compact) {
    return (
      <section className="learned-group learned-section" aria-label="Memory">
        <SectionHead
          title="Memory"
          count={count}
          aside={waitingCount > 0 ? <WaitingLink count={waitingCount} /> : undefined}
        />
        {body}
      </section>
    );
  }

  return (
    <section className="learned-group learned-memory" aria-label="Memory">
      <h3>Memory</h3>
      {body}
    </section>
  );
}

/** "N waiting" in a section header: the Brain is where they are decided. */
export function WaitingLink({ count }: { count: number }) {
  return (
    <Link to="/brain" className="learned-waiting" title={`${count} more waiting for you in the Brain`}>
      {count} waiting
    </Link>
  );
}
