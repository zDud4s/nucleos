import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useProjects } from "../data/system";
import { StateBadge } from "../ui";
import { ModeState } from "./ModeState";
import { ModeMap } from "./ModeMap";
import { ModeCode } from "./ModeCode";
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
 * Four modes and not seven tabs, because the four have genuinely different
 * shapes — a dense grid of panels, a graph of the repository, three columns
 * with a tree that persists, a graph of one workflow. Seven tabs would have
 * been seven variations on one grid, which is a second sidebar wearing a
 * disguise. The rule was never the count: a mode earns its place by having a
 * shape and a subject of its own, which is why Map and Workflows can both be
 * graphs without being the same mode — one draws the project, the other draws
 * one pipeline installed in it.
 */

/** The four modes, in the order the tabs read. */
const MODES = ["state", "map", "code", "workflows"] as const;
export type ProjectMode = (typeof MODES)[number];

const MODE_LABEL: Record<ProjectMode, string> = {
  state: "State",
  map: "Map",
  code: "Code",
  workflows: "Workflows",
};

/**
 * The segments three of these modes used to be, still answered.
 *
 * `estado`, `mapa` and `codigo` were the design's words, and this route was the
 * one place in the app where a segment was not English. Renaming them is only
 * safe because of this map: a URL is not an identifier somebody can rename for
 * you — it lives in a bookmark, a chat message, a browser's history, and none of
 * those are in the repository.
 *
 * **Accepted, and never redirected.** An old link draws the mode it always
 * meant, and every tab above points at the new segment, so the address
 * canonicalises itself the first time anybody clicks. Rewriting the location on
 * render would be a navigation nobody asked for, inside a component whose whole
 * job is to draw what the URL already says.
 *
 * It is a window and not a permanent alias. Deleting it is a judgement about
 * whether anybody still holds one of these links, which is a question about
 * people rather than about code — so it wants a date, and no test here can tell
 * you when.
 */
const RENAMED: Record<string, ProjectMode> = {
  estado: "state",
  mapa: "map",
  codigo: "code",
};

/**
 * A `$view` parameter as one of the four, or as one of the three it used to be.
 *
 * Falls back to `state` rather than 404ing, which is the rule the inspector this
 * replaces already followed: a route parameter is a string, anybody can type
 * one, and a typo in a path is not a missing page. `state` is the right landing
 * because it is the mode that answers the question somebody arrives with.
 */
export function normaliseMode(candidate: string | undefined): ProjectMode {
  const asked = candidate ?? "";
  if ((MODES as readonly string[]).includes(asked)) return asked as ProjectMode;
  return RENAMED[asked] ?? "state";
}

export function Workspace() {
  const params = useParams({ strict: false }) as { projectId?: string; view?: string };
  const search = useSearch({ strict: false }) as { run?: number };
  const navigate = useNavigate();
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

      {mode === "state" ? (
        <ModeState projectId={projectId} answered={projects.data !== undefined} />
      ) : null}
      {mode === "map" ? <ModeMap projectId={projectId} /> : null}
      {mode === "code" ? (
        <ModeCode
          projectId={projectId}
          run={search.run ?? null}
          /*
            Replaces rather than pushes: choosing a different run to review is changing what you are
            looking at, not going somewhere new, and a back button that walked through every run
            somebody glanced at would be a worse back button.
          */
          onPickRun={(run) =>
            void navigate({ to: `/projects/${projectId}/code`, search: { run }, replace: true })
          }
        />
      ) : null}
      {mode === "workflows" ? <ModeWorkflows projectId={projectId} /> : null}
    </div>
  );
}
