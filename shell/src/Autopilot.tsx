import { useCallback, useEffect, useState } from "react";
import {
  approveProposal, getBudget, getFeed, getKillSwitch, getProjects, getProposals,
  getScopedKills, getScoreboard, getShadowDecisions, rejectProposal, setKillSwitch,
  setProjectMode, setScopedKill,
  setVerdict, type AutopilotMode, type Budget, type ClassTally, type ConnectionState,
  type FeedEntry, type ProjectSummary, type Proposal, type ScopedKill,
  type ShadowDecision,
} from "./api";
import {
  agreementRate, budgetStatusLabel, formatUsd, groupScoreboardByMode,
  killSwitchLabel, modeBadge, periodLabel, promotionReadiness, totalPending,
} from "./derive";

interface AutopilotProps { token: string | null; connection: ConnectionState; }
interface ProjectCardProps { project: ProjectSummary; scopedKills: ScopedKill[] | null; token: string; refresh: () => Promise<void>; selected: boolean; onSelect: () => void; }

function ProjectCard({ project, scopedKills, token, refresh, selected, onSelect }: ProjectCardProps) {
  const [root, setRoot] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);
  const badge = modeBadge(project.mode);
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
    const result = await setProjectMode(
      token,
      project.project_id,
      next,
      root.trim() || undefined,
    );

    if (result.ok) {
      setError(null);
      await refresh();
    } else {
      setError(
        result.status === 422
          ? "Cannot enable: prerequisites not met — the project must be onboarded to .ai/workflow, have a registered PreToolUse hook, be a git repo (for active), and a valid project root must be supplied."
          : `Request failed (status ${result.status}).`,
      );
    }
    setChanging(false);
  }

  return (
    <article className="project">
      <span className="name">{project.project_id}</span>
      {project.pending > 0 && <span className="badge pending">{project.pending} pending</span>}
      <span className={`badge ${badge.tone}`}>{badge.label}</span>
      <span className="seg"><span className={project.mode === "off" ? "on off" : ""}>off</span><span className={project.mode === "shadow" ? "on shadow" : ""}>shadow</span><span className={project.mode === "active" ? "on active" : ""}>active</span></span>
      <div className="project-controls">
        <label className="field">Mode
          <select value={project.mode} disabled={changing} onChange={(event) => void changeMode(event.target.value as AutopilotMode)}>
            <option value="off">Off</option><option value="shadow">Shadow</option><option value="active">Active</option>
          </select>
        </label>
        <button type="button" disabled={changing} onClick={() => void toggleProjectKill()}>
          {projectKilled ? "Resume this project" : "Pause this project"}
        </button>
        <label className="field">Project root (needed for shadow/active)
          <input type="text" value={root} onChange={(event) => setRoot(event.target.value)} placeholder="C:\\path\\to\\project" />
        </label>
        <button className="view" type="button" onClick={onSelect}>{selected ? "Hide details" : "View"}</button>
      </div>
      {error !== null && <p className="error" role="alert">{error}</p>}
    </article>
  );
}

interface FeedPanelProps { feed: FeedEntry[] | null; loading: boolean; selectedProject: string | null; }
function FeedPanel({ feed, loading, selectedProject }: FeedPanelProps) {
  return (
    <section className="flat dim">
      <h2>Feed <small>{selectedProject === null ? "latest across all projects" : selectedProject}</small></h2>
      {feed === null ? !loading && <p className="error" role="alert">Could not load activity from the daemon.</p>
        : feed.length === 0 ? <div className="teach"><span className="t-title">The record starts here.</span>Runs, proposals, verdicts and budget events will leave their paper trail here.</div>
        : feed.map((entry) => <article className="feed-item" key={entry.id}>
            <div className="f-meta"><time dateTime={entry.created_at}>{entry.created_at}</time><span>{entry.kind}</span></div>
            <p className="f-body"><b>{entry.project_id ?? "global"}</b> — {entry.summary}</p>
          </article>)}
    </section>
  );
}

