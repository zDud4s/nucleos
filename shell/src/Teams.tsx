import { useCallback, useEffect, useState } from "react";
import {
  cancelTeamRun, createTeam, deleteTeam, deleteTeamRun, deleteTeamTrigger, getTeamRun,
  getTeamTriggerNext, listAgents, listTeamRunActions, listTeamRuns, listTeams, listTeamTriggers,
  setTeamTriggerEnabled, startTeamRun, teamRunIsLive, updateTeam,
  TEAM_DEFAULT_MAX_OPEN_ACTIONS, TEAM_GRANTABLE_ACTIONS, TEAM_MAX_LIVE_RUNS_CEILING,
  TEAM_MAX_OPEN_ACTIONS_CEILING, TEAM_MAX_PARALLEL_CEILING, TEAM_MAX_ROUNDS_CEILING,
  type Agent, type ConnectionState, type Team, type TeamAction, type TeamInput, type TeamRun,
  type TeamRunDetail, type TeamTrigger, type TeamTriggerNext,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/** How often a live run is read back. A round is minutes; this is well under one. */
const POLL_MS = 4000;

const BLANK: TeamInput = {
  name: "", mission: "", director_agent_id: "", max_rounds: 3, max_parallel: 2,
  budget_usd: null, max_open_actions: TEAM_DEFAULT_MAX_OPEN_ACTIONS, max_live_runs: 1,
  members: [], grants: [],
};

/** What a department may do with one kind of action. `""` is the daemon's default: nothing. */
type GrantMode = "" | "propose" | "allow";

function actionTone(state: string): "active" | "off" | "pending" | "paused" {
  if (state === "working") return "active";
  if (state === "done") return "pending";
  if (state === "failed") return "off";
  return "paused";
}

/**
 * One action rendered as what it would DO, not as the JSON it is stored as.
 *
 * A person who cannot read what they are approving is not approving anything, so an email shows its
 * recipient, subject and body rather than a blob with quotes in it. An unrecognised kind — a daemon
 * newer than this window — falls back to the raw payload rather than to nothing.
 */
function ActionBody({ kind, payload }: { kind: string; payload: string }) {
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(payload) as Record<string, unknown>;
  } catch {
    return <pre className="a-note">{payload}</pre>;
  }
  const field = (name: string) => String(parsed[name] ?? "");

  if (kind === "send_email") {
    return (
      <div className="team-action__body">
        <div><strong>To</strong> {field("to")}</div>
        <div><strong>Subject</strong> {field("subject")}</div>
        <pre>{field("body")}</pre>
      </div>
    );
  }
  if (kind === "file_document") {
    return (
      <div className="team-action__body">
        <div><strong>File</strong> {field("path")}</div>
        <pre>{field("content")}</pre>
      </div>
    );
  }
  if (kind === "calendar_event") {
    return (
      <div className="team-action__body">
        <div><strong>{field("title")}</strong></div>
        <div className="a-note">
          {field("starts_at_local")} · {field("duration_minutes")} min · {field("tz")}
        </div>
      </div>
    );
  }
  return <pre className="a-note">{payload}</pre>;
}

/**
 * State to badge, for the eight a run can be in.
 *
 * `stopped` and `expired` share `paused` with nothing that failed, and that is the whole point of
 * the daemon keeping them distinct from `failed`: a ceiling was reached, and an owner shown a
 * failure colour goes looking for an error that does not exist.
 */
function toneFor(state: string): "active" | "off" | "pending" | "paused" {
  if (teamRunIsLive(state)) return "active";
  if (state === "done") return "pending";
  if (state === "failed" || state === "cancelled") return "off";
  return "paused";
}

function itemTone(state: string): "active" | "off" | "pending" | "paused" {
  if (state === "running") return "active";
  if (state === "done") return "pending";
  if (state === "failed") return "off";
  return "paused";
}

/**
 * What starts a department when nobody is asking, and — for a clock rule — when it next will.
 *
 * The next time is fetched per rule and not computed here, for the reason the daemon's own route
 * gives: a screen that worked out "next at 08:00" by a second route would eventually disagree with
 * the tick that fires it. The ERROR half is why the route exists at all — an invalid cron makes a
 * rule that never runs and says so nowhere.
 */
