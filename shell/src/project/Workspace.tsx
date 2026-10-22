import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { MoreHorizontal } from "lucide-react";
import { AnalyticsView } from "../analytics/AnalyticsView";
import { useConcurrency } from "../data/fleet";
import { useProjects } from "../data/system";
import { useProjectWorkflows } from "../data/workflows";
import { openInVscode } from "../lib/vscode";
import { Button, Count, CopyButton, ErrorNote, PageHeader, QuietTips, StateBadge, Tip } from "../ui";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import { LeavingDialog, type Leaving } from "./LeavingDialog";
import { ModeState } from "./ModeState";
import { ModeMap } from "./ModeMap";
import { ModeCode } from "./ModeCode";
import { ModeWorkflows } from "./ModeWorkflows";
import { ModeGithub } from "./ModeGithub";
import "../pages/workspace.css";

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
 * Six modes and not seven tabs, because each has a genuinely different shape —
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
 * too. The mode and ceiling now live here too, in Permissions (Autonomy).
 *
 * **Analytics is the sixth (2026-10-17), on the same rule.** Its subject is this project over
 * time — cost, tokens, gate results and deliveries by UTC day — which none of the others read:
 * State is now, the rest are structure. Its shape is a column of daily series. It is the same
 * `AnalyticsView` the `/analytics` page draws for every project, scoped here, with its window kept
 * in the mode rather than in the address.
 */

/** The six modes, in the order the tabs read. Analytics is last: it reads time, which none of the others do. */
const MODES = ["state", "map", "code", "workflows", "authority", "analytics"] as const;
export type ProjectMode = (typeof MODES)[number];

/**
 * What each tab says, named for the task rather than for the author's model. The URL keys above are
 * unchanged on purpose (see `RENAMED`): "Overview" is `state`, "Review" is `code` (a run's worktree
 * to look over) and "Permissions" is `authority` (what this project's autonomous runs may do).
 */
const MODE_LABEL: Record<ProjectMode, string> = {
  state: "Overview",
  map: "Map",
  code: "Review",
  workflows: "Workflows",
  authority: "Permissions",
  analytics: "Analytics",
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
 * A `$view` parameter as one of the six, or as one of the four it used to be.
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
  /** `undefined` when the daemon could not be asked (`unavailable`): never a zero in its place. */
  count: number | undefined;
  means: string;
  /** The query failed and nothing was cached: the tab shows a mark instead of a number. */
  unavailable?: boolean;
}

/** What a tab says when the query behind its number failed. */
const COULD_NOT_READ = "Could not read this just now.";

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
 *
 * `unavailable` is the third answer next to a number and nothing: the query failed and has nothing
 * cached, so the tab says "could not read" rather than staying blank as if it had not asked yet.
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
  } else if (concurrency.isError) {
    holding.code = { count: undefined, means: COULD_NOT_READ, unavailable: true };
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
  } else if (workflows.isError) {
    holding.workflows = { count: undefined, means: COULD_NOT_READ, unavailable: true };
  }

  return holding;
}

/**
 * Whether a key press belongs to a field somebody is typing in — the switcher's own rule in
 * `Sidebar.tsx`, for the same reason: an editor may mean something by Ctrl+digit.
 */
/** Whether the chord is Cmd rather than Ctrl, for the hint and for `aria-keyshortcuts`. */
function onMac(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
}

function typing(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.isContentEditable || target.matches("input, textarea, select");
}

/**
 * Ctrl+1…6 (Cmd on a Mac) opens the modes in the order the tabs read.
 *
 * A desktop app with six fixed views and no way to reach them but the pointer is a keyboard user
 * tabbing through the whole header every time. The chord is the one every tabbed desktop app uses
 * for "the nth tab", and `aria-keyshortcuts` on each link says it to a screen reader. Anything that
 * already answered the chord keeps it (`defaultPrevented`), as with the switcher's Ctrl+P.
 */
