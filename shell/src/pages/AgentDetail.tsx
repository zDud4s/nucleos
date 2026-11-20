import { useId, useState, type ReactNode } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import { employmentOf, useAgents, useDeleteAgent, useUpdateAgent, type Agent } from "../data/agents";
import { useTeams } from "../data/teams";
import { ContextRefs } from "../context/ContextRefs";
import { ScopedMemory } from "../brain/knowledge/ScopedMemory";
import { ScopedTools } from "../brain/knowledge/ScopedTools";
import { Button, ConfirmButton, ErrorNote, Panel, Quiet, RefusalNote, Skeleton, Tip, toastManager } from "../ui";
import { DAEMON_NAME } from "../copy";
import {
  AgentFields,
  DeleteRefusal,
  SaveRefusal,
  draftFromAgent,
  draftIsValid,
  holdSentence,
  invalidReason,
  requestFromDraft,
  type AgentDraft,
} from "./Agents";
import { LoadoutPreview } from "./LoadoutPreview";
import "./agents.css";
import "./agent-detail.css";

/**
 * One agent, on a page of its own at `/agents/$agentId`.
 *
 * It replaced a modal with four tabs. The definition sits on the left and what
 * hangs off the agent (memory, context, tools, loadout) is stacked on the right,
 * so nothing is behind a tab and the two columns end at the same height.
 */
export function AgentDetail() {
  const params = useParams({ strict: false }) as { agentId?: string };
  const agentId = params.agentId ?? "";
  const agents = useAgents();
  const agent = agents.data?.find((row) => row.id === agentId);

  if (agent !== undefined) return <AgentPage key={agent.id} agent={agent} />;

  return (
    <>
      <DetailHeader title="Agent" />
      {agents.data === undefined && !agents.isError && <Skeleton label="Reading the agent" variant="row" count={3} />}
      {agents.data === undefined && agents.isError && (
        <>
          {isApiRefusal(agents.error) ? (
            <RefusalNote refusal={agents.error} />
          ) : (
            <ErrorNote onRetry={() => void agents.refetch()}>
              the {DAEMON_NAME} is not responding: nothing is known about this agent
            </ErrorNote>
          )}
        </>
      )}
      {agents.data !== undefined && <Quiet says="Not found" />}
    </>
  );
}

/** The back link, the name and whatever acts on the agent. */
function DetailHeader({ title, actions }: { title: string; actions?: ReactNode }) {
  return (
    <header className="ui-page-header">
      <div className="ui-page-header-text">
        <Link to="/agents" className="agent-detail-back">
          ← Agents
        </Link>
        <h1 className="ui-page-title">{title}</h1>
      </div>
      {actions === undefined ? null : <div className="ui-page-actions">{actions}</div>}
    </header>
  );
}

function AgentPage({ agent }: { agent: Agent }) {
  const navigate = useNavigate();
  const teams = useTeams();
  const del = useDeleteAgent();
  const employment = employmentOf(agent.id, teams.data ?? []);

  return (
    <>
      <DetailHeader
        title={agent.name}
        actions={
          <>
            <Tip name="What stands on this agent" tip={holdSentence(agent, employment, teams.data !== undefined)} />
            <ConfirmButton
              label="Delete"
              confirmLabel={`Delete ${agent.name}`}
              variant="danger"
              intent="stop"
              disabled={del.isPending}
              onConfirm={() => del.mutate(agent.id, { onSuccess: () => void navigate({ to: "/agents" }) })}
            />
          </>
        }
      />
      {del.isError && <DeleteRefusal error={del.error} />}

      <div className="agent-detail-grid">
        <div className="agent-detail-col">
          <Panel title="Definition">
            <DefinitionForm agent={agent} />
          </Panel>
        </div>
        <div className="agent-detail-col">
          <Panel>
            <ScopedMemory scopeKind="agent" scopeId={agent.id} />
          </Panel>
          <Panel>
            <ContextRefs ownerKind="agent" ownerId={agent.id} />
          </Panel>
          <Panel>
            <ScopedTools ownerKind="agent" ownerId={agent.id} />
          </Panel>
          <Panel>
            <LoadoutPreview agentId={agent.id} />
          </Panel>
        </div>
      </div>
    </>
  );
}

function DefinitionForm({ agent }: { agent: Agent }) {
  const [draft, setDraft] = useState<AgentDraft>(() => draftFromAgent(agent));
  const update = useUpdateAgent();
  const formId = useId();
  const valid = draftIsValid(draft);
  const saved = draftFromAgent(agent);
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);

  function patch(next: Partial<AgentDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  return (
    <>
      <form
        id={formId}
        className="agents-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!valid || update.isPending) return;
          // Reseeds from the daemon's answer: what shows next is what it stored (trimmed, a blank
          // model back to the engine's default).
          update.mutate(
            { id: agent.id, request: requestFromDraft(draft) },
            {
              onSuccess: (savedAgent) => {
                setDraft(draftFromAgent(savedAgent));
                toastManager.add({ type: "success", title: "Agent saved", description: savedAgent.name });
              },
            },
          );
        }}
      >
        <AgentFields draft={draft} onChange={patch} />
        <div className="agent-detail-actions">
          <span className="agents-save-status" role="status">
            {update.isPending ? "Saving…" : !valid ? invalidReason(draft) : dirty ? "Unsaved changes" : ""}
          </span>
          <Button variant="ghost" disabled={!dirty || update.isPending} onClick={() => setDraft(saved)}>
            Revert
          </Button>
          <Button type="submit" intent="go" disabled={!valid || update.isPending}>
            {update.isPending ? "Saving…" : "Save changes"}
          </Button>
          <Tip
            name="About saving"
            tip="Saving reaches work that is already queued: an item reads its agent's prompt when it is dispatched, not when it was planned."
          />
        </div>
      </form>
      {update.isError && <SaveRefusal error={update.error} />}
    </>
  );
}
