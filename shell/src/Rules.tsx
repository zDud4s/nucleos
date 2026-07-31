import { useCallback, useEffect, useState } from "react";
import {
  getProjectRules, setProjectWipLimit,
  type ProjectRules, type RepoTriggerView, type ScheduleView,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

interface RulesProps {
  token: string;
  projectId: string;
}

/** When a rule next fires, or the reason it never will. */
function ScheduleRow({ rule }: { rule: ScheduleView }) {
  return (
    <article className="rule">
      <div className="f-meta">
        <span className="d-id">{rule.name}</span>
        <code className="r-cron">{rule.cron}</code>
        <span className="r-zone">{rule.timezone ?? "UTC"}</span>
        {rule.problem !== null
          ? <Badge tone="paused">never fires</Badge>
          : <Badge tone="active">armed</Badge>}
      </div>
      {rule.problem !== null ? (
        // The rule is in the file, looks like a rule, and does nothing. The daemon skips it and logs
        // at debug twice a minute, which is the same as saying nothing — so it is said here.
        <ErrorNote>{rule.problem}</ErrorNote>
      ) : (
        <p className="r-when">
          next {rule.next_fire_at === null ? "—" : relativeTime(rule.next_fire_at)}
          {rule.next_fire_at !== null && (
            <span className="r-exact" title={rule.next_fire_at}>{rule.next_fire_at}</span>
          )}
        </p>
      )}
      <p className="r-facts">
        <span>
          last {rule.last_fired_at === null ? "never" : relativeTime(rule.last_fired_at)}
        </span>
        {/* Against the cap rather than alone: the count only means something next to the ceiling
            that silences the rule for the rest of the day when it is reached. */}
        <span>{rule.fires_today}/{rule.daily_cap} today</span>
        {rule.cwd !== null && <span className="r-cwd">{rule.cwd}</span>}
      </p>
      <p className="r-prompt">{rule.prompt}</p>
    </article>
  );
}

function TriggerRow({ trigger }: { trigger: RepoTriggerView }) {
  return (
    <article className="rule">
      <div className="f-meta">
        <span className="d-id">{trigger.name}</span>
        <code className="r-cron">{trigger.branch}</code>
        {trigger.last_sha === null
          ? <Badge tone="off">arming</Badge>
          : <Badge tone="active">watching</Badge>}
      </div>
      <p className="r-facts">
        {trigger.last_sha === null
          // Being armed is not the same as being broken, and the difference matters: the first
          // commit it sees becomes the baseline, so nothing fires until the one after that.
          ? <span>no commit seen yet — the next one becomes its baseline</span>
          : <span>last seen <code>{trigger.last_sha.slice(0, 10)}</code></span>}
      </p>
      <p className="r-prompt">{trigger.prompt}</p>
    </article>
  );
}

interface QueueBrakeProps {
  token: string;
  projectId: string;
  rules: ProjectRules;
  onChanged: () => void;
}

/**
 * How much unreviewed work this project may leave waiting.
 *
 * The budget bounds what autonomy costs; this bounds what it costs YOU. It is the one brake that
 * clears itself — reviewing a proposal releases it — and until now it lived only in a column nothing
 * could write.
 */
function QueueBrake({ token, projectId, rules, onChanged }: QueueBrakeProps) {
  const [draft, setDraft] = useState(rules.wip_limit === null ? "" : String(rules.wip_limit));
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  async function save(limit: number | null) {
    setBusy(true);
    setFailed(null);
    const result = await setProjectWipLimit(token, projectId, limit);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 400
          ? "A ceiling cannot be negative — that would mean never starting anything again."
          : result.status === 404
            ? "The daemon has no such project."
            : "Could not set the ceiling.",
      );
      return;
    }
    onChanged();
  }

  const parsed = draft.trim() === "" ? null : Number(draft);
  const usable = parsed === null || (Number.isInteger(parsed) && parsed >= 0);

  return (
    <Panel
      title="Approval queue"
      aside={rules.queue_full ? "full — new work deferred" : undefined}
    >
      <p className="r-prompt">
        {rules.wip_limit === null
          ? <>No ceiling. This project starts autonomous work however much is already waiting for you.</>
          : <>
              <b>{rules.open_proposals}</b> of <b>{rules.wip_limit}</b> proposals waiting.
              {rules.queue_full
                ? " New autonomous work is deferred until you review one."
                : " Autonomous work continues until the ceiling is reached."}
            </>}
      </p>
      <form
        className="filters"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy || !usable) return;
          void save(parsed);
        }}
      >
        <label>
          Ceiling
          <input
            value={draft}
            inputMode="numeric"
            placeholder="(no ceiling)"
            onChange={(event) => setDraft(event.target.value)}
          />
        </label>
        <div className="form-actions">
          <Button type="submit" variant="approve" size="sm" disabled={busy || !usable}>
            {busy ? "Saving…" : "Set ceiling"}
          </Button>
          {rules.wip_limit !== null && (
            <Button
              size="sm"
              disabled={busy}
              onClick={() => { setDraft(""); void save(null); }}
            >
              Remove the ceiling
            </Button>
          )}
          <span className="cta-note">
            {/* Said here because it is the thing that makes this brake different from the budget:
                it is not a punishment with a timer, it is a queue that drains when you look at it. */}
            It releases itself the moment you review something.
          </span>
        </div>
      </form>
      {!usable && <p className="gate-note">A ceiling is a whole number, or empty for none.</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </Panel>
  );
}