interface ScoreboardPanelProps { projectId: string; scoreboard: ClassTally[] | null; }
function ScoreboardPanel({ projectId, scoreboard }: ScoreboardPanelProps) {
  const groupedTallies = groupScoreboardByMode(scoreboard ?? []);
  return (
    <section className="flat dim">
      <h2>Scoreboard <small>{projectId}</small></h2>
      {Object.keys(groupedTallies).length === 0 ? <div className="teach"><span className="t-title">Trust has a shape.</span>Reviewed shadow decisions will show how each action class earns confidence.</div>
        : Object.entries(groupedTallies).map(([mode, tallies]) => <details className="scoreboard" key={mode} open>
            <summary>{mode} — shadow agreement</summary>
            <table><thead><tr><th>class</th><th>total</th><th>allow</th><th>pend</th><th>deny</th><th>reviewed</th><th>agree</th><th>ready?</th></tr></thead>
              <tbody>{tallies.map((tally) => { const rate = agreementRate(tally); const readiness = promotionReadiness(tally); return <tr key={tally.action_class}><td>{tally.action_class}</td><td>{tally.total}</td><td>{tally.would_allow}</td><td>{tally.would_pend}</td><td>{tally.would_deny}</td><td>{tally.reviewed}</td><td>{rate === null ? "—" : `${Math.round(rate * 100)}%`}</td><td>{readiness.ready ? "✓ ready" : "—"}</td></tr>; })}</tbody>
            </table>
          </details>)}
    </section>
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
    <section className="flat dim">
      <h2>Shadow review <small>{projectId}</small></h2>
      {decisions === null ? !loading && <p className="error" role="alert">Could not load shadow decisions from the daemon.</p>
        : decisions.length === 0 ? <div className="teach"><span className="t-title">Nothing needs a second pair of eyes.</span>Shadow verdicts appear here when the agent has an action for you to judge.</div>
        : decisions.map((decision) => { const pending = pendingIds.has(decision.id); return <article className="decision" key={decision.id}>
            <div className="dc-meta"><span className="tool">{decision.tool_name}</span><span className="class">{decision.action_class}</span><span className={decision.decision === "deny" ? "deny" : "allow"}>would {decision.decision}</span></div>
            {decision.reason !== null && <p className="dc-why">{decision.reason}</p>}
            {decision.tool_input !== null && <details className="dc-input"><summary>input</summary><pre>{decision.tool_input}</pre></details>}
            <div className="dc-act"><button className="mini" type="button" disabled={pending} onClick={() => void reviewDecision(decision.id, "approve")}>Agree</button><button className="mini no" type="button" disabled={pending} onClick={() => void reviewDecision(decision.id, "reject")}>Disagree</button></div>
            {errors[decision.id] !== undefined && <p className="error" role="alert">{errors[decision.id]}</p>}
          </article>; })}
    </section>
  );
}

