import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  useAgents,
  useCreateAgent,
  useDeleteAgent,
  useUpdateAgent,
  type Agent,
  type AgentEngine,
  type AgentRequest,
  type AgentToolPolicy,
} from "../data/agents";
import { Badge, Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, StaleNote, Teach } from "../ui";
import "./agents.css";

/**
 * Agents — the house catalogue: hire one, edit one in place, delete one.
 *
 * One panel, always on screen — there is no detail route for this page (the
 * whole record is short enough to show in a row, and there is nothing to
 * navigate to that isn't already there). Create and edit share one field set
 * so the two forms cannot drift apart on what an `AgentRequest` needs.
 *
 * **Refusals never leave the row that caused them.** Create's form carries its
 * own; each row carries its own edit and delete refusals; there is no
 * page-level banner for any of the three.
 */
export function Agents() {
  const agents = useAgents();
  const rows = agents.data ?? [];
  const answered = agents.data !== undefined;
  const stale = agents.isError && answered;

  return (
    <>
      <PageHeader title="Agents" headline={headlineFor(rows, answered)} />

      <NewAgentForm />

      {stale && <StaleNote dataUpdatedAt={agents.dataUpdatedAt} />}
      {agents.isError && !answered && <ListError error={agents.error} />}

      <Panel title="Catalogue" aside={<Count n={answered ? rows.length : undefined} />}>
        {!answered && !agents.isError && <p className="agents-loading">reading the catalogue…</p>}
        {answered && rows.length === 0 && (
          <Teach title="No agent has been hired yet">
            <p>
              An agent is a name, a speciality a director reads to delegate, a prompt, an engine and a
              tool policy — add one above to start the catalogue.
            </p>
            <p>
              This catalogue records no recruitment link yet: which recruitment run asked for an agent
              is a fact for a later slice, once the núcleo mounts a route for it.
            </p>
          </Teach>
        )}
        {rows.length > 0 && (
          <ul className="agents-list" aria-label="Agents">
            {rows.map((agent) => (
              <AgentRow key={agent.id} agent={agent} />
            ))}
          </ul>
        )}
      </Panel>
    </>
  );
}

/** One derived sentence about the whole catalogue. */
function headlineFor(rows: Agent[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no agent in the catalogue";
  const noun = rows.length === 1 ? "agent" : "agents";
  return `${rows.length} ${noun} in the catalogue`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the catalogue</ErrorNote>;
}

/**
 * Why a create or an edit was refused.
 *
 * Both routes share one cause per code (`core/src/agent.rs`, `validate` and
 * the `UNIQUE` on `name`), and every error arm answers a bare `StatusCode`
 * with an empty body — `client.ts` falls back to `statusText`, which is under
 * the four-word floor, so this page supplies the sentence for all three codes
 * rather than showing the daemon's silence back.
 */
function SaveRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this agent was not saved</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: "an agent of that name already exists",
        bad_request: "the núcleo refused this — check the name, engine, model and tool policy",
        not_found: "this agent no longer exists — it may have been deleted elsewhere",
      }}
    />
  );
}

/**
 * Why a delete was refused.
 *
 * `409` here has exactly one cause and it is not the create/update one: a
 * team's director or a team's member cannot be deleted
 * (`core/src/agent.rs:215-235`). A council roster does not hold an agent
 * back, and this sentence must not claim that it does.
 */
function DeleteRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this agent was not deleted</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: "a team is standing on this agent",
        not_found: "this agent is already gone",
      }}
    />
  );
}

/* -------------------------------------------------------------- field set -- */

/** The draft a form edits — `AgentRequest` with `model` as a plain string so a blank input is possible. */
interface AgentDraft {
  name: string;
  speciality: string;
  prompt: string;
  engine: AgentEngine;
  model: string;
  tool_policy: AgentToolPolicy;
}

const EMPTY_DRAFT: AgentDraft = {
  name: "",
  speciality: "",
  prompt: "",
  engine: "claude",
  model: "",
  tool_policy: "mcp_only",
};

function draftFromAgent(agent: Agent): AgentDraft {
  return {
    name: agent.name,
    speciality: agent.speciality,
    prompt: agent.prompt,
    engine: isAgentEngine(agent.engine) ? agent.engine : "claude",
    model: agent.model ?? "",
    tool_policy: isAgentToolPolicy(agent.tool_policy) ? agent.tool_policy : "mcp_only",
  };
}

function isAgentEngine(value: string): value is AgentEngine {
  return value === "claude" || value === "codex" || value === "local";
}

function isAgentToolPolicy(value: string): value is AgentToolPolicy {
  return value === "mcp_only" || value === "none";
}

/** A `local` engine must name a model — refused client-side before the round trip, per `agent::validate`. */
function draftIsValid(draft: AgentDraft): boolean {
  return (
    draft.name.trim() !== "" &&
    draft.speciality.trim() !== "" &&
    (draft.engine !== "local" || draft.model.trim() !== "")
  );
}

/** Every field, always — the route replaces the whole row, so an edit sends the unchanged ones too. */
function requestFromDraft(draft: AgentDraft): AgentRequest {
  return {
    name: draft.name.trim(),
    speciality: draft.speciality.trim(),
    prompt: draft.prompt.trim(),
    engine: draft.engine,
    model: draft.model.trim() === "" ? null : draft.model.trim(),
    tool_policy: draft.tool_policy,
  };
}

