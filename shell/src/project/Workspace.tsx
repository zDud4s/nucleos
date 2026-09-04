import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useConcurrency } from "../data/fleet";
import { useProjects } from "../data/system";
import { useProjectWorkflows } from "../data/workflows";
import { StateBadge } from "../ui";
import { ModeState } from "./ModeState";
import { ModeMap } from "./ModeMap";
import { ModeCode } from "./ModeCode";
import { ModeWorkflows } from "./ModeWorkflows";
import { ModeGithub } from "./ModeGithub";

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
 * Five modes and not seven tabs, because each has a genuinely different shape —
 * a dense grid of panels, a graph of the repository, three columns with a tree
 * that persists, a graph of one workflow, four sections read top to bottom.
 * Seven tabs would have been seven variations on one grid, which is a second
 * sidebar wearing a disguise. The rule was never the count: a mode earns its
 * place by having a shape and a subject of its own, which is why Map and
 * Workflows can both be graphs without being the same mode — one draws the
 * project, the other draws one pipeline installed in it.
 *
 * **GitHub is the fifth, and it is held to that same rule rather than excused
 * from it.** Its subject is this project's *authority* — three tables deciding
 * what its autonomous runs may do — which is nothing the other four are about:
 * State reads what has happened, Map reads what is in the folder, Code reads a
 * run's checkout, Workflows reads an installed bundle. And its shape is its
 * own: a column of four declarations, each with its own form, which is neither
 * a grid nor a graph. Folding it into State's Settings panel would have put a
 * live autonomy control among the preferences.
 */

/** The five modes, in the order the tabs read. */
const MODES = ["state", "map", "code", "workflows", "github"] as const;
export type ProjectMode = (typeof MODES)[number];

const MODE_LABEL: Record<ProjectMode, string> = {
  state: "State",
  map: "Map",
  code: "Code",
  workflows: "Workflows",
  github: "GitHub",
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
 * A `$view` parameter as one of the five, or as one of the three it used to be.
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

/** What a conditional mode is holding: how much, and what that number counts. */
interface Holding {
  count: number;
  means: string;
}

/**
 * How much the two conditional modes are holding.
 *
 * **Two of the five are doors rather than places.** Code reads a *run's* worktree and Workflows
 * reads an installed bundle, and on most projects most of the time there is neither — so a strip
 * of equal tabs sends somebody through a click onto a page whose whole content explains why
 * it is empty, and after the third time they stop pressing either. State, Map and GitHub are never
 * in here: all three are true of a project the moment it exists, so a number beside them would be
 * measuring the project rather than saying whether the tab has anything in it. GitHub in
 * particular would be numbered *zero* on every project that has declared nothing, which is the
 * project whose owner most needs to open it.
 *
 * **A number and not a mark, because a number is what was measured.** `0` here is a real answer —
 * the daemon said how many runs hold a worktree in this project and the answer was none — and this
 * app keeps the em dash for the opposite case, a reading nobody took. The State mode below has
 * four of those in its readings and there is a test that counts them; borrowing the mark for a
 * measured nought would take that distinction away at the top of every page in the workspace.
 *
 * **Nothing is hidden and nothing is disabled.** The tab keeps its route, its link and its press;
 * what changes is that it stops presenting itself as a peer while it holds nothing.
 *
 * **Unanswered is not zero.** A mode is numbered only once the query behind it has come back.
 * Numbering on `undefined` would put a nought on both tabs on every open and then take it off,
 * and the flicker would be the most eye-catching thing on the screen.
 *
 * The two queries are the ones `Occupancy` and `WorkflowSummary` already ask. React Query answers
 * both out of one cache entry apiece, so this is a second reader and never a second source: the
 * tab and the panel cannot disagree about whether a run is working here.
 */
function useHolding(projectId: string): Partial<Record<ProjectMode, Holding>> {
  const concurrency = useConcurrency();
  const workflows = useProjectWorkflows(projectId);

  const holding: Partial<Record<ProjectMode, Holding>> = {};

  if (concurrency.data !== undefined) {
    /*
      Runs and not slots. A job or a team's item holds a worktree here too, and the Code mode reads
      a RUN's checkout — counting those would leave the tab looking live over a page that says
      there is nothing to review.
    */
    const runs =
      concurrency.data.projects
        .find((row) => row.project_id === projectId)
        ?.slots.filter((slot) => slot.owner_kind === "run").length ?? 0;
    holding.code = {
      count: runs,
      means:
        runs === 0
          ? "No run is working here, so there is nothing to review."
          : `${runs} run${runs === 1 ? "" : "s"} working here, with a worktree to review.`,
    };
  }

  if (workflows.data !== undefined) {
    const installed = workflows.data.length;
    holding.workflows = {
      count: installed,
      means:
        installed === 0
          ? "No workflow is installed here."
          : `${installed} workflow${installed === 1 ? "" : "s"} installed in this project.`,
    };
  }

  return holding;
}

export function Workspace() {
  const params = useParams({ strict: false }) as { projectId?: string; view?: string };
  const search = useSearch({ strict: false }) as { run?: number };
  const navigate = useNavigate();
  const projectId = params.projectId ?? "";
  const mode = normaliseMode(params.view);

  const projects = useProjects();
  const project = projects.data?.find((row) => row.project_id === projectId);
  const holding = useHolding(projectId);

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
        {MODES.map((candidate) => {
          const holds = holding[candidate];
          const quiet = holds !== undefined && holds.count === 0;
          return (
            <Link
              key={candidate}
              to={`/projects/${projectId}/${candidate}`}
              aria-current={candidate === mode ? "page" : undefined}
              title={holds?.means}
              className={
                candidate === mode
                  ? "-mb-px flex items-baseline gap-2 border-b-2 border-accent px-3 py-2 text-sm font-medium text-text"
                  : quiet
                    ? "-mb-px flex items-baseline gap-2 border-b-2 border-transparent px-3 py-2 text-sm text-text-faint hover:text-text"
                    : "-mb-px flex items-baseline gap-2 border-b-2 border-transparent px-3 py-2 text-sm text-text-muted hover:text-text"
              }
            >
              {MODE_LABEL[candidate]}
              {/*
                Said where the decision to press is taken rather than after the press. Absent until
                the answer is in, so the strip never puts a nought on a tab it has not asked about.
              */}
              {holds === undefined ? null : (
                <span className="font-mono text-xs text-text-faint">{holds.count}</span>
              )}
            </Link>
          );
        })}
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
      {mode === "github" ? <ModeGithub projectId={projectId} /> : null}
    </div>
  );
}