interface ApprovalQueuePanelProps { proposals: Proposal[] | null; loading: boolean; token: string; refresh: () => Promise<void>; isKill: boolean; isSwamped: boolean; }
function ApprovalQueuePanel({ proposals, loading, token, refresh, isKill, isSwamped }: ApprovalQueuePanelProps) {
  const [pendingIds, setPendingIds] = useState<Set<number>>(() => new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  function startAction(proposalId: number) {
    setPendingIds((current) => new Set(current).add(proposalId));
    setErrors((current) => { const next = { ...current }; delete next[proposalId]; return next; });
  }
  function finishAction(proposalId: number) {
    setPendingIds((current) => { const next = new Set(current); next.delete(proposalId); return next; });
  }
  async function approve(proposalId: number) {
    startAction(proposalId);
    const resumedRunId = await approveProposal(token, proposalId);
    if (resumedRunId !== null) { await refresh(); } else { setErrors((current) => ({ ...current, [proposalId]: "Could not approve this proposal." })); }
    finishAction(proposalId);
  }
  async function reject(proposalId: number) {
    startAction(proposalId);
    const ok = await rejectProposal(token, proposalId);
    if (ok) { await refresh(); } else { setErrors((current) => ({ ...current, [proposalId]: "Could not reject this proposal." })); }
    finishAction(proposalId);
  }
  const actionButtons = (proposal: Proposal, pending: boolean, compact = false) => <div className={compact ? "d-act" : "a-actions"}>
    <button type="button" className="btn-approve" disabled={pending || isKill} onClick={() => void approve(proposal.id)}>{compact ? "Approve" : "Approve & resume"}</button>
    <button type="button" className="btn-reject" disabled={pending || isKill} onClick={() => void reject(proposal.id)}>{compact ? "Reject" : "Reject and discard worktree"}</button>
  </div>;
  return (
    <section className={isKill ? "dim" : ""}>
      <h2>Approval queue <small>approve resumes the run in its worktree · reject discards it</small></h2>
      {isKill && <p className="a-note">Read-only while the kill switch is engaged — disengage to act.</p>}
      {proposals === null ? !loading && <p className="error" role="alert">Could not load pending proposals from the daemon.</p>
        : proposals.length === 0 ? <div className="teach"><span className="t-title">Nothing waits for you.</span>Proposals appear when an active run reaches outside its allowlist. For now, every run has finished clean.</div>
        : isSwamped ? <div className="dense-queue">{proposals.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="dense-row" key={proposal.id}><div className="d-id"><b>#{proposal.id} · run {proposal.run_id ?? "—"}</b>{proposal.created_at}</div><div className="d-main"><div className="d-cmd">{proposal.tool_name ?? "—"}</div><div className="d-why">{proposal.reasoning}</div>{errors[proposal.id] !== undefined && <p className="error" role="alert">{errors[proposal.id]}</p>}</div>{actionButtons(proposal, pending, true)}</article>; })}</div>
        : proposals.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="approval-card" key={proposal.id}><div className="a-meta"><span>#{proposal.id} · run {proposal.run_id ?? "—"}</span><span className="proj">{proposal.project_id ?? "global"}</span><time dateTime={proposal.created_at}>{proposal.created_at}</time></div><div className="a-cmd"><span className="verb">wants to run </span>{proposal.tool_name ?? "—"}</div><p className="a-reason">{proposal.reasoning}</p>{actionButtons(proposal, pending)}{errors[proposal.id] !== undefined && <p className="error" role="alert">{errors[proposal.id]}</p>}</article>; })}
    </section>
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
    <section className="panel scoped-kill-panel">
      <h3>Trigger kill switches</h3>
      <p className="muted">
        Pause one trigger type without the global panic switch. In-flight runs
        finish; only new firing stops.
      </p>
      {TRIGGER_TYPES.map((type) => {
        const engaged = isEngaged("trigger", type);
        return (
          <div key={type} className="scoped-kill-row">
            <span>
              {type} triggers{engaged ? " — paused" : ""}
            </span>
            <button
              type="button"
              disabled={busy}
              onClick={() => void toggle("trigger", type)}
            >
              {engaged ? "Resume" : "Pause"}
            </button>
          </div>
        );
      })}
    </section>
  );
}

function Autopilot({ token, connection }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [scopedKills, setScopedKills] = useState<ScopedKill[] | null>(null);
  const [feed, setFeed] = useState<FeedEntry[] | null>(null);
  const [scoreboard, setScoreboard] = useState<ClassTally[] | null>(null);
  const [shadowDecisions, setShadowDecisions] = useState<ShadowDecision[] | null>(null);
  const [proposals, setProposals] = useState<Proposal[] | null>(null);
  const [budget, setBudget] = useState<Budget | null>(null);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [togglingKillSwitch, setTogglingKillSwitch] = useState(false);

  const refresh = useCallback(async () => {
    if (token === null || connection !== "connected") return;

    setLoading(true);
    setScoreboard(null);
    setShadowDecisions(null);
    const [nextProjects, nextKillEngaged, nextFeed, nextProposals, nextBudget, nextScopedKills] =
      await Promise.all([
        getProjects(token),
        getKillSwitch(token),
        getFeed(
          token,
          selectedProject ? { projectId: selectedProject } : { scope: "all" },
        ),
        getProposals(token),
        getBudget(token),
        getScopedKills(token),
      ]);
    let nextScoreboard: ClassTally[] | null = null;
    let nextShadowDecisions: ShadowDecision[] | null = null;
    if (selectedProject !== null) {
      [nextScoreboard, nextShadowDecisions] = await Promise.all([
        getScoreboard(token, selectedProject),
        getShadowDecisions(token, selectedProject),
      ]);
    }
    setProjects(nextProjects);
    setKillEngaged(nextKillEngaged);
    setScopedKills(nextScopedKills);
    setFeed(nextFeed);
    setScoreboard(nextScoreboard);
    setShadowDecisions(nextShadowDecisions);
    setProposals(nextProposals);
    setBudget(nextBudget);
    setLoading(false);
  }, [connection, selectedProject, token]);

  useEffect(() => {
    if (unavailable) {
      setProjects(null);
      setKillEngaged(null);
      setScopedKills(null);
      setFeed(null);
      setScoreboard(null);
      setShadowDecisions(null);
      setProposals(null);
      setBudget(null);
      setLoading(true);
      return;
    }
    void refresh();
  }, [refresh, unavailable]);

  async function toggleKillSwitch() {
    if (token === null) return;

    setTogglingKillSwitch(true);
    await setKillSwitch(token, !killEngaged);
    await refresh();
    setTogglingKillSwitch(false);
  }

  const isFirst = projects?.length === 0;
  const isKill = killEngaged === true;
  const isBudget = budget?.paused === true;
  const isSwamped = (proposals?.length ?? 0) > 3;
  const pending = totalPending(projects ?? []);
  const state = isKill ? "kill" : isBudget ? "budget" : isFirst ? "first" : isSwamped ? "swamped" : pending > 0 ? "pending" : "quiet";
  const activeCount = (projects ?? []).filter((project) => project.mode === "active").length;
  const shadowCount = (projects ?? []).filter((project) => project.mode === "shadow").length;

  return (
    <section className="autopilot" data-state={state}>
      {unavailable ? <div className="teach"><span className="t-title">Autopilot is waiting for the daemon.</span>Connect to the daemon to load projects, decisions and budget state.</div> : <>
        {isKill && <div className="banner kill-banner"><div><span className="b-title">Kill switch engaged.</span><p>Every run is stopped, schedulers are parked, and approvals are read-only until you disengage.</p></div><button type="button" disabled={togglingKillSwitch} onClick={() => void toggleKillSwitch()}>Disengage</button></div>}
        {isBudget && <div className="banner budget-banner"><div><span className="b-title">Autopilot is paused by budget.</span><p>{budget?.reason ?? "No new runs will start until the budget window reopens. Approvals remain available."}</p></div></div>}
        <ScopedKillPanel scopedKills={scopedKills} token={token} refresh={refresh} />
        <h1 className="headline">{isKill ? <span className="bad">Everything is stopped.</span> : isBudget ? <><em>Paused by budget</em> — approvals still work.</> : isFirst ? <>No projects under autopilot yet.</> : isSwamped ? <><em>{proposals?.length ?? 0} decisions</em>, oldest last. Clear the queue.</> : pending > 0 ? <>All quiet — <em>{pending} decisions</em> waiting on you.</> : <>All quiet. <span className="ok">Nothing needs your signature.</span></>}</h1>
        <div className="statusline" title={budget ? budgetStatusLabel(budget) : undefined}>
          <span>{killSwitchLabel(killEngaged ?? false)}</span>
          <span>{budget ? <>{periodLabel(budget.period)} <b>{formatUsd(budget.window_spend_usd)}</b> <span className="cap">/ {budget.limit_usd === null ? "no limit" : formatUsd(budget.limit_usd)}</span></> : <>budget <b>—</b></>}</span>
          <span>{budget ? <>hour <b>{formatUsd(budget.hourly_spend_usd)}</b> <span className="cap">/ {budget.hourly_limit_usd === null ? "no limit" : formatUsd(budget.hourly_limit_usd)}</span></> : <>hour <b>—</b></>}</span>
          <span>reserve per run <b>{budget ? formatUsd(budget.per_run_reserve_usd) : "—"}</b></span>
          <span>{projects?.length ?? 0} projects · {activeCount} active · {shadowCount} shadow</span>
        </div>
        {loading && projects === null && <p className="a-note">Loading…</p>}
        {!loading && projects === null && <p className="error" role="alert">Could not load projects from the daemon.</p>}
        <div className="grid">
          <ApprovalQueuePanel proposals={proposals} loading={loading} token={token} refresh={refresh} isKill={isKill} isSwamped={isSwamped} />
          <div className="stack">
            <section className={isKill ? "flat dim" : "flat"}><h2>Projects</h2>{projects !== null && projects.length === 0 ? <div className="teach"><span className="t-title">Bring your first project aboard.</span>Start in shadow mode, see what it would do, then promote it when the scoreboard earns your trust.</div> : projects?.map((project) => <ProjectCard key={project.project_id} project={project} scopedKills={scopedKills} token={token} refresh={refresh} selected={selectedProject === project.project_id} onSelect={() => setSelectedProject((current) => current === project.project_id ? null : project.project_id)} />)}</section>
            {selectedProject !== null && <div className="panel-scope"><strong>Viewing {selectedProject}</strong><button type="button" onClick={() => setSelectedProject(null)}>Show all</button></div>}
            {selectedProject !== null && <><ShadowReviewPanel projectId={selectedProject} decisions={shadowDecisions} loading={loading} token={token} refresh={refresh} /><ScoreboardPanel projectId={selectedProject} scoreboard={scoreboard} /></>}
            <FeedPanel feed={feed} loading={loading} selectedProject={selectedProject} />
          </div>
        </div>
      </>}
    </section>
  );
}

export default Autopilot;
