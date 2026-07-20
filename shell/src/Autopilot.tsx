import { useCallback, useEffect, useState } from "react";
import {
  getFeed,
  getKillSwitch,
  getProjects,
  getScoreboard,
  setKillSwitch,
  setProjectMode,
  type AutopilotMode,
  type ClassTally,
  type ConnectionState,
  type FeedEntry,
  type ProjectSummary,
} from "./api";
import {
  agreementRate,
  groupScoreboardByMode,
  killSwitchLabel,
  modeBadge,
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

function Autopilot({ token, connection }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [feed, setFeed] = useState<FeedEntry[] | null>(null);
  const [scoreboard, setScoreboard] = useState<ClassTally[] | null>(null);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [togglingKillSwitch, setTogglingKillSwitch] = useState(false);

  const refresh = useCallback(async () => {
    if (token === null || connection !== "connected") return;

    setLoading(true);
    const [nextProjects, nextKillEngaged, nextFeed] = await Promise.all([
      getProjects(token),
      getKillSwitch(token),
      getFeed(
        token,
        selectedProject ? { projectId: selectedProject } : { scope: "all" },
      ),
    ]);
    const nextScoreboard = selectedProject
      ? await getScoreboard(token, selectedProject)
      : null;
    setProjects(nextProjects);
    setKillEngaged(nextKillEngaged);
    setFeed(nextFeed);
    setScoreboard(nextScoreboard);
    setLoading(false);
  }, [connection, selectedProject, token]);

  useEffect(() => {
    if (unavailable) {
      setProjects(null);
      setKillEngaged(null);
      setFeed(null);
      setScoreboard(null);
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
              <ScoreboardPanel
                projectId={selectedProject}
                scoreboard={scoreboard}
              />
            )}
          </div>
        </>
      )}
    </section>
  );
}

export default Autopilot;
