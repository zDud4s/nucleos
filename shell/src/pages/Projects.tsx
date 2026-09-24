import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  DIFF_LINE_CAP,
  FOLDER_FIX_PATH,
  VIEWS,
  concernsOf,
  diffLines,
  groupMatches,
  headlineFor,
  joinPath,
  normaliseView,
  parentPath,
  pathSegments,
  pathUpTo,
  useProjectCat,
  useProjectDiff,
  useProjectGrep,
  useProjectLs,
  useProjectRules,
  type Concern,
  type ConcernWeight,
  type InspectEntry,
  type ProjectRules,
  type ProjectView,
} from "../data/projects";
import { useProjects } from "../data/system";
import { OnItsOwn, ReadAt, Why } from "../project/OnItsOwn";
import {
  Button,
  Count,
  Crumb,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  StaleNote,
} from "../ui";
import "./projects.css";

/**
 * Projects — the inspector: a project's tree, read-only, and for now the
 * "On its own" view, which is not.
 *
 * The page exists because two facts about a project were only readable by
 * leaving the app: what it will do on its own (`.ai/autopilot.yaml`, on disk)
 * and what its tree currently looks like. Both are shown here, and this page
 * edits neither FILE.
 *
 * **The rule that used to justify that, and what is left of it.** The sentence
 * was: a shell that offered to write the rules file would be a second author of
 * a document git already owns. That is still true of every `.rs`, every
 * `package.json`, every file in the tree below — which is the whole of what
 * this page browses, and why nothing here is editable.
 *
 * It stopped being true of one file, and the exception is worth writing down so
 * nobody restores the rule over it in six months. `.ai/autopilot.yaml` is not a
 * document git owns: it is **gitignored, per-developer configuration** that the
 * núcleo itself parses, with a schema, a range check and `deny_unknown_fields`.
 * An editor that holds text to that schema before saving is not a distracted
 * second author, it is a better-informed one — `vim` saves `gate_commmand:`
 * happily and leaves the project silently ungated for ever. The boundary is
 * therefore not *read vs. write* but **who is the file's legitimate author**,
 * and it lives as data in `core/src/ownership.rs`, served by
 * `GET /projects/{id}/ownership`. The editor for it is in the project workspace,
 * which is where a person goes to change how a project behaves; this page is
 * where they go to look at what is in it.
 *
 * The sentence still constrains the *form* even there, which is the part most
 * easily lost: the workspace edits that file as raw text and not as a form,
 * because a form would have to re-serialise the YAML and re-serialising deletes
 * the comment somebody left explaining why a schedule is switched off.
 *
 * **It is not read-only, whatever this header used to say.** The "On its own"
 * view makes two writes, both to the núcleo's
 * database and neither to a file: the WIP ceiling and the judge that answers for
 * a conversation on Auto. Both are safety controls, and both now live in
 * `project/OnItsOwn.tsx` — lifted out whole so they can move to the project
 * workspace, where a person goes to change how a project behaves. Until they do,
 * this page mounts that component for its `rules` view. Browse, Search and Diff
 * are the part that reads and only reads.
 *
 * The four views are in the route (`/projects/$projectId/inspect/$view`) so a
 * folder somebody is looking at survives a reload and can be linked to. An
 * unknown `$view` falls back to `browse` rather than 404ing: a route parameter
 * is a string, anybody can type one, and a typo in a path is not a missing page.
 *
 * ---
 *
 * **What the 2026-08-30 pass changed, and why none of it is a new author.**
 *
 * The page had every right answer and no hierarchy. Four views drawn as equal
 * peers, though three of them read the tree — and are superseded by the Código
 * mode, as `router.tsx` says — while the fourth reads what the project does
 * without you and is superseded by nothing that exists. Under that fourth tab,
 * five panels of equal weight in the order the struct declares its fields.
 *
 * Three things were wrong in ways only pixels showed:
 *
 * - A completely halted project — rules that will not parse, a queue refusing
 *   every merge, a brake holding — produced one loud alert followed by two
 *   panels saying "nothing is scheduled" and "no commit starts anything here",
 *   which the alert directly above had just disclaimed, and then the two most
 *   consequential sentences on the page in the quietest style on it. So: the
 *   findings are lifted into a strip at the top, visible from every view; the
 *   empty lists are not drawn at all when the alert has already explained them;
 *   and the gate's contradiction is an alert rather than muted body text.
 * - `Badge` was doing four jobs, and `armed` meant two different things in two
 *   adjacent panels. Now a filled badge is the state of a **rule** and nothing
 *   else. A file's state is a word beside its name, a gate is a command, a
 *   ceiling is a `Meter`.
 * - The header said what the page *is* rather than what it *found*, which
 *   `PageHeader`'s own docstring forbids — and said it about a folder even on
 *   the view that reads no folder.
 *
 * And the header's oldest claim was not true: the *view* was in the route, but
 * the folder, the open file and the query were component state that died on
 * every reload. They are search params now, so the promise the module made is
 * one the module keeps.
 */