/**
 * The fields `NewAgentForm` and each row's inline editor share.
 *
 * One field set for both, so the two forms cannot answer "what does an
 * `AgentRequest` need" differently. Labels are plain ("Name", "Engine", …)
 * rather than disambiguated per instance — only one editor is ever open on
 * this page at a time (the create form, or one row), and a caller that needs
 * to tell two apart scopes the query with `within(...)`.
 */
function AgentFields({ draft, onChange }: { draft: AgentDraft; onChange: (next: Partial<AgentDraft>) => void }) {
  const modelRequired = draft.engine === "local";

  return (
    <>
      <label className="agents-field">
        <span>Name</span>
        <input aria-label="Name" value={draft.name} onChange={(event) => onChange({ name: event.target.value })} />
      </label>

      <label className="agents-field">
        <span>Speciality</span>
        <input
          aria-label="Speciality"
          value={draft.speciality}
          onChange={(event) => onChange({ speciality: event.target.value })}
        />
      </label>

      <label className="agents-field agents-field-wide">
        <span>Prompt</span>
        <textarea
          rows={3}
          aria-label="Prompt"
          value={draft.prompt}
          onChange={(event) => onChange({ prompt: event.target.value })}
        />
      </label>

      <label className="agents-field">
        <span>Engine</span>
        <select
          aria-label="Engine"
          value={draft.engine}
          onChange={(event) => onChange({ engine: event.target.value as AgentEngine })}
        >
          <option value="claude">claude</option>
          <option value="codex">codex</option>
          <option value="local">local</option>
        </select>
      </label>

      <label className="agents-field">
        <span>{modelRequired ? "Model (required for a local engine)" : "Model (blank is the engine's default)"}</span>
        <input
          aria-label="Model"
          aria-required={modelRequired}
          value={draft.model}
          onChange={(event) => onChange({ model: event.target.value })}
        />
      </label>

      <label className="agents-field">
        <span>Tool policy</span>
        <select
          aria-label="Tool policy"
          value={draft.tool_policy}
          onChange={(event) => onChange({ tool_policy: event.target.value as AgentToolPolicy })}
        >
          <option value="mcp_only">mcp_only</option>
          <option value="none">none</option>
        </select>
      </label>
    </>
  );
}

/* ------------------------------------------------------------------- create -- */

function NewAgentForm() {
  const [draft, setDraft] = useState<AgentDraft>(EMPTY_DRAFT);
  const create = useCreateAgent();
  const valid = draftIsValid(draft);

  function patch(next: Partial<AgentDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  return (
    <Panel title="New agent">
      <form
        className="agents-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!valid || create.isPending) return;
          create.mutate(requestFromDraft(draft), { onSuccess: () => setDraft(EMPTY_DRAFT) });
        }}
      >
        <AgentFields draft={draft} onChange={patch} />
        <Button type="submit" intent="go" disabled={!valid || create.isPending}>
          Add agent
        </Button>
      </form>
      {create.isError && <SaveRefusal error={create.error} />}
    </Panel>
  );
}

/* --------------------------------------------------------------------- row -- */

function AgentRow({ agent }: { agent: Agent }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<AgentDraft>(() => draftFromAgent(agent));
  const update = useUpdateAgent();
  const del = useDeleteAgent();
  const valid = draftIsValid(draft);

  function patch(next: Partial<AgentDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  function toggleEdit() {
    if (editing) {
      setEditing(false);
      return;
    }
    setDraft(draftFromAgent(agent));
    setEditing(true);
  }

  return (
    <li className="agents-row">
      <div className="agents-row-head">
        <span className="agents-row-name">{agent.name}</span>
        <Badge tone="info">{agent.engine}</Badge>
        <span className="agents-row-model">{agent.model ?? "the engine's default"}</span>
        <span className="agents-row-policy">{agent.tool_policy}</span>
        <div className="agents-row-actions">
          <Button variant="ghost" onClick={toggleEdit}>
            {editing ? "Cancel" : "Edit"}
          </Button>
          <ConfirmButton
            label="Delete"
            confirmLabel={`Delete ${agent.name}`}
            intent="stop"
            disabled={del.isPending}
            onConfirm={() => del.mutate(agent.id)}
          />
        </div>
      </div>

      <p className="agents-row-speciality">{agent.speciality}</p>

      <details className="agents-row-prompt">
        <summary>Prompt</summary>
        <p>{agent.prompt}</p>
      </details>

      {editing && (
        <form
          className="agents-form"
          onSubmit={(event) => {
            event.preventDefault();
            if (!valid || update.isPending) return;
            update.mutate(
              { id: agent.id, request: requestFromDraft(draft) },
              { onSuccess: () => setEditing(false) },
            );
          }}
        >
          <AgentFields draft={draft} onChange={patch} />
          <Button type="submit" intent="go" disabled={!valid || update.isPending}>
            Save changes
          </Button>
        </form>
      )}

      {update.isError && <SaveRefusal error={update.error} />}
      {del.isError && <DeleteRefusal error={del.error} />}
    </li>
  );
}

/* ---------------------------------------------------------------- shared -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="agents-count">{n}</span>;
}
