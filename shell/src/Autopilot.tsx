import { useCallback, useEffect, useState } from "react";
import {
  approveProposal, getBudget, getFeed, getProjects, getProposals,
  getScopedKills, getScoreboard, getShadowDecisions, rejectProposal,
  setProjectMode, setScopedKill, setVerdict,
  type AutopilotMode, type Budget, type ClassTally, type ConnectionState,
  type FeedEntry, type ProjectSummary, type Proposal, type ScopedKill,
  type ShadowDecision,
} from "./api";
import {
  agreementRate, autopilotState, budgetStatusLabel, formatUsd, groupScoreboardByMode,
  killSwitchLabel, periodLabel, promotionBlock, promotionReadiness, readinessCriterionLabel,
  readinessGap, relativeTime, scoreboardReadiness, SWAMPED_THRESHOLD, totalPending,
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
  // The §8.2 shadow-exit gate: `active` stays locked until the scoreboard says the project earned it.
  const blocked = promotionBlock(project);
  const gateId = `gate-${project.project_id}`;
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
      setError(`Request failed (status ${result.status}).`);
    }
    setChanging(false);
  }

  return (
    <article className="project">
      <span className="name">{project.project_id}</span>
      {project.pending > 0 && <Badge tone="pending">{project.pending} pending</Badge>}
      {projectKilled && <Badge tone="paused">paused</Badge>}
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

interface FeedPanelProps { feed: FeedEntry[] | null; loading: boolean; selectedProject: string | null; }
function FeedPanel({ feed, loading, selectedProject }: FeedPanelProps) {
  return (
    <Panel dim title="Feed" aside={selectedProject === null ? "latest across all projects" : selectedProject}>
      {feed === null ? !loading && <ErrorNote>Could not load activity from the daemon.</ErrorNote>
        : feed.length === 0 ? <Teach title="The record starts here.">Runs, proposals, verdicts and budget events will leave their paper trail here.</Teach>
        : feed.map((entry) => <article className="feed-item" key={entry.id}>
            <div className="f-meta"><time dateTime={entry.created_at} title={entry.created_at}>{relativeTime(entry.created_at)}</time><span>{entry.kind}</span></div>
            <p className="f-body"><b>{entry.project_id ?? "global"}</b> — {entry.summary}</p>
          </article>)}
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
            <div className="dc-meta"><span className="tool">{decision.tool_name}</span><span className="class">{decision.action_class}</span><span className={decision.decision === "deny" ? "deny" : "allow"}>would {decision.decision}</span></div>
            {decision.reason !== null && <p className="dc-why">{decision.reason}</p>}
            {decision.tool_input !== null && <details className="dc-input"><summary>input</summary><pre>{decision.tool_input}</pre></details>}
            <div className="dc-act">
              <Button size="sm" intent="go" disabled={pending} onClick={() => void reviewDecision(decision.id, "approve")}>Agree</Button>
              <Button size="sm" intent="stop" disabled={pending} onClick={() => void reviewDecision(decision.id, "reject")}>Disagree</Button>
            </div>
            {errors[decision.id] !== undefined && <ErrorNote>{errors[decision.id]}</ErrorNote>}
          </article>; })}
    </Panel>
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
    <Button variant="approve" size={compact ? "sm" : "md"} disabled={pending || isKill} onClick={() => void approve(proposal.id)}>{compact ? "Approve" : "Approve & resume"}</Button>
    <ConfirmButton variant="link" size="sm" confirmLabel="Discard worktree?" disabled={pending || isKill} onConfirm={() => void reject(proposal.id)}>{compact ? "Reject" : "Reject and discard worktree"}</ConfirmButton>
  </div>;
  return (
    <Panel flat={false} dim={isKill} title="Approval queue" aside="approve resumes the run in its worktree · reject discards it">
      {isKill && <p className="a-note">Read-only while the kill switch is engaged — disengage to act.</p>}
      {proposals === null ? !loading && <ErrorNote>Could not load pending proposals from the daemon.</ErrorNote>
        : proposals.length === 0 ? <Teach title="Nothing waits for you.">Proposals appear when an active run reaches outside its allowlist. For now, every run has finished clean.</Teach>
        : isSwamped ? <div className="dense-queue">{proposals.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="dense-row" key={proposal.id}><div className="d-id"><b>#{proposal.id} · run {proposal.run_id ?? "—"}</b><time dateTime={proposal.created_at} title={proposal.created_at}>{relativeTime(proposal.created_at)}</time></div><div className="d-main"><div className="d-cmd">{proposal.tool_name ?? "—"}</div><div className="d-why">{proposal.reasoning}</div>{errors[proposal.id] !== undefined && <ErrorNote>{errors[proposal.id]}</ErrorNote>}</div>{actionButtons(proposal, pending, true)}</article>; })}</div>
        : proposals.map((proposal) => { const pending = pendingIds.has(proposal.id); return <article className="approval-card" key={proposal.id}><div className="a-meta"><span>#{proposal.id} · run {proposal.run_id ?? "—"}</span><span className="proj">{proposal.project_id ?? "global"}</span><time dateTime={proposal.created_at} title={proposal.created_at}>{relativeTime(proposal.created_at)}</time></div><div className="a-cmd"><span className="verb">wants to run </span>{proposal.tool_name ?? "—"}</div><p className="a-reason">{proposal.reasoning}</p>{actionButtons(proposal, pending)}{errors[proposal.id] !== undefined && <ErrorNote>{errors[proposal.id]}</ErrorNote>}</article>; })}
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
  const [budget, setBudget] = useState<Budget | null>(null);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async (background = false) => {
    if (token === null || connection !== "connected") return;

    if (!background) setLoading(true);
    const [nextProjects, nextFeed, nextProposals, nextBudget, nextScopedKills] =
      await Promise.all([
        getProjects(token),
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
    setScopedKills(nextScopedKills);
    setFeed(nextFeed);
    setScoreboard(nextScoreboard);
    setShadowDecisions(nextShadowDecisions);
    setProposals(nextProposals);
    setBudget(nextBudget);
    if (!background) setLoading(false);
  }, [connection, selectedProject, token]);

  // Switching projects clears the previous project's scoped data so a switch
  // never shows the wrong project's numbers; an ambient refresh of the SAME
  // project keeps them so nothing flickers.
  useEffect(() => {
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
            {selectedProject !== null && <><ShadowReviewPanel projectId={selectedProject} decisions={shadowDecisions} loading={loading} token={token} refresh={refresh} /><ScoreboardPanel projectId={selectedProject} scoreboard={scoreboard} /></>}
            <ScopedKillPanel scopedKills={scopedKills} token={token} refresh={refresh} />
            <FeedPanel feed={feed} loading={loading} selectedProject={selectedProject} />
          </div>
        </div>
      </>}
    </section>
  );
}

export default Autopilot;