/** What the inspector carries in the location besides the view. */
interface InspectSearch {
  /** The folder being browsed. Absent is the project root. */
  path?: string;
  /** The file open beside the listing, if any. */
  file?: string;
  /** What was searched for — what was *asked*, never what is being typed. */
  q?: string;
  /** The subtree the search was run under. Absent is the whole project. */
  under?: string;
  /**
   * The line a search hit points at, in the open file. `unknown` because it is
   * read straight off the location and `lineOf` decides what it means — until
   * `router.tsx` validates it, the real route strips it and a hit opens the file
   * at the top, which is what it did before this param existed.
   */
  line?: unknown;
}

/**
 * Where this inspector lives, now that the workspace owns the shorter path.
 *
 * One function rather than three template literals, because the two callers
 * below drifted apart the moment there was a prefix to forget.
 */
function inspectPath(projectId: string, view: ProjectView): string {
  return `/projects/${projectId}/inspect/${view}`;
}

/**
 * What each view is called, which is not what its route parameter is.
 *
 * `rules` reads as **On its own** — the phrase the Teams console already uses
 * for the same question about a department (`Teams.tsx`, the "On its own"
 * column). One concept, one name, across the app. The parameter stays `rules`
 * because it is a URL somebody may have kept.
 */
const VIEW_LABEL: Record<ProjectView, string> = {
  browse: "Browse",
  search: "Search",
  diff: "Diff",
  rules: "On its own",
};

export function Projects() {
  const params = useParams({ strict: false }) as { projectId?: string; view?: string };
  const projects = useProjects();

  const rows = projects.data ?? [];
  const projectId = params.projectId ?? null;
  const view = normaliseView(params.view);
  const project = rows.find((row) => row.project_id === projectId);

  /*
    Read here rather than inside the rules view, which is the one cost this pass
    added. The header and the concerns strip report on a project from EVERY
    view, and they cannot do that from a query that only runs under one tab.
    It is a file read per page open and not per tick — `data/projects.ts` fixes
    that cadence and explains it — so the price is one `stat` and one parse.
  */
  const rules = useProjectRules(projectId);

  /*
    This page is the inspector and nothing else. It used to draw the whole roster above itself —
    twenty-five chips carried down the page every time somebody opened one project's file tree —
    and that roster is now `Roster` on `/projects`, which is the page whose question it answers.
    The route always carries an id; the guard is for a URL typed by hand.
  */
  if (projectId === null) {
    return (
      <>
        <PageHeader title="Inspect" />
        <p className="pj-absence" role="status">
          This page looks inside one project. Choose one on <Link to="/projects">Projects</Link>.
        </p>
      </>
    );
  }

  return (
    <>
      <PageHeader
        title={projectId}
        /* What it found, never what the page is — `PageHeader` says so itself.
           Nothing until the read answers: a page with nothing true to say here
           says nothing. */
        headline={rules.data === undefined ? undefined : headlineFor(rules.data)}
      />

      {/*
        Both ways back, because they are different places: the workspace is this project seen
        through the app's own readings, and the roster is every project. Somebody who arrived here
        from the Código mode wants the first, which is why it is the one the arrow points at; the
        roster sits in the crumb's second slot rather than in a second paragraph.
      */}
      <Crumb
        to={`/projects/${projectId}/code`}
        here={<Link to="/projects">all projects</Link>}
      >
        {projectId}
      </Crumb>

      <Concerns
        rules={rules.data}
        rootExists={project?.root_exists}
        here={view}
      />

      <ViewTabs
        projectId={projectId}
        view={view}
        /* Only once the roster has answered: a project not yet read is not a
           project without a folder, and greying out two tabs on a guess would
           be the page claiming something it does not know. */
        noFolder={project !== undefined && project.project_root === null}
      />
      {/* Keyed on the project so a query somebody is halfway through typing
          resets when the subject changes. The folder and the open file live in
          the location now and change with it; this key is for the drafts that
          cannot. */}
      <ProjectViews
        key={projectId}
        projectId={projectId}
        projectRoot={project?.project_root ?? null}
        view={view}
      />
    </>
  );
}