function TriggerList({
  triggers, token, refresh,
}: {
  triggers: TeamTrigger[];
  token: string;
  refresh: () => Promise<void>;
}) {
  const [next, setNext] = useState<Record<number, TeamTriggerNext>>({});
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const answers = await Promise.all(
        triggers
          .filter((trigger) => trigger.source === "cron")
          .map(async (trigger) => [trigger.id, await getTeamTriggerNext(token, trigger.id)] as const),
      );
      if (cancelled) return;
      setNext(
        Object.fromEntries(
          answers.filter((pair): pair is [number, TeamTriggerNext] => pair[1] !== null),
        ),
      );
    })();
    return () => {
      cancelled = true;
    };
  }, [triggers, token]);

  if (triggers.length === 0) return null;

  async function toggle(trigger: TeamTrigger) {
    setBusy(true);
    await setTeamTriggerEnabled(token, trigger.id, trigger.enabled === 0);
    await refresh();
    setBusy(false);
  }

  async function remove(id: number) {
    setBusy(true);
    await deleteTeamTrigger(token, id);
    await refresh();
    setBusy(false);
  }

  return (
    <div>
      <h4>What starts it</h4>
      {triggers.map((trigger) => {
        const when = next[trigger.id];
        return (
          <article className="feed-item" key={trigger.id}>
            <div className="f-meta">
              <b>{trigger.name}</b>
              <Badge tone={trigger.enabled === 1 ? "active" : "off"}>
                {trigger.enabled === 1 ? "armed" : "not armed"}
              </Badge>
              <span>
                {trigger.source === "cron" && `${trigger.cron} ${trigger.timezone ?? "UTC"}`}
                {trigger.source === "team_finished" && `after ${trigger.from_team}`}
                {trigger.source === "email_triaged" && `on ${trigger.email_class} mail`}
              </span>
              {/* The error, when there is one, rather than a next time that will never come. */}
              {when?.error != null && <span className="a-note">{when.error}</span>}
              {when?.next != null && trigger.enabled === 1 && (
                <span className="a-note">next {relativeTime(when.next)}</span>
              )}
            </div>
            <p className="f-body">{trigger.request}</p>
            <div className="a-actions">
              <Button size="sm" disabled={busy} onClick={() => void toggle(trigger)}>
                {trigger.enabled === 1 ? "Disarm" : "Arm"}
              </Button>
              <ConfirmButton
                size="sm"
                variant="danger"
                confirmLabel="Delete the rule?"
                disabled={busy}
                onConfirm={() => void remove(trigger.id)}
              >
                Delete
              </ConfirmButton>
            </div>
          </article>
        );
      })}
    </div>
  );
}

interface TeamFormProps {
  agents: Agent[];
  initial: TeamInput;
  busy: boolean;
  submitLabel: string;
  onSubmit: (input: TeamInput) => void;
  onCancel: () => void;
}

/**
 * The team editor: a director, a roster, and the two ceilings.
 *
 * Both the director and the roster are chosen from the catalogue rather than typed, because the
 * daemon refuses an agent it does not know and typing is how you discover that by 400. The ceilings
 * are `number` inputs bounded by the daemon's own limits for the same reason.
 */
