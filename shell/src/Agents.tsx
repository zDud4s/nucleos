import { useCallback, useEffect, useState } from "react";
import {
  createAgent, deleteAgent, listAgents,
  AGENT_ENGINES, AGENT_TOOL_POLICIES,
  type Agent, type AgentInput, type ConnectionState,
} from "./api";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

const BLANK: AgentInput = {
  name: "", speciality: "", prompt: "", engine: "claude", model: null, tool_policy: "mcp_only",
};

/** The empty string a text input yields, stored the way the daemon reads "not set". */
function orNull(value: string): string | null {
  return value.trim() === "" ? null : value.trim();
}

interface AgentFormProps {
  busy: boolean;
  onSubmit: (input: AgentInput) => void;
  onCancel: () => void;
}

/**
 * The declaration form.
 *
 * `engine` and `tool_policy` are selects rather than free text because the daemon accepts a closed
 * set of each and refuses everything else. Typing is how you discover that by 400.
 */
function AgentForm({ busy, onSubmit, onCancel }: AgentFormProps) {
  const [name, setName] = useState(BLANK.name);
  const [speciality, setSpeciality] = useState(BLANK.speciality);
  const [prompt, setPrompt] = useState(BLANK.prompt);
  const [engine, setEngine] = useState(BLANK.engine);
  const [model, setModel] = useState("");
  const [toolPolicy, setToolPolicy] = useState(BLANK.tool_policy);

  // A local engine with no model is refused by the daemon, so it is refused here too — one round
  // trip earlier, and next to the field that fixes it.
  const incomplete =
    name.trim() === "" ||
    speciality.trim() === "" ||
    prompt.trim() === "" ||
    (engine === "local" && model.trim() === "");

  return (
    <form
      className="form-grid"
      onSubmit={(event) => {
        event.preventDefault();
        if (incomplete || busy) return;
        onSubmit({
          name: name.trim(),
          speciality: speciality.trim(),
          prompt: prompt.trim(),
          engine,
          model: orNull(model),
          tool_policy: toolPolicy,
        });
      }}
    >
      <label>
        Name
        <input value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <label>
        Speciality
        <input
          value={speciality}
          placeholder="what this one is for, in a line"
          onChange={(event) => setSpeciality(event.target.value)}
        />
      </label>
      <label>
        Engine
        <select value={engine} onChange={(event) => setEngine(event.target.value)}>
          {AGENT_ENGINES.map((option) => <option key={option} value={option}>{option}</option>)}
        </select>
      </label>
      <label>
        Model
        <input
          value={model}
          placeholder={engine === "local" ? "(required for a local engine)" : "(the default)"}
          onChange={(event) => setModel(event.target.value)}
        />
      </label>
      <label>
        Tools
        <select value={toolPolicy} onChange={(event) => setToolPolicy(event.target.value)}>
          {AGENT_TOOL_POLICIES.map((option) => (
            <option key={option} value={option}>{option}</option>
          ))}
        </select>
      </label>
      <label className="wide">
        Prompt
        <textarea rows={4} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
      </label>
      <div className="form-actions">
        <Button type="submit" variant="approve" disabled={incomplete || busy}>
          {busy ? "Saving…" : "Save agent"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>Cancel</Button>
      </div>
    </form>
  );
}

/** What each refusal means in words the owner can act on. */
function explainWrite(status: number): string {
  if (status === 409) return "An agent with that name — or the id it shortens to — already exists.";
  if (status === 400) return "The daemon rejected this declaration. Check the engine and the model.";
  return "Could not save this agent.";
}

function explainDelete(status: number): string {
  if (status === 409) {
    return "This agent is a member or the director of a team. Take it off the team first.";
  }
  if (status === 404) return "That agent is already gone.";
  return "Could not delete this agent.";
}

function Roster({ token }: { token: string }) {
  const [agents, setAgents] = useState<Agent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setAgents(await listAgents(token));
    setLoading(false);
  }, [token]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function save(input: AgentInput) {
    setBusy(true);
    setFailed(null);
    const result = await createAgent(token, input);
    setBusy(false);
    if (!result.ok) {
      setFailed(explainWrite(result.status));
      return;
    }
    setCreating(false);
    // Read the catalogue back rather than splice the answer in: the id is slugged by the daemon,
    // and it is what every later reference points at.
    await refresh();
  }

  async function remove(id: string) {
    setBusy(true);
    setFailed(null);
    const result = await deleteAgent(token, id);
    setBusy(false);
    if (!result.ok) {
      setFailed(explainDelete(result.status));
      return;
    }
    await refresh();
  }

  return (
    <Panel title="Agents" aside={agents === null ? undefined : `${agents.length} in the house`}>
      <div className="form-actions">
        <Button
          onClick={() => { setCreating((open) => !open); setFailed(null); }}
          disabled={busy}
        >
          {creating ? "Close" : "New agent"}
        </Button>
      </div>
      {creating && (
        <AgentForm
          busy={busy}
          onSubmit={(input) => void save(input)}
          onCancel={() => setCreating(false)}
        />
      )}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      {loading && agents === null && <p className="a-note">Loading…</p>}
      {!loading && agents === null && (
        <ErrorNote>Could not load the agent catalogue from the daemon.</ErrorNote>
      )}
      {agents !== null && agents.length === 0 && !creating && (
        <Teach title="No agents yet.">
          An agent is a named specialist: what it is for, the prompt it runs under, and which engine
          answers for it. Write down the ones you keep asking for by hand, then put them on a team.
        </Teach>
      )}
      {(agents ?? []).map((agent) => (
        <article className="feed-item" key={agent.id}>
          <div className="f-meta">
            <b>{agent.name}</b>
            <Badge tone="active">{agent.engine}</Badge>
            <span>{agent.model ?? "the default model"}</span>
            <span>{agent.tool_policy}</span>
          </div>
          <p className="f-body">{agent.speciality}</p>
          <p className="a-note p-prompt">{agent.prompt}</p>
          <div className="a-actions">
            <ConfirmButton
              size="sm"
              variant="danger"
              confirmLabel="Confirm delete?"
              disabled={busy}
              onConfirm={() => void remove(agent.id)}
            >
              Delete
            </ConfirmButton>
          </div>
        </article>
      ))}
    </Panel>
  );
}

interface AgentsProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * The house catalogue of named agents.
 *
 * Its own tab rather than a corner of Council, because an agent is not a council seat: the council
 * borrows one for a question, a department keeps one on staff, and the same specialist should be
 * declared once and reachable from both. Nothing on this screen runs anything or costs anything —
 * it is a catalogue, and every place that spends says so where the spending happens.
 */
function Agents({ token, connection }: AgentsProps) {
  if (token === null || connection !== "connected") {
    return (
      <section className="agents">
        <Teach title="The catalogue is waiting for the daemon.">
          Agents live in the núcleo, not here. Connect to it and the roster comes back exactly as
          you left it.
        </Teach>
      </section>
    );
  }

  return (
    <section className="agents">
      <Roster token={token} />
    </section>
  );
}

export default Agents;