/* ---------------------------------------------------------- what is wrong -- */

/** Three weights, three marks. The glyph carries it, so none of this is colour alone. */
const WEIGHT_MARK: Record<ConcernWeight, string> = {
  stopped: "✕",
  held: "!",
  unfinished: "?",
};

/**
 * Everything wrong with this project, at the top, on every view.
 *
 * Modelled on the Teams console's in-flight strip, including the part that
 * matters most: **it renders nothing when there is nothing**. An "all clear"
 * row would be a permanent hole in every healthy project's page, and the
 * headline already reports the state.
 *
 * It exists because these findings were unreachable. Three of them lived under
 * the fourth tab — the one with the most generic name, behind a link from the
 * Código mode that advertises only the other three — and two of those were
 * muted body text. Somebody landing on `browse`, which is where every arrival
 * lands, could not learn from this page that the project in front of them was
 * doing nothing at all.
 *
 * **Each finding leads to where it is fixed, named by what you do there.** It
 * used to link to the inspector tab that showed it — "On its own" on every row,
 * including "no folder has been recorded", which that tab cannot fix — so the
 * person who has to make the fix got a diagnosis and a link back to it.
 *
 * **On the view that shows a finding at length, the row is not drawn at all.**
 * Dropping only its link left the same fact twice, a hundred and fifty pixels
 * apart; the block below says it in full and carries its own way to the fix.
 *
 * The weight is spoken as well as drawn. The glyph is `aria-hidden`, so a screen
 * reader heard every row as the same kind of sentence; "Stopped:" and "Held:"
 * are the difference between a fault and a brake doing its job.
 */
function Concerns({
  rules,
  rootExists,
  here,
}: {
  rules: ProjectRules | undefined;
  rootExists: boolean | null | undefined;
  here: ProjectView;
}) {
  if (rules === undefined) return null;
  const found = concernsOf(rules, rootExists).filter((concern) => concern.view !== here);
  if (found.length === 0) return null;

  return (
    <section className="pj-concerns" aria-label="What is wrong here">
      <ul className="pj-concern-list">
        {found.map((concern) => (
          <ConcernRow key={concern.kind} concern={concern} />
        ))}
      </ul>
    </section>
  );
}

/** What a screen reader hears in place of the glyph. */
const WEIGHT_SAID: Record<ConcernWeight, string> = {
  stopped: "Stopped:",
  held: "Held:",
  unfinished: "Unfinished:",
};

function ConcernRow({ concern }: { concern: Concern }) {
  return (
    <li className={`pj-concern pj-concern-${concern.weight}`}>
      <span className="pj-concern-mark" aria-hidden="true">
        {WEIGHT_MARK[concern.weight]}
      </span>
      <span className="pj-concern-said">
        <span className="sr-only">{WEIGHT_SAID[concern.weight]} </span>
        {concern.said}
      </span>
      <Link className="pj-concern-where" to={concern.fix.to}>
        {concern.fix.label}
      </Link>
    </li>
  );
}

/* ------------------------------------------------------------------ tabs -- */

/**
 * The four views, with a divider that says they are not four peers.
 *
 * Browse, Search and Diff are three ways of reading the tree and are superseded
 * by the Código mode the day it grows them. "On its own" answers a different
 * question, is superseded by nothing that exists, and holds the page's two
 * controls — which is why it is leaving for the project workspace. Drawn as four
 * equal tabs, the durable one was the last of four and had the most generic name
 * on the screen.
 *
 * **Search and Diff are not links when there is no folder.** Both would open on
 * the same refusal the browse view already gives, which is a click spent learning
 * something the page knew. They stay on the row — a tab that vanishes leaves no
 * trace of what is missing — as text, with the reason on it.
 */