function TeamForm({ agents, initial, busy, submitLabel, onSubmit, onCancel }: TeamFormProps) {
  const [name, setName] = useState(initial.name);
  const [mission, setMission] = useState(initial.mission);
  const [director, setDirector] = useState(initial.director_agent_id);
  const [rounds, setRounds] = useState(String(initial.max_rounds));
  const [parallel, setParallel] = useState(String(initial.max_parallel));
  const [budget, setBudget] = useState(initial.budget_usd === null ? "" : String(initial.budget_usd));
  const [openActions, setOpenActions] = useState(String(initial.max_open_actions));
  const [liveRuns, setLiveRuns] = useState(String(initial.max_live_runs));
  const [members, setMembers] = useState<string[]>(initial.members);
  const [grants, setGrants] = useState<Record<string, GrantMode>>(() =>
    Object.fromEntries(initial.grants.map((grant) => [grant.kind, grant.mode as GrantMode])),
  );

  const roundsValue = Number(rounds);
  const parallelValue = Number(parallel);
  const budgetValue = budget.trim() === "" ? null : Number(budget);
  const openActionsValue = Number(openActions);
  const liveRunsValue = Number(liveRuns);

  const incomplete =
    name.trim() === "" ||
    mission.trim() === "" ||
    director === "" ||
    !Number.isInteger(roundsValue) ||
    roundsValue < 1 ||
    roundsValue > TEAM_MAX_ROUNDS_CEILING ||
    !Number.isInteger(parallelValue) ||
    parallelValue < 1 ||
    parallelValue > TEAM_MAX_PARALLEL_CEILING ||
    !Number.isInteger(openActionsValue) ||
    openActionsValue < 0 ||
    openActionsValue > TEAM_MAX_OPEN_ACTIONS_CEILING ||
    !Number.isInteger(liveRunsValue) ||
    liveRunsValue < 1 ||
    liveRunsValue > TEAM_MAX_LIVE_RUNS_CEILING ||
    (budgetValue !== null && (!Number.isFinite(budgetValue) || budgetValue <= 0));

  function toggle(id: string) {
    setMembers((current) =>
      current.includes(id) ? current.filter((each) => each !== id) : [...current, id],
    );
  }

  return (
    <form
      className="form-grid"
      onSubmit={(event) => {
        event.preventDefault();
        if (incomplete || busy) return;
        onSubmit({
          name: name.trim(),
          mission: mission.trim(),
          director_agent_id: director,
          max_rounds: roundsValue,
          max_parallel: parallelValue,
          budget_usd: budgetValue,
          max_open_actions: openActionsValue,
          max_live_runs: liveRunsValue,
          members,
          // Only the kinds actually chosen. A `""` row is the absence of a grant, and the absence
          // of a row is what the daemon reads as "no" — there is no `deny` to send.
          grants: Object.entries(grants)
            .filter(([, mode]) => mode !== "")
            .map(([kind, mode]) => ({ kind, mode })),
        });
      }}
    >
      <label>
        Name
        <input value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <label>
        Director
        <select value={director} onChange={(event) => setDirector(event.target.value)}>
          <option value="">choose an agent…</option>
          {agents.map((agent) => (
            <option key={agent.id} value={agent.id}>{agent.name}</option>
          ))}
        </select>
      </label>
      <label>
        Rounds
        <input
          type="number"
          min={1}
          max={TEAM_MAX_ROUNDS_CEILING}
          value={rounds}
          onChange={(event) => setRounds(event.target.value)}
        />
      </label>
      <label>
        At once
        <input
          type="number"
          min={1}
          max={TEAM_MAX_PARALLEL_CEILING}
          value={parallel}
          onChange={(event) => setParallel(event.target.value)}
        />
      </label>
      <label>
        Budget per run
        <input
          value={budget}
          placeholder="(only the house budget)"
          onChange={(event) => setBudget(event.target.value)}
        />
      </label>
      <label className="wide">
        Mission
        <textarea
          rows={2}
          value={mission}
          placeholder="what this department is for — the director reads it every round"
          onChange={(event) => setMission(event.target.value)}
        />
      </label>
      <label>
        Waiting at once
        <input
          type="number"
          min={0}
          max={TEAM_MAX_OPEN_ACTIONS_CEILING}
          value={openActions}
          onChange={(event) => setOpenActions(event.target.value)}
        />
      </label>
      <label>
        At once, in all
        <input
          type="number"
          min={1}
          max={TEAM_MAX_LIVE_RUNS_CEILING}
          value={liveRuns}
          onChange={(event) => setLiveRuns(event.target.value)}
        />
      </label>
      <fieldset className="wide">
        <legend>What it may do</legend>
        <p className="a-note">
          {/* Said here rather than assumed, because the default is the safe one and a reader who
              does not know that will grant more than they meant to. */}
          A department writes documents into its own folder whatever is set here. These are the
          things it may ask the core to do outside it — and it asks; it never does them itself.
        </p>
        {TEAM_GRANTABLE_ACTIONS.map((action) => (
          <label key={action.kind} className="check">
            <select
              value={grants[action.kind] ?? ""}
              onChange={(event) =>
                setGrants((current) => ({
                  ...current,
                  [action.kind]: event.target.value as GrantMode,
                }))
              }
            >
              <option value="">no</option>
              <option value="propose">ask me first</option>
              <option value="allow">without asking</option>
            </select>
            <span>{action.label}</span>
            <span className="a-note">{action.note}</span>
          </label>
        ))}
      </fieldset>
      <fieldset className="wide">
        <legend>Specialists</legend>
        {agents.length === 0 && <p className="a-note">Declare an agent first, in the Agents tab.</p>}
        {agents.map((agent) => (
          <label key={agent.id} className="check">
            <input
              type="checkbox"
              checked={members.includes(agent.id)}
              onChange={() => toggle(agent.id)}
            />
            <span>{agent.name}</span>
            <span className="a-note">{agent.speciality}</span>
          </label>
        ))}
      </fieldset>
      <div className="form-actions">
        <Button type="submit" variant="approve" disabled={incomplete || busy}>
          {busy ? "Saving…" : submitLabel}
        </Button>
        <Button onClick={onCancel} disabled={busy}>Cancel</Button>
      </div>
    </form>
  );
}

