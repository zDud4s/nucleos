import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  canTakeASeat,
  employmentOf,
  hasTools,
  renamed,
  unemployed,
  useAgents,
  useCreateAgent,
  useDeleteAgent,
  useUpdateAgent,
  type Agent,
  type AgentEngine,
  type AgentRequest,
  type AgentToolPolicy,
  type Employer,
  type Employment,
} from "../data/agents";
import { useTeams } from "../data/teams";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  Teach,
} from "../ui";
import "./agents.css";

/**
 * Agents — the house catalogue.
 *
 * `/teams` answers *how is my organisation doing*. This one answers a question
 * the console cannot: **who exists, and does each one serve what it is given?**
 * Aptitude in the columns, consequence beside the button that makes it matter.
 *
 * The page this replaced was a CRUD form with a list under it: a create form of
 * six fields permanently open above a catalogue you had not read, then one
 * bordered box per agent, each of a different height because the speciality
 * wrapped, a `<details>` opened, and an inline editor opened under it. Nine
 * agents were nine documents to read one at a time. It is the same defect the
 * `/teams` rewrite named in its cards, and it takes the same remedy: a table,
 * where the columns line up because they are columns.
 *
 * **The id is drawn, and that is the point of the page.** `agent::slug`
 * computes it once, at creation, and `update` never recomputes it — deliberately,
 * because `teams.director_agent_id`, `team_members`, `team_items`, `job_items`
 * and `council_seats` all point at it. So after a rename the name and the id
 * disagree forever, `RosterMatrix` and `Who` draw the **id**, this page used to
 * draw only the **name**, and nothing anywhere joined the two. Now every row
 * carries both, and a divergence is marked rather than left to be noticed.
 *
 * **Nothing here is a `Badge`.** The seven tones are the app's closed vocabulary
 * for state, and an engine is not a state — it is identity, like the accent ring
 * on `Who`. Engine, model and tool policy each get their own form, and each
 * carries its own consequence: a null model is exactly an agent that can never
 * take a council seat (`core/src/council.rs:841`), and `none` is an agent with
 * no tools at all.
 *
 * Three readings this page deliberately does not draw:
 *
 * - **Who works where.** That is `RosterMatrix` on `/teams`, and it is not
 *   redrawn here. This page says how MUCH employment an agent has, which is a
 *   different question and the one that decides whether it can be deleted.
 * - **Work in flight per agent.** `team_items` and `job_items` carry `agent_id`
 *   and no route serves them by agent. `GET /team-runs/{id}` would answer one
 *   run at a time over a list capped at the newest hundred across every
 *   department — an N+1 over a window that is not the question.
 * - **Provenance.** The `agents` table has ten columns and none of them records
 *   a recruitment, and every proposal route is pending-only. The empty state
 *   says so and says nothing more.
 */
export function Agents() {
  const agents = useAgents();
  const teams = useTeams();
  const [creating, setCreating] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);

  const rows = agents.data ?? [];
  const answered = agents.data !== undefined;
  const stale = agents.isError && answered;

  /*
    Departments are read for ONE derived number per row — how much employment an
    agent has. `GET /teams` already carries every roster and is not polled, so
    this shares the cache `/teams` and the charter editor fill; arriving from
    either, the column is populated before the page paints.
  */
  const departments: Employer[] = teams.data ?? [];
  const employmentKnown = teams.data !== undefined;

  /*
    Derived from the list rather than held as a second copy: deleting the
    selected agent, or losing it to an edit elsewhere, closes the editor by
    itself instead of leaving a panel open over a row that is gone.
  */
  const chosen = rows.find((row) => row.id === selected) ?? null;

  return (
    <>
      <PageHeader
        title="Agents"
        headline={headlineFor(rows, departments, answered, employmentKnown)}
        actions={
          <Button intent="go" onClick={() => setCreating((open) => !open)} aria-expanded={creating}>
            {creating ? "Close" : "New agent"}
          </Button>
        }
      />

      {stale && <StaleNote dataUpdatedAt={agents.dataUpdatedAt} />}
      {agents.isError && !answered && <ListError error={agents.error} />}

      {/* Closed by default. The old page opened six fields above a catalogue
          nobody had read yet — the very thing the console rewrite undid. */}
      {creating && (
        <Panel title="New agent">
          <NewAgentForm onDone={() => setCreating(false)} />
        </Panel>
      )}

      {!answered && !agents.isError && <p className="agents-loading">reading the catalogue…</p>}

      {answered && rows.length === 0 && (
        <Teach title="No agent has been hired yet">
          <p>
            An agent is a name, a speciality a director reads to delegate, a prompt, an engine and a
            tool policy — add one to start the catalogue.
          </p>
          <p>
            This catalogue records no recruitment link: nothing on an agent&rsquo;s row says which
            recruitment run asked for it, because nothing in the núcleo stores that.
          </p>
        </Teach>
      )}

      {rows.length > 0 && (
        <Panel title="Catalogue">
          <Catalogue
            rows={rows}
            departments={departments}
            employmentKnown={employmentKnown}
            selected={chosen?.id ?? null}
            onSelect={(id) => setSelected((current) => (current === id ? null : id))}
          />
        </Panel>
      )}

      {/*
        One editor, always in the same place, below the table — so opening it
        never changes the height of the catalogue above it. `key` remounts it on
        a change of selection, which is what reseeds the draft; without it a
        half-typed edit would survive onto a different agent.
      */}
      {chosen !== null && (
        <AgentEditor
          key={chosen.id}
          agent={chosen}
          employment={employmentOf(chosen.id, departments)}
          employmentKnown={employmentKnown}
          onClose={() => setSelected(null)}
        />
      )}
    </>
  );
}

