import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { useConcurrency } from "../data/fleet";
import {
  absolutePath,
  asTree,
  useChanged,
  useRunBlame,
  useRunDiff,
  useRunFile,
  useRunWorktree,
  type TreeNode,
} from "../data/project-code";
import { openInVscode, vscodeUrl } from "../lib/vscode";
import { isApiRefusal } from "../data/client";

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

export interface ModeCodigoProps {
  projectId: string;
  /** The run being reviewed, from the route so a slot on the State mode can link straight to it. */
  run: number | null;
  onPickRun: (run: number) => void;
}

type View = "diff" | "file" | "blame";

export function ModeCodigo({ projectId, run, onPickRun }: ModeCodigoProps) {
  const concurrency = useConcurrency();
  const [path, setPath] = useState("");
  const [view, setView] = useState<View>("diff");

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
  const changed = useChanged(projectId, selected);
  const worktree = useRunWorktree(projectId, selected);

  if (concurrency.data === undefined) {
    return <p className="text-sm text-text-faint">Reading what there is to review…</p>;
  }

  if (selected === null) {
    return (
      <div className="rounded-lg border border-border bg-surface p-6">
        <p className="font-display text-lg text-text-muted">Nothing to review in {projectId}.</p>
        <p className="mt-2 max-w-prose text-sm text-text-faint">
          This mode reads a run&rsquo;s worktree. When one is working here, its changed files appear
          on the left — and the whole repository stays where it is, in the editor.
        </p>
        <InspectorLink projectId={projectId} />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <InspectorLink projectId={projectId} />
      {runs.length > 1 ? (
        <nav aria-label="Runs to review" className="flex flex-wrap gap-2">
          {runs.map((candidate) => (
            <button
              key={candidate}
              type="button"
              onClick={() => onPickRun(candidate)}
              aria-current={candidate === selected ? "true" : undefined}
              className={
                candidate === selected
                  ? "rounded-md border border-accent bg-surface-raised px-3 py-1 font-mono text-xs text-text"
                  : "rounded-md border border-border px-3 py-1 font-mono text-xs text-text-muted hover:text-text"
              }
            >
              run #{candidate}
            </button>
          ))}
        </nav>
      ) : null}

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-[16rem_1fr]">
        <ChangedFiles
          changed={changed}
          worktree={worktree.data?.path}
          path={path}
          onPick={setPath}
        />
        <Centre
          projectId={projectId}
          run={selected}
          worktree={worktree.data?.path}
          path={path}
          view={view}
          onView={setView}
        />
      </div>
    </div>
  );
}

/**
 * The left column, which **opens on what changed**.
 *
 * The whole tree is not the first thing offered and is not offered here at all yet: this mode's
 * subject is the run, and the run touched twelve files. What the panel does say is how many it did
 * not touch, because "twelve changed" alone tells you nothing about the size of what you are
 * trusting.
 */
/**
 * The door to the read-only inspector, and the one place in the app that opens it.
 *
 * **It is not this mode with a different skin, and that is why it survives.** The Código mode reads
 * a RUN&rsquo;s worktree: what one piece of work changed, against the branch it started from. The
 * inspector reads the project&rsquo;s own folder as it is on disk right now — no run, no branch
 * point — which is what somebody wants when they are asking whether a file is even there, or
 * grepping for a name across the repository, or reading the schedule rules.
 *
 * The link lives here rather than in the rail because that is the design&rsquo;s rule about the
 * rail, and here rather than on the roster because the roster answers about every project at once
 * and this is about one. Until the Código mode grows a browse and a search of its own, removing the
 * link would be losing a working capability quietly — which the router&rsquo;s own comment says is
 * the failure mode to avoid.
 */
function InspectorLink({ projectId }: { projectId: string }) {
  return (
    <p className="text-xs text-text-faint">
      <Link
        className="underline underline-offset-2"
        to="/projects/$projectId/inspect/$view"
        params={{ projectId, view: "browse" }}
      >
        Browse, search and diff the folder itself
      </Link>{" "}
      — the project as it is on disk, with no run in the way.
    </p>
  );
}

function ChangedFiles({
  changed,
  worktree,
  path,
  onPick,
}: {
  changed: ReturnType<typeof useChanged>;
  /** The absolute checkout, when the daemon has answered. Every door to the editor needs it. */
  worktree: string | undefined;
  path: string;
  onPick: (path: string) => void;
}) {
  if (changed.isError) {
    // Three refusals, three sentences. 422 is a worktree the daemon never recorded a branch point
    // for; 404 is one that is gone or was never this project's. Neither is "nothing changed", and a
    // panel that said so would be the most reassuring possible way to be wrong.
    const status = isApiRefusal(changed.error) ? changed.error.status : 0;
    return (
      <p className="text-sm text-tone-paused-fg">
        {status === 422
          ? "This worktree has no recorded branch point, so what it changed cannot be measured."
          : status === 404
            ? "That worktree is gone."
            : "The núcleo could not read this worktree."}
      </p>
    );
  }

  if (changed.data === undefined) {
    return <p className="text-sm text-text-faint">Reading what changed…</p>;
  }

  const { paths, tracked } = changed.data;
  const untouched = Math.max(tracked - paths.length, 0);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-xs uppercase tracking-wide text-text-faint">
        Changed {paths.length}
      </p>
      {paths.length === 0 ? (
        <p className="text-sm text-text-muted">This run has changed nothing yet.</p>
      ) : (
        <ul className="flex flex-col">
          {asTree(paths).map((node) => (
            <Node key={node.label} node={node} depth={0} selected={path} onPick={onPick} />
          ))}
        </ul>
      )}
      {/*
        The sentence that makes this a review surface. Without it, twelve files is a number with
        nothing to be twelve out of.
      */}
      <p className="mt-1 text-xs text-text-faint">
        {`${untouched.toLocaleString()} ${untouched === 1 ? "file" : "files"} nobody touched`}
      </p>
      {/*
        Absent until the path is known, rather than a button that opens nothing. A door built from a
        repository-relative path fails silently — VS Code takes a full path and nothing else — and a
        seam that fails silently is worse than one that is not there yet.
      */}
      {worktree === undefined ? null : (
        <button
          type="button"
          className="mt-1 self-start text-xs text-accent hover:underline"
          onClick={() => void openInVscode(worktree, null)}
        >
          open the worktree in VS Code
        </button>
      )}
    </div>
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
        className={
          node.path === selected
            ? "w-full truncate rounded-sm bg-surface-raised px-1 py-0.5 text-left font-mono text-xs text-text"
            : "w-full truncate rounded-sm px-1 py-0.5 text-left font-mono text-xs text-text-muted hover:text-text"
        }
        style={{ paddingLeft: `${depth * 0.75 + 0.25}rem` }}
      >
        {node.label}
      </button>
    </li>
  );
}

