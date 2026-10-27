import { aboutYou, useKnowledge, useRevertKnowledge } from "../../data/knowledge";
import { ConfirmButton, Count, Panel, Rows } from "../../ui";
import { KnownRow } from "./KnownRow";
import { DecisionRefusal } from "./WaitingPanel";
import "./knowledge.css";

/**
 * What the agent holds about the owner and about every project, in force, with a way to
 * revoke each row. Draws nothing when there is nothing to show: the section is not a
 * promise that the agent knows something about you.
 */
export function AboutYou() {
  const rows = aboutYou(useKnowledge().data ?? []);
  const revert = useRevertKnowledge();
  if (rows.length === 0) return null;
  return (
    <Panel title="About you" aside={<Count n={rows.length} />}>
      <p className="learned-lede">
        What the agent holds about you and about every project. Revoke any of it: it stays on the
        record and does not come back.
      </p>
      {revert.error !== null && <DecisionRefusal error={revert.error} />}
      <Rows label="About you">
        {rows.map((row) => (
          <KnownRow
            key={row.id}
            row={row}
            decisions={
              <ConfirmButton
                label="Revert"
                confirmLabel="It no longer applies"
                variant="quiet"
                disabled={revert.isPending}
                onConfirm={() => revert.mutate(row.id)}
              />
            }
          />
        ))}
      </Rows>
    </Panel>
  );
}