/* -------------------------------------------------------------- readings -- */

/** One derived sentence about the whole catalogue. */
function headlineFor(
  rows: Agent[],
  departments: Employer[],
  answered: boolean,
  employmentKnown: boolean,
): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no agent in the catalogue";

  const parts = [`${rows.length} ${rows.length === 1 ? "agent" : "agents"}`];

  const seatless = rows.filter((row) => !canTakeASeat(row)).length;
  if (seatless > 0) parts.push(`${seatless} without a model`);

  // Only once the departments have answered. An empty list before the call
  // returns would say every agent is unused, which is the loudest wrong thing
  // this page could say.
  if (employmentKnown) {
    const idle = rows.filter((row) => unemployed(employmentOf(row.id, departments))).length;
    if (idle > 0) parts.push(`${idle} nobody uses`);
  }

  return parts.join(" · ");
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
 * `409` here has exactly one cause and it is not the create/update one, and it
 * has **four** grounds rather than the two this page used to name.
 * `agent::delete` asks one question over four tables before the DELETE:
 * `teams.director_agent_id`, `team_members`, `team_items` and `job_items`. The
 * two work tables are there for a stated reason — being on a roster and having
 * been given work are separate facts, and an agent taken off every team still
 * owns every item already assigned to it.
 *
 * The old sentence here, "a team is standing on this agent", sent somebody who
 * had already emptied every roster looking for a team that does not exist. That
 * is precisely the failure `agent::delete` says it exists to prevent.
 *
 * A council roster still does not hold an agent back, and this must not claim
 * that it does.
 */
function DeleteRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — this agent was not deleted</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict:
          "this agent directs or belongs to a team, or is holding an item of a team run or of a job",
        not_found: "this agent is already gone",
      }}
    />
  );
}

/* ------------------------------------------------------------- catalogue -- */

/**
 * Every agent, one per row.
 *
 * A real `<table>`, for the reason `RosterMatrix` is one: an agent is a row, a
 * reading is a column, and a screen reader landing in a cell is told both.
 * Built out of `div`s it would be a picture of a table.
 *
 * Its own horizontal scroller, so a narrow window slides the table and never
 * the whole document — which would take the sidebar and the header with it.
 */
function Catalogue({
  rows,
  departments,
  employmentKnown,
  selected,
  onSelect,
}: {
  rows: Agent[];
  departments: Employer[];
  employmentKnown: boolean;
  selected: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="agents-scroller">
      <table className="agents-table">
        <caption className="agents-said">
          Every agent in the catalogue, what it runs on and how much of it is spoken for
        </caption>
        <thead>
          <tr>
            <th scope="col">Agent</th>
            <th scope="col">Engine</th>
            <th scope="col">Model</th>
            <th scope="col">Tools</th>
            <th scope="col" className="agents-col-num">
              Employed
            </th>
            <th scope="col">Changed</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((agent) => (
            <CatalogueRow
              key={agent.id}
              agent={agent}
              employment={employmentOf(agent.id, departments)}
              employmentKnown={employmentKnown}
              open={selected === agent.id}
              onSelect={() => onSelect(agent.id)}
            />
          ))}
        </tbody>
      </table>
      <p className="agents-key">
        ◉ directs a team · ● on a team&rsquo;s roster · ■ has tools · □ no tools · ? this shell has no
        reading for that policy · ≠ renamed since the núcleo learned its id
      </p>
    </div>
  );
}

