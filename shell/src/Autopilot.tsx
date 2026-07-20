import { useCallback, useEffect, useState } from "react";
import {
  approveProposal,
  getBudget,
  getFeed,
  getKillSwitch,
  getProjects,
  getProposals,
  getScoreboard,
  getShadowDecisions,
  rejectProposal,
  setKillSwitch,
  setProjectMode,
  setVerdict,
  type AutopilotMode,
  type Budget,
  type ClassTally,
  type ConnectionState,
  type FeedEntry,
  type ProjectSummary,
  type Proposal,
  type ShadowDecision,
} from "./api";
import {
  agreementRate,
  budgetStatusLabel,
  formatUsd,
  groupScoreboardByMode,
  killSwitchLabel,
  modeBadge,
  periodLabel,
  totalPending,
} from "./derive";

interface AutopilotProps {
  token: string | null;
  connection: ConnectionState;
}

interface ProjectCardProps {
  project: ProjectSummary;
  token: string;
  refresh: () => Promise<void>;
  selected: boolean;
  onSelect: () => void;
}

function ProjectCard({
  project,
  token,
  refresh,
  selected,
  onSelect,
}: ProjectCardProps) {
  const [root, setRoot] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);
  const badge = modeBadge(project.mode);

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
    <article className={`card${selected ? " selected" : ""}`}>
      <div className="card-header">
        <h3>{project.project_id}</h3>
        <div className="badges">
          <span className={`badge ${badge.tone}`}>{badge.label}</span>
          {project.pending > 0 && (
            <span className="badge pending">{project.pending} pending</span>
          )}
        </div>
      </div>

      <button type="button" onClick={onSelect}>
        {selected ? "Hide details" : "View"}
      </button>

      <label className="field">
        Mode
        <select
          value={project.mode}
          disabled={changing}
          onChange={(event) =>
            void changeMode(event.target.value as AutopilotMode)
          }
        >
          <option value="off">Off</option>
          <option value="shadow">Shadow</option>
          <option value="active">Active</option>
        </select>
      </label>

      <label className="field">
        Project root (needed for shadow/active)
        <input
          type="text"
          value={root}
          onChange={(event) => setRoot(event.target.value)}
          placeholder="C:\\path\\to\\project"
        />
      </label>

      {error !== null && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </article>
  );
}

interface FeedPanelProps {
  feed: FeedEntry[] | null;
  loading: boolean;
  selectedProject: string | null;
}