function ViewTabs({
  projectId,
  view,
  noFolder,
}: {
  projectId: string;
  view: ProjectView;
  noFolder: boolean;
}) {
  return (
    <nav className="pj-tabs" aria-label="Project views">
      {VIEWS.map((candidate) => {
        const classes = ["pj-tab"];
        if (candidate === view) classes.push("pj-tab-active");
        /* The divider, on the one view that is not one of the three. */
        if (candidate === "rules") classes.push("pj-tab-apart");
        if (noFolder && candidate !== view && (candidate === "search" || candidate === "diff")) {
          return (
            <span
              key={candidate}
              className="pj-tab pj-tab-off"
              aria-disabled="true"
              title="No folder is recorded for this project, so there is nothing to read"
            >
              {VIEW_LABEL[candidate]}
              <span className="sr-only">, unavailable: no folder is recorded</span>
            </span>
          );
        }
        return (
          <Link
            key={candidate}
            className={classes.join(" ")}
            to={inspectPath(projectId, candidate)}
            aria-current={candidate === view ? "page" : undefined}
          >
            {VIEW_LABEL[candidate]}
          </Link>
        );
      })}
    </nav>
  );
}

function ProjectViews({
  projectId,
  projectRoot,
  view,
}: {
  projectId: string;
  projectRoot: string | null;
  view: ProjectView;
}) {
  return (
    <div className="pj-sections">
      {view === "browse" && <BrowsePanel projectId={projectId} projectRoot={projectRoot} />}
      {view === "search" && <GrepPanel projectId={projectId} projectRoot={projectRoot} />}
      {view === "diff" && <DiffPanel projectId={projectId} projectRoot={projectRoot} />}
      {/* Its own component now, reading the rules itself; it moves to the
          project workspace, and this line becomes a link to it. */}
      {view === "rules" && <OnItsOwn projectId={projectId} />}
    </div>
  );
}

/* ------------------------------------------------------------ the absence -- */

/**
 * What a failed inspect read actually means.
 *
 * The inspect routes answer **404 for three different things**, and the page
 * must not collapse them: the project has no recorded root at all
 * (`resolve_project_root`), the recorded root is gone from the disk, or the
 * path inside it is not there (`inspect_status`). The first is a setting nobody
 * has filled in, the second is a folder that moved, the third is a typo — and
 * they send a person to three different places.
 *
 * The shell can tell the first apart with certainty (it holds `project_root`).
 * It cannot tell the second from the third, and says so rather than picking.
 *
 * **All three answers are `RefusalNote` now.** Two of them were a hand-rolled
 * copy of it — `.pj-absence`, an info tint with a 3px left rule, which is that
 * component's recipe under a page's own name — and they carried the same
 * `role="status"` for the same reason: a refusal is a considered answer and
 * nothing broke, so nothing should interrupt what a screen reader is in the
 * middle of saying. What the primitive adds is the refusal's own code beside
 * the sentence, which is the string that survives a rewording and the one a
 * person quotes when the sentence does not explain enough.
 *
 * The 404 sentence is keyed on `error.code` rather than on the literal
 * `not_found`, and that is not cleverness for its own sake: `client.ts` derives
 * a code from the status only when the daemon did not name one itself, so a
 * route answering 404 under a name of its own would slip past a hardcoded key
 * and take the generic sentence instead. The branch has already decided this is
 * the 404; the key only says "whatever this one was called".
 *
 * **And the 404 carries the way to the fix.** Both of its causes that are not a
 * typo are put right on the Autopilot page, where a folder is recorded — which
 * the sentence used to say as plain text, leaving the person to go and find it.
 * `RefusalNote` takes a sentence and not a link, so the link sits under it.
 */
