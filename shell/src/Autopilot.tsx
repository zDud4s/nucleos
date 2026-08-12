import { useCallback, useEffect, useRef, useState } from "react";
import {
  approveProposal, cancelJob, createJob, getBudget, getFeed, getJob, getJobs, getProjects,
  getProposals, getScopedKills, getScoreboard, getShadowDecisions, rejectProposal,
  setProjectMode, setScopedKill, setVerdict,
  type AutopilotMode, type Budget, type ClassTally, type ConnectionState,
  type FeedEntry, type Job, type JobDetail, type ProjectSummary, type Proposal,
  type ScopedKill, type ShadowDecision,
} from "./api";
import {
  agreementRate, autopilotState, budgetStatusLabel, classifierVerdictLabel, feedKindLabel,
  formatUsd, groupScoreboardByMode, jobIsLive, jobItemLabel, jobItemTone, jobProgress,
  jobStageLabel, killSwitchLabel, periodLabel, promotionBlock, promotionReadiness,
  queueBlock, readinessCriterionLabel,
  readinessGap, relativeTime, REVIEW_ALLOW, REVIEW_BLOCK, scoreboardReadiness,
  SWAMPED_THRESHOLD, totalPending,
} from "./derive";
import { Badge, Banner, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

interface AutopilotProps {
  token: string | null;
  connection: ConnectionState;
  killEngaged: boolean | null;
  killBusy: boolean;
  toggleKill: (engaged: boolean) => Promise<void>;
}

interface ProjectCardProps { project: ProjectSummary; scopedKills: ScopedKill[] | null; token: string; refresh: () => Promise<void>; selected: boolean; onSelect: () => void; }

const MODES = ["off", "shadow", "active"] as const;

function ProjectCard({ project, scopedKills, token, refresh, selected, onSelect }: ProjectCardProps) {
  const [rootAsk, setRootAsk] = useState<AutopilotMode | null>(null);
  const [root, setRoot] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);
  // The §8.2 shadow-exit gate: `active` stays locked until the scoreboard says the project earned
  // it — every exercised class cleared, AND at least one of those a class the classifier withheld,
  // since a corpus of pure `allow` only ever proves it is permissive in the right places.
  const blocked = promotionBlock(project);
  const gateId = `gate-${project.project_id}`;
  // The WIP brake: a full approval queue is why an otherwise healthy project has gone quiet.
  const queueFull = queueBlock(project);
  const projectKilled =
    scopedKills?.some(
      (k) =>
        k.scope_type === "project" &&
        k.scope_id === project.project_id &&
        k.engaged,
    ) ?? false;

  async function toggleProjectKill() {
    setChanging(true);
    await setScopedKill(token, "project", project.project_id, !projectKilled);
    await refresh();
    setChanging(false);
  }

  async function changeMode(next: AutopilotMode) {
    setChanging(true);
    const typedRoot = root.trim();
    const result = await setProjectMode(
      token,
      project.project_id,
      next,
      typedRoot !== "" ? typedRoot : project.project_root ?? undefined,
    );

    if (result.ok) {
      setError(null);
      setRootAsk(null);
      setRoot("");
      await refresh();
    } else if (result.status === 422) {
      setRootAsk(next);
      setRoot((current) => (current !== "" ? current : project.project_root ?? ""));
      setError(
        "Prerequisites not met — the project needs .ai/workflow onboarding, a registered PreToolUse hook, git (for active), and a project root.",
      );
    } else {
      // "status 0" was the network path leaking an HTTP vocabulary it never
      // had: there was no response to have a status. Each of these asks the
      // user for a different move, so they cannot share one sentence.
      setError(
        result.fault === "unreachable"
          ? "Could not reach the daemon — it looks like the núcleo stopped. The mode was not changed."
          : result.fault === "unauthorized"
            ? "The daemon rejected this token, so the mode was not changed. Restart the núcleo so the shell can pick up the current one."
            : `The daemon refused the change (status ${result.status}).`,
      );
    }
    setChanging(false);
  }

  return (
    <article className="project">
      <span className="name">{project.project_id}</span>
      {project.pending > 0 && <Badge tone="pending">{project.pending} pending</Badge>}
      {projectKilled && <Badge tone="paused">paused</Badge>}
      {queueFull !== null && <Badge tone="paused">queue full</Badge>}
      <div className="seg" role="group" aria-label={`${project.project_id} autopilot mode`}>
        {MODES.map((mode) => {
          const locked = mode === "active" && blocked !== null;
          return (
            <button
              key={mode}
              type="button"
              disabled={changing || locked}
              aria-pressed={project.mode === mode}
              aria-describedby={locked ? gateId : undefined}
              title={locked ? `Not promotable yet — ${blocked}` : undefined}
              className={project.mode === mode ? `on ${mode}` : undefined}
              onClick={() => { if (project.mode !== mode) void changeMode(mode); }}
            >
              {mode}
            </button>
          );
        })}
      </div>
      {blocked !== null && (
        <p className="gate-note" id={gateId}>
          Locked until the scoreboard earns it — {blocked}. {readinessCriterionLabel()}.
        </p>
      )}
      {queueFull !== null && <p className="gate-note">{queueFull}</p>}
      <Button size="sm" intent="stop" disabled={changing} onClick={() => void toggleProjectKill()}>
        {projectKilled ? "Resume" : "Pause"}
      </Button>
      <Button size="sm" onClick={onSelect}>{selected ? "Hide details" : "View"}</Button>
      {rootAsk !== null && (
        <div className="root-ask">
          <label className="field">Project root — needed for shadow/active
            <input
              type="text"
              value={root}
              onChange={(event) => setRoot(event.target.value)}
              placeholder="C:\\path\\to\\project"
            />
          </label>
          <Button
            size="sm"
            intent="go"
            disabled={changing || root.trim() === ""}
            onClick={() => void changeMode(rootAsk)}
          >
            Retry {rootAsk}
          </Button>
        </div>
      )}
      {error !== null && <ErrorNote>{error}</ErrorNote>}
    </article>
  );
}