/** What each refusal means in words the owner can act on. */
function explainWrite(status: number): string {
  if (status === 409) return "A team with that name — or the id it shortens to — already exists.";
  if (status === 400) return "The daemon rejected this team. Check the director and the ceilings.";
  return "Could not save this team.";
}

function explainStart(status: number): string {
  if (status === 400) {
    return "This team cannot run yet: it may have no specialists, a director that was deleted, or " +
      "a local member on a machine with no local model.";
  }
  if (status === 429) return "The budget is spent for now. It reopens when the window rolls over.";
  if (status === 404) return "That team is gone.";
  return "Could not start this run.";
}

interface StartBoxProps {
  team: Team;
  busy: boolean;
  onStart: (request: string) => void;
}

function StartBox({ team, busy, onStart }: StartBoxProps) {
  const [request, setRequest] = useState("");
  return (
    <form
      className="a-actions"
      onSubmit={(event) => {
        event.preventDefault();
        if (request.trim() === "" || busy) return;
        onStart(request.trim());
        setRequest("");
      }}
    >
      <input
        value={request}
        placeholder={`ask ${team.name} for something`}
        aria-label={`ask ${team.name} for something`}
        onChange={(event) => setRequest(event.target.value)}
      />
      <Button type="submit" size="sm" variant="approve" disabled={busy || request.trim() === ""}>
        Start
      </Button>
    </form>
  );
}

interface RunViewProps {
  token: string;
  runId: string;
  onClose: () => void;
  onChanged: () => void;
}

/**
 * One run: what was asked, where it got to, and every item of every round.
 *
 * Polled while the run is live and left alone once it is not, because a finished run's rows cannot
 * change and a poll that never stops is a poll nobody notices costing something.
 */
function RunView({ token, runId, onClose, onChanged }: RunViewProps) {
  const [run, setRun] = useState<TeamRunDetail | null>(null);
  const [actions, setActions] = useState<TeamAction[]>([]);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const [detail, asked] = await Promise.all([
      getTeamRun(token, runId),
      listTeamRunActions(token, runId),
    ]);
    setRun(detail);
    setActions(asked ?? []);
  }, [token, runId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (run === null || !teamRunIsLive(run.state)) return;
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [run, refresh]);

  if (run === null) {
    return (
      <Panel title="Run">
        <p className="a-note">Loading…</p>
        <div className="form-actions"><Button onClick={onClose}>Back</Button></div>
      </Panel>
    );
  }

  const rounds = [...new Set(run.items.map((item) => item.round))].sort((a, b) => a - b);
  const live = teamRunIsLive(run.state);

  async function cancel() {
    setBusy(true);
    setFailed(null);
    const result = await cancelTeamRun(token, runId);
    setBusy(false);
    if (!result.ok) {
      setFailed("Could not cancel this run.");
      return;
    }
    await refresh();
    onChanged();
  }

  async function remove() {
    setBusy(true);
    setFailed(null);
    const result = await deleteTeamRun(token, runId);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 400
          ? "That run is still going. Cancel it first."
          : "Could not delete this run.",
      );
      return;
    }
    onChanged();
    onClose();
  }

  return (
    <Panel title="Run" aside={`$${run.cost_usd.toFixed(2)}`}>
      <div className="f-meta">
        <Badge tone={toneFor(run.state)}>{run.state}</Badge>
        <span>round {run.round}</span>
        {run.director_node !== "none" && <span>{run.director_node}…</span>}
      </div>
      <p className="f-body">{run.request}</p>
      {run.why !== null && <p className="a-note">{run.why}</p>}
      {/* The folder is inside the managed root, so the Files tab reaches it for free — saying where
          is more useful than a second file browser here. */}
      <p className="a-note">Delivery: {run.workspace}/</p>

      {run.items.length === 0 && (
        <p className="a-note">No work has been handed out yet.</p>
      )}
      {rounds.map((round) => (
        <div key={round}>
          <h4>Round {round}</h4>
          {run.items.filter((item) => item.round === round).map((item) => (
            <article className="feed-item" key={item.ordinal}>
              <div className="f-meta">
                <b>{item.agent_id}</b>
                <Badge tone={itemTone(item.state)}>{item.state}</Badge>
                {item.output_path !== null && <span>{item.output_path}</span>}
              </div>
              <p className="f-body">{item.description}</p>
            </article>
          ))}
        </div>
      ))}

      {actions.length > 0 && (
        <div>
          {/* Below the items, because that is the order they happen in — and shown even when the
              run has ended, since an action outlives the run that asked for it. */}
          <h4>What it asked for</h4>
          {actions.map((action) => (
            <article className="feed-item" key={action.id}>
              <div className="f-meta">
                <b>{action.kind}</b>
                <Badge tone={actionTone(action.state)}>{action.state}</Badge>
                {action.proposal_id === null
                  ? <span>without asking</span>
                  : <span>proposal #{action.proposal_id}</span>}
              </div>
              <p className="f-body">{action.why}</p>
              <ActionBody kind={action.kind} payload={action.payload} />
              {action.error !== null && <p className="a-note">{action.error}</p>}
            </article>
          ))}
        </div>
      )}

      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      <div className="form-actions">
        <Button onClick={onClose}>Back</Button>
        {live && (
          <ConfirmButton
            size="sm"
            variant="danger"
            confirmLabel="Confirm cancel?"
            disabled={busy}
            onConfirm={() => void cancel()}
          >
            Cancel run
          </ConfirmButton>
        )}
        {!live && (
          <ConfirmButton
            size="sm"
            variant="danger"
            confirmLabel="Delete the delivery too?"
            disabled={busy}
            onConfirm={() => void remove()}
          >
            Delete
          </ConfirmButton>
        )}
      </div>
    </Panel>
  );
}

