import { Link, useParams } from "@tanstack/react-router";
import { useProjects } from "../data/system";
import { StateBadge } from "../ui";
import { ModeEstado } from "./ModeEstado";
import { ModeCodigo } from "./ModeCodigo";
import { ModeWorkflows } from "./ModeWorkflows";

/**
 * One project, as a place rather than as a row.
 *
 * The rest of the app is *current*: Runs, Fleet and the Feed answer "what has
 * been happening", ordered by time with the exception first. This is the other
 * question — "how is this one now, and what can I do to it" — ordered by
 * structure, with the action beside the thing it acts on. The same run appears
 * in both without being duplicated: in `/runs` it is a line in a stream, and
 * here it is *this worktree has been busy for 41 minutes*, which is a fact that
 * either blocks you or frees you.
 *
 * Three modes and not seven tabs, because the three have genuinely different
 * shapes — a dense grid of panels, three columns with a tree that persists, a
 * graph canvas. Seven tabs would have been seven variations on one grid, which
 * is a second sidebar wearing a disguise.
 */

/** The three modes, in the order the tabs read. */
const MODES = ["estado", "codigo", "workflows"] as const;
export type ProjectMode = (typeof MODES)[number];

const MODE_LABEL: Record<ProjectMode, string> = {
  estado: "State",
  codigo: "Code",
  workflows: "Workflows",
};

/**
 * A `$view` parameter as one of the three.
 *
 * Falls back to `estado` rather than 404ing, which is the rule the inspector
 * this replaces already followed: a route parameter is a string, anybody can
 * type one, and a typo in a path is not a missing page. `estado` is the right
 * landing because it is the mode that answers the question somebody arrives
 * with.
 */
export function normaliseMode(candidate: string | undefined): ProjectMode {
  return (MODES as readonly string[]).includes(candidate ?? "")
    ? (candidate as ProjectMode)
    : "estado";
}

export function Workspace() {
  const params = useParams({ strict: false }) as { projectId?: string; view?: string };
  const projectId = params.projectId ?? "";
  const mode = normaliseMode(params.view);

  const projects = useProjects();
  const project = projects.data?.find((row) => row.project_id === projectId);

  return (
    <div className="flex flex-col gap-6">
      <header className="flex items-baseline gap-3">
        <h1 className="font-display text-2xl font-semibold tracking-[-0.02em] text-text">
          {projectId}
        </h1>
        {/*
          The mode through the one non-collapsing map, never a literal: off,
          shadow and active are three different promises about what happens here
          without being asked, and picking a tone locally is how that distinction
          starts to drift page by page.
        */}
        <StateBadge domain="autopilot" state={project?.mode} />
        {project?.project_root === null ? (
          <span className="text-sm text-text-faint">no folder named</span>
        ) : (
          <span className="truncate font-mono text-xs text-text-faint">
            {project?.project_root}
          </span>
        )}
      </header>

      <nav aria-label="Project modes" className="flex gap-1 border-b border-border">
        {MODES.map((candidate) => (
          <Link
            key={candidate}
            to={`/projects/${projectId}/${candidate}`}
            aria-current={candidate === mode ? "page" : undefined}
            className={
              candidate === mode
                ? "-mb-px border-b-2 border-accent px-3 py-2 text-sm font-medium text-text"
                : "-mb-px border-b-2 border-transparent px-3 py-2 text-sm text-text-muted hover:text-text"
            }
          >
            {MODE_LABEL[candidate]}
          </Link>
        ))}
      </nav>

      {mode === "estado" ? (
        <ModeEstado projectId={projectId} answered={projects.data !== undefined} />
      ) : null}
      {mode === "codigo" ? <ModeCodigo projectId={projectId} /> : null}
      {mode === "workflows" ? <ModeWorkflows projectId={projectId} /> : null}
    </div>
  );
}
