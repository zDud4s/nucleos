import type { DevtimeAgents } from "../data/devtime";
import { percentText } from "../lib/format";
import { plural } from "../lib/plural";
import { Table, Td, Th } from "../ui";
import { agentTypeText, checkedNote, medianText, passText, roleText, topAgentTypes } from "./agent-effectiveness";

/**
 * Agent effectiveness (metric 10, follow-up C): how the owner's subagent runs ended, by type, and how
 * often an implementer's work passed the next check. Types and counts only — never a session, an agent
 * id, a worktree or a prompt (spec analytics-devtime §8).
 */
export function AgentEffectivenessPanel({ agents }: { agents: DevtimeAgents }) {
  const types = topAgentTypes(agents.by_type);
  const extras = [
    agents.outcome.interrupted > 0 ? `${agents.outcome.interrupted} interrupted` : "",
    agents.outcome.other > 0 ? `${agents.outcome.other} ended otherwise` : "",
  ].filter((clause) => clause !== "");
  return (
    <section aria-label="Agent effectiveness">
      <p className="an-dt-share-title">Agent effectiveness</p>
      <p className="an-note">
        {`${plural(agents.attempts, "agent run")} · ${agents.outcome.failed} failed (${percentText(agents.failure_rate)})`}
      </p>
      {extras.length > 0 ? <p className="an-note">{extras.join(" · ")}</p> : null}
      {types.length > 0 ? (
        <Table label="Agents by type">
          <thead>
            <tr>
              <Th>Type</Th>
              <Th>Role</Th>
              <Th num>Runs</Th>
              <Th num>Failed</Th>
              <Th num>Checks passed</Th>
              <Th num>Median time</Th>
            </tr>
          </thead>
          <tbody>
            {types.map((row) => (
              <tr key={row.agent_type}>
                <Th scope="row">{agentTypeText(row.agent_type)}</Th>
                <Td>{roleText(row.role)}</Td>
                <Td num>{String(row.attempts)}</Td>
                <Td num>{String(row.outcome.failed)}</Td>
                <Td num>{passText(row)}</Td>
                <Td num>{medianText(row)}</Td>
              </tr>
            ))}
          </tbody>
        </Table>
      ) : null}
      <p className="an-note">{checkedNote(agents)}</p>
    </section>
  );
}
