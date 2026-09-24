import { useCallback, useId, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useConcurrency, useLiveRuns, type RunSearchResult } from "../data/fleet";
import { useProposals } from "../data/system";
import {
  absolutePath,
  asTree,
  useChanged,
  useRefreshRunReads,
  useRunBlame,
  useRunDiff,
  useRunFile,
  useRunWorktree,
  type TreeNode,
  type Worktree,
} from "../data/project-code";
import { openInVscode, vscodeUrl } from "../lib/vscode";
import { UI_LOCALE } from "../lib/locale";
import { isApiRefusal } from "../data/client";
import {
  Button,
  Count,
  ErrorNote,
  Panel,
  Quiet,
  RefusalNote,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
// By its own path until the barrel exports it — `ui/index.ts` is not this change's file.
import { Diff, type LineLink } from "../ui/Diff";

/**
 * "What is this?" — a review surface, not an IDE.
 *
 * The difference is about the **subject**. A file browser's subject is the repository, and it opens
 * on four thousand files; this one's subject is the **run**, so it opens on what changed and says
 * how many files nobody touched. The button that closes the screen is not *save* — it is *approve
 * and land*, and it lives with the proposal that asked.
 *
 * Where code is *changed* is VS Code, already open on the same machine and the same repository,
 * reached at the file and the line. Not embedded, and the argument is not about the desktop: this
 * núcleo approves work from Telegram and e-mail too, where an editor is useless and a good diff
 * reader is exactly right.
 */

export interface ModeCodeProps {
  projectId: string;
  /** The run being reviewed, from the route so a slot on the State mode can link straight to it. */
  run: number | null;
  onPickRun: (run: number) => void;
}

type View = "diff" | "file" | "blame";

export function ModeCode({ projectId, run, onPickRun }: ModeCodeProps) {
  const concurrency = useConcurrency();

  /*
    What each run IS, from the live listing — the only reading that carries a run's prompt, status
    and mode. Not fetched per run: the listing is already polled for the Fleet, and a run that has
    left it has left its slot, which the header then simply does not describe.
  */
  const live = useLiveRuns();
  const described = new Map((live.data ?? []).map((row) => [row.id, row]));

  /**
   * The runs there are to review, from the capacity readout.
   *
   * No route of its own: `/concurrency` already reports which runs hold a worktree in this project,
   * which is exactly the set that has anything to review. A second route would be a second answer
   * to a question already answered.
   */
  const runs =
    concurrency.data?.projects
      .find((row) => row.project_id === projectId)
      ?.slots.filter((slot) => slot.owner_kind === "run")
      .map((slot) => slot.owner_id) ?? [];

  const selected = run ?? runs[0] ?? null;

  /*
    Disconnected is not loading. A daemon that is down leaves `data` undefined forever, and a
    sentence saying "reading" over it would be the page lying about what it is doing — the one
    thing it is not allowed to do about freshness. A run named in the route can still be reviewed
    without the capacity reading, so only the case with nothing to go on stops here.
  */
  if (concurrency.data === undefined && concurrency.isError && selected === null) {
    return (
      <ErrorNote>
        The núcleo did not answer, so there is no telling which runs have anything to review here.
      </ErrorNote>
    );
  }

  if (concurrency.data === undefined && selected === null) {
    return (
      <p className="text-sm text-text-muted" role="status">
        Reading what there is to review…
      </p>
    );
  }

  if (selected === null) {
    /*
      `Teach` and not a card of this mode's own. This is the one shape that primitive exists for —
      an empty list somebody arrived at meaning to do something, where the useful thing to hold the
      space is how the machine works — and a bordered card beside it was a second answer to the
      same question at a different padding. The tab strip now says this before the press; what is
      left here is what a person needs once they have pressed anyway.
    */
    return (
      <Teach title={`Nothing to review in ${projectId}`}>
        <p>
          This mode reads a run&rsquo;s worktree. When one is working here, its changed files appear
          on the left — and the whole repository stays where it is, in the editor.
        </p>
        <InspectorLink projectId={projectId} />
      </Teach>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* The runs listed are the capacity reading's, so a failed poll says how old that list is. */}
      {concurrency.isError ? <StaleNote dataUpdatedAt={concurrency.dataUpdatedAt} /> : null}
      {/* Outside the keyed review below, so the pill just pressed keeps focus when its run opens. */}
      {runs.length > 1 ? (
        <RunPicker runs={runs} selected={selected} described={described} onPick={onPickRun} />
      ) : null}
      {/*
        Keyed by the run, so the file somebody had open and the view they read it in go with the run
        they belonged to. Carried over, a path from the last run opened the next one on a 404 —
        "that path is not in this run's worktree" — which read as a fault in the daemon.
      */}
      <Review
        key={selected}
        projectId={projectId}
        run={selected}
        described={described.get(selected)}
      />
    </div>
  );
}

/**
 * One run, under review.
 *
 * `path` and `view` are this component's state and not the location's, and that is a gap rather
 * than a choice: the route's search validator (`router.tsx`) keeps `run` and drops everything else,
 * so a `?path=` written here would be stripped on the next navigation. Until it admits them, a
 * reload returns to the whole run's diff.
 */
function Review({
  projectId,
  run,
  described,
}: {
  projectId: string;
  run: number;
  described: RunSearchResult | undefined;
}) {
  const [path, setPath] = useState("");
  const [view, setView] = useState<View>("diff");
  const changed = useChanged(projectId, run);
  const worktree = useRunWorktree(projectId, run);
  const refresh = useRefreshRunReads(projectId, run);
  const door = useEditorDoor();

  return (
    <>
      <ReviewHeader
        projectId={projectId}
        run={run}
        described={described}
        worktree={worktree.data}
      />

      <div className="grid grid-cols-1 gap-4 min-[60rem]:grid-cols-[16rem_minmax(0,1fr)]">
        <ChangedFiles
          changed={changed}
          worktree={worktree.data?.path}
          path={path}
          onPick={setPath}
          onRefresh={refresh}
          door={door}
        />
        <Centre
          projectId={projectId}
          run={run}
          worktree={worktree.data?.path}
          path={path}
          view={view}
          onView={setView}
          door={door}
        />
      </div>

      <InspectorLink projectId={projectId} />
    </>
  );
}

/**
 * Which run this is, what state it is in, and where it is decided.
 *
 * Always drawn, even for the only run. Somebody arrives here from Waiting or from a slot carrying
 * "the proposal from that agent", and a review that said only what changed left them holding which
 * run it was in their head — at the moment of deciding whether to trust it. The two links are the
 * way out that the review ends on: the run's own page, and the queue where its decision waits.
 */
function ReviewHeader({
  projectId,
  run,
  described,
  worktree,
}: {
  projectId: string;
  run: number;
  described: RunSearchResult | undefined;
  worktree: Worktree | undefined;
}) {
  const proposals = useProposals();
  const waiting = (proposals.data ?? []).filter((proposal) => proposal.run_id === run).length;
  const headingId = useId();

  return (
    <section aria-labelledby={headingId} className="flex min-w-0 flex-col gap-1">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 id={headingId} className="font-display text-lg font-semibold text-text">
          Run #{run}
        </h2>
        {described === undefined ? null : (
          <>
            <StateBadge domain="run" state={described.status} />
            {/* Plain words and not a badge: `shadow` and `real` are how the run was asked to act,
                not a state it is in, and the run domain has no reading for either. */}
            <span className="text-xs text-text-muted">{described.mode} run</span>
          </>
        )}
        {worktree === undefined ? null : (
          <span className="font-mono text-xs text-text-muted">
            {worktree.branch}
            {worktree.base_sha === null
              ? " · no recorded branch point"
              : ` · from ${worktree.base_sha.slice(0, 7)}`}
          </span>
        )}
        <span className="ml-auto flex flex-wrap gap-x-4 text-sm">
          {waiting === 0 ? null : (
            <Link to="/waiting" search={{ project: projectId }}>
              {waiting === 1 ? "Its decision is waiting →" : `${waiting} decisions waiting →`}
            </Link>
          )}
          <Link to="/runs/$runId" params={{ runId: String(run) }}>
            Run details →
          </Link>
        </span>
      </div>
      {described === undefined ? null : (
        <p className="max-w-prose truncate text-sm text-text" title={described.prompt_excerpt}>
          {described.prompt_excerpt}
        </p>
      )}
    </section>
  );
}

function RunPicker({
  runs,
  selected,
  described,
  onPick,
}: {
  runs: number[];
  selected: number;
  described: Map<number, RunSearchResult>;
  onPick: (run: number) => void;
}) {
  return (
    <nav aria-label="Runs to review" className="flex flex-wrap gap-2">
      {runs.map((candidate) => {
        const prompt = described.get(candidate)?.prompt_excerpt;
        return (
          <button
            key={candidate}
            type="button"
            onClick={() => onPick(candidate)}
            aria-current={candidate === selected ? "true" : undefined}
            title={prompt}
            /*
              The run being reviewed is `.ui-current`, a 2px rule on the leading edge. It was the
              brand colour over a raised fill: cyan marks identity, links and focus and never a
              selection, and a fill marks nothing at all in the light theme, where `--surface` and
              `--surface-raised` are both white.

              The prompt rides with the number because three pills reading `run #39 / #40 / #41`
              could not be told apart without leaving for another page.
            */
            className={`flex max-w-72 items-baseline gap-2 rounded-md border border-border px-3 py-1 text-xs ${
              candidate === selected ? "ui-current text-text" : "text-text-muted hover:text-text"
            }`}
          >
            <span className="font-mono">run #{candidate}</span>
            {prompt === undefined ? null : <span className="truncate">{prompt}</span>}
          </button>
        );
      })}
    </nav>
  );
}

/**
 * The doors to the read-only inspector, and the one place in the app that opens them.
 *
 * **It is not this mode with a different skin, and that is why it survives.** The Código mode reads
 * a RUN&rsquo;s worktree: what one piece of work changed, against the branch it started from. The
 * inspector reads the project&rsquo;s own folder as it is on disk right now — no run, no branch
 * point — which is what somebody wants when they are asking whether a file is even there, or
 * grepping for a name across the repository.
 *
 * **Two links and not one, because the inspector answers two questions with different
 * lifespans.** Browse, search and diff read the tree, and this mode supersedes all three the day
 * it grows them. *On its own* reads what the project does when nobody asks — its schedules, its
 * repo triggers, its gate, its ceiling — and nothing that exists supersedes it; the config editor
 * that would is still only designed. One link advertising only the three condemned views left the
 * one durable view unmentioned at the only entrance to the page that holds it, which is how a
 * capability gets lost without anybody deciding to lose it.
 *
 * The links live here rather than in the rail because that is the design&rsquo;s rule about the
 * rail, and here rather than on the roster because the roster answers about every project at once
 * and this is about one. In a review they come LAST: they lead away from the run, and they used to
 * be the first thing read, above the run they lead away from.
 */
function InspectorLink({ projectId }: { projectId: string }) {
  return (
    /* The same measure as the sentence it follows in the empty state. Without
       it the two doors ran 1,590px on one line — a ribbon twice the width of
       everything around it, which is the sort of thing a passing suite says
       nothing about. Muted and not faint: this is a sentence with two links in
       it, content somebody reads, not a caption. */
    <p className="mt-2 max-w-prose text-xs text-text-muted">
      <Link
        className="underline underline-offset-2"
        to="/projects/$projectId/inspect/$view"
        params={{ projectId, view: "browse" }}
      >
        Browse, search and diff the folder itself
      </Link>{" "}
      — the project as it is on disk, with no run in the way. Or{" "}
      <Link
        className="underline underline-offset-2"
        to="/projects/$projectId/inspect/$view"
        params={{ projectId, view: "rules" }}
      >
        what it does on its own
      </Link>{" "}
      — its schedules, its repo triggers, its gate and its ceiling.
    </p>
  );
}

/** Which door failed, so the note stands beside the control that was pressed. */
type DoorPlace = "worktree" | "file";

interface EditorDoor {
  failed: DoorPlace | null;
  open: (absolute: string, line: number | null, place: DoorPlace) => void;
}

/**
 * The editor's door, with its failure said out loud.
 *
 * `openInVscode` rejects when nothing on this machine claims `vscode://` or the opener's scope
 * refuses the URL, and its own docstring leaves the sentence to the caller. `void` swallowed it: a
 * press that did nothing and said nothing, which is exactly the seam-that-fails-silently the
 * absolute-path rule below exists to prevent.
 */
function useEditorDoor(): EditorDoor {
  const [failed, setFailed] = useState<DoorPlace | null>(null);
  const open = useCallback((absolute: string, line: number | null, place: DoorPlace) => {
    setFailed(null);
    openInVscode(absolute, line).catch(() => setFailed(place));
  }, []);
  return { failed, open };
}

function DoorFailed() {
  return (
    <ErrorNote>
      VS Code did not answer. Is it installed, and does it open <code>vscode://</code> links on this
      machine?
    </ErrorNote>
  );
}

/**
 * The left column, which **opens on what changed**.
 *
 * The whole tree is not the first thing offered and is not offered here at all yet: this mode's
 * subject is the run, and the run touched twelve files. What the panel does say is how many it did
 * not touch, because "twelve changed" alone tells you nothing about the size of what you are
 * trusting.
 *
 * And when it was read. Nothing here polls — each read walks the worktree with git — so the list
 * is as old as the moment it was read, and a run keeps writing after that. The time and the button
 * beside it are what keep "changed nothing yet" from quietly becoming a lie.
 */
function ChangedFiles({
  changed,
  worktree,
  path,
  onPick,
  onRefresh,
  door,
}: {
  changed: ReturnType<typeof useChanged>;
  /** The absolute checkout, when the daemon has answered. Every door to the editor needs it. */
  worktree: string | undefined;
  path: string;
  onPick: (path: string) => void;
  onRefresh: () => Promise<void>;
  door: EditorDoor;
}) {
  const headingId = useId();
  const read =
    changed.dataUpdatedAt > 0
      ? new Date(changed.dataUpdatedAt).toLocaleTimeString(UI_LOCALE, {
          hour: "2-digit",
          minute: "2-digit",
        })
      : null;

  return (
    <section aria-labelledby={headingId} className="flex min-w-0 flex-col gap-2">
      <div className="flex items-baseline justify-between gap-2">
        <h3 id={headingId} className="text-xs uppercase tracking-wide text-text-muted">
          Changed <Count n={changed.data?.paths.length} />
        </h3>
        <Button variant="ghost" disabled={changed.isFetching} onClick={() => void onRefresh()}>
          {changed.isFetching ? "Reading…" : "Refresh"}
        </Button>
      </div>
      <p className="text-xs text-text-muted">
        {read === null ? "Not read yet." : `Read at ${read}.`} Read when you open this and again when
        you ask — never on a timer, because every read walks the worktree with git.
      </p>
      <ChangedBody changed={changed} path={path} onPick={onPick} />
      {/*
        Absent until the path is known, rather than a button that opens nothing. A door built from a
        repository-relative path fails silently — VS Code takes a full path and nothing else — and a
        seam that fails silently is worse than one that is not there yet.
      */}
      {worktree === undefined ? null : (
        <div className="mt-1">
          <Button variant="link" onClick={() => door.open(worktree, null, "worktree")}>
            Open the worktree in VS Code
          </Button>
        </div>
      )}
      {door.failed === "worktree" ? <DoorFailed /> : null}
    </section>
  );
}

function ChangedBody({
  changed,
  path,
  onPick,
}: {
  changed: ReturnType<typeof useChanged>;
  path: string;
  onPick: (path: string) => void;
}) {
  if (changed.isError) {
    // Three refusals, three sentences. 422 is a worktree the daemon never recorded a branch point
    // for; 404 is one that is gone or was never this project's. Neither is "nothing changed", and a
    // panel that said so would be the most reassuring possible way to be wrong.
    return (
      <Refusal
        error={changed.error}
        says={{
          422: "This worktree has no recorded branch point, so what it changed cannot be measured.",
          404: "That worktree is gone — the run may have finished and given its slot back. Its own page says how it ended.",
        }}
        otherwise="The núcleo could not read this worktree."
      />
    );
  }

  if (changed.data === undefined) {
    return (
      <p className="text-sm text-text-muted" role="status">
        Reading what changed…
      </p>
    );
  }

  const { paths, tracked } = changed.data;
  const untouched = Math.max(tracked - paths.length, 0);

  return (
    <>
      {paths.length === 0 ? (
        <Quiet says="This run has changed nothing yet." />
      ) : (
        <ul className="flex flex-col">
          {asTree(paths).map((node) => (
            <Node key={node.label} node={node} depth={0} selected={path} onPick={onPick} />
          ))}
        </ul>
      )}
      {/*
        The sentence that makes this a review surface. Without it, twelve files is a number with
        nothing to be twelve out of. "By this run", because that is what the subtraction measures —
        the files this run did not change, which is not the same as files nobody ever touched.
      */}
      <p className="mt-1 text-xs text-text-muted">
        {`${untouched.toLocaleString(UI_LOCALE)} ${untouched === 1 ? "file" : "files"} nobody touched in this run`}
      </p>
    </>
  );
}

function Node({
  node,
  depth,
  selected,
  onPick,
}: {
  node: TreeNode;
  depth: number;
  selected: string;
  onPick: (path: string) => void;
}) {
  if (node.path === null) {
    return (
      <li>
        <p
          className="truncate py-0.5 font-mono text-xs text-text-faint"
          style={{ paddingLeft: `${depth * 0.75}rem` }}
        >
          {node.label}
        </p>
        <ul>
          {node.children.map((child) => (
            <Node
              key={child.label}
              node={child}
              depth={depth + 1}
              selected={selected}
              onPick={onPick}
            />
          ))}
        </ul>
      </li>
    );
  }

  return (
    <li>
      <button
        type="button"
        onClick={() => onPick(node.path ?? "")}
        aria-current={node.path === selected ? "true" : undefined}
        /*
          `.ui-current`, the same mark the run picker uses, and not the raised fill it had: in the
          light theme `--surface` and `--surface-raised` are both white, and the open file vanished.
        */
        className={
          node.path === selected
            ? "ui-current w-full truncate rounded-sm px-1 py-0.5 text-left font-mono text-xs text-text"
            : "w-full truncate rounded-sm px-1 py-0.5 text-left font-mono text-xs text-text-muted hover:text-text"
        }
        style={{ paddingLeft: `${depth * 0.75 + 0.25}rem` }}
      >
        {node.label}
      </button>
    </li>
  );
}

const VIEWS: { view: View; label: string }[] = [
  { view: "diff", label: "Diff" },
  { view: "file", label: "File" },
  { view: "blame", label: "Blame" },
];

function Centre({
  projectId,
  run,
  worktree,
  path,
  view,
  onView,
  door,
}: {
  projectId: string;
  run: number;
  worktree: string | undefined;
  path: string;
  view: View;
  onView: (view: View) => void;
  door: EditorDoor;
}) {
  const absolute = absolutePath(worktree, path);
  const reasonId = useId();

  /*
    A line number is a door to that line, once there is a full path to put behind it. The same
    rule as the `edit` link: no door until the absolute path is known, because a door built from
    half a path opens nothing and says nothing.
  */
  const lineLink = (file: string | null, line: number): LineLink | null => {
    const target = absolutePath(worktree, file ?? path);
    if (target === null || (file ?? path) === "") return null;
    return {
      href: vscodeUrl(target, line),
      open: () => door.open(target, line, "file"),
      label: `Open line ${line} in VS Code`,
    };
  };

  return (
    <Panel>
      <div className="flex min-w-0 flex-col gap-3">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-muted">
            {path === "" ? "everything this run changed" : path}
          </span>
          {/*
            `aria-pressed` in a labelled group — the idiom of `ModeSwitch` and its track — rather
            than `aria-current`, which announces a location and not a setting. A file view and a
            blame need a file, and the reason they are off is said below the bar, in words, rather
            than only in a greyed button.
          */}
          <div role="group" aria-label="How to read it" className="ui-switch">
            {VIEWS.map(({ view: candidate, label }) => (
              <button
                key={candidate}
                type="button"
                className="ui-switch-seg"
                aria-pressed={candidate === view}
                disabled={candidate !== "diff" && path === ""}
                aria-describedby={candidate !== "diff" && path === "" ? reasonId : undefined}
                onClick={() => onView(candidate)}
              >
                {label}
              </button>
            ))}
          </div>
          {/*
            The door, and only once there is a full path to put behind it. `href` carries the same URL
            the click opens so that the destination is visible on hover and copyable — the click goes
            through the opener plugin, because an external protocol followed by the webview itself is
            handled differently per platform and can simply be swallowed.
          */}
          {absolute === null || path === "" ? null : (
            <a
              href={vscodeUrl(absolute, null)}
              onClick={(event) => {
                event.preventDefault();
                door.open(absolute, null, "file");
              }}
              className="text-sm underline underline-offset-2"
            >
              Edit in VS Code
            </a>
          )}
        </div>
        {path === "" ? (
          <p id={reasonId} className="text-xs text-text-muted">
            Pick a file on the left to read it whole or to see who wrote each line.
          </p>
        ) : null}
        {door.failed === "file" ? <DoorFailed /> : null}

        {view === "diff" ? (
          <DiffView projectId={projectId} run={run} path={path} lineLink={lineLink} />
        ) : null}
        {view === "file" ? (
          <FileView projectId={projectId} run={run} path={path} lineLink={lineLink} />
        ) : null}
        {view === "blame" ? (
          <BlameView projectId={projectId} run={run} path={path} lineLink={lineLink} />
        ) : null}
      </div>
    </Panel>
  );
}

type LineLinker = (file: string | null, line: number) => LineLink | null;

function Reading() {
  return (
    <p className="text-sm text-text-muted" role="status">
      Reading…
    </p>
  );
}

function DiffView({
  projectId,
  run,
  path,
  lineLink,
}: {
  projectId: string;
  run: number;
  path: string;
  lineLink: LineLinker;
}) {
  const diff = useRunDiff(projectId, run, path);
  if (diff.isError) return <ViewRefusal what="diff" error={diff.error} />;
  if (diff.data === undefined) return <Reading />;
  // An empty diff is a fact, not a failure — and the most likely answer of the four.
  if (diff.data.trim() === "") {
    return <p className="text-sm text-text-muted">No difference from the branch point.</p>;
  }
  return (
    <Diff
      text={diff.data}
      label={path === "" ? `What run ${run} changed` : `What run ${run} changed in ${path}`}
      lineLink={lineLink}
    />
  );
}

/**
 * Past this many lines a file is drawn without a door per line, for the reason the diff has its
 * own cap: a link per line is a page element per line, and a file has no upper bound either.
 */
const FILE_LINK_CAP = 2000;

function FileView({
  projectId,
  run,
  path,
  lineLink,
}: {
  projectId: string;
  run: number;
  path: string;
  lineLink: LineLinker;
}) {
  const file = useRunFile(projectId, run, path);
  if (file.isError) return <ViewRefusal what="file" error={file.error} />;
  if (file.data === undefined) return <Reading />;

  const lines = file.data.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  const linked = lines.length <= FILE_LINK_CAP;

  /*
    Two columns that never wrap, so the numbers stay level with their lines — numbers beside
    wrapping text desynchronise on the first long line. The numbers are their own column and
    unselectable, so a copied block does not carry `1 2 3` down its left edge.
  */
  return (
    <div
      role="region"
      aria-label={`${path}, as run ${run} has it`}
      tabIndex={0}
      className="grid max-h-[32rem] grid-cols-[auto_minmax(0,1fr)] gap-3 overflow-auto overscroll-none rounded-sm border border-border bg-surface-sunken px-3 py-2 font-mono text-sm leading-snug [tab-size:4]"
    >
      <span className="flex select-none flex-col text-right tabular-nums text-text-faint">
        {lines.map((_, at) => {
          const link = linked ? lineLink(path, at + 1) : null;
          return link === null ? (
            <span key={at}>{at + 1}</span>
          ) : (
            <a
              key={at}
              href={link.href}
              aria-label={link.label}
              title={link.label}
              onClick={(event) => {
                event.preventDefault();
                link.open();
              }}
              className="text-inherit hover:text-text hover:underline"
            >
              {at + 1}
            </a>
          );
        })}
      </span>
      <pre className="m-0 whitespace-pre text-text">{lines.join("\n")}</pre>
    </div>
  );
}

function BlameView({
  projectId,
  run,
  path,
  lineLink,
}: {
  projectId: string;
  run: number;
  path: string;
  lineLink: LineLinker;
}) {
  const blame = useRunBlame(projectId, run, path);
  if (blame.isError) return <ViewRefusal what="blame" error={blame.error} />;
  if (blame.data === undefined) return <Reading />;

  return (
    <div className="max-h-[32rem] overflow-auto overscroll-none">
      <table className="w-full font-mono text-sm leading-snug">
        <tbody>
          {blame.data.map((line) => {
            const link = lineLink(path, line.line);
            return (
              <tr key={line.line} className="align-top">
                <td className="select-none pr-2 text-right tabular-nums text-text-faint">
                  {link === null ? (
                    line.line
                  ) : (
                    <a
                      href={link.href}
                      aria-label={link.label}
                      title={link.label}
                      onClick={(event) => {
                        event.preventDefault();
                        link.open();
                      }}
                      className="text-inherit hover:text-text hover:underline"
                    >
                      {line.line}
                    </a>
                  )}
                </td>
                {/*
                  An uncommitted line says so instead of naming an author. Git fills those rows with
                  placeholders — "Not Committed Yet", the current time — and drawing them like any other
                  would credit somebody who never wrote the line.
                */}
                <td className="max-w-32 truncate pr-3 text-xs text-text-muted" title={line.summary}>
                  {line.uncommitted ? "uncommitted" : line.author}
                </td>
                <td className="whitespace-pre text-text">{line.text}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/**
 * A refusal, said as what it is.
 *
 * The three the daemon can give here mean different things and want different responses: a path
 * that does not exist in this run, a worktree that is gone, and a read that failed. Collapsing them
 * into "error" would send somebody looking for a file when the answer is that the run finished.
 */
function ViewRefusal({ what, error }: { what: string; error: unknown }) {
  return (
    <Refusal
      error={error}
      says={{
        404: `No ${what} — that path is not in this run's worktree, or the worktree is gone. A file this run deleted has no ${what} to read; its diff says what it held.`,
        400: "That path was refused.",
      }}
      otherwise={`The núcleo could not read the ${what}.`}
    />
  );
}

/**
 * The daemon answered no, or did not answer — two different notes.
 *
 * A refusal keeps the status's sentence this mode wrote for it, carried under the daemon's own code
 * so `RefusalNote` shows that code beside it: the code is what somebody quotes when the sentence is
 * not enough, and an error on a review surface is read as a bug report. Anything that is not a
 * refusal is the núcleo not answering, which is an `ErrorNote`.
 */
function Refusal({
  error,
  says,
  otherwise,
}: {
  error: unknown;
  says: Record<number, string>;
  otherwise: string;
}) {
  if (!isApiRefusal(error)) return <ErrorNote>{otherwise}</ErrorNote>;
  const sentence = says[error.status] ?? otherwise;
  return <RefusalNote refusal={error} sentences={{ [error.code]: sentence }} />;
}
