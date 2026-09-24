import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { useConcurrency } from "../data/fleet";
import { useProjects } from "../data/system";
import { useProjectWorkflows } from "../data/workflows";
import { Count, PageHeader, StateBadge } from "../ui";
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
 * that persists, a graph of one workflow, a status line over five sections read
 * top to bottom.
 * Seven tabs would have been seven variations on one grid, which is a second
 * sidebar wearing a disguise. The rule was never the count: a mode earns its
 * place by having a shape and a subject of its own, which is why Map and
 * Workflows can both be graphs without being the same mode — one draws the
 * project, the other draws one pipeline installed in it.
 *
 * **Authority is the fifth, and it is held to that same rule rather than
 * excused from it.** Its subject is this project's *authority* — four tables
 * (GitHub operations, git operations, shell rules, landing targets) deciding
 * what its autonomous runs may do — which is nothing the other four are about:
 * State reads what has happened, Map reads what is in the folder, Code reads a
 * run's checkout, Workflows reads an installed bundle. And its shape is its
 * own: the remote, then a column of four tables, each with its own controls,
 * which is neither a grid nor a graph. Folding it into State's Settings panel
 * would have put a live autonomy control among the preferences.
 *
 * It was called "GitHub" until 2026-09-23, after its first section. Only that
 * section is about GitHub; the other four govern the queue, the worktrees'
 * shell and where work lands, which a project that never touches GitHub has
 * too. The mode and ceiling in State › Settings are the same subject and are
 * meant to move here.
 */

/** The five modes, in the order the tabs read. */
const MODES = ["state", "map", "code", "workflows", "authority"] as const;
export type ProjectMode = (typeof MODES)[number];

const MODE_LABEL: Record<ProjectMode, string> = {
  state: "State",
  map: "Map",
  code: "Code",
  workflows: "Workflows",
  authority: "Authority",
};

/**
 * The segments four of these modes used to be, still answered.
 *
 * `estado`, `mapa` and `codigo` were the design's words, and this route was the
 * one place in the app where a segment was not English; `github` was the
 * Authority mode's name while its label promised only its first section.
 * Renaming them is only safe because of this map: a URL is not an identifier somebody can rename for
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
  github: "authority",
};

/**
 * A `$view` parameter as one of the five, or as one of the four it used to be.
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
 * it is empty, and after the third time they stop pressing either. State, Map and Authority are
 * never in here: all three are true of a project the moment it exists, so a number beside them
 * would be measuring the project rather than saying whether the tab has anything in it. Authority
 * in particular would be numbered *zero* on every project that has declared nothing, which is the
 * project whose owner most needs to open it.
 *
 * **A number and not a mark, because a number is what was measured.** `0` here is a real answer —
 * the daemon said how many runs hold a worktree in this project and the answer was none — and this
 * app keeps the em dash for the opposite case, a reading nobody took. The State mode below has
 * four of those in its readings and there is a test that counts them; borrowing the mark for a
 * measured nought would take that distinction away at the top of every page in the workspace.
 *
 * **Nothing is hidden and nothing is disabled.** The tab keeps its route, its link and its press;
 * what changes is that it stops presenting itself as a peer while it holds nothing — and only while
 * it is NOT the page on screen. Dimming the current tab too took the "you are here" away from
 * somebody who had just pressed it: an empty Code mode read as current and faded at once.
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
    <>
      <PageHeader
        title={projectId}
        /*
          The mode through the one non-collapsing map, never a literal: off,
          shadow and active are three different promises about what happens here
          without being asked, and picking a tone locally is how that distinction
          starts to drift page by page. The folder is the other half of the
          sentence — what this project IS on this disk — and both belong on the
          header's one derived line rather than beside the name as a second title.
        */
        headline={
          <span className="inline-flex min-w-0 max-w-full items-baseline gap-2">
            <StateBadge domain="autopilot" state={project?.mode} />
            {project?.project_root === null ? (
              <span>no folder named</span>
            ) : (
              <span className="truncate font-mono text-xs">{project?.project_root}</span>
            )}
          </span>
        }
      />

      {/*
        The five modes, dressed as the vendored `Tabs` and still links.

        `ui-tab-list` and `ui-tab` are the shared classes Radix's Tabs wears, so this
        strip is the same object on screen as the tabs in the Bench. What it is NOT is
        Radix's `Tabs`, and that is deliberate: each mode is a URL, so a tab here has to
        be an `<a href>` that can be copied, opened in a second window, and — the case
        `Workspace.test.tsx` pins — carry an old segment's replacement (Portuguese, or
        `github`) in its `href` so the address canonicalises itself on the first press. A `role="tab"`
        button has none of that — Radix's `Trigger` would swap the link role and
        `aria-current` for `role="tab"` and `aria-selected`, and collapse five tab stops
        into one roving one. `pj-tabs` on the inspector is the same decision.

        What IS adopted is the rule underneath the appearance: **the active indicator is
        `--text` and never `--accent`**, drawn by `.ui-tab[data-state="active"]`. `.ui-current`
        does not fit here: it is an inset rule on the LEADING edge, and the mark a tab strip
        needs is under the label.
      */}
      <nav aria-label="Project modes" className="ui-tab-list mb-6">
        {MODES.map((candidate) => {
          const holds = holding[candidate];
          // Quiet is for a door somebody has not walked through. The tab they are standing in keeps
          // full weight whatever it counts, or the strip's one "here" mark is the dimmest thing on it.
          const quiet = holds !== undefined && holds.count === 0 && candidate !== mode;
          return (
            <Link
              key={candidate}
              to={`/projects/${projectId}/${candidate}`}
              aria-current={candidate === mode ? "page" : undefined}
              /* What the vendored trigger says about itself, said the same way, so one
                 rule in `ui.css` draws the selected tab wherever it is. */
              data-state={candidate === mode ? "active" : "inactive"}
              title={holds?.means}
              className={quiet ? "ui-tab opacity-[var(--opacity-quiet)]" : "ui-tab"}
            >
              {MODE_LABEL[candidate]}
              {/*
                Said where the decision to press is taken rather than after the press. Absent until
                the answer is in, so the strip never puts a nought on a tab it has not asked about —
                which is `Count`'s own rule for an `undefined` reading, so the ternary this used to
                spell out is now the component's. What the primitive adds over the hand-rolled span
                is `tabular-nums` and `nowrap`: five of these sit on one line and a digit that
                changes width under a poll would shift the labels beside it.
              */}
              <Count n={holds?.count} />
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
      {mode === "authority" ? <ModeGithub projectId={projectId} /> : null}
    </>
  );
}
