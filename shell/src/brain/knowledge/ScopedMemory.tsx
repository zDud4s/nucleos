import type { ReactNode } from "react";
import { useKnowledge } from "../../data/knowledge";
import { Quiet, Rows } from "../../ui";
import { KnownRow } from "./KnownRow";
import "./knowledge.css";

export interface ScopedMemoryProps {
  scopeKind: "agent" | "team";
  scopeId: string;
}

/**
 * What one agent or one team knows on its own: the active knowledge rows of
 * exactly that scope. Waiting rows are only counted, never listed — deciding
 * them is the Brain's job, and a second place to decide would be a second
 * place to get the decision wrong.
 */
export function ScopedMemory({ scopeKind, scopeId }: ScopedMemoryProps) {
  const knowledge = useKnowledge();

  let body: ReactNode;
  if (knowledge.isError) {
    body = <Quiet says="the núcleo did not answer — this memory is unknown" />;
  } else if (knowledge.data === undefined) {
    body = <Quiet says="Loading…" />;
  } else {
    const inScope = knowledge.data.filter(
      (row) => row.scope_kind === scopeKind && row.scope_id === scopeId,
    );
    const active = inScope.filter((row) => row.status === "active");
    const waiting = inScope.filter((row) => row.status === "proposed").length;
    body = (
      <>
        {active.length === 0 ? (
          <Quiet says="Nothing is known in this scope yet." />
        ) : (
          <Rows label={`Memory of ${scopeKind} ${scopeId}`}>
            {active.map((row) => (
              <KnownRow key={row.id} row={row} />
            ))}
          </Rows>
        )}
        {waiting > 0 && <p className="learned-lede">{waiting} more waiting for you in the Brain</p>}
      </>
    );
  }

  return (
    <section className="learned-group" aria-label="Memory">
      <h3>Memory</h3>
      {body}
    </section>
  );
}