const VIEWS: View[] = ["diff", "file", "blame"];

function Centre({
  projectId,
  run,
  worktree,
  path,
  view,
  onView,
}: {
  projectId: string;
  run: number;
  worktree: string | undefined;
  path: string;
  view: View;
  onView: (view: View) => void;
}) {
  const absolute = absolutePath(worktree, path);
  return (
    <div className="flex min-w-0 flex-col gap-2 rounded-lg border border-border bg-surface">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-muted">
          {path === "" ? "everything this run changed" : path}
        </span>
        {VIEWS.map((candidate) => (
          <button
            key={candidate}
            type="button"
            onClick={() => onView(candidate)}
            // The file and the blame need a file; the diff is meaningful for the whole run.
            disabled={candidate !== "diff" && path === ""}
            aria-current={candidate === view ? "true" : undefined}
            className={
              candidate === view
                ? "rounded-sm bg-surface-raised px-2 py-0.5 text-xs text-text"
                : "rounded-sm px-2 py-0.5 text-xs text-text-faint hover:text-text disabled:opacity-40"
            }
          >
            {candidate}
          </button>
        ))}
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
              void openInVscode(absolute, null);
            }}
            className="text-xs text-accent hover:underline"
          >
            edit
          </a>
        )}
      </div>

      <div className="min-h-64 overflow-auto px-3 pb-3">
        {view === "diff" ? <DiffView projectId={projectId} run={run} path={path} /> : null}
        {view === "file" ? <FileView projectId={projectId} run={run} path={path} /> : null}
        {view === "blame" ? <BlameView projectId={projectId} run={run} path={path} /> : null}
      </div>
    </div>
  );
}

function DiffView({ projectId, run, path }: { projectId: string; run: number; path: string }) {
  const diff = useRunDiff(projectId, run, path);
  if (diff.isError) return <Refusal what="diff" error={diff.error} />;
  if (diff.data === undefined) return <p className="text-sm text-text-faint">Reading…</p>;
  // An empty diff is a fact, not a failure — and the most likely answer of the four.
  if (diff.data.trim() === "") {
    return <p className="text-sm text-text-muted">No difference from the branch point.</p>;
  }
  return <pre className="whitespace-pre font-mono text-xs text-text-muted">{diff.data}</pre>;
}

function FileView({ projectId, run, path }: { projectId: string; run: number; path: string }) {
  const file = useRunFile(projectId, run, path);
  if (file.isError) return <Refusal what="file" error={file.error} />;
  if (file.data === undefined) return <p className="text-sm text-text-faint">Reading…</p>;
  return <pre className="whitespace-pre font-mono text-xs text-text-muted">{file.data}</pre>;
}

function BlameView({ projectId, run, path }: { projectId: string; run: number; path: string }) {
  const blame = useRunBlame(projectId, run, path);
  if (blame.isError) return <Refusal what="blame" error={blame.error} />;
  if (blame.data === undefined) return <p className="text-sm text-text-faint">Reading…</p>;

  return (
    <table className="w-full font-mono text-xs">
      <tbody>
        {blame.data.map((line) => (
          <tr key={line.line} className="align-top">
            <td className="select-none pr-2 text-right text-text-faint">{line.line}</td>
            {/*
              An uncommitted line says so instead of naming an author. Git fills those rows with
              placeholders — "Not Committed Yet", the current time — and drawing them like any other
              would credit somebody who never wrote the line.
            */}
            <td className="max-w-32 truncate pr-3 text-text-faint" title={line.summary}>
              {line.uncommitted ? "uncommitted" : line.author}
            </td>
            <td className="whitespace-pre text-text-muted">{line.text}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * A refusal, said as what it is.
 *
 * The three the daemon can give here mean different things and want different responses: a path
 * that does not exist in this run, a worktree that is gone, and a read that failed. Collapsing them
 * into "error" would send somebody looking for a file when the answer is that the run finished.
 */
function Refusal({ what, error }: { what: string; error: unknown }) {
  const status = isApiRefusal(error) ? error.status : 0;
  return (
    <p className="text-sm text-tone-paused-fg">
      {status === 404
        ? `No ${what} — that path is not in this run's worktree, or the worktree is gone.`
        : status === 400
          ? "That path was refused."
          : `The núcleo could not read the ${what}.`}
    </p>
  );
}