function useModeShortcuts(projectId: string) {
  const navigate = useNavigate();
  useEffect(() => {
    function onKey(event: globalThis.KeyboardEvent) {
      // A held key repeats keydown; one press opens one mode.
      if (event.defaultPrevented || event.repeat || event.altKey || event.shiftKey) return;
      if (!(event.ctrlKey || event.metaKey) || typing(event.target)) return;
      const index = Number(event.key) - 1;
      const mode = MODES[index];
      if (!Number.isInteger(index) || mode === undefined) return;
      event.preventDefault();
      void navigate({ to: `/projects/${projectId}/${mode}` });
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate, projectId]);
}

export function Workspace() {
  const params = useParams({ strict: false }) as {
    projectId?: string;
    view?: string;
  };
  const search = useSearch({ strict: false }) as { run?: number };
  const navigate = useNavigate();
  const projectId = params.projectId ?? "";
  const mode = normaliseMode(params.view);

  const [editorFailed, setEditorFailed] = useState(false);
  const [leaving, setLeaving] = useState<Leaving | null>(null);
  const more = useRef<HTMLButtonElement>(null);
  const projects = useProjects();
  const project = projects.data?.find((row) => row.project_id === projectId);
  const holding = useHolding(projectId);
  useModeShortcuts(projectId);
  // A failure belongs to the project it happened on.
  useEffect(() => {
    setEditorFailed(false);
  }, [projectId]);
  const mac = onMac();
  // Known to be missing only once the roster has answered; until then the shell is drawn as usual.
  const missing = projects.data !== undefined && project === undefined;
  // Removing on a stale read would act on a project nobody can say is still there.
  const canLeaveRoster = project !== undefined && !(projects.isError && projects.data !== undefined);
  const folder = project?.project_root ?? null;
  // The folder's name reads better than the slug; the slug stays beside it when they differ.
  const name = folder === null ? projectId : (folder.split(/[\\/]/).filter(Boolean).pop() ?? projectId);

  // The title and the actions are drawn together, once the roster has answered. Until then the page
  // has no heading: a bare-slug title that the folder's name replaces a tick later is a header that
  // is up before its actions, and a reader (or a test waiting on the heading) takes it for done.
  const loadingHeader = (
    <header className="ui-page-header ui-page-header-unruled">
      <div className="ui-page-header-text">
        {/* Holds the title line's height, so the tabs below do not jump when the real h1 arrives.
            Not a heading: the first h1 on the page must be the project's own. */}
        <p className="ui-page-title invisible" aria-hidden="true">
          &nbsp;
        </p>
        <p role="status" className="ui-page-headline">
          Loading project…
        </p>
      </div>
    </header>
  );

  return (
    <>
      {projects.isPending ? (
        loadingHeader
      ) : (
        <PageHeader
          title={name}
          // The tab strip below carries the page's one rule.
          ruled={false}
          actions={
            missing ? undefined : (
              <>
                {folder === null ? null : (
                  <Button
                    variant="ghost"
                    onClick={() => {
                      setEditorFailed(false);
                      openInVscode(folder, null).catch(() => setEditorFailed(true));
                    }}
                  >
                    Open in VS Code
                  </Button>
                )}
                <Link
                  to="/projects/$projectId/inspect/$view"
                  params={{ projectId, view: "browse" }}
                  className="ui-button ui-button-ghost"
                >
                  Browse files
                </Link>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <button
                      ref={more}
                      type="button"
                      className="ui-icon-button"
                      aria-label="More actions"
                      title="More actions"
                    >
                      <MoreHorizontal className="ui-icon-button-glyph" strokeWidth={1.5} aria-hidden="true" />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {canLeaveRoster ? (
                      <DropdownMenuItem onSelect={() => setLeaving("remove")}>
                        Remove from NucleOS…
                      </DropdownMenuItem>
                    ) : null}
                    <DropdownMenuItem variant="destructive" onSelect={() => setLeaving("delete")}>
                      Delete this folder…
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            )
          }
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
              {missing ? (
                <span>Not found</span>
              ) : (
                <>
                  {/* "shadow" alone does not say shadow of what. */}
                  <span>Autopilot</span>
                  <Link to="/projects/$projectId/authority" params={{ projectId }}>
                    <StateBadge domain="autopilot" state={project?.mode} />
                  </Link>
                  {name !== projectId ? <span className="font-mono text-xs">{projectId}</span> : null}
                  {folder === null ? (
                    <span>no folder named</span>
                  ) : (
                    <>
                      <span className="truncate font-mono text-xs" title={folder}>
                        {folder}
                      </span>
                      <CopyButton value={folder} label="project folder" spoken={false} />
                    </>
                  )}
                </>
              )}
            </span>
          }
        />
      )}

      <LeavingDialog
        leaving={leaving}
        projectId={projectId}
        projectRoot={project?.project_root ?? null}
        onClose={() => setLeaving(null)}
        returnFocus={() => more.current?.focus()}
      />

      {editorFailed ? (
        <ErrorNote>Could not open VS Code: nothing on this machine answers vscode://.</ErrorNote>
      ) : null}

      {projects.isError && projects.data === undefined ? (
        <ErrorNote onRetry={() => void projects.refetch()}>
          Could not load the project list, so this project cannot be checked. Is the background service running?
        </ErrorNote>
      ) : null}

      {missing ? (
        <section className="ws-missing" aria-label="Project not found">
          <p className="ui-note">
            No project called <code>{projectId}</code> is registered. It may have been removed, or the link
            is out of date.
          </p>
          <Link to="/projects">Back to Projects</Link>
        </section>
      ) : (
        <>
          {/*
            The six modes, dressed as the vendored `Tabs` and still links.

            `ui-tab-list` and `ui-tab` are the shared classes Radix's Tabs wears, so this
            strip is the same object on screen as the tabs in the Bench. What it is NOT is
            Radix's `Tabs`, and that is deliberate: each mode is a URL, so a tab here has to
            be an `<a href>` that can be copied, opened in a second window, and — the case
            `Workspace.test.tsx` pins — carry an old segment's replacement (Portuguese, or
            `github`) in its `href` so the address canonicalises itself on the first press. A `role="tab"`
            button has none of that — Radix's `Trigger` would swap the link role and
            `aria-current` for `role="tab"` and `aria-selected`, and collapse six tab stops
            into one roving one. `pj-tabs` on the inspector is the same decision.

            What IS adopted is the rule underneath the appearance: **the active indicator is
            `--text` and never `--accent`**, drawn by `.ui-tab[data-state="active"]`. `.ui-current`
            does not fit here: it is an inset rule on the LEADING edge, and the mark a tab strip
            needs is under the label.
          */}
          <nav aria-label="Project modes" className="ui-tab-list ws-tabs">
            {MODES.map((candidate, index) => {
              const holds = holding[candidate];
              // Quiet is for a door somebody has not walked through. The tab they are standing in keeps
              // full weight whatever it counts, or the strip's one "here" mark is the dimmest thing on it.
              const quiet = holds !== undefined && holds.count === 0 && candidate !== mode;
              return (
                <Tip
                  key={candidate}
                  className="ws-tab-tip"
                  persist
                  tip={
                    <>
                      {holds === undefined ? null : <span className="ws-tab-means">{holds.means}</span>}
                      <span className="ws-tab-keys">{`${mac ? "Cmd" : "Ctrl"} ${index + 1}`}</span>
                    </>
                  }
                >
                  {({ tipId }) => (
                    <Link
                      to={`/projects/${projectId}/${candidate}`}
                      aria-current={candidate === mode ? "page" : undefined}
                      /* Only the chord this machine answers; announcing both said a key that does nothing. */
                      aria-keyshortcuts={`${mac ? "Meta" : "Control"}+${index + 1}`}
                      aria-describedby={tipId}
                      /* What the vendored trigger says about itself, said the same way, so one
                         rule in `ui.css` draws the selected tab wherever it is. */
                      data-state={candidate === mode ? "active" : "inactive"}
                      /* A colour and not an opacity: `--text-muted` at 0.7 measured 4.17:1 in dark and 3.18:1
                         in light, under the 4.5:1 a 14px label needs. `.ui-tab[data-quiet]` stays above it. */
                      data-quiet={quiet ? "" : undefined}
                      className="ui-tab"
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
                      {holds?.unavailable === true ? (
                        <span className="ui-count" aria-hidden="true">
                          ·
                        </span>
                      ) : (
                        <Count n={holds?.count} />
                      )}
                    </Link>
                  )}
                </Tip>
              );
            })}
          </nav>

          {/* Every mode's ⓘ opens as a tip over the page, never as a paragraph inside its group. */}
          <QuietTips.Provider value={true}>
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
                  void navigate({
                    to: `/projects/${projectId}/code`,
                    search: { run },
                    replace: true,
                  })
                }
              />
            ) : null}
            {mode === "workflows" ? <ModeWorkflows projectId={projectId} /> : null}
            {mode === "authority" ? <ModeGithub projectId={projectId} /> : null}
            {mode === "analytics" ? <AnalyticsView projectId={projectId} /> : null}
          </QuietTips.Provider>
        </>
      )}
    </>
  );
}