function CatalogueRow({
  agent,
  employment,
  employmentKnown,
  open,
  onSelect,
}: {
  agent: Agent;
  employment: Employment;
  employmentKnown: boolean;
  open: boolean;
  onSelect: () => void;
}) {
  return (
    <tr className={open ? "agents-row agents-row-open" : "agents-row"}>
      <th scope="row" className="agents-who">
        {/*
          The name is the control. There is no separate Edit button per row: the
          row IS the way in, and the editor it opens is the only one on the page.
        */}
        <button type="button" className="agents-name" onClick={onSelect} aria-expanded={open}>
          {agent.name}
        </button>
        <Called agent={agent} />
        {/* Prose, so it goes under the name rather than into a column of its
            own — the treatment a department's remit gets on the console. */}
        <span className="agents-speciality">{agent.speciality}</span>
      </th>
      <td className="agents-engine">{agent.engine}</td>
      <td>
        <Model agent={agent} />
      </td>
      <td>
        <Tools agent={agent} />
      </td>
      <td className="agents-col-num">
        <Employed employment={employment} known={employmentKnown} />
      </td>
      <td className="agents-changed">
        <RelativeTime at={agent.updated_at} />
      </td>
    </tr>
  );
}

/**
 * What the machine calls this one.
 *
 * Drawn on every row, not only the divergent ones: the id is the address every
 * other surface in the app uses, and a field that appears only when something is
 * wrong teaches nobody what it is. Marked when `slugOf(name)` no longer matches,
 * which is the permanent state of any agent that has been renamed.
 *
 * The mark is a `≠` and a sentence. Not the dashed underline the model cell
 * uses one column over — dashed means "absent" there, and a shape asked to mean
 * two things stops meaning either. Never colour: renaming is not a state, and
 * the seven tones are spoken for.
 */
function Called({ agent }: { agent: Agent }) {
  const diverged = renamed(agent);
  return (
    <span
      className={diverged ? "agents-id agents-id-diverged" : "agents-id"}
      title={
        diverged
          ? `the núcleo knows this one as ${agent.id}; it has been renamed since, and every roster still points at the id`
          : `the núcleo knows this one as ${agent.id}`
      }
    >
      {/*
        Says what the second token IS. Everything telling the two apart on
        screen is spatial — a margin, a mono face, a fainter colour — and none
        of it survives being read out: `preview-why` dumped this row's text as
        `tradutortradutor`, and a screen reader gets no better than the same
        word twice with nothing to say why. The `≠` is left out of this on
        purpose: it comes from `::before`, so it is never in the accessibility
        tree, and the sentence below carries that fact in language instead.
      */}
      <span className="agents-said">known to the núcleo as </span>
      {agent.id}
      {diverged && (
        <span className="agents-said">
          {" "}
          — renamed since; rosters, team items, job items and council seats all still point at this
          id
        </span>
      )}
    </span>
  );
}

/**
 * Which model, and what naming none costs.
 *
 * `null` still reads as the engine's default and never as blank — but it is no
 * longer the end of the sentence. `council::seat_from_spec` refuses an agent
 * that names no model, because *"a seat records the model it ran"*
 * (`core/src/council.rs:841`), and every engine this catalogue accepts maps to
 * a seat kind (`ENGINE_SEAT_KINDS`, total by test). So a null model is exactly
 * an agent that can never take a council seat, and nothing else is.
 *
 * It is still perfectly good for a department, so this says what the null costs
 * rather than drawing it as a fault.
 */
function Model({ agent }: { agent: Agent }) {
  if (canTakeASeat(agent)) return <span className="agents-model">{agent.model}</span>;
  return (
    <span
      className="agents-model agents-model-default"
      title="names no model, so it can never take a council seat — a seat records the model it ran"
    >
      the engine&rsquo;s default
      <span className="agents-said"> — names no model, so it can never take a council seat</span>
    </span>
  );
}

/** What this shell can read out of `tool_policy`. */
type ToolsReading = "tools" | "none" | "unmapped";

/**
 * `tool_policy` is a bare `string` on the wire, so a policy this shell has never
 * heard of is possible and is its own answer — the treatment `StateBadge` gives
 * an unmapped state, and the treatment `/teams` gives an unmapped grant mode.
 * Falling through to "no tools" would be a claim about a barrier this shell
 * cannot see.
 */
function toolsOf(agent: Agent): ToolsReading {
  if (hasTools(agent)) return "tools";
  return agent.tool_policy === "none" ? "none" : "unmapped";
}

const TOOLS_MARK: Record<ToolsReading, string> = { tools: "■", none: "□", unmapped: "?" };

const TOOLS_SAID: Record<ToolsReading, string> = {
  tools: "has tools",
  none: "no tools at all",
  unmapped: "this shell has no reading for that policy",
};

