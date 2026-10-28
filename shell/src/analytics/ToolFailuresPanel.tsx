import type { DevtimeToolFailures } from "../data/devtime";
import { percentText } from "../lib/format";
import { plural } from "../lib/plural";
import { ShareBar, Table, Td, Th } from "../ui";
import { classShares, errorClassLabel, programText, topTools } from "./tool-failures";

/**
 * Tool failures (metric 7, follow-up A): which tool calls of the owner's sessions failed, by class and
 * by tool. Programs only — never full arguments, a worktree or a session (spec analytics-devtime §8).
 */
export function ToolFailuresPanel({ failures }: { failures: DevtimeToolFailures }) {
  const tools = topTools(failures.by_tool);
  const extras = [
    failures.interrupted > 0 ? `${failures.interrupted} interrupted` : "",
    failures.unresolved > 0 ? `${failures.unresolved} without a result` : "",
  ].filter((clause) => clause !== "");
  return (
    <section aria-label="Tool failures">
      <p className="an-dt-share-title">Tool failures</p>
      {failures.failures === 0 ? (
        <p className="an-note">{failures.attempts > 0 ? "No failed tool calls in this window" : "No finished tool calls in this window"}</p>
      ) : (
        <p className="an-note">
          {`${failures.failures} of ${plural(failures.attempts, "tool call")} failed (${percentText(failures.failure_rate)})`}
        </p>
      )}
      {extras.length > 0 ? <p className="an-note">{extras.join(" · ")}</p> : null}
      {failures.failures > 0 ? (
        <ShareBar items={classShares(failures.by_error_class)} format={(value) => plural(value, "failure")}
                  label="Failures by error class" />
      ) : null}
      {tools.length > 0 ? (
        <Table label="Failures by tool">
          <thead>
            <tr>
              <Th>Tool</Th>
              <Th>Program</Th>
              <Th num>Calls</Th>
              <Th num>Failed</Th>
              <Th num>Rate</Th>
              <Th>Most common</Th>
            </tr>
          </thead>
          <tbody>
            {tools.map((row) => (
              <tr key={row.key}>
                <Th scope="row">{row.label}</Th>
                <Td>{programText(row.cmd_program)}</Td>
                <Td num>{String(row.attempts)}</Td>
                <Td num>{String(row.failures)}</Td>
                <Td num>{percentText(row.failure_rate)}</Td>
                <Td>{row.top_error_class === null ? "—" : errorClassLabel(row.top_error_class)}</Td>
              </tr>
            ))}
          </tbody>
        </Table>
      ) : null}
    </section>
  );
}