function Departments({ token }: { token: string }) {
  const [teams, setTeams] = useState<Team[] | null>(null);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [runs, setRuns] = useState<TeamRun[]>([]);
  const [triggers, setTriggers] = useState<TeamTrigger[]>([]);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const [nextTeams, nextAgents, nextRuns, nextTriggers] = await Promise.all([
      listTeams(token),
      listAgents(token),
      listTeamRuns(token),
      listTeamTriggers(token),
    ]);
    setTeams(nextTeams);
    setAgents(nextAgents ?? []);
    setRuns(nextRuns ?? []);
    setTriggers(nextTriggers ?? []);
    setLoading(false);
  }, [token]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // The list of runs keeps moving while any of them is live, so the tab shows a department
  // finishing without anybody pressing anything.
  useEffect(() => {
    if (!runs.some((run) => teamRunIsLive(run.state))) return;
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [runs, refresh]);

  async function save(input: TeamInput, id: string | null) {
    setBusy(true);
    setFailed(null);
    const result = id === null
      ? await createTeam(token, input)
      : await updateTeam(token, id, input);
    setBusy(false);
    if (!result.ok) {
      setFailed(explainWrite(result.status));
      return;
    }
    setCreating(false);
    setEditing(null);
    // Read back rather than splice: the id is slugged by the daemon, and it is what the folder on
    // disk is named after.
    await refresh();
  }

  async function remove(id: string) {
    setBusy(true);
    setFailed(null);
    const result = await deleteTeam(token, id);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 400
          ? "This team has runs on record. Delete those first — their deliveries point at it."
          : "Could not delete this team.",
      );
      return;
    }
    await refresh();
  }

  async function start(id: string, request: string) {
    setBusy(true);
    setFailed(null);
    const result = await startTeamRun(token, id, request);
    setBusy(false);
    if (!result.ok) {
      setFailed(explainStart(result.status));
      return;
    }
    setOpen(result.value.id);
    await refresh();
  }

  if (open !== null) {
    return (
      <RunView
        token={token}
        runId={open}
        onClose={() => setOpen(null)}
        onChanged={() => void refresh()}
      />
    );
  }

  return (
    <>
      <Panel title="Teams" aside={teams === null ? undefined : `${teams.length} in the house`}>
        <div className="form-actions">
          <Button
            onClick={() => { setCreating((wasOpen) => !wasOpen); setEditing(null); setFailed(null); }}
            disabled={busy}
          >
            {creating ? "Close" : "New team"}
          </Button>
        </div>
        {creating && (
          <TeamForm
            agents={agents}
            initial={BLANK}
            busy={busy}
            submitLabel="Save team"
            onSubmit={(input) => void save(input, null)}
            onCancel={() => setCreating(false)}
          />
        )}
        {failed !== null && <ErrorNote>{failed}</ErrorNote>}
        {loading && teams === null && <p className="a-note">Loading…</p>}
        {!loading && teams === null && (
          <ErrorNote>Could not load the teams from the daemon.</ErrorNote>
        )}
        {teams !== null && teams.length === 0 && !creating && (
          <Teach title="No teams yet.">
            A team is a director and the specialists it hands work to. Ask it for something and the
            director splits the work, waits for the answers, and writes the delivery — up to the
            ceilings you set here.
          </Teach>
        )}
        {(teams ?? []).map((team) => (
          <article className="feed-item" key={team.id}>
            <div className="f-meta">
              <b>{team.name}</b>
              <Badge tone="active">{team.director_agent_id}</Badge>
              <span>{team.members.length} specialists</span>
              <span>{team.max_rounds} rounds, {team.max_parallel} at once</span>
              <span>{team.budget_usd === null ? "no ceiling of its own" : `$${team.budget_usd}`}</span>
              {/* Said only when there IS one. "may only write documents" on every card would be
                  noise on the default, and the default is what almost every team is. */}
              {team.grants.length > 0 && (
                <span>
                  may {team.grants.map((grant) =>
                    grant.mode === "allow" ? `${grant.kind} freely` : grant.kind,
                  ).join(", ")}
                </span>
              )}
            </div>
            <p className="f-body">{team.mission}</p>
            {/* Beside the department rather than on a rules tab of its own: what starts a
                department is part of what the department IS, and a rule read away from the team it
                starts is a rule nobody connects to anything. */}
            <TriggerList
              triggers={triggers.filter((trigger) => trigger.team_id === team.id)}
              token={token}
              refresh={refresh}
            />
            {editing === team.id ? (
              <TeamForm
                agents={agents}
                initial={{
                  name: team.name,
                  mission: team.mission,
                  director_agent_id: team.director_agent_id,
                  max_rounds: team.max_rounds,
                  max_parallel: team.max_parallel,
                  budget_usd: team.budget_usd,
                  max_open_actions: team.max_open_actions,
                  max_live_runs: team.max_live_runs,
                  members: team.members,
                  grants: team.grants,
                }}
                busy={busy}
                submitLabel="Save changes"
                onSubmit={(input) => void save(input, team.id)}
                onCancel={() => setEditing(null)}
              />
            ) : (
              <>
                <StartBox
                  team={team}
                  busy={busy}
                  onStart={(request) => void start(team.id, request)}
                />
                <div className="a-actions">
                  <Button
                    size="sm"
                    onClick={() => { setEditing(team.id); setCreating(false); setFailed(null); }}
                    disabled={busy}
                  >
                    Edit
                  </Button>
                  <ConfirmButton
                    size="sm"
                    variant="danger"
                    confirmLabel="Confirm delete?"
                    disabled={busy}
                    onConfirm={() => void remove(team.id)}
                  >
                    Delete
                  </ConfirmButton>
                </div>
              </>
            )}
          </article>
        ))}
      </Panel>

      <Panel title="Runs" aside={runs.length === 0 ? undefined : `${runs.length} on record`}>
        {runs.length === 0 && <p className="a-note">Nothing has been asked of a team yet.</p>}
        {runs.map((run) => (
          <article className="feed-item" key={run.id}>
            <div className="f-meta">
              <Badge tone={toneFor(run.state)}>{run.state}</Badge>
              <b>{run.team_id}</b>
              <span>round {run.round}</span>
            </div>
            <p className="f-body">{run.request}</p>
            <div className="a-actions">
              <Button size="sm" onClick={() => setOpen(run.id)}>Open</Button>
            </div>
          </article>
        ))}
      </Panel>
    </>
  );
}

interface TeamsProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * Departments: a director agent that plans, specialists that answer, and a folder that is the
 * delivery.
 *
 * Beside the Agents tab rather than inside it, because the two answer different questions: that one
 * is who exists, this one is who works together and on what. Nothing here reads a delivery — the
 * folder lives in the managed root, so the Files tab already shows it, and a second file browser
 * would be a second answer to where a department's work is.
 */
function Teams({ token, connection }: TeamsProps) {
  if (token === null || connection !== "connected") {
    return (
      <section className="agents">
        <Teach title="The departments are waiting for the daemon.">
          Teams live in the núcleo, and so does the work they do. Connect to it and they come back
          exactly as you left them.
        </Teach>
      </section>
    );
  }

  return (
    <section className="agents">
      <Departments token={token} />
    </section>
  );
}

export default Teams;