/**
 * Whether this agent has any tools.
 *
 * A mark and a word, which is the grammar the console uses for a granted power:
 * filled has, hollow has not, question mark is this shell being behind its
 * núcleo. The shape carries it, so none of this is colour-only, and the sentence
 * is beside it for anything that does not render.
 */
function Tools({ agent }: { agent: Agent }) {
  const reading = toolsOf(agent);
  return (
    <span className={`agents-tools agents-tools-${reading}`} title={`${agent.tool_policy}: ${TOOLS_SAID[reading]}`}>
      <span aria-hidden="true">{TOOLS_MARK[reading]}</span>
      <span aria-hidden="true">{agent.tool_policy}</span>
      <span className="agents-said">
        {agent.tool_policy}: {TOOLS_SAID[reading]}
      </span>
    </span>
  );
}

/**
 * How much of this agent is spoken for.
 *
 * How MUCH, never WHERE. Where is the matrix on `/teams`, it is already drawn,
 * and a second copy here would be the same relation stated twice. What the
 * console cannot answer is per-agent: nobody uses this one.
 *
 * The glyphs are the matrix's own, with the matrix's meanings — `◉` directs,
 * `●` on staff — on purpose. Two vocabularies for one relation is how the two
 * pages start to look like they are about different things. So is two
 * arithmetics: a department gets ONE standing here, exactly as `standingOf`
 * gives it one there, which is what makes these two figures add up to the
 * number of departments the agent is in. Counted the other way, a director who
 * is also on the roster of the department it directs drew `◉ 1 ● 1` — one
 * department, read as two by anybody who added them, under a heading that
 * invites adding them.
 *
 * Three arms and not two, because "not known yet" and "nobody" are opposite
 * answers and only one of them is worth acting on. Until `GET /teams` returns,
 * this says it does not know.
 */