function InspectAbsence({
  error,
  projectRoot,
  what,
}: {
  error: unknown;
  projectRoot: string | null;
  what: string;
}) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing is known about {what}</ErrorNote>;
  }
  if (error.status === 404) {
    const said =
      projectRoot === null
        ? "This project has no folder recorded, so there is nothing to look inside. A folder is named when the project is put into shadow or acting, on the Autopilot page."
        : `Not found — and the núcleo answers the same way for two different things: that path is not there, or the folder recorded for this project (${projectRoot}) is gone from the disk. Check the folder before hunting for the file.`;
    return (
      <>
        <RefusalNote refusal={error} sentences={{ [error.code]: said }} />
        <p className="pj-fix pj-fix-under">
          <Link to={FOLDER_FIX_PATH}>
            {projectRoot === null ? "Record a folder on Autopilot" : "Check the folder on Autopilot"}
          </Link>
        </p>
      </>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        bad_request: "that path leaves the project's folder, which the núcleo will not follow",
        internal: "the núcleo could not read that from the disk",
      }}
    />
  );
}

/* ---------------------------------------------------------------- browsing -- */

/**
 * The tree, and one file beside it.
 *
 * Two columns rather than a listing with the file underneath it: a folder and
 * the file you opened from it are read together, and a file pushed below a nine
 * row listing puts the thing you asked for off the bottom of the screen. The
 * shape is the Código mode's, deliberately — that surface solved this first.
 *
 * The folder and the open file live in the location. Walking into a folder
 * **pushes**, because it is going somewhere and back should walk you out again.
 * Opening a file **replaces**, because it is changing what you are looking at
 * inside the folder you are already in, and a back button that stepped through
 * every file somebody glanced at would be a worse back button.
 */
