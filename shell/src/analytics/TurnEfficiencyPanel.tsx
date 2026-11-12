import type { DevtimeTurnClasses, DevtimeUnexplained, UnexplainedItem } from "../data/devtime";
import { humanMinutes } from "../data/project-readings";
import { percentText } from "../lib/format";
import { Table, Td, Th } from "../ui";
import { classLabel, criteriaText, explainedText, perCallText, slowTurns } from "./turn-efficiency";

const minutes = (ms: number) => humanMinutes(Math.round(ms / 60_000));

/**
 * Turn efficiency (metric 9, follow-up B): the owner's turns by size (tool calls per turn) and the slow
 * turns no rule explains. A turn is located by project and day only — never a session, a worktree or a
 * prompt (spec analytics-devtime §8).
 */
export function TurnEfficiencyPanel({ classes, unexplained, showProject }: {
  classes: DevtimeTurnClasses;
  unexplained?: DevtimeUnexplained;
  showProject: boolean;
}) {
  const labels = new Map(classes.classes.map((row) => [row.turn_class, classLabel(row)]));
  const sizeOf = (item: UnexplainedItem) => labels.get(item.turn_class) ?? item.turn_class;
  const slow = unexplained === undefined || unexplained.turns === 0 ? [] : slowTurns(unexplained.items);
  return (
    <section aria-label="Turn efficiency">
      <p className="an-dt-share-title">Turn efficiency</p>
      <Table label="Turns by size">
        <thead>
          <tr>
            <Th>Size</Th>
            <Th num>Turns</Th>
            <Th num>Median active</Th>
            <Th num>Per call</Th>
            <Th num>Explained</Th>
            <Th num>Slow</Th>
          </tr>
        </thead>
        <tbody>
          {classes.classes.map((row) => (
            <tr key={row.turn_class}>
              <Th scope="row">{classLabel(row)}</Th>
              <Td num>{String(row.turns)}</Td>
              <Td num>{minutes(row.median_active_ms)}</Td>
              <Td num>{perCallText(row.ms_per_call)}</Td>
              <Td num>{explainedText(row)}</Td>
              <Td num>{String(row.unexplained_turns)}</Td>
            </tr>
          ))}
        </tbody>
      </Table>
      {unexplained === undefined || slow.length === 0 ? (
        <p className="an-note">No slow turn without an explanation in this window</p>
      ) : (
        <>
          <Table label="Slow turns no rule explains">
            <thead>
              <tr>
                <Th>Day</Th>
                {showProject ? <Th>Project</Th> : null}
                <Th>Size</Th>
                <Th num>Calls</Th>
                <Th num>Active</Th>
                <Th num>Class median</Th>
                <Th num>Explained</Th>
              </tr>
            </thead>
            <tbody>
              {slow.map((item, index) => (
                <tr key={`${item.started_at}-${item.project_id}-${index}`}>
                  <Td>{item.day}</Td>
                  {showProject ? <Td>{item.project_id}</Td> : null}
                  <Td>{sizeOf(item)}</Td>
                  <Td num>{String(item.calls)}</Td>
                  <Td num>{minutes(item.active_ms)}</Td>
                  <Td num>{minutes(item.class_median_ms)}</Td>
                  <Td num>{percentText(item.active_ms > 0 ? item.explained_ms / item.active_ms : null)}</Td>
                </tr>
              ))}
            </tbody>
          </Table>
          {unexplained.turns > slow.length ? (
            <p className="an-note">{`Showing ${slow.length} of ${unexplained.turns}`}</p>
          ) : null}
          <p className="an-note">{criteriaText(unexplained.criteria)}</p>
        </>
      )}
    </section>
  );
}
