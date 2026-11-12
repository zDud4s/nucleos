import { measuredByGenerator, type Known } from "../../data/knowledge";
import { Count, Panel, Row, Rows } from "../../ui";
import "./knowledge.css";

export interface MeasuredSummaryProps {
  /** The rows to summarise (already scoped); measured ones are picked out here. */
  rows: Known[];
}

/** Measured rows counted per scope and per generator. Draws nothing when there are none. */
export function MeasuredSummary({ rows }: MeasuredSummaryProps) {
  const measured = measuredByGenerator(rows);
  const measuredTotal = measured.reduce((sum, group) => sum + group.total, 0);
  if (measured.length === 0) return null;

  return (
    <Panel title="Measured, by generator" aside={<Count n={measuredTotal} />}>
      <Rows label="Measured, by generator">
        {measured.map((group) => (
          <Row key={group.scope} className="learned-measured-row">
            <span className="learned-scope">{group.scope}</span>
            {group.byGenerator.map((entry) => (
              <span key={entry.generator}>
                {entry.generator}: {entry.count}
              </span>
            ))}
          </Row>
        ))}
      </Rows>
    </Panel>
  );
}
