import { useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import { useAgents } from "../data/agents";
import {
  GRANTABLE_ACTIONS,
  GRANT_MODES,
  TEAM_RUN_LIST_LIMIT,
  TRIGGER_SOURCES,
  teamRunIsAlive,
  useCreateTeam,
  useCreateTrigger,
  useDeleteTeam,
  useDeleteTrigger,
  useSetTriggerEnabled,
  useStartTeamRun,
  useTeam,
  useTeams,
  useTeamRuns,
  useTeamTriggers,
  useTriggerNext,
  useUpdateTeam,
  type TeamGrant,
  type TeamRequest,
  type TeamRun,
  type TeamTrigger,
  type TeamView,
  type TriggerRequest,
} from "../data/teams";
import {
  Badge,
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./teams.css";

/**
 * Teams — one component serving `/teams` and `/teams/$teamId`, the `Council`
 * pattern: the list stays on screen and the detail is added below it once a
 * department is selected, rather than replacing the list.
 *
 * A department is created and edited through **one** editor (`TeamEditor`),
 * because `PUT /teams/{id}` is a full replace — an edit IS a create that
 * already has an id, and the roster and the grants are resent wholesale every
 * time. Omitting either wipes it.
 *
 * A rule cannot be edited: the núcleo mounts `DELETE` on `/team-triggers/{id}`
 * and nothing else, so this page offers delete-and-write-another rather than a
 * control that would answer 405.
 *
 * The run list is the newest hundred and says so: `GET /team-runs` is a hard
 * `LIMIT 100` with no paging.
 */
export function Teams() {
  const params = useParams({ strict: false }) as { teamId?: string };
  const teamId = params.teamId ?? null;

  const teams = useTeams();
  const runs = useTeamRuns();
  const rows = teams.data ?? [];
  const allRuns = runs.data ?? [];
  const stale = teams.isError && teams.data !== undefined;

  return (
    <>
      <PageHeader title="Teams" headline={headlineFor(rows, allRuns, teams.data !== undefined)} />

      <NewTeamPanel />

      {stale && <StaleNote dataUpdatedAt={teams.dataUpdatedAt} />}
      {teams.isError && teams.data === undefined && <ListError error={teams.error} />}

      <TeamList rows={rows} runs={allRuns} selected={teamId} />

      {teamId === null && (
        <Teach title="Choose a department">
          <p>
            Pick a department from the list, or create one above. A department is created and
            edited through one editor — the roster and the grants are always resent whole, because
            a full replace omitting either would wipe it.
          </p>
        </Teach>
      )}

      {teamId !== null && <TeamDetail key={teamId} id={teamId} allRuns={allRuns} />}
    </>
  );
}

/** One derived sentence about the whole roster of departments. */
function headlineFor(rows: TeamView[], runs: TeamRun[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no department yet";
  const noun = rows.length === 1 ? "department" : "departments";
  const runningIds = new Set(runs.filter((run) => teamRunIsAlive(run.state)).map((run) => run.team_id));
  const running = rows.filter((row) => runningIds.has(row.id)).length;
  return running === 0 ? `${rows.length} ${noun}, none running` : `${rows.length} ${noun}, ${running} running`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the departments</ErrorNote>;
}

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so a
 * bare status arrives carrying only the status word — four words is the floor
 * between that and a sentence the daemon wrote on purpose.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* --------------------------------------------------------------- new team -- */

function NewTeamPanel() {
  return (
    <Panel title="New department">
      <TeamEditor existing={null} />
    </Panel>
  );
}

/* ------------------------------------------------------------------- list -- */

function TeamList({
  rows,
  runs,
  selected,
}: {
  rows: TeamView[];
  runs: TeamRun[];
  selected: string | null;
}) {
  return (
    <Panel title="Departments">
      {rows.length === 0 && <p className="teams-empty">no department has been created yet.</p>}
      {rows.length > 0 && (
        <ul className="teams-list" aria-label="Departments">
          {rows.map((team) => {
            const live = runs.filter((run) => run.team_id === team.id && teamRunIsAlive(run.state)).length;
            return <TeamRow key={team.id} team={team} live={live} active={team.id === selected} />;
          })}
        </ul>
      )}
    </Panel>
  );
}

function TeamRow({ team, live, active }: { team: TeamView; live: number; active: boolean }) {
  return (
    <li className={active ? "teams-row teams-row-open" : "teams-row"}>
      <Link to={`/teams/${team.id}`} aria-current={active ? "page" : undefined}>
        <span className="teams-name">{team.name}</span>
      </Link>
      <p className="teams-mission">{team.mission}</p>
      <div className="teams-meta">
        <span>director: {team.director_agent_id}</span>
        <span>
          {live} run{live === 1 ? "" : "s"} live
        </span>
      </div>
      <ul className="teams-ceilings" aria-label="Ceilings">
        <li className="teams-ceiling">rounds ≤ {team.max_rounds}</li>
        <li className="teams-ceiling">parallel ≤ {team.max_parallel}</li>
        {/* A `null` ceiling is not a ceiling of zero — absent is not zero. */}
        <li className="teams-ceiling">
          {team.budget_usd === null ? "no ceiling of its own" : `$${team.budget_usd.toFixed(2)}`}
        </li>
        <li className="teams-ceiling">open actions ≤ {team.max_open_actions}</li>
        <li className="teams-ceiling">live runs ≤ {team.max_live_runs}</li>
      </ul>
    </li>
  );
}

/* ----------------------------------------------------------------- detail -- */

function TeamDetail({ id, allRuns }: { id: string; allRuns: TeamRun[] }) {
  const team = useTeam(id);
  const triggers = useTeamTriggers();
  const del = useDeleteTeam();
  const navigate = useNavigate();
  const detail = team.data;

  if (detail === undefined) {
    return (
      <Panel title="Department">
        {team.isError ? <TeamDetailError error={team.error} /> : <p className="teams-loading">reading the department…</p>}
      </Panel>
    );
  }

  const teamRuns = allRuns.filter((run) => run.team_id === id);
  const teamTriggers = (triggers.data ?? []).filter((rule) => rule.team_id === id);

  return (
    <div className="teams-detail">
      <Panel
        title={detail.name}
        aside={
          <ConfirmButton
            label="Delete department"
            confirmLabel="Delete it now"
            intent="stop"
            disabled={del.isPending}
            onConfirm={() => del.mutate(id, { onSuccess: () => void navigate({ to: "/teams" }) })}
          />
        }
      >
        <p className="teams-mission">{detail.mission}</p>
        {del.isError && <DeleteTeamRefusal error={del.error} />}
        <RosterPanel members={detail.members} />
      </Panel>

      <Panel title="Edit department" variant="dim">
        <TeamEditor existing={detail} />
      </Panel>

      <TriggerRules teamId={id} team={detail} rules={teamTriggers} />
      <StartRunForm teamId={id} />
      <TeamRunList runs={teamRuns} />
    </div>
  );
}

function RosterPanel({ members }: { members: string[] }) {
  return (
    <>
      <p className="teams-label">Roster</p>
      {members.length === 0 ? (
        <p className="teams-empty">no member yet — a run cannot start without a roster.</p>
      ) : (
        <ul className="teams-roster" aria-label="Roster">
          {members.map((member) => (
            <li className="teams-member" key={member}>
              {member}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function TeamDetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "there is no department with that id", ...daemonProse(error) }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this department</ErrorNote>;
}

function DeleteTeamRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this department was not deleted</ErrorNote>;
}

/* ---------------------------------------------------------------- editor -- */

interface TeamFormState {
  name: string;
  mission: string;
  directorAgentId: string;
  maxRounds: string;
  maxParallel: string;
  budgetUsd: string;
  maxOpenActions: string;
  maxLiveRuns: string;
  members: string[];
  grants: TeamGrant[];
}

function emptyTeamForm(): TeamFormState {
  return {
    name: "",
    mission: "",
    directorAgentId: "",
    maxRounds: "3",
    maxParallel: "2",
    budgetUsd: "",
    maxOpenActions: "5",
    maxLiveRuns: "1",
    members: [],
    grants: [],
  };
}

function teamFormFromView(team: TeamView): TeamFormState {
  return {
    name: team.name,
    mission: team.mission,
    directorAgentId: team.director_agent_id,
    maxRounds: String(team.max_rounds),
    maxParallel: String(team.max_parallel),
    budgetUsd: team.budget_usd === null ? "" : String(team.budget_usd),
    maxOpenActions: String(team.max_open_actions),
    maxLiveRuns: String(team.max_live_runs),
    members: team.members,
    grants: team.grants,
  };
}

/** A blank ceiling box is `null` — no ceiling — never `0`; a typed `0` is a real ceiling of zero. */
function parseCeiling(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

function teamRequestFromForm(form: TeamFormState): TeamRequest {
  return {
    name: form.name.trim(),
    mission: form.mission.trim(),
    director_agent_id: form.directorAgentId,
    max_rounds: Number(form.maxRounds),
    max_parallel: Number(form.maxParallel),
    budget_usd: parseCeiling(form.budgetUsd),
    max_open_actions: Number(form.maxOpenActions),
    max_live_runs: Number(form.maxLiveRuns),
    members: form.members,
    grants: form.grants,
  };
}

/**
 * Shared by create and edit. `existing === null` is create.
 *
 * Editing seeds the form **once** from the query (`form === null` guard, the
 * System `BudgetPanel` precedent): a poll tick must not overwrite a half-typed
 * edit. Never sends an `id` — the daemon slugs one from `name` and renaming
 * never changes it.
 */
function TeamEditor({ existing }: { existing: TeamView | null }) {
  const agents = useAgents();
  const agentRows = agents.data ?? [];
  const create = useCreateTeam();
  const update = useUpdateTeam();
  const navigate = useNavigate();
  const [form, setForm] = useState<TeamFormState | null>(existing === null ? emptyTeamForm() : null);

  useEffect(() => {
    if (existing !== null && form === null) setForm(teamFormFromView(existing));
  }, [existing, form]);

  if (form === null) return <p className="teams-loading">reading the department…</p>;

  const mutation = existing === null ? create : update;
  const valid = form.name.trim() !== "" && form.mission.trim() !== "" && form.directorAgentId.trim() !== "";

  function submit() {
    if (form === null || !valid || mutation.isPending) return;
    const body = teamRequestFromForm(form);
    if (existing === null) {
      create.mutate(body, {
        onSuccess: (view) => {
          setForm(emptyTeamForm());
          void navigate({ to: `/teams/${view.id}` });
        },
      });
    } else {
      update.mutate({ id: existing.id, body });
    }
  }

  return (
    <form
      className="teams-form"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <label className="teams-field">
        <span className="teams-label">Name</span>
        <input
          className="teams-input"
          value={form.name}
          onChange={(event) => setForm({ ...form, name: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Mission</span>
        <textarea
          className="teams-textarea"
          rows={2}
          value={form.mission}
          onChange={(event) => setForm({ ...form, mission: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Director</span>
        <select
          className="teams-select"
          value={form.directorAgentId}
          onChange={(event) => setForm({ ...form, directorAgentId: event.target.value })}
        >
          <option value="">choose an agent</option>
          {agentRows.map((agent) => (
            <option key={agent.id} value={agent.id}>
              {agent.name}
            </option>
          ))}
        </select>
      </label>
      <label className="teams-field">
        <span className="teams-label">Max rounds (1-6)</span>
        <input
          className="teams-input"
          type="number"
          min={1}
          max={6}
          value={form.maxRounds}
          onChange={(event) => setForm({ ...form, maxRounds: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Max parallel (1-8)</span>
        <input
          className="teams-input"
          type="number"
          min={1}
          max={8}
          value={form.maxParallel}
          onChange={(event) => setForm({ ...form, maxParallel: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Budget ceiling (USD, blank = no ceiling)</span>
        <input
          className="teams-input"
          type="text"
          inputMode="decimal"
          placeholder="no ceiling"
          value={form.budgetUsd}
          onChange={(event) => setForm({ ...form, budgetUsd: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Max open actions (0-20)</span>
        <input
          className="teams-input"
          type="number"
          min={0}
          max={20}
          value={form.maxOpenActions}
          onChange={(event) => setForm({ ...form, maxOpenActions: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Max live runs (1-4)</span>
        <input
          className="teams-input"
          type="number"
          min={1}
          max={4}
          value={form.maxLiveRuns}
          onChange={(event) => setForm({ ...form, maxLiveRuns: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Members</span>
        <select
          className="teams-select"
          multiple
          aria-label="Members"
          value={form.members}
          onChange={(event) =>
            setForm({ ...form, members: Array.from(event.target.selectedOptions, (option) => option.value) })
          }
        >
          {agentRows.map((agent) => (
            <option key={agent.id} value={agent.id}>
              {agent.name}
            </option>
          ))}
        </select>
      </label>
      <GrantsEditor grants={form.grants} onChange={(grants) => setForm({ ...form, grants })} />
      <div className="teams-actions">
        <Button type="submit" intent="go" disabled={!valid || mutation.isPending}>
          {existing === null ? "Create department" : "Save changes"}
        </Button>
      </div>
      <p className="teams-note">
        This is the one editor for a department: an edit is a create that already has an id, and
        the roster and the grants are always resent whole — omitting either wipes it.
      </p>
      {mutation.isError && <TeamMutationRefusal error={mutation.error} />}
    </form>
  );
}

function TeamMutationRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this department was not saved</ErrorNote>;
}

/* ------------------------------------------------------------------ grants -- */

/**
 * One row per grantable action, three states: nothing / proposes / does.
 *
 * The absence of a grant row IS the denial — there is no `deny` mode. `propose`
 * reads "asks first", `allow` reads "does it".
 */
function GrantsEditor({ grants, onChange }: { grants: TeamGrant[]; onChange: (grants: TeamGrant[]) => void }) {
  return (
    <div className="teams-field">
      <span className="teams-label">Grants</span>
      <ul className="teams-grants" aria-label="Grants">
        {GRANTABLE_ACTIONS.map((kind) => {
          const current = grants.find((grant) => grant.kind === kind)?.mode ?? "";
          return (
            <li className="teams-grant" key={kind}>
              <span className="teams-grant-name">{kind}</span>
              <select
                className="teams-select teams-grant-modes"
                aria-label={`${kind} grant`}
                value={current}
                onChange={(event) => {
                  const value = event.target.value;
                  const rest = grants.filter((grant) => grant.kind !== kind);
                  onChange(value === "" ? rest : [...rest, { kind, mode: value }]);
                }}
              >
                <option value="">nothing</option>
                {GRANT_MODES.map((mode) => (
                  <option key={mode} value={mode}>
                    {mode === "propose" ? "asks first" : "does it"}
                  </option>
                ))}
              </select>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/* ------------------------------------------------------------------- rules -- */

function TriggerRules({ teamId, team, rules }: { teamId: string; team: TeamView; rules: TeamTrigger[] }) {
  return (
    <Panel title="Rules">
      <p className="teams-note">A rule cannot be edited, only deleted and rewritten.</p>
      {rules.length === 0 && <p className="teams-empty">no rule is armed for this department.</p>}
      {rules.length > 0 && (
        <ul className="teams-rules" aria-label="Rules">
          {rules.map((rule) => (
            <TriggerRuleRow key={rule.id} rule={rule} noCeiling={team.budget_usd === null} />
          ))}
        </ul>
      )}
      <NewTriggerForm teamId={teamId} />
    </Panel>
  );
}

function TriggerRuleRow({ rule, noCeiling }: { rule: TeamTrigger; noCeiling: boolean }) {
  const setEnabled = useSetTriggerEnabled();
  const del = useDeleteTrigger();
  // `enabled` arrives 0 or 1 on the wire, never a real boolean.
  const armed = rule.enabled !== 0;

  return (
    <li className="teams-rule">
      <div className="teams-rule-head">
        <span className="teams-rule-name">{rule.name}</span>
        <span>{rule.source}</span>
        {rule.cron !== null && <code>{rule.cron}</code>}
        {rule.timezone !== null && <span>{rule.timezone}</span>}
        <Badge tone={armed ? "active" : "off"}>{armed ? "armed" : "disarmed"}</Badge>
        {armed ? (
          // Disarming is always plain and never asks.
          <Button onClick={() => setEnabled.mutate({ id: rule.id, enabled: false })} disabled={setEnabled.isPending}>
            Disarm
          </Button>
        ) : noCeiling ? (
          // An armed rule on a team with no ceiling is a loop that can spend
          // without limit — the one place in this design where being wrong
          // costs money without bound, so arming it goes through the
          // interlock. Everything else on this row stays plain, per the
          // standing rule that ConfirmButton is for destructive writes only.
          <ConfirmButton
            label="Arm with no ceiling"
            confirmLabel="Arm it anyway"
            intent="go"
            disabled={setEnabled.isPending}
            onConfirm={() => setEnabled.mutate({ id: rule.id, enabled: true })}
          />
        ) : (
          <Button
            intent="go"
            onClick={() => setEnabled.mutate({ id: rule.id, enabled: true })}
            disabled={setEnabled.isPending}
          >
            Arm
          </Button>
        )}
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete this rule"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(rule.id)}
        />
      </div>
      <p className="teams-rule-what">{rule.request}</p>
      <TriggerNextLine id={rule.id} />
    </li>
  );
}

/**
 * Its own component per rule, so each rule owns its own `useTriggerNext(id)`
 * query — the row-scoped hook pattern.
 *
 * A rule that does not fire on a clock answers 200 with
 * `"this rule does not fire on a clock"`, and a bad cron answers 200 with the
 * daemon's parse message. Neither is an error state.
 */
function TriggerNextLine({ id }: { id: number }) {
  const next = useTriggerNext(id);
  if (next.data === undefined) return null;
  if (next.data.error !== null) return <p className="teams-rule-next">{next.data.error}</p>;
  if (next.data.next !== null) {
    return (
      <p className="teams-rule-next">
        next: <RelativeTime at={next.data.next} />
      </p>
    );
  }
  return null;
}

function NewTriggerForm({ teamId }: { teamId: string }) {
  const [name, setName] = useState("");
  const [source, setSource] = useState<(typeof TRIGGER_SOURCES)[number]>("cron");
  const [cron, setCron] = useState("");
  const [timezone, setTimezone] = useState("");
  const [fromTeam, setFromTeam] = useState("");
  const [emailClass, setEmailClass] = useState("");
  const [request, setRequest] = useState("");
  const create = useCreateTrigger();

  const valid = name.trim() !== "" && request.trim() !== "" && (source !== "cron" || cron.trim() !== "");

  function submit() {
    if (!valid || create.isPending) return;
    const body: TriggerRequest = {
      team_id: teamId,
      name: name.trim(),
      source,
      cron: source === "cron" ? cron.trim() : null,
      timezone: source === "cron" && timezone.trim() !== "" ? timezone.trim() : null,
      from_team: source === "team_finished" && fromTeam.trim() !== "" ? fromTeam.trim() : null,
      email_class: source === "email_triaged" && emailClass.trim() !== "" ? emailClass.trim() : null,
      request: request.trim(),
    };
    create.mutate(body, {
      onSuccess: () => {
        setName("");
        setCron("");
        setTimezone("");
        setFromTeam("");
        setEmailClass("");
        setRequest("");
      },
    });
  }

  return (
    <form
      className="teams-form"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <p className="teams-note">Creating never arms — arm the rule once it is written.</p>
      <label className="teams-field">
        <span className="teams-label">Name</span>
        <input className="teams-input" value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <label className="teams-field">
        <span className="teams-label">Source</span>
        <select
          className="teams-select"
          value={source}
          onChange={(event) => setSource(event.target.value as (typeof TRIGGER_SOURCES)[number])}
        >
          {TRIGGER_SOURCES.map((kind) => (
            <option key={kind} value={kind}>
              {kind}
            </option>
          ))}
        </select>
      </label>
      {source === "cron" && (
        <>
          <label className="teams-field">
            <span className="teams-label">Cron</span>
            <input className="teams-input" value={cron} onChange={(event) => setCron(event.target.value)} />
          </label>
          <label className="teams-field">
            <span className="teams-label">Timezone</span>
            <input className="teams-input" value={timezone} onChange={(event) => setTimezone(event.target.value)} />
          </label>
        </>
      )}
      {source === "team_finished" && (
        <label className="teams-field">
          <span className="teams-label">From team</span>
          <input className="teams-input" value={fromTeam} onChange={(event) => setFromTeam(event.target.value)} />
        </label>
      )}
      {source === "email_triaged" && (
        <label className="teams-field">
          <span className="teams-label">Email class</span>
          <input
            className="teams-input"
            value={emailClass}
            onChange={(event) => setEmailClass(event.target.value)}
          />
        </label>
      )}
      <label className="teams-field">
        <span className="teams-label">Request</span>
        <textarea
          className="teams-textarea"
          rows={3}
          value={request}
          onChange={(event) => setRequest(event.target.value)}
        />
      </label>
      <div className="teams-actions">
        <Button type="submit" intent="go" disabled={!valid || create.isPending}>
          Add rule
        </Button>
      </div>
      {create.isError && <CreateTriggerRefusal error={create.error} />}
    </form>
  );
}

function CreateTriggerRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this rule was not written</ErrorNote>;
}

/* ------------------------------------------------------------------- runs -- */

function StartRunForm({ teamId }: { teamId: string }) {
  const [request, setRequest] = useState("");
  const start = useStartTeamRun();

  return (
    <Panel title="Start a run">
      <form
        className="teams-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (request.trim() === "" || start.isPending) return;
          start.mutate({ id: teamId, request: request.trim() }, { onSuccess: () => setRequest("") });
        }}
      >
        <label className="teams-field">
          <span className="teams-label">Request</span>
          <textarea
            className="teams-textarea"
            rows={3}
            value={request}
            onChange={(event) => setRequest(event.target.value)}
          />
        </label>
        <div className="teams-actions">
          <Button type="submit" intent="go" disabled={request.trim() === "" || start.isPending}>
            Start
          </Button>
        </div>
      </form>
      {start.isError && <StartRunRefusal error={start.error} />}
      {start.data !== undefined && (
        <p className="teams-note">
          Started — <Link to={`/team-runs/${start.data.id}`}>this run</Link> has nothing in it yet; the núcleo has
          not picked it up.
        </p>
      )}
    </Panel>
  );
}

/**
 * Why a run would not start.
 *
 * The 400s name the missing specialist, the empty roster, the deleted
 * director or the local model this machine has not got — the daemon's own
 * sentence. The 429 is the budget window and reads as a ceiling that reopens,
 * never as a failure.
 */
function StartRunRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no run was started</ErrorNote>;
  if (error.status === 429) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ too_many_requests: "the budget window is exhausted for now — it reopens", ...daemonProse(error) }}
      />
    );
  }
  return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
}

/**
 * The newest {@link TEAM_RUN_LIST_LIMIT} runs, filtered to one department by
 * the caller. No cost column — `cost_usd` is not in this response and an N+1
 * fetch to build one would be a lie about what the list knows.
 */
function TeamRunList({ runs }: { runs: TeamRun[] }) {
  return (
    <Panel title="Runs">
      {runs.length === 0 && <p className="teams-empty">no run yet for this department.</p>}
      {runs.length > 0 && (
        <ul className="teams-runs" aria-label="Runs">
          {runs.map((run) => (
            <li className="teams-run" key={run.id}>
              <div className="teams-run-head">
                <Link to={`/team-runs/${run.id}`}>{run.id}</Link>
                <StateBadge domain="team_run" state={run.state} />
                <RelativeTime at={run.created_at} />
              </div>
              <p className="teams-run-request">{run.request}</p>
              {run.why !== null && <p className="teams-run-why">{run.why}</p>}
            </li>
          ))}
        </ul>
      )}
      <p className="teams-cap">showing the newest hundred runs — there is no paging past the cap.</p>
    </Panel>
  );
}