function FeedPanel({ feed, loading, selectedProject }: FeedPanelProps) {
  return (
    <section className="panel feed-panel">
      <h3>
        Feed — {selectedProject === null ? "all projects" : selectedProject}
      </h3>
      {feed === null ? (
        !loading && (
          <p className="muted error" role="alert">
            Could not load activity from the daemon.
          </p>
        )
      ) : feed.length === 0 ? (
        <p className="muted">No activity yet.</p>
      ) : (
        <div className="feed">
          {feed.map((entry) => (
            <article className="feed-row" key={entry.id}>
              <div className="feed-meta">
                <time dateTime={entry.created_at}>{entry.created_at}</time>
                <span>{entry.project_id ?? "global"}</span>
              </div>
              <strong>{entry.kind}</strong>
              <p>{entry.summary}</p>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}

interface ScoreboardPanelProps {
  projectId: string;
  scoreboard: ClassTally[] | null;
}

function ScoreboardPanel({ projectId, scoreboard }: ScoreboardPanelProps) {
  const groupedTallies = groupScoreboardByMode(scoreboard ?? []);

  return (
    <section className="panel scoreboard">
      <h3>Scoreboard — {projectId}</h3>
      {Object.keys(groupedTallies).length === 0 ? (
        <p className="muted">No scoreboard data.</p>
      ) : (
        Object.entries(groupedTallies).map(([mode, tallies]) => (
          <div className="scoreboard-mode" key={mode}>
            <h4>{mode}</h4>
            <div className="scoreboard-table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>action_class</th>
                    <th>total</th>
                    <th>allow</th>
                    <th>pend</th>
                    <th>deny</th>
                    <th>reviewed</th>
                    <th>agreement</th>
                  </tr>
                </thead>
                <tbody>
                  {tallies.map((tally) => {
                    const rate = agreementRate(tally);
                    return (
                      <tr key={tally.action_class}>
                        <td>{tally.action_class}</td>
                        <td>{tally.total}</td>
                        <td>{tally.would_allow}</td>
                        <td>{tally.would_pend}</td>
                        <td>{tally.would_deny}</td>
                        <td>{tally.reviewed}</td>
                        <td>
                          {rate === null ? "—" : `${Math.round(rate * 100)}%`}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        ))
      )}
    </section>
  );
}

interface ShadowReviewPanelProps {
  projectId: string;
  decisions: ShadowDecision[] | null;
  loading: boolean;
  token: string;
  refresh: () => Promise<void>;
}

function ShadowReviewPanel({
  projectId,
  decisions,
  loading,
  token,
  refresh,
}: ShadowReviewPanelProps) {
  const [pendingIds, setPendingIds] = useState<Set<number>>(() => new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  async function reviewDecision(
    decisionId: number,
    verdict: "approve" | "reject",
  ) {
    setPendingIds((current) => new Set(current).add(decisionId));
    setErrors((current) => {
      const next = { ...current };
      delete next[decisionId];
      return next;
    });

    const ok = await setVerdict(token, decisionId, verdict);
    if (ok) {
      await refresh();
    } else {
      setErrors((current) => ({
        ...current,
        [decisionId]: "Could not save this verdict.",
      }));
    }

    setPendingIds((current) => {
      const next = new Set(current);
      next.delete(decisionId);
      return next;
    });
  }

  return (
    <section className="panel">
      <h3>Shadow review — {projectId}</h3>
      {decisions === null ? (
        !loading && (
          <p className="muted error" role="alert">
            Could not load shadow decisions from the daemon.
          </p>
        )
      ) : decisions.length === 0 ? (
        <p className="muted">No decisions awaiting review.</p>
      ) : (
        <div className="queue">
          {decisions.map((decision) => {
            const pending = pendingIds.has(decision.id);
            return (
              <article className="queue-row" key={decision.id}>
                <div className="queue-meta">
                  <strong>{decision.tool_name}</strong>
                  <span>{decision.action_class}</span>
                  <span>{decision.decision}</span>
                </div>
                {decision.reason !== null && <p>{decision.reason}</p>}
                {decision.tool_input !== null && (
                  <details>
                    <summary>input</summary>
                    <pre>{decision.tool_input}</pre>
                  </details>
                )}
                <div className="queue-actions">
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() =>
                      void reviewDecision(decision.id, "approve")
                    }
                  >
                    Approve
                  </button>
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => void reviewDecision(decision.id, "reject")}
                  >
                    Reject
                  </button>
                </div>
                {errors[decision.id] !== undefined && (
                  <p className="error" role="alert">
                    {errors[decision.id]}
                  </p>
                )}
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}

interface ApprovalQueuePanelProps {
  proposals: Proposal[] | null;
  loading: boolean;
  token: string;
  refresh: () => Promise<void>;
}

function ApprovalQueuePanel({
  proposals,
  loading,
  token,
  refresh,
}: ApprovalQueuePanelProps) {
  const [pendingIds, setPendingIds] = useState<Set<number>>(() => new Set());
  const [errors, setErrors] = useState<Record<number, string>>({});

  function startAction(proposalId: number) {
    setPendingIds((current) => new Set(current).add(proposalId));
    setErrors((current) => {
      const next = { ...current };
      delete next[proposalId];
      return next;
    });
  }

  function finishAction(proposalId: number) {
    setPendingIds((current) => {
      const next = new Set(current);
      next.delete(proposalId);
      return next;
    });
  }

  async function approve(proposalId: number) {
    startAction(proposalId);

    const resumedRunId = await approveProposal(token, proposalId);
    if (resumedRunId !== null) {
      await refresh();
    } else {
      setErrors((current) => ({
        ...current,
        [proposalId]: "Could not approve this proposal.",
      }));
    }

    finishAction(proposalId);
  }

  async function reject(proposalId: number) {
    startAction(proposalId);

    const ok = await rejectProposal(token, proposalId);
    if (ok) {
      await refresh();
    } else {
      setErrors((current) => ({
        ...current,
        [proposalId]: "Could not reject this proposal.",
      }));
    }

    finishAction(proposalId);
  }

  return (
    <section className="panel approval-panel">
      <h3>Approval queue (pending proposals)</h3>
      <p className="muted approval-help">
        Approve resumes the paused run in its worktree with a single-use
        authorization for the blocked action. Reject discards the worktree.
      </p>
      {proposals === null ? (
        !loading && (
          <p className="muted error" role="alert">
            Could not load pending proposals from the daemon.
          </p>
        )
      ) : proposals.length === 0 ? (
        <p className="muted">No proposals awaiting approval.</p>
      ) : (
        <div className="queue">
          {proposals.map((proposal) => {
            const pending = pendingIds.has(proposal.id);
            return (
              <article className="queue-row" key={proposal.id}>
                <div className="queue-meta">
                  <strong>Run {proposal.run_id ?? "—"}</strong>
                  <span>{proposal.project_id ?? "global"}</span>
                  <time dateTime={proposal.created_at}>
                    {proposal.created_at}
                  </time>
                </div>
                <p>
                  <strong>Blocked action:</strong> {proposal.tool_name ?? "—"}
                </p>
                <p>{proposal.reasoning}</p>
                <div className="queue-actions">
                  <button
                    type="button"
                    className="approve-action"
                    disabled={pending}
                    onClick={() => void approve(proposal.id)}
                  >
                    Approve &amp; resume
                  </button>
                  <button
                    type="button"
                    className="discard-action"
                    disabled={pending}
                    onClick={() => void reject(proposal.id)}
                  >
                    Reject (discard worktree)
                  </button>
                </div>
                {errors[proposal.id] !== undefined && (
                  <p className="error" role="alert">
                    {errors[proposal.id]}
                  </p>
                )}
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}

interface BudgetPanelProps {
  budget: Budget | null;
  loading: boolean;
}

function BudgetPanel({ budget, loading }: BudgetPanelProps) {
  return (
    <section
      className={budget?.paused ? "panel budget-panel paused" : "panel budget-panel"}
    >
      <h3>Budget</h3>
      {budget === null ? (
        !loading && (
          <p className="muted error" role="alert">
            Could not load the budget from the daemon.
          </p>
        )
      ) : (
        <>
          <strong>{budgetStatusLabel(budget)}</strong>
          {budget.paused && budget.reason !== null && (
            <p className="error" role="alert">
              {budget.reason}
            </p>
          )}
          <dl className="budget-detail">
            <div>
              <dt>Spent {periodLabel(budget.period)}</dt>
              <dd>
                {formatUsd(budget.window_spend_usd)}
                {budget.limit_usd !== null && ` / ${formatUsd(budget.limit_usd)}`}
              </dd>
            </div>
            <div>
              <dt>Spent in the last hour</dt>
              <dd>
                {formatUsd(budget.hourly_spend_usd)}
                {budget.hourly_limit_usd !== null &&
                  ` / ${formatUsd(budget.hourly_limit_usd)}`}
              </dd>
            </div>
            <div>
              <dt>Reserve per run</dt>
              <dd>{formatUsd(budget.per_run_reserve_usd)}</dd>
            </div>
          </dl>
        </>
      )}
    </section>
  );
}

function Autopilot({ token, connection }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [feed, setFeed] = useState<FeedEntry[] | null>(null);
  const [scoreboard, setScoreboard] = useState<ClassTally[] | null>(null);
  const [shadowDecisions, setShadowDecisions] = useState<
    ShadowDecision[] | null
  >(null);
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
    const [nextProjects, nextKillEngaged, nextFeed, nextProposals, nextBudget] =
      await Promise.all([
        getProjects(token),
        getKillSwitch(token),
        getFeed(
          token,
          selectedProject ? { projectId: selectedProject } : { scope: "all" },
        ),
        getProposals(token),
        getBudget(token),
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

  return (
    <section className="autopilot">
      <h2>Autopilot</h2>
      {unavailable && (
        <p className="muted">
          Connect to the daemon (open the desktop app) to load Autopilot.
        </p>
      )}

      {!unavailable && (
        <>
          <div className={killEngaged ? "kill-switch engaged" : "kill-switch"}>
            <span>{killSwitchLabel(killEngaged ?? false)}</span>
            <button
              type="button"
              disabled={togglingKillSwitch}
              onClick={() => void toggleKillSwitch()}
            >
              {killEngaged ? "Disengage" : "Engage"}
            </button>
          </div>

          <BudgetPanel budget={budget} loading={loading} />

          <div className="autopilot-summary">
            <strong>
              {totalPending(projects ?? [])} pending across {projects?.length ?? 0}{" "}
              projects
            </strong>
            <button type="button" disabled={loading} onClick={() => void refresh()}>
              Refresh
            </button>
          </div>

          {loading && projects === null && <p className="muted">Loading…</p>}
          {!loading && projects === null && (
            <p className="muted error" role="alert">
              Could not load projects from the daemon.
            </p>
          )}

          {projects !== null && (
            <div className="cards">
              {projects.map((project) => (
                <ProjectCard
                  key={project.project_id}
                  project={project}
                  token={token}
                  refresh={refresh}
                  selected={selectedProject === project.project_id}
                  onSelect={() =>
                    setSelectedProject((current) =>
                      current === project.project_id ? null : project.project_id,
                    )
                  }
                />
              ))}
            </div>
          )}

          <div className="panel-scope">
            {selectedProject === null ? (
              <span className="muted">Viewing all projects</span>
            ) : (
              <>
                <strong>Viewing {selectedProject}</strong>
                <button type="button" onClick={() => setSelectedProject(null)}>
                  Show all
                </button>
              </>
            )}
          </div>

          <div className="autopilot-panels">
            <FeedPanel
              feed={feed}
              loading={loading}
              selectedProject={selectedProject}
            />
            {selectedProject !== null && (
              <>
                <ScoreboardPanel
                  projectId={selectedProject}
                  scoreboard={scoreboard}
                />
                <ShadowReviewPanel
                  projectId={selectedProject}
                  decisions={shadowDecisions}
                  loading={loading}
                  token={token}
                  refresh={refresh}
                />
              </>
            )}
            <ApprovalQueuePanel
              proposals={proposals}
              loading={loading}
              token={token}
              refresh={refresh}
            />
          </div>
        </>
      )}
    </section>
  );
}

export default Autopilot;
