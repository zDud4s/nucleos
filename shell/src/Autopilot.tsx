import { useCallback, useEffect, useState } from "react";
import {
  getKillSwitch,
  getProjects,
  setKillSwitch,
  setProjectMode,
  type AutopilotMode,
  type ConnectionState,
  type ProjectSummary,
} from "./api";
import { killSwitchLabel, modeBadge, totalPending } from "./derive";

interface AutopilotProps {
  token: string | null;
  connection: ConnectionState;
}

interface ProjectCardProps {
  project: ProjectSummary;
  token: string;
  refresh: () => Promise<void>;
}

function ProjectCard({ project, token, refresh }: ProjectCardProps) {
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
    <article className="card">
      <div className="card-header">
        <h3>{project.project_id}</h3>
        <div className="badges">
          <span className={`badge ${badge.tone}`}>{badge.label}</span>
          {project.pending > 0 && (
            <span className="badge pending">{project.pending} pending</span>
          )}
        </div>
      </div>

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

function Autopilot({ token, connection }: AutopilotProps) {
  const unavailable = connection !== "connected" || token === null;
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [killEngaged, setKillEngaged] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(true);
  const [togglingKillSwitch, setTogglingKillSwitch] = useState(false);

  const refresh = useCallback(async () => {
    if (token === null || connection !== "connected") return;

    setLoading(true);
    const [nextProjects, nextKillEngaged] = await Promise.all([
      getProjects(token),
      getKillSwitch(token),
    ]);
    setProjects(nextProjects);
    setKillEngaged(nextKillEngaged);
    setLoading(false);
  }, [connection, token]);

  useEffect(() => {
    if (unavailable) {
      setProjects(null);
      setKillEngaged(null);
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
                />
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}

export default Autopilot;