/** How far back a deliberate search reaches. The daemon's own ceiling; the ambient feed shows 50. */
const FEED_SEARCH_LIMIT = 200;

interface FeedPanelProps { token: string | null; feed: FeedEntry[] | null; loading: boolean; selectedProject: string | null; }

/**
 * The paper trail, and a way to look through it.
 *
 * The search runs on its own rather than through the page's 3-second batch, and its results are NOT
 * refreshed on that cadence. Two reasons, and they point the same way: rebuilding the batch on every
 * keystroke would restart six other requests that have nothing to do with this, and a list of
 * results that silently reorders while it is being read is one you lose your place in. The ambient
 * feed stays live; a search is a question, asked once.
 */
function FeedPanel({ token, feed, loading, selectedProject }: FeedPanelProps) {
  const [q, setQ] = useState("");
  const [kind, setKind] = useState("");
  const [since, setSince] = useState("");
  const [until, setUntil] = useState("");
  const [results, setResults] = useState<FeedEntry[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const asked = q.trim() !== "" || kind.trim() !== "" || since !== "" || until !== "";

  async function search() {
    if (token === null) return;
    setSearching(true);
    setFailed(null);
    const found = await getFeed(token, {
      ...(selectedProject === null ? { scope: "all" as const } : { projectId: selectedProject }),
      q: q.trim() || undefined,
      kind: kind.trim() || undefined,
      // The daemon wants RFC 3339 and answers 400 for anything else, so a date box's `YYYY-MM-DD`
      // is widened here rather than sent as-is. `since` opens the day and `until` closes it, which
      // is what picking one day in both boxes has to mean.
      since: since === "" ? undefined : `${since}T00:00:00Z`,
      until: until === "" ? undefined : `${until}T23:59:59Z`,
      limit: FEED_SEARCH_LIMIT,
    });
    setSearching(false);
    if (found === null) {
      setFailed("The daemon could not answer that search.");
      return;
    }
    setResults(found);
  }

  function clear() {
    setQ(""); setKind(""); setSince(""); setUntil("");
    setResults(null);
    setFailed(null);
  }

  // What is on screen: the answer to a question if one was asked, otherwise the live feed.
  const shown = results ?? feed;
  // Offered as suggestions, drawn from what is actually here. The daemon's set of kinds grows with
  // the daemon, so a hard-coded list would be wrong the first time a new one is emitted.
  const kinds = [...new Set((shown ?? []).map((entry) => entry.kind))].sort();

  return (
    <Panel
      dim
      title="Feed"
      aside={results !== null ? `${results.length} found` : selectedProject === null ? "latest across all projects" : selectedProject}
    >
      <details className="feed-search">
        <summary>search the record</summary>
        <form
          className="filters"
          onSubmit={(event) => { event.preventDefault(); if (!searching) void search(); }}
        >
          <label className="wide">
            Contains
            <input value={q} placeholder="anything in the summary" onChange={(event) => setQ(event.target.value)} />
          </label>
          <label>
            Kind
            <input list="feed-kinds" value={kind} placeholder="any" onChange={(event) => setKind(event.target.value)} />
            <datalist id="feed-kinds">
              {kinds.map((option) => <option key={option} value={option} />)}
            </datalist>
          </label>
          <label>
            From
            <input type="date" value={since} onChange={(event) => setSince(event.target.value)} />
          </label>
          <label>
            To
            <input type="date" value={until} onChange={(event) => setUntil(event.target.value)} />
          </label>
          <div className="form-actions">
            <Button type="submit" size="sm" disabled={searching || !asked}>
              {searching ? "Searching…" : "Search"}
            </Button>
            {results !== null && (
              <Button size="sm" onClick={clear}>Back to live</Button>
            )}
            <span className="cta-note">
              {results === null
                ? `Searching reaches back ${FEED_SEARCH_LIMIT} entries; the live feed shows the latest 50.`
                : "These results are frozen — the feed keeps moving underneath them."}
            </span>
          </div>
        </form>
        {failed !== null && <ErrorNote>{failed}</ErrorNote>}
      </details>
      {shown === null ? !loading && <ErrorNote>Could not load activity from the daemon.</ErrorNote>
        : shown.length === 0 && results !== null ? <Teach title="Nothing matches.">Widen the dates, or drop the kind — the record only goes back as far as the daemon has been running.</Teach>
        : shown.length === 0 ? <Teach title="The record starts here.">Runs, proposals, verdicts and budget events will leave their paper trail here.</Teach>
        : shown.map((entry) => <article className="feed-item" key={entry.id}>
            <div className="f-meta"><time dateTime={entry.created_at} title={entry.created_at}>{relativeTime(entry.created_at)}</time><span title={entry.kind}>{feedKindLabel(entry.kind)}</span></div>
            <p className="f-body"><b>{entry.project_id ?? "global"}</b> — {entry.summary}</p>
          </article>)}
    </Panel>
  );
}

interface JobsPanelProps {
  jobs: Job[] | null;
  loading: boolean;
  selectedProject: string | null;
  token: string;
  refresh: () => Promise<void>;
  isKill: boolean;
}

/**
 * What one job is doing, and what its list looks like.
 *
 * The queue is fetched per job and only while it is open. A job's items are the one thing here
 * that cannot be derived from the listing, and asking for every job's queue on a 3-second tick
 * would multiply the poll by the number of jobs on screen to show rows nobody has opened.
 */
function JobRow({ job, token, refresh, isKill }: { job: Job; token: string; refresh: () => Promise<void>; isKill: boolean }) {
  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const live = jobIsLive(job.status);

  useEffect(() => {
    if (!open) return;
    let current = true;
    void getJob(token, job.id).then((next) => { if (current) setDetail(next); });
    // A live job's queue moves under the reader — an item goes from running to passed while they
    // are looking at it — so an open row keeps up. A finished one never changes again, so polling
    // it would be a request per tick for a row that has already said everything it has to say.
    if (!live) return () => { current = false; };
    const id = setInterval(() => {
      void getJob(token, job.id).then((next) => { if (current) setDetail(next); });
    }, 3000);
    return () => { current = false; clearInterval(id); };
  }, [job.id, live, open, token]);

  async function cancel() {
    setCancelling(true);
    const result = await cancelJob(token, job.id);
    if (result.ok) {
      setError(null);
      await refresh();
    } else {
      // 409 is not a failure to report as one: the job ended between the render and the click, and
      // telling the user it "could not be cancelled" would send them looking for a fault.
      setError(
        result.status === 409
          ? "That job had already finished — nothing to stop."
          : result.fault === "unreachable"
            ? "Could not reach the daemon, so the job was not stopped."
            : "The daemon refused to stop this job.",
      );
    }
    setCancelling(false);
  }

  const progress = detail === null ? null : jobProgress(detail.items);

  return (
    <article className="job" key={job.id}>
      <div className="j-meta">
        <span className="j-id">job {job.id}{job.rule_name !== null && <> · {job.rule_name}</>}</span>
        <span className="j-proj">{job.project_id}</span>
        <time dateTime={job.created_at} title={job.created_at}>{relativeTime(job.created_at)}</time>
      </div>
      <p className="j-stage">
        {live ? <Badge tone={job.status === "waiting" ? "paused" : "pending"}>{job.status}</Badge>
              : <Badge tone={job.status === "completed" ? "active" : "off"}>{job.status}</Badge>}
        {" "}{jobStageLabel(job.status, job.wait_reason)}
      </p>
      <div className="j-act">
        <Button size="sm" onClick={() => setOpen((current) => !current)}>
          {open ? "Hide list" : "Show list"}
        </Button>
        {/* Only for a live job, and deliberately a different button from cancelling a run:
            cancelling a run stops one node, and a job parked for the budget or waiting for the
            slot has no node to stop. Without this its only ending is the four-hour ceiling. */}
        {live && (
          <ConfirmButton
            variant="link"
            size="sm"
            confirmLabel="Stop the whole job?"
            disabled={cancelling || isKill}
            onConfirm={() => void cancel()}
          >
            Stop job
          </ConfirmButton>
        )}
        {progress !== null && <span className="j-count">{progress.done}/{progress.total} done</span>}
      </div>
      {error !== null && <ErrorNote>{error}</ErrorNote>}
      {open && (detail === null
        ? <p className="j-note">Loading its list…</p>
        : detail.items.length === 0
          ? <Teach title="Nothing was on the list.">Its planner looked and found no work — a quiet, successful night, not a failure.</Teach>
          : <>
              <ol className="j-items">
                {detail.items.map((item) => (
                  <li className="j-item" key={item.ordinal}>
                    <Badge tone={jobItemTone(item.status)}>{jobItemLabel(item)}</Badge>
                    <span className="j-desc">{item.description}</span>
                  </li>
                ))}
              </ol>
              {detail.branch !== null && <p className="j-branch">Its work is on <code>{detail.branch}</code>.</p>}
            </>)}
    </article>
  );
}

/**
 * Starts a job by hand, without waiting for a schedule rule to fire.
 *
 * Until now a job could only be born from a `graph:` rule in `.ai/autopilot.yaml`, which meant the
 * only way to try one was to write a schedule and wait for it. It goes through the same front door
 * the scheduler uses, so it inherits every refusal: the kill switch, the roster, the project's mode
 * and the concurrency slots all answer here exactly as they would at 3am.
 *
 * Folded shut by default. A form that stands open above the job list would answer "start something"
 * on a page whose job is to answer "what is running".
 */
function NewJob({ token, projectId, onStarted, isKill }: {
  token: string;
  projectId: string | null;
  onStarted: () => Promise<void>;
  isKill: boolean;
}) {
  const [prompt, setPrompt] = useState("");
  const [project, setProject] = useState(projectId ?? "");
  const [budget, setBudget] = useState("");
  const [rounds, setRounds] = useState("");
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  async function start() {
    setBusy(true);
    setFailed(null);
    setNote(null);
    /**
     * Blank means "the house limit governs", which is not the same as zero and must not be sent as
     * one. `Number.parseFloat("")` is NaN rather than 0, but an explicit check is what says the
     * distinction was noticed rather than survived by accident.
     */
    const budgetUsd = budget.trim() === "" ? undefined : Number.parseFloat(budget);
    const maxRounds = rounds.trim() === "" ? undefined : Number.parseInt(rounds, 10);
    if (budgetUsd !== undefined && !Number.isFinite(budgetUsd)) {
      setBusy(false);
      setFailed("The budget must be a number of dollars, or blank for the house limit.");
      return;
    }
    if (maxRounds !== undefined && !Number.isInteger(maxRounds)) {
      setBusy(false);
      setFailed("Rounds must be a whole number, or blank for one round.");
      return;
    }
    const outcome = await createJob(token, {
      projectId: project.trim(),
      prompt: prompt.trim(),
      budgetUsd,
      maxRounds,
    });
    setBusy(false);
    if (!outcome.ok) {
      // The daemon's own sentence, verbatim. It distinguishes the two 409s — the kill switch and no
      // free slot — which the status code alone does not.
      setFailed(outcome.reason);
      return;
    }
    setNote(`Job ${outcome.jobId} started.`);
    setPrompt("");
    await onStarted();
  }

  return (
    <details className="job-new">
      <summary>Start a job by hand</summary>
      <form
        className="form-grid"
        onSubmit={(event) => {
          event.preventDefault();
          if (prompt.trim() === "" || project.trim() === "" || busy) return;
          void start();
        }}
      >
        <label>
          Project
          <input
            value={project}
            placeholder="which project"
            onChange={(event) => setProject(event.target.value)}
          />
        </label>
        <label>
          Budget
          <input
            value={budget}
            inputMode="decimal"
            placeholder="(the house limit)"
            onChange={(event) => setBudget(event.target.value)}
          />
        </label>
        <label>
          Rounds
          <input
            value={rounds}
            inputMode="numeric"
            placeholder="(one)"
            onChange={(event) => setRounds(event.target.value)}
          />
        </label>
        <label className="wide">
          What should it work on?
          <textarea
            rows={3}
            value={prompt}
            placeholder="The same sentence a graph: rule would carry."
            onChange={(event) => setPrompt(event.target.value)}
          />
        </label>
        <div className="form-actions">
          <Button
            type="submit"
            variant="approve"
            disabled={prompt.trim() === "" || project.trim() === "" || busy || isKill}
          >
            {busy ? "Starting…" : "Start job"}
          </Button>
          <span className="cta-note">
            {isKill
              ? "The kill switch is engaged, so nothing autonomous starts."
              : "It plans its own list, gates every item, and reviews at the end."}
          </span>
        </div>
      </form>
      {note !== null && <p className="gate-note">{note}</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </details>
  );
}

export function JobsPanel({ jobs, loading, selectedProject, token, refresh, isKill }: JobsPanelProps) {
  return (
    <Panel dim={isKill} title="Jobs" aside={selectedProject ?? "one trigger, several runs, one worktree"}>
      <NewJob token={token} projectId={selectedProject} onStarted={refresh} isKill={isKill} />
      {jobs === null ? !loading && <ErrorNote>Could not load jobs from the daemon.</ErrorNote>
        : jobs.length === 0
          ? <Teach title="No job has run yet.">A schedule rule with a <code>graph:</code> block turns one trigger into a sequence of runs over a shared worktree, so a night's work is not capped by one context window.</Teach>
          : jobs.map((job) => <JobRow key={job.id} job={job} token={token} refresh={refresh} isKill={isKill} />)}
    </Panel>
  );
}

interface ScoreboardPanelProps { projectId: string; scoreboard: ClassTally[] | null; }
function ScoreboardPanel({ projectId, scoreboard }: ScoreboardPanelProps) {
  const groupedTallies = groupScoreboardByMode(scoreboard ?? []);
  return (
    <Panel dim title="Scoreboard" aside={projectId}>
      {Object.keys(groupedTallies).length === 0
        ? <Teach title="Trust has a shape.">Reviewed shadow decisions show how each action class earns confidence. {readinessCriterionLabel()}, and promotion stays your call.</Teach>
        : <><p className="sb-criterion">{readinessCriterionLabel()} — promotion stays your call.</p>
          {Object.entries(groupedTallies).map(([mode, tallies]) => { const summary = scoreboardReadiness(tallies); return <div className="sb-group" key={mode}>
            <div className="sb-head"><span className="sb-mode">{mode}</span><span className="sb-count">{summary.ready} of {summary.total} ready</span></div>
            <ul className="readiness">{tallies.map((tally) => { const readiness = promotionReadiness(tally); const gap = readinessGap(tally); return <li className={readiness.ready ? "rc is-ready" : "rc"} key={tally.action_class}><span className="rc-class">{tally.action_class}</span><span className="rc-state">{readiness.ready ? "✓ ready" : gap ?? "—"}</span></li>; })}</ul>
            <details className="scoreboard"><summary>numbers</summary>
              <table><thead><tr><th>class</th><th>total</th><th>allow</th><th>pend</th><th>deny</th><th>reviewed</th><th>agree</th></tr></thead>
                <tbody>{tallies.map((tally) => { const rate = agreementRate(tally); return <tr key={tally.action_class}><td>{tally.action_class}</td><td>{tally.total}</td><td>{tally.would_allow}</td><td>{tally.would_pend}</td><td>{tally.would_deny}</td><td>{tally.reviewed}</td><td>{rate === null ? "—" : `${Math.round(rate * 100)}%`}</td></tr>; })}</tbody>
              </table>
            </details>
          </div>; })}</>}
    </Panel>
  );
}

interface ShadowReviewPanelProps { projectId: string; decisions: ShadowDecision[] | null; loading: boolean; token: string; refresh: () => Promise<void>; }
function ShadowReviewPanel({ projectId, decisions, loading, token, refresh }: ShadowReviewPanelProps) {
  const [pendingIds, setPendingIds] = useState<Set<number>>(() => new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  async function reviewDecision(decisionId: number, verdict: "approve" | "reject") {
    setPendingIds((current) => new Set(current).add(decisionId));
    setErrors((current) => { const next = { ...current }; delete next[decisionId]; return next; });
    const ok = await setVerdict(token, decisionId, verdict);
    if (ok) { await refresh(); } else { setErrors((current) => ({ ...current, [decisionId]: "Could not save this verdict." })); }
    setPendingIds((current) => { const next = new Set(current); next.delete(decisionId); return next; });
  }

  return (
    <Panel dim title="Shadow review" aside={projectId}>
      {decisions === null ? !loading && <ErrorNote>Could not load shadow decisions from the daemon.</ErrorNote>
        : decisions.length === 0 ? <Teach title="Nothing needs a second pair of eyes.">Shadow verdicts appear here when the agent has an action for you to judge.</Teach>
        : decisions.map((decision) => { const pending = pendingIds.has(decision.id); return <article className="decision" key={decision.id}>
            <div className="dc-meta"><span className="tool">{decision.tool_name}</span><span className="class">{decision.action_class}</span><span className={decision.decision === "deny" ? "deny" : "allow"}>{classifierVerdictLabel(decision.decision)}</span></div>
            {decision.reason !== null && <p className="dc-why">{decision.reason}</p>}
            {decision.tool_input !== null && <details className="dc-input"><summary>input</summary><pre>{decision.tool_input}</pre></details>}
            <div className="dc-act">
              <span className="dc-ask">Would you have allowed it?</span>
              <Button size="sm" intent="go" disabled={pending} onClick={() => void reviewDecision(decision.id, REVIEW_ALLOW.verdict)}>{REVIEW_ALLOW.label}</Button>
              <Button size="sm" intent="stop" disabled={pending} onClick={() => void reviewDecision(decision.id, REVIEW_BLOCK.verdict)}>{REVIEW_BLOCK.label}</Button>
            </div>
            {errors[decision.id] !== undefined && <ErrorNote>{errors[decision.id]}</ErrorNote>}
          </article>; })}
    </Panel>
  );
}

interface ApprovalQueuePanelProps { proposals: Proposal[] | null; loading: boolean; token: string; refresh: () => Promise<void>; isKill: boolean; isSwamped: boolean; }
export function ApprovalQueuePanel({ proposals, loading, token, refresh, isKill, isSwamped }: ApprovalQueuePanelProps) {
  const [pendingIds, setPendingIds] = useState<Set<number>>(() => new Set());
  // Keyed by proposal AND action: approve and reject are separate buttons and
  // either one being armed is a decision the panel must not disturb.
  const [armedKeys, setArmedKeys] = useState<Set<string>>(() => new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  function setArmed(key: string, armed: boolean) {
    setArmedKeys((current) => {
      const next = new Set(current);
      if (armed) next.add(key); else next.delete(key);
      return next;
    });
  }

  function startAction(proposalId: number) {
    setPendingIds((current) => new Set(current).add(proposalId));
    setErrors((current) => { const next = { ...current }; delete next[proposalId]; return next; });
  }
  function finishAction(proposalId: number) {
    setPendingIds((current) => { const next = new Set(current); next.delete(proposalId); return next; });
  }
  async function approve(proposalId: number) {
    startAction(proposalId);
    // The daemon's own sentence rather than one written here: its 409 means either "somebody
    // already decided this" or "this can never resume", and only it knows which.
    const outcome = await approveProposal(token, proposalId);
    if (outcome.ok) { await refresh(); } else { setErrors((current) => ({ ...current, [proposalId]: outcome.reason })); }
    finishAction(proposalId);
  }
  async function reject(proposalId: number) {
    startAction(proposalId);
    const ok = await rejectProposal(token, proposalId);
    if (ok) { await refresh(); } else { setErrors((current) => ({ ...current, [proposalId]: "Could not reject this proposal." })); }
    finishAction(proposalId);
  }
  // Approve is the MORE consequential of the two — it resumes an agent run
  // that was stopped precisely because it asked to leave its allowlist — so it
  // asks the same second question reject always did.
  const actionButtons = (proposal: Proposal, pending: boolean, compact = false) => <div className={compact ? "d-act" : "a-actions"}>
    <ConfirmButton variant="approve" size={compact ? "sm" : "md"} confirmLabel={compact ? "Approve?" : "Approve & resume?"} disabled={pending || isKill} onArmedChange={(armed) => setArmed(`${proposal.id}:approve`, armed)} onConfirm={() => void approve(proposal.id)}>{compact ? "Approve" : "Approve & resume"}</ConfirmButton>
    <ConfirmButton variant="link" size="sm" confirmLabel="Discard worktree?" disabled={pending || isKill} onArmedChange={(armed) => setArmed(`${proposal.id}:reject`, armed)} onConfirm={() => void reject(proposal.id)}>{compact ? "Reject" : "Reject and discard worktree"}</ConfirmButton>
  </div>;

  // A background refresh every 3 seconds re-sorts this list; the daemon decides
  // the order, so a row can move between the click that arms a button and the
  // click that confirms it — and the confirm would land on a different run's
  // proposal. While any decision is open, the panel shows the order the user is
  // actually looking at and lets the fresh data wait.
  const deciding = armedKeys.size > 0 || pendingIds.size > 0;
  const held = useRef<Proposal[] | null>(null);
  if (!deciding) held.current = proposals;
  const shown = deciding ? held.current ?? proposals : proposals;

  return (
    <Panel flat={false} dim={isKill} title="Approval queue" aside="approve resumes the run in its worktree · reject discards it">
      {isKill && <p className="a-note">Read-only while the kill switch is engaged — disengage to act.</p>}
      {shown === null ? !loading && <ErrorNote>Could not load pending proposals from the daemon.</ErrorNote>
        : shown.length === 0 ? <Teach title="Nothing waits for you.">Proposals appear when an active run reaches outside its allowlist. For now, every run has finished clean.</Teach>
        : isSwamped ? <div className="dense-queue">{shown.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="dense-row" key={proposal.id}><div className="d-id"><b>#{proposal.id} · run {proposal.run_id ?? "—"}</b><time dateTime={proposal.created_at} title={proposal.created_at}>{relativeTime(proposal.created_at)}</time></div><div className="d-main"><div className="d-cmd">{proposal.tool_name ?? "—"}</div><div className="d-why">{proposal.reasoning}</div>{errors[proposal.id] !== undefined && <ErrorNote>{errors[proposal.id]}</ErrorNote>}</div>{actionButtons(proposal, pending, true)}</article>; })}</div>
        : shown.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="approval-card" key={proposal.id}><div className="a-meta"><span>#{proposal.id} · run {proposal.run_id ?? "—"}</span><span className="proj">{proposal.project_id ?? "global"}</span><time dateTime={proposal.created_at} title={proposal.created_at}>{relativeTime(proposal.created_at)}</time></div><div className="a-cmd"><span className="verb">wants to run </span>{proposal.tool_name ?? "—"}</div><p className="a-reason">{proposal.reasoning}</p>{actionButtons(proposal, pending)}{errors[proposal.id] !== undefined && <ErrorNote>{errors[proposal.id]}</ErrorNote>}</article>; })}
    </Panel>
  );
}

interface ScopedKillPanelProps {
  scopedKills: ScopedKill[] | null;
  token: string;
  refresh: () => Promise<void>;
}

const TRIGGER_TYPES = ["scheduled", "repo"] as const;

function ScopedKillPanel({ scopedKills, token, refresh }: ScopedKillPanelProps) {
  const [busy, setBusy] = useState(false);

  function isEngaged(scopeType: string, scopeId: string): boolean {
    return (
      scopedKills?.some(
        (k) =>
          k.scope_type === scopeType && k.scope_id === scopeId && k.engaged,
      ) ?? false
    );
  }

  async function toggle(scopeType: string, scopeId: string) {
    setBusy(true);
    await setScopedKill(token, scopeType, scopeId, !isEngaged(scopeType, scopeId));
    await refresh();
    setBusy(false);
  }

  return (
    <Panel title="Trigger kill switches">
      <p className="scoped-kill-note">
        Pause one trigger type without the global panic switch. In-flight runs
        finish; only new firing stops.
      </p>
      {TRIGGER_TYPES.map((type) => {
        const engaged = isEngaged("trigger", type);
        return (
          <div key={type} className="scoped-kill-row">
            <span>
              {type} triggers{engaged ? " — paused" : ""}
              {engaged && <> <Badge tone="paused">paused</Badge></>}
            </span>
            <Button
              size="sm"
              intent={engaged ? "go" : "stop"}
              disabled={busy}
              onClick={() => void toggle("trigger", type)}
            >
              {engaged ? "Resume" : "Pause"}
            </Button>
          </div>
        );
      })}
    </Panel>
  );
}

function Autopilot({ token, connection, killEngaged, killBusy, toggleKill }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [scopedKills, setScopedKills] = useState<ScopedKill[] | null>(null);
  const [feed, setFeed] = useState<FeedEntry[] | null>(null);
  const [scoreboard, setScoreboard] = useState<ClassTally[] | null>(null);
  const [shadowDecisions, setShadowDecisions] = useState<ShadowDecision[] | null>(null);
  const [proposals, setProposals] = useState<Proposal[] | null>(null);
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [budget, setBudget] = useState<Budget | null>(null);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const batchSeq = useRef(0);
  const inFlight = useRef(0);

  const refresh = useCallback(async (background = false) => {
    if (token === null || connection !== "connected") return;
    // One batch at a time. Each round is ~7 requests; on a daemon slower than
    // the 3s tick they would pile up, and their answers can then land in any
    // order — the panel would flicker between two ages of the same truth.
    if (background && inFlight.current > 0) return;

    // A batch is only allowed to write the panel if it is still the newest one
    // and still about the project on screen. Switching projects bumps this
    // counter, so the previous project's in-flight answers are dropped instead
    // of overwriting the new project's numbers.
    const batch = (batchSeq.current += 1);
    const project = selectedProject;

    inFlight.current += 1;
    if (!background) setLoading(true);
    try {
      const [nextProjects, nextFeed, nextProposals, nextJobs, nextBudget, nextScopedKills] =
        await Promise.all([
          getProjects(token),
          getFeed(token, project ? { projectId: project } : { scope: "all" }),
          getProposals(token),
          getJobs(token, project ?? undefined),
          getBudget(token),
          getScopedKills(token),
        ]);
      let nextScoreboard: ClassTally[] | null = null;
      let nextShadowDecisions: ShadowDecision[] | null = null;
      if (project !== null) {
        [nextScoreboard, nextShadowDecisions] = await Promise.all([
          getScoreboard(token, project),
          getShadowDecisions(token, project),
        ]);
      }
      if (batch !== batchSeq.current) return;
      setProjects(nextProjects);
      setScopedKills(nextScopedKills);
      setFeed(nextFeed);
      setScoreboard(nextScoreboard);
      setShadowDecisions(nextShadowDecisions);
      setProposals(nextProposals);
      setJobs(nextJobs);
      setBudget(nextBudget);
    } finally {
      inFlight.current -= 1;
      if (!background && batch === batchSeq.current) setLoading(false);
    }
  }, [connection, selectedProject, token]);

  // Switching projects clears the previous project's scoped data so a switch
  // never shows the wrong project's numbers; an ambient refresh of the SAME
  // project keeps them so nothing flickers. Bumping the batch counter here is
  // what stops an already in-flight refresh from putting them back.
  useEffect(() => {
    batchSeq.current += 1;
    setScoreboard(null);
    setShadowDecisions(null);
  }, [selectedProject]);

  useEffect(() => {
    if (unavailable) {
      setProjects(null);
      setScopedKills(null);
      setFeed(null);
      setScoreboard(null);
      setShadowDecisions(null);
      setProposals(null);
      setJobs(null);
      setBudget(null);
      setLoading(true);
      return;
    }
    void refresh();
  }, [refresh, unavailable]);

  // Keep the tab live on the same 3s cadence as the health poll, silently:
  // a background refresh sets no loading flag and keeps prior data until fresh
  // data lands, so nothing flickers.
  useEffect(() => {
    if (unavailable) return;
    const id = setInterval(() => void refresh(true), 3000);
    return () => clearInterval(id);
  }, [refresh, unavailable]);

  const isFirst = projects?.length === 0;
  const isKill = killEngaged === true;
  const isBudget = budget?.paused === true;
  const isSwamped = (proposals?.length ?? 0) > SWAMPED_THRESHOLD;
  const pending = totalPending(projects ?? []);
  const state = autopilotState({
    killEngaged,
    budgetPaused: isBudget,
    isFirstProject: isFirst,
    proposalCount: proposals?.length ?? 0,
    pending,
  });
  const activeCount = (projects ?? []).filter((project) => project.mode === "active").length;
  const shadowCount = (projects ?? []).filter((project) => project.mode === "shadow").length;

  return (
    <section className="autopilot" data-state={state}>
      {unavailable ? <Teach title="Autopilot is waiting for the daemon.">Connect to the daemon to load projects, decisions and budget state.</Teach> : <>
        {isKill && (
          <Banner
            tone="kill"
            title="Kill switch engaged."
            action={
              <ConfirmButton
                variant="danger-solid"
                confirmLabel="Confirm disengage?"
                disabled={killBusy}
                onConfirm={() => void toggleKill(false)}
              >
                Disengage
              </ConfirmButton>
            }
          >
            Every run is stopped, schedulers are parked, and approvals are read-only until you disengage.
          </Banner>
        )}
        {isBudget && (
          <Banner tone="budget" title="Autopilot is paused by budget.">
            {budget?.reason ?? "No new runs will start until the budget window reopens. Approvals remain available."}
          </Banner>
        )}
        <h1 className="headline">{isKill ? <span className="bad">Everything is stopped.</span> : isBudget ? <><em>Paused by budget</em> — approvals still work.</> : isFirst ? <>No projects under autopilot yet.</> : isSwamped ? <><em>{proposals?.length ?? 0} decisions</em>, oldest last. Clear the queue.</> : pending > 0 ? <>All quiet — <em>{pending} decisions</em> waiting on you.</> : <>All quiet. <span className="ok">Nothing needs your signature.</span></>}</h1>
        <div className="statusline" title={budget ? budgetStatusLabel(budget) : undefined}>
          <span>{killSwitchLabel(killEngaged ?? false)}</span>
          <span>{budget ? <>{periodLabel(budget.period)} <b>{formatUsd(budget.window_spend_usd)}</b> <span className="cap">/ {budget.limit_usd === null ? "no limit" : formatUsd(budget.limit_usd)}</span></> : <>budget <b>—</b></>}</span>
          <span>{budget ? <>hour <b>{formatUsd(budget.hourly_spend_usd)}</b> <span className="cap">/ {budget.hourly_limit_usd === null ? "no limit" : formatUsd(budget.hourly_limit_usd)}</span></> : <>hour <b>—</b></>}</span>
          <span>reserve per run <b>{budget ? formatUsd(budget.per_run_reserve_usd) : "—"}</b></span>
          <span>{projects?.length ?? 0} projects · {activeCount} active · {shadowCount} shadow</span>
        </div>
        {loading && projects === null && <p className="a-note">Loading…</p>}
        {!loading && projects === null && <ErrorNote>Could not load projects from the daemon.</ErrorNote>}
        <div className="grid">
          <ApprovalQueuePanel proposals={proposals} loading={loading} token={token} refresh={refresh} isKill={isKill} isSwamped={isSwamped} />
          <div className="stack">
            <Panel dim={isKill} title="Projects">
              {projects !== null && projects.length === 0
                ? <Teach title="Bring your first project aboard.">Start in shadow mode, see what it would do, then promote it when the scoreboard earns your trust.</Teach>
                : projects?.map((project) => <ProjectCard key={project.project_id} project={project} scopedKills={scopedKills} token={token} refresh={refresh} selected={selectedProject === project.project_id} onSelect={() => setSelectedProject((current) => current === project.project_id ? null : project.project_id)} />)}
            </Panel>
            {selectedProject !== null && <div className="panel-scope"><strong>Viewing {selectedProject}</strong><Button size="sm" onClick={() => setSelectedProject(null)}>Show all</Button></div>}
            <JobsPanel jobs={jobs} loading={loading} selectedProject={selectedProject} token={token} refresh={refresh} isKill={isKill} />
            {selectedProject !== null && <><ShadowReviewPanel projectId={selectedProject} decisions={shadowDecisions} loading={loading} token={token} refresh={refresh} /><ScoreboardPanel projectId={selectedProject} scoreboard={scoreboard} /></>}
            <ScopedKillPanel scopedKills={scopedKills} token={token} refresh={refresh} />
            <FeedPanel token={token} feed={feed} loading={loading} selectedProject={selectedProject} />
          </div>
        </div>
      </>}
    </section>
  );
}

export default Autopilot;