function BrowsePanel({ projectId, projectRoot }: { projectId: string; projectRoot: string | null }) {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as InspectSearch;
  const path = search.path ?? "";
  const openFile = search.file ?? null;

  const listing = useProjectLs(projectId, path);
  const entries = listing.data ?? [];

  // Directories first, then files, each alphabetically: a listing sorted only by
  // name buries the folders among the files and makes a deep tree unwalkable.
  const ordered = [...entries].sort(compareEntries);

  const go = (next: InspectSearch, replace: boolean) =>
    void navigate({ to: inspectPath(projectId, "browse"), search: next, replace });

  return (
    <div className={openFile === null ? "pj-browse" : "pj-browse pj-browse-split"}>
      <Panel
        title="Folder"
        aside={
          <span className="pj-aside">
            <ReadAt at={listing.dataUpdatedAt} />
            <Count n={listing.data?.length} />
          </span>
        }
      >
        <Breadcrumbs path={path} onGo={(next) => go({ path: next || undefined }, false)} />
        {/* A failed re-read over a listing already on screen is a stale listing,
            not an absent folder: the last good one stays, labelled with its age. */}
        {listing.isError && listing.data !== undefined && (
          <StaleNote dataUpdatedAt={listing.dataUpdatedAt} />
        )}
        {listing.isError && listing.data === undefined && (
          <InspectAbsence error={listing.error} projectRoot={projectRoot} what="that folder" />
        )}
        {!listing.isError && listing.data === undefined && (
          <p className="pj-loading">reading the folder…</p>
        )}
        {listing.data !== undefined && ordered.length === 0 && (
          <Quiet says="this folder is empty." />
        )}
        {ordered.length > 0 && (
          <ul className="pj-listing" aria-label="Folder contents">
            {ordered.map((entry) => (
              <li className="pj-entry" key={entry.name}>
                {/*
                  A link and not a button, which it could not be until the folder
                  and the file lived in the location. Both are places now, so
                  both are addresses: middle-clickable, copyable, and reachable
                  by somebody you sent the URL to.
                */}
                <Link
                  className={
                    entry.is_dir ? "pj-entry-link pj-entry-dir" : "pj-entry-link pj-entry-file"
                  }
                  to={inspectPath(projectId, "browse")}
                  search={
                    entry.is_dir
                      ? { path: joinPath(path, entry.name) || undefined }
                      : { path: path || undefined, file: joinPath(path, entry.name) }
                  }
                  replace={!entry.is_dir}
                  aria-current={joinPath(path, entry.name) === openFile ? "true" : undefined}
                >
                  {entry.name}
                  {entry.is_dir ? "/" : ""}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </Panel>

      {openFile !== null && (
        <FileView
          projectId={projectId}
          projectRoot={projectRoot}
          path={openFile}
          line={lineOf(search.line)}
          onClose={() => go({ path: path || undefined }, true)}
        />
      )}
    </div>
  );
}

/**
 * A `line` search param as a line number, or nothing.
 *
 * Read defensively because it is a claim in a URL: anything that is not a
 * positive whole number is the same as not asking.
 */
function lineOf(raw: unknown): number | null {
  const line = typeof raw === "number" ? raw : Number(raw);
  return Number.isInteger(line) && line > 0 ? line : null;
}

function compareEntries(a: InspectEntry, b: InspectEntry): number {
  if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
  return a.name.localeCompare(b.name);
}

/**
 * Where in the tree you are, and every step back out of it.
 *
 * The segment you are standing on is **text**, not a disabled control. It was a
 * `Button variant="link" disabled`, which is the accent colour at 45% opacity
 * with no background of its own — so the one crumb somebody most wants to read
 * was drawn as the faintest thing in the trail, and looked broken rather than
 * current. jsdom cannot see that; the screenshot could.
 */
function Breadcrumbs({ path, onGo }: { path: string; onGo: (path: string) => void }) {
  const segments = pathSegments(path);
  return (
    <nav className="pj-crumbs" aria-label="Folder path">
      {/* "the project root" is a phrase somebody wrote, not a name the núcleo
          sent, so it is in the body face; the segments after it are real path
          names and keep the mono one. The face is a claim about origin. */}
      {path === "" ? (
        <span className="pj-crumb pj-crumb-root pj-crumb-here" aria-current="location">
          the project root
        </span>
      ) : (
        <Button variant="link" onClick={() => onGo("")}>
          <span className="pj-crumb pj-crumb-root">the project root</span>
        </Button>
      )}
      {segments.map((segment, index) =>
        index === segments.length - 1 ? (
          <span
            className="pj-crumb pj-crumb-here"
            key={`${segment}-${String(index)}`}
            aria-current="location"
          >
            / {segment}
          </span>
        ) : (
          <Button
            key={`${segment}-${String(index)}`}
            variant="link"
            onClick={() => onGo(pathUpTo(path, index + 1))}
          >
            <span className="pj-crumb">/ {segment}</span>
          </Button>
        ),
      )}
    </nav>
  );
}

/**
 * One file, as text, with its lines numbered.
 *
 * Never rendered as markup, and there is no `dangerouslySetInnerHTML` on this
 * page: everything shown here is somebody else's file, and a project under
 * autopilot is a project where an agent may have written it. The numbers are
 * text nodes in a column of their own, so that stays true — and they are here
 * because the search beside this view answers in line numbers, and a hit at
 * line 612 was previously unfindable in the file it named.
 *
 * Titled by its path. It was titled "File" with the path on a faint second
 * line — a heading that named the kind of thing and a caption that named the
 * thing, which is the wrong way round for the one fact the panel is about.
 *
 * A `line` from a search hit is scrolled to and marked, so "612" is somewhere
 * the page takes you rather than a number you carry in your head and scroll for.
 */
function FileView({
  projectId,
  projectRoot,
  path,
  line,
  onClose,
}: {
  projectId: string;
  projectRoot: string | null;
  path: string;
  line: number | null;
  onClose: () => void;
}) {
  const file = useProjectCat(projectId, path);

  return (
    <Panel
      title={path}
      aside={
        <span className="pj-aside">
          {line !== null && <span className="pj-read">line {line}</span>}
          <ReadAt at={file.dataUpdatedAt} />
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
        </span>
      }
    >
      {file.isError && file.data !== undefined && <StaleNote dataUpdatedAt={file.dataUpdatedAt} />}
      {file.isError && file.data === undefined && (
        <InspectAbsence error={file.error} projectRoot={projectRoot} what="that file" />
      )}
      {!file.isError && file.data === undefined && <p className="pj-loading">reading the file…</p>}
      {file.data !== undefined && file.data === "" && (
        <Quiet says="that file is empty — a real answer, not a failed read." />
      )}
      {file.data !== undefined && file.data !== "" && <FileBody text={file.data} line={line} />}
    </Panel>
  );
}

/**
 * The text, with its numbers, and one line marked when a search hit asked for it.
 *
 * The mark is one band laid behind both columns at the line's offset, not a
 * span per line: the file is two `pre`s so its numbers cannot be copied with it,
 * and splitting it into a node per line to style one of them would cost a page
 * element per line of somebody's lock file. The band is `aria-hidden`; what a
 * screen reader gets is the "line 612" beside the panel's title.
 */
function FileBody({ text, line }: { text: string; line: number | null }) {
  const lines = text.split("\n");
  const target = line !== null && line <= lines.length ? line : null;
  const mark = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    // `?.` on the method because jsdom has no `scrollIntoView`, and a missing
    // method there is not a reason to throw in a test.
    if (target !== null) mark.current?.scrollIntoView?.({ block: "center" });
  }, [target, text]);

  return (
    <div className="pj-file-body">
      {target !== null && (
        <span
          ref={mark}
          className="pj-file-mark"
          aria-hidden="true"
          data-line={target}
          style={{ top: `calc(var(--space-3) + ${String(target - 1)} * 1.45em)` }}
        />
      )}
      <pre className="pj-file-nums" aria-hidden="true">
        <code>{lines.map((_, index) => String(index + 1)).join("\n")}</code>
      </pre>
      <pre className="pj-file-text">
        <code>{text}</code>
      </pre>
    </div>
  );
}

/* ---------------------------------------------------------------- searching -- */

/**
 * Plain text, over a subtree, answered in line numbers.
 *
 * What was **asked** lives in the location and what is being **typed** does
 * not: typing is not searching, and a query per keystroke would walk somebody's
 * whole tree once per character. Putting the asked query in the route is what
 * makes a result something you can send to a person — which the module header
 * has claimed since it was written and which was not true until now.
 */
function GrepPanel({ projectId, projectRoot }: { projectId: string; projectRoot: string | null }) {
  const navigate = useNavigate();
  const search = useSearch({ strict: false }) as InspectSearch;
  const asked = search.q ?? null;
  const under = search.under ?? "";

  /* Seeded from the location, so a link somebody followed shows the query that
     produced what they are looking at rather than an empty box over it. */
  const [draftQ, setDraftQ] = useState(asked ?? "");
  const [draftUnder, setDraftUnder] = useState(under);

  const matches = useProjectGrep(projectId, asked ?? "", under, asked !== null);
  const groups = groupMatches(matches.data ?? []);

  return (
    <Panel
      title="Search"
      aside={
        <span className="pj-aside">
          {asked !== null && <ReadAt at={matches.dataUpdatedAt} />}
          <Count n={matches.data?.length} />
        </span>
      }
    >
      <Why lead="Plain text, over the files under the folder you name.">
        <p>
          The núcleo does the walking, so this reaches files no editor has open — and it reads, only
          ever reads.
        </p>
      </Why>
      <form
        className="pj-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (draftQ.trim() === "") return;
          void navigate({
            to: inspectPath(projectId, "search"),
            search: { q: draftQ.trim(), under: draftUnder.trim() || undefined },
          });
        }}
      >
        <label className="pj-field-label" htmlFor="pj-grep-q">
          Text to find
        </label>
        <input
          id="pj-grep-q"
          className="pj-field-input"
          type="text"
          value={draftQ}
          spellCheck={false}
          onChange={(event) => setDraftQ(event.target.value)}
        />
        <label className="pj-field-label" htmlFor="pj-grep-path">
          Under this folder (blank is the whole project)
        </label>
        <input
          id="pj-grep-path"
          className="pj-field-input"
          type="text"
          value={draftUnder}
          spellCheck={false}
          onChange={(event) => setDraftUnder(event.target.value)}
        />
        {/* A real submit, so Enter in either field searches. `Button` defaults to
            `type="button"` precisely so that inline controls do not submit the
            forms they sit in; this is the one here that should. `ghost`, because
            a search is a read: `approve` is the one affirmative fill in the
            system, and a green search button teaches that green means "click". */}
        <Button variant="ghost" type="submit" disabled={draftQ.trim() === ""}>
          Search
        </Button>
      </form>

      {asked !== null && matches.isError && matches.data !== undefined && (
        <StaleNote dataUpdatedAt={matches.dataUpdatedAt} />
      )}
      {asked !== null && matches.isError && matches.data === undefined && (
        <InspectAbsence error={matches.error} projectRoot={projectRoot} what="that search" />
      )}
      {asked !== null && !matches.isError && matches.data === undefined && (
        <p className="pj-loading">searching…</p>
      )}
      {matches.data !== undefined && groups.length === 0 && (
        <Quiet says="nothing in that folder contains it." />
      )}
      {groups.length > 0 && (
        <ul className="pj-matches" aria-label="Matches">
          {groups.map((group) => (
            <li className="pj-match-group" key={group.path}>
              {/*
                The path once, as a heading, and each hit a link into the file.
                A flat list repeated the path on every row — forty times for a
                common word, and it is the longest thing on the row.

                Both carry the file's folder as `path`, so the listing beside
                the opened file is the folder it is in rather than the project
                root — the link used to carry the file alone. A hit also carries
                its line, which the file view scrolls to and marks.
              */}
              <Link
                className="pj-match-path"
                to={inspectPath(projectId, "browse")}
                search={{ path: parentPath(group.path) || undefined, file: group.path }}
              >
                {group.path}
              </Link>
              <ul className="pj-match-lines">
                {group.matches.map((match) => (
                  <li className="pj-match" key={`${match.path}:${String(match.line)}`}>
                    <Link
                      className="pj-match-link"
                      to={inspectPath(projectId, "browse")}
                      search={{
                        path: parentPath(match.path) || undefined,
                        file: match.path,
                        line: match.line,
                      }}
                    >
                      <span className="pj-match-line">{match.line}</span>
                      <code className="pj-match-text">{match.text}</code>
                      <span className="sr-only">, open {match.path} at this line</span>
                    </Link>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------------- diff -- */

function DiffPanel({ projectId, projectRoot }: { projectId: string; projectRoot: string | null }) {
  const diff = useProjectDiff(projectId);

  return (
    <Panel
      title="Uncommitted changes"
      aside={
        /* The time beside the button, because "read once" was the whole promise
           and the panel never said WHEN once was. */
        <span className="pj-aside">
          <ReadAt at={diff.dataUpdatedAt} />
          <Button variant="ghost" disabled={diff.isFetching} onClick={() => void diff.refetch()}>
            {diff.isFetching ? "Reading…" : "Refresh"}
          </Button>
        </span>
      }
    >
      <Why lead="What is in the working tree and not yet in a commit.">
        <p>
          Read once when you open this and again when you ask — a tree changes because a person or a
          job changed it, not on a timer, so polling it would be a git call every three seconds for
          an answer nobody is watching.
        </p>
      </Why>
      {diff.isError && diff.data !== undefined && <StaleNote dataUpdatedAt={diff.dataUpdatedAt} />}
      {diff.isError && diff.data === undefined && (
        <InspectAbsence error={diff.error} projectRoot={projectRoot} what="the diff" />
      )}
      {!diff.isError && diff.data === undefined && <p className="pj-loading">reading the tree…</p>}
      {diff.data !== undefined && diff.data.trim() === "" && (
        <Quiet says="nothing is uncommitted — the tree is clean." />
      )}
      {diff.data !== undefined && diff.data.trim() !== "" && <DiffBody diff={diff.data} />}
    </Panel>
  );
}

/**
 * A diff, with the four line kinds told apart.
 *
 * The `+` and the `-` are already the first character of every line and carry
 * the meaning on their own; the colour reinforces what is there rather than
 * being the only copy of it, which is the rule the whole app's badges follow.
 *
 * Past the cap it is one block of text and the page says so. A `git diff` of a
 * dirty tree has no upper bound — a regenerated lock file alone is tens of
 * thousands of lines — and a span per line is a DOM node per line.
 */
function DiffBody({ diff }: { diff: string }) {
  const lines = diffLines(diff);
  if (lines.length > DIFF_LINE_CAP) {
    return (
      <>
        <p className="pj-note">
          {lines.length.toLocaleString()} lines — past {DIFF_LINE_CAP.toLocaleString()} this is
          drawn as plain text, because a span per line is a page element per line.
        </p>
        <pre className="pj-diff">
          <code>{diff}</code>
        </pre>
      </>
    );
  }
  return (
    <pre className="pj-diff">
      <code>
        {lines.map((line, index) => (
          <span className={`pj-diff-line pj-diff-${line.kind}`} key={index}>
            {line.text}
          </span>
        ))}
      </code>
    </pre>
  );
}