/**
 * What this project does without being asked.
 *
 * Three things decided in two places that never met: the schedules and repo triggers in the
 * project's own `.ai/autopilot.yaml`, the gate command beside them, and the approval-queue ceiling
 * in the daemon's database. Each one decides whether autonomous work happens, and none of them was
 * visible anywhere — a project could be doing nothing because of a misspelt YAML key, a cron that
 * does not parse, or a queue that filled up last Tuesday, and all three looked identical from here.
 *
 * Deliberately readable for a project the inspector cannot open. A root that is gone still has a
 * ceiling and still has a recorded schedule state, and "the rules cannot be read" is itself the
 * answer someone came here for.
 */
function Rules({ token, projectId }: RulesProps) {
  const [rules, setRules] = useState<ProjectRules | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    const next = await getProjectRules(token, projectId);
    setRules(next);
    setLoading(false);
  }, [projectId, token]);

  useEffect(() => {
    setLoading(true);
    void refresh();
  }, [refresh]);

  if (loading) return <p className="a-note">Reading the rules…</p>;
  if (rules === null) return <ErrorNote>Could not read this project&apos;s rules.</ErrorNote>;

  return (
    <div className="stack">
      <Panel
        title="Schedules"
        aside={rules.rules_file === "present" ? `${rules.schedules.length} rules` : rules.rules_file}
      >
        {rules.rules_error !== null ? (
          // `deny_unknown_fields` turns a typo into an error precisely so it is not read as "no
          // rules" — but that error only ever reached a log line, so the project went quiet and
          // looked exactly like a project with nothing scheduled.
          <>
            <ErrorNote>
              The núcleo could not read <code>.ai/autopilot.yaml</code>, so every schedule and
              trigger in it is inert.
            </ErrorNote>
            <pre className="rd-stream">{rules.rules_error}</pre>
          </>
        ) : rules.project_root === null ? (
          <Teach title="This project has no root, so it has no rules file.">
            Schedules and triggers live in <code>.ai/autopilot.yaml</code> under the project root, and
            a root is recorded only while the project is in shadow or active mode.
          </Teach>
        ) : rules.schedules.length === 0 ? (
          <Teach title="Nothing is scheduled.">
            A <code>schedules:</code> entry in <code>.ai/autopilot.yaml</code> makes this project
            start work on a cron of its own. Each rule fires at most once a minute and{" "}
            {rules.schedules[0]?.daily_cap ?? 24} times a day.
          </Teach>
        ) : (
          rules.schedules.map((rule) => <ScheduleRow key={rule.name} rule={rule} />)
        )}
      </Panel>

      <Panel title="Repo triggers" aside={`${rules.repo_triggers.length} watching`}>
        {rules.repo_triggers.length === 0 ? (
          <Teach title="No branch is being watched.">
            A <code>repo_triggers:</code> entry starts work when a watched branch moves. It arms on
            the first commit it sees and fires on the next one.
          </Teach>
        ) : (
          rules.repo_triggers.map((trigger) => (
            <TriggerRow key={trigger.name} trigger={trigger} />
          ))
        )}
      </Panel>

      <Panel title="Gate" aside={rules.gate_command === null ? "none" : "configured"}>
        {rules.gate_command === null ? (
          <Teach title="No gate command.">
            A <code>gate_command:</code> in <code>.ai/autopilot.yaml</code> is run after a worktree
            run finishes, and its exit code is what decides whether the work is offered for approval
            at all. Without one, nothing checks the work before you do.
          </Teach>
        ) : (
          <pre className="rd-stream">{rules.gate_command}</pre>
        )}
      </Panel>

      <QueueBrake
        token={token}
        projectId={projectId}
        rules={rules}
        onChanged={() => void refresh()}
      />
    </div>
  );
}

export default Rules;