function Employed({ employment, known }: { employment: Employment; known: boolean }) {
  if (!known) {
    return (
      <span className="agents-figure agents-figure-unknown" title="the team list has not answered yet">
        <span aria-hidden="true">·</span>
        <span className="agents-said">the team list has not answered — this is not zero</span>
      </span>
    );
  }

  if (unemployed(employment)) {
    return (
      <span className="agents-figure agents-figure-none" title="no team names this one">
        <span aria-hidden="true">—</span>
        <span className="agents-said">no team names this one</span>
      </span>
    );
  }

  return (
    <span className="agents-figure">
      {employment.directs.length > 0 && (
        <span className="agents-standing" title={`directs ${employment.directs.join(", ")}`}>
          <span aria-hidden="true">◉ {employment.directs.length}</span>
          <span className="agents-said">directs {employment.directs.join(", ")}. </span>
        </span>
      )}
      {employment.staffs.length > 0 && (
        <span className="agents-standing" title={`on the roster of ${employment.staffs.join(", ")}`}>
          <span aria-hidden="true">● {employment.staffs.length}</span>
          <span className="agents-said">on the roster of {employment.staffs.join(", ")}.</span>
        </span>
      )}
    </span>
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
 * The fields `NewAgentForm` and the editor share.
 *
 * One field set for both, so the two cannot answer "what does an `AgentRequest`
 * need" differently. Labels are plain ("Name", "Engine", …) rather than
 * disambiguated per instance — only one editor is ever open on this page at a
 * time, and a caller that needs to tell two apart scopes the query with
 * `within(...)`.
 */
function AgentFields({ draft, onChange }: { draft: AgentDraft; onChange: (next: Partial<AgentDraft>) => void }) {
  const modelRequired = draft.engine === "local";

  return (
    <>
      <label className="agents-field">
        <span>Name</span>
        <input aria-label="Name" value={draft.name} onChange={(event) => onChange({ name: event.target.value })} />
      </label>

      <label className="agents-field agents-field-sentence">
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
          rows={5}
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

      <label className="agents-field agents-field-half">
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

/* ------------------------------------------------------------------ create -- */

function NewAgentForm({ onDone }: { onDone: () => void }) {
  const [draft, setDraft] = useState<AgentDraft>(EMPTY_DRAFT);
  const create = useCreateAgent();
  const valid = draftIsValid(draft);

  function patch(next: Partial<AgentDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  return (
    <>
      <form
        className="agents-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!valid || create.isPending) return;
          create.mutate(requestFromDraft(draft), {
            onSuccess: () => {
              setDraft(EMPTY_DRAFT);
              onDone();
            },
          });
        }}
      >
        <AgentFields draft={draft} onChange={patch} />
        <div className="agents-form-foot">
          <Button type="submit" intent="go" disabled={!valid || create.isPending}>
            Add agent
          </Button>
          <p className="agents-aside">
            The id is taken from the name, once, and never recomputed. Renaming later changes the
            label and not the address.
          </p>
        </div>
      </form>
      {create.isError && <SaveRefusal error={create.error} />}
    </>
  );
}

/* ------------------------------------------------------------------ editor -- */

/**
 * The one editor on the page, below the table.
 *
 * Below and not inside the row, so opening it never changes the height of the
 * catalogue above it — the same move the console made when it lifted live work
 * out of the cards. "One editor at a time" was already this page's stated
 * invariant; here it is structural rather than a promise.
 *
 * The prompt is read here too. It used to sit behind a per-row `<details>`,
 * which was a second shape for a thing this panel already shows — and the
 * textarea is a draft, not a write: nothing leaves until Save.
 */
function AgentEditor({
  agent,
  employment,
  employmentKnown,
  onClose,
}: {
  agent: Agent;
  employment: Employment;
  employmentKnown: boolean;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<AgentDraft>(() => draftFromAgent(agent));
  const update = useUpdateAgent();
  const del = useDeleteAgent();
  const valid = draftIsValid(draft);

  function patch(next: Partial<AgentDraft>) {
    setDraft((current) => ({ ...current, ...next }));
  }

  return (
    <Panel
      title={`Editing ${agent.name}`}
      aside={
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
      }
    >
      <form
        className="agents-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!valid || update.isPending) return;
          /*
            Stays open and reseeds from the daemon's answer, the way the charter
            editor does. Two things come of it: what you see next is what the
            núcleo stored rather than what you typed — trimmed, and a blank model
            turned back into the engine's default — and the row above changes
            under you, which is the acknowledgement that a form closing itself
            used to be standing in for.
          */
          update.mutate(
            { id: agent.id, request: requestFromDraft(draft) },
            { onSuccess: (saved) => setDraft(draftFromAgent(saved)) },
          );
        }}
      >
        <AgentFields draft={draft} onChange={patch} />
        <div className="agents-form-foot">
          <Button type="submit" intent="go" disabled={!valid || update.isPending}>
            Save changes
          </Button>
          {/*
            Said here rather than nowhere, because it is the one consequence of
            this form that is not obvious: `job::persona_for` reads an agent's
            prompt when an item is DISPATCHED and does not store it on the row,
            so saving reaches work that was planned before the edit.
          */}
          <p className="agents-aside">
            Saving reaches work that is already queued — an item reads its agent&rsquo;s prompt when
            it is dispatched, not when it was planned.
          </p>
        </div>
      </form>
      {update.isError && <SaveRefusal error={update.error} />}

      {/*
        The delete, and what this page can see standing on it — before the
        confirmation rather than after the refusal. Never disabled from that
        reading: the daemon is the authority, rosters move underneath, and a
        button greyed out by a stale reading is worse than a refusal that tells
        the truth.
      */}
      <div className="agents-remove">
        <p className="agents-holds">{holdSentence(agent, employment, employmentKnown)}</p>
        <ConfirmButton
          label="Delete"
          confirmLabel={`Delete ${agent.name}`}
          variant="danger"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(agent.id, { onSuccess: onClose })}
        />
      </div>
      {del.isError && <DeleteRefusal error={del.error} />}
    </Panel>
  );
}

/**
 * What is standing on this agent, in words, before you arm the delete.
 *
 * Honest about its own reach in every arm. This page sees two of the four
 * tables `agent::delete` asks about — the director column and the rosters — and
 * cannot see either work table. So "nobody names this one" is never allowed to
 * read as "this will delete cleanly".
 */
function holdSentence(agent: Agent, employment: Employment, known: boolean): string {
  const work =
    "an item of a team run, or of a job, is not something this page can see, and it is the other half of what the núcleo checks";

  if (!known) {
    return `The team list has not answered, so what stands on ${agent.name} is not known here. The núcleo refuses a delete on four grounds: it directs a team, it is on a roster, or it holds an item of a team run or of a job.`;
  }

  if (unemployed(employment)) {
    return `No team names ${agent.name}. Work it is already holding — ${work}.`;
  }

  const standings = [
    employment.directs.length > 0 ? `directs ${employment.directs.join(", ")}` : null,
    employment.staffs.length > 0 ? `is on the roster of ${employment.staffs.join(", ")}` : null,
  ].filter((part) => part !== null);

  return `${agent.name} ${standings.join(" and ")}. The núcleo refuses a delete while that is true — and also while ${work}.`;
}
