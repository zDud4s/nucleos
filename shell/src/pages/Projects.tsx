import { useState } from "react";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  DIFF_LINE_CAP,
  RULE_STATE_WORD,
  VIEWS,
  autonomyOf,
  concernsOf,
  diffLines,
  groupMatches,
  headlineFor,
  joinPath,
  normaliseView,
  pathSegments,
  pathUpTo,
  useProjectCat,
  useProjectDiff,
  useProjectGrep,
  useProjectLs,
  useProjectRules,
  useSetWipLimit,
  type AutonomyRule,
  type Concern,
  type ConcernWeight,
  type InspectEntry,
  type ProjectRules,
  type ProjectView,
  type RuleState,
} from "../data/projects";
import { useProjects } from "../data/system";
import {
  Badge,
  Button,
  ErrorNote,
  Meter,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Teach,
  type BadgeTone,
} from "../ui";
import "./projects.css";

/**
 * Projects — the read-only inspector, and the one brake that is a write.
 *
 * The page exists because two facts about a project were only readable by
 * leaving the app: what it will do on its own (`.ai/autopilot.yaml`, on disk)
 * and what its tree currently looks like. Both are shown here, and this page
 * edits neither.
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
 * **The single write on this page is the WIP ceiling**, because it is the one of
 * these facts that lives in the database rather than in a file, and until now
 * could only be changed with `sqlite3`.
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
        from the Código mode wants the first.
      */}
      <p className="pj-back">
        <Link to="/projects/$projectId/$view" params={{ projectId, view: "codigo" }}>
          ‹ back to {projectId}
        </Link>
        {" · "}
        <Link to="/projects">all projects</Link>
      </p>

      <Concerns
        projectId={projectId}
        rules={rules.data}
        rootExists={project?.root_exists}
        here={view}
      />

      <ViewTabs projectId={projectId} view={view} />
      {/* Keyed on the project so a query somebody is halfway through typing
          resets when the subject changes. The folder and the open file live in
          the location now and change with it; this key is for the drafts that
          cannot. */}
      <ProjectViews
        key={projectId}
        projectId={projectId}
        projectRoot={project?.project_root ?? null}
        rules={rules}
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
 * The link is dropped on the view that already answers the finding: a link to
 * where you are standing is furniture.
 */
function Concerns({
  projectId,
  rules,
  rootExists,
  here,
}: {
  projectId: string;
  rules: ProjectRules | undefined;
  rootExists: boolean | null | undefined;
  here: ProjectView;
}) {
  if (rules === undefined) return null;
  const found = concernsOf(rules, rootExists);
  if (found.length === 0) return null;

  return (
    <section className="pj-concerns" aria-label="What is wrong here">
      <ul className="pj-concern-list">
        {found.map((concern) => (
          <ConcernRow key={concern.kind} concern={concern} projectId={projectId} here={here} />
        ))}
      </ul>
    </section>
  );
}

function ConcernRow({
  concern,
  projectId,
  here,
}: {
  concern: Concern;
  projectId: string;
  here: ProjectView;
}) {
  return (
    <li className={`pj-concern pj-concern-${concern.weight}`}>
      <span className="pj-concern-mark" aria-hidden="true">
        {WEIGHT_MARK[concern.weight]}
      </span>
      <span className="pj-concern-said">{concern.said}</span>
      {concern.view === here ? null : (
        <Link className="pj-concern-where" to={inspectPath(projectId, concern.view)}>
          {VIEW_LABEL[concern.view]}
        </Link>
      )}
    </li>
  );
}

/* ------------------------------------------------------------------ tabs -- */

/**
 * The four views, with a divider that says they are not four peers.
 *
 * Browse, Search and Diff are three ways of reading the tree and are superseded
 * by the Código mode the day it grows them. "On its own" answers a different
 * question, is superseded by nothing that exists, and holds the only control on
 * the page. Drawn as four equal tabs, the durable one was the last of four and
 * had the most generic name on the screen.
 */
function ViewTabs({ projectId, view }: { projectId: string; view: ProjectView }) {
  return (
    <nav className="pj-tabs" aria-label="Project views">
      {VIEWS.map((candidate) => {
        const classes = ["pj-tab"];
        if (candidate === view) classes.push("pj-tab-active");
        /* The divider, on the one view that is not one of the three. */
        if (candidate === "rules") classes.push("pj-tab-apart");
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
  rules,
  view,
}: {
  projectId: string;
  projectRoot: string | null;
  rules: ReturnType<typeof useProjectRules>;
  view: ProjectView;
}) {
  return (
    <div className="pj-sections">
      {view === "browse" && <BrowsePanel projectId={projectId} projectRoot={projectRoot} />}
      {view === "search" && <GrepPanel projectId={projectId} projectRoot={projectRoot} />}
      {view === "diff" && <DiffPanel projectId={projectId} projectRoot={projectRoot} />}
      {view === "rules" && <RulesView projectId={projectId} rules={rules} />}
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
  if (error.status === 404 && projectRoot === null) {
    return (
      <p className="pj-absence" role="status">
        This project has no folder recorded, so there is nothing to look inside. A folder is named
        when the project is put into shadow or acting, on the Autopilot page.
      </p>
    );
  }
  if (error.status === 404) {
    return (
      <p className="pj-absence" role="status">
        Not found — and the núcleo answers the same way for two different things: that path is not
        there, or the folder recorded for this project ({projectRoot}) is gone from the disk. Check
        the folder before hunting for the file.
      </p>
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
      <Panel title="Folder" aside={<Count n={listing.data?.length} />}>
        <Breadcrumbs path={path} onGo={(next) => go({ path: next || undefined }, false)} />
        {listing.isError && (
          <InspectAbsence error={listing.error} projectRoot={projectRoot} what="that folder" />
        )}
        {!listing.isError && listing.data === undefined && (
          <p className="pj-loading">reading the folder…</p>
        )}
        {listing.data !== undefined && ordered.length === 0 && (
          <p className="pj-empty">this folder is empty.</p>
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
          onClose={() => go({ path: path || undefined }, true)}
        />
      )}
    </div>
  );
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
      {path === "" ? (
        <span className="pj-crumb pj-crumb-here" aria-current="location">
          the project root
        </span>
      ) : (
        <Button variant="link" onClick={() => onGo("")}>
          <span className="pj-crumb">the project root</span>
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
 */
function FileView({
  projectId,
  projectRoot,
  path,
  onClose,
}: {
  projectId: string;
  projectRoot: string | null;
  path: string;
  onClose: () => void;
}) {
  const file = useProjectCat(projectId, path);

  return (
    <Panel
      title="File"
      aside={
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
      }
    >
      <p className="pj-file-path">{path}</p>
      {file.isError && (
        <InspectAbsence error={file.error} projectRoot={projectRoot} what="that file" />
      )}
      {!file.isError && file.data === undefined && <p className="pj-loading">reading the file…</p>}
      {file.data !== undefined && file.data === "" && (
        <p className="pj-empty">that file is empty — a real answer, not a failed read.</p>
      )}
      {file.data !== undefined && file.data !== "" && <FileBody text={file.data} />}
    </Panel>
  );
}

function FileBody({ text }: { text: string }) {
  const lines = text.split("\n");
  return (
    <div className="pj-file-body">
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
    <Panel title="Search" aside={<Count n={matches.data?.length} />}>
      <p className="pj-note">
        Plain text, over the files under the folder you name. The núcleo does the walking, so this
        reaches files no editor has open — and it reads, only ever reads.
      </p>
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
            forms they sit in; this is the one here that should. */}
        <Button variant="approve" type="submit" disabled={draftQ.trim() === ""}>
          Search
        </Button>
      </form>

      {asked !== null && matches.isError && (
        <InspectAbsence error={matches.error} projectRoot={projectRoot} what="that search" />
      )}
      {asked !== null && !matches.isError && matches.data === undefined && (
        <p className="pj-loading">searching…</p>
      )}
      {matches.data !== undefined && groups.length === 0 && (
        <p className="pj-empty">nothing in that folder contains it.</p>
      )}
      {groups.length > 0 && (
        <ul className="pj-matches" aria-label="Matches">
          {groups.map((group) => (
            <li className="pj-match-group" key={group.path}>
              {/*
                The path once, as a heading, and each hit a link into the file.
                A flat list repeated the path on every row — forty times for a
                common word, and it is the longest thing on the row.
              */}
              <Link
                className="pj-match-path"
                to={inspectPath(projectId, "browse")}
                search={{ file: group.path }}
              >
                {group.path}
              </Link>
              <ul className="pj-match-lines">
                {group.matches.map((match) => (
                  <li className="pj-match" key={`${match.path}:${String(match.line)}`}>
                    <span className="pj-match-line">{match.line}</span>
                    <code className="pj-match-text">{match.text}</code>
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
        <Button variant="ghost" disabled={diff.isFetching} onClick={() => void diff.refetch()}>
          {diff.isFetching ? "Reading…" : "Refresh"}
        </Button>
      }
    >
      <p className="pj-note">
        What is in the working tree and not yet in a commit. Read once when you open this and again
        when you ask — a tree changes because a person or a job changed it, not on a timer, so
        polling it would be a git call every three seconds for an answer nobody is watching.
      </p>
      {diff.isError && (
        <InspectAbsence error={diff.error} projectRoot={projectRoot} what="the diff" />
      )}
      {!diff.isError && diff.data === undefined && <p className="pj-loading">reading the tree…</p>}
      {diff.data !== undefined && diff.data.trim() === "" && (
        <p className="pj-empty">nothing is uncommitted — the tree is clean.</p>
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

/* ------------------------------------------------------------ on its own -- */

function RulesView({
  projectId,
  rules,
}: {
  projectId: string;
  rules: ReturnType<typeof useProjectRules>;
}) {
  if (rules.isError) {
    return (
      <Panel title="On its own">
        {isApiRefusal(rules.error) ? (
          <RefusalNote refusal={rules.error} />
        ) : (
          <ErrorNote>
            the núcleo did not answer — nothing is known about this project&apos;s rules
          </ErrorNote>
        )}
      </Panel>
    );
  }
  if (rules.data === undefined) {
    return (
      <Panel title="On its own">
        <p className="pj-loading">reading the rules…</p>
      </Panel>
    );
  }

  return (
    <>
      <RulesFileState rules={rules.data} />
      <Autonomy rules={rules.data} />
      <GatePanel command={rules.data.gate_command} beforePublish={rules.data.gate_before_publish} />
      <WipPanel projectId={projectId} rules={rules.data} />
    </>
  );
}

/**
 * What state the rules file is in, and why that is information rather than a
 * fault.
 *
 * `absent` is ordinary: `.ai/autopilot.yaml` is gitignored, so a fresh clone and
 * every worktree legitimately has none, and a project with no file simply does
 * nothing on its own.
 *
 * `unreadable` is the row that has to be loud. `config.rs` parses with
 * `deny_unknown_fields` precisely so a typo is an error rather than a silently
 * empty ruleset — but that error used to reach only a log line, so writing
 * `schedule:` for `schedules:` stopped all autonomy for the project and looked
 * exactly like nothing happening. The daemon's own message is the whole content
 * of that finding, so it is shown verbatim and first.
 *
 * **Not a `Panel` and not a `Badge`.** It was a whole bordered section spending
 * a hundred and forty pixels to say a path and one word, with that word in the
 * same filled pill a rule's state uses — so a file's condition and a schedule's
 * condition were the same shape. A file reads as a file: its name, its state
 * beside it, and where it is.
 */
function RulesFileState({ rules }: { rules: ProjectRules }) {
  return (
    <div className="pj-source">
      <p className="pj-source-line">
        <code className="pj-source-name">.ai/autopilot.yaml</code>
        <span className={`pj-source-state pj-source-${rules.rules_file}`}>{rules.rules_file}</span>
        <span className="pj-meta">
          {rules.project_root === null ? "no folder recorded" : `in ${rules.project_root}`}
        </span>
      </p>
      {rules.rules_file === "unreadable" && (
        <div className="pj-rules-error" role="alert">
          <p className="pj-rules-error-title">
            The núcleo could not read this project&apos;s rules, so it is doing nothing on its own.
          </p>
          <pre className="pj-rules-error-detail">
            <code>{rules.rules_error ?? "the núcleo reported no detail"}</code>
          </pre>
          <p className="pj-note">
            The file is parsed strictly on purpose: an unknown key is an error rather than a silently
            empty ruleset. Until it parses, nothing this project might run is known — not because
            there is nothing, but because none of it could be loaded.
          </p>
        </div>
      )}
      {rules.rules_file === "absent" && (
        <p className="pj-note">
          There is no .ai/autopilot.yaml under this folder. That is an ordinary state and not a fault
          — the file is gitignored, so a fresh clone has none — and it means this project starts
          nothing by itself.
        </p>
      )}
    </div>
  );
}

/** A clock or a commit, as a mark and a word. Never colour alone, like every mark in this app. */
const CLOCK_MARK: Record<AutonomyRule["clock"], string> = { cron: "◷", commit: "◆" };
const CLOCK_SAID: Record<AutonomyRule["clock"], string> = {
  cron: "on a clock",
  commit: "on a commit",
};

/**
 * The one place a filled badge is used on this page, and it means the state of
 * a rule.
 *
 * `unseen` is `pending` and not `active`: a trigger with no commit to compare
 * against fires nothing, by design, and drawing it in the same green as a rule
 * that will run tonight is how "armed" came to mean two different things.
 */
const STATE_TONE: Record<RuleState, BadgeTone> = {
  armed: "active",
  "never-fires": "danger",
  capped: "paused",
  unseen: "pending",
};

/**
 * Everything that starts work here without you, as one table.
 *
 * Two panels became one for the reason the Teams console became a table: a
 * project with two schedules and one trigger read as two half-empty lists
 * rather than as *three things run here on their own*, and a card whose blocks
 * appear only sometimes starts the next row at a different height every time. A
 * table cannot have that defect, because the columns line up by being columns.
 *
 * **Not drawn at all when the file will not parse.** The two lists it replaces
 * printed "nothing is scheduled" and "no commit starts anything here" directly
 * under an alert that had just said every rule below was absent because none
 * could be loaded — four hundred pixels spent saying something the page had
 * disclaimed one paragraph earlier.
 */
function Autonomy({ rules }: { rules: ProjectRules }) {
  if (rules.rules_file === "unreadable") return null;

  const running = autonomyOf(rules);
  if (running.length === 0) {
    return (
      <Panel title="On its own">
        <Teach title="Nothing starts work here by itself">
          <p>
            No schedule and no repo trigger, so this project only ever does what somebody asks it to.
            Both are written in <code>.ai/autopilot.yaml</code> under the project&rsquo;s folder — a
            schedule runs on a clock, a repo trigger runs when a branch gets a commit — and the file
            is edited in the project workspace, as text, so the comments in it survive.
          </p>
        </Teach>
      </Panel>
    );
  }

  return (
    <Panel title="What starts work here without you" aside={<Count n={running.length} />}>
      <div className="pj-table-scroller">
        <table className="pj-table">
          <caption className="pj-said">
            Every rule that can start work in this project with nobody asking, what makes it go, and
            when it last did.
          </caption>
          <thead>
            <tr>
              <th scope="col">Rule</th>
              <th scope="col">What makes it go</th>
              <th scope="col">State</th>
              <th scope="col" className="pj-col-num">
                Next
              </th>
              <th scope="col" className="pj-col-num">
                Last
              </th>
              <th scope="col" className="pj-col-num">
                Today
              </th>
            </tr>
          </thead>
          <tbody>
            {running.map((rule) => (
              <RuleRows key={`${rule.clock}:${rule.name}`} rule={rule} />
            ))}
          </tbody>
        </table>
      </div>
    </Panel>
  );
}

function RuleRows({ rule }: { rule: AutonomyRule }) {
  return (
    <>
      <tr className={rule.problem === null ? undefined : "pj-row-problem"}>
        <th scope="row" className="pj-row-name">
          <span className="pj-row-title">
            <span className="pj-row-kind" aria-hidden="true">
              {CLOCK_MARK[rule.clock]}
            </span>
            {rule.name}
            <span className="pj-said">, {CLOCK_SAID[rule.clock]}</span>
          </span>
          {/* Always drawn, whatever its length, and clamped to one line. A field
              that appears on some rows and not others starts the next column at
              two different heights — the defect the Teams cards had. */}
          <span className="pj-row-asks">{rule.prompt}</span>
          {rule.cwd !== null && <span className="pj-row-where">in {rule.cwd}</span>}
        </th>
        <td>
          <Trigger rule={rule} />
        </td>
        <td>
          <Badge tone={STATE_TONE[rule.state]}>{RULE_STATE_WORD[rule.state]}</Badge>
        </td>
        <td className="pj-col-num">
          <Moment
            at={rule.next}
            absent={
              rule.clock === "commit" || rule.state === "never-fires" ? "—" : "not scheduled"
            }
          />
        </td>
        <td className="pj-col-num">
          <Moment at={rule.last} absent={rule.clock === "commit" ? "—" : "never"} />
        </td>
        <td className="pj-col-num">
          <Today today={rule.today} />
        </td>
      </tr>
      {/*
        First-class and spanning, not a tooltip: an unparseable cron or an
        unknown timezone makes the tick skip this rule 2,880 times a day and log
        at debug, which is how a rule silently never runs.
      */}
      {rule.problem !== null && (
        <tr>
          <td className="pj-problem-cell" colSpan={6}>
            <p className="pj-problem" role="alert">
              {rule.problem}
            </p>
          </td>
        </tr>
      )}
    </>
  );
}

/**
 * What makes a rule go, drawn as what it is.
 *
 * A cron expression is a literal and gets a box; a branch is a name and does
 * not. They used to share one chip, so `0 3 * * *` and `main` — a schedule and
 * a git ref, which have nothing in common — wore identical clothes.
 */
function Trigger({ rule }: { rule: AutonomyRule }) {
  if (rule.clock === "commit") {
    return (
      <span className="pj-branch">
        <span className="pj-branch-what">
          a commit on <span className="pj-branch-name">{rule.when}</span>
        </span>
        {rule.sha !== null && (
          <span className="pj-branch-seen">last saw {rule.sha.slice(0, 12)}</span>
        )}
      </span>
    );
  }
  return (
    <span className="pj-when">
      <code className="pj-cron">{rule.when}</code>
      {/* `null` is UTC — the scheduler's own default, not an unset field. */}
      <span className="pj-meta">{rule.zone}</span>
    </span>
  );
}

/** A time, or the word that says why there is not one. */
function Moment({ at, absent }: { at: string | null; absent: string }) {
  if (at === null) return <span className="pj-figure pj-figure-none">{absent}</span>;
  return (
    <span className="pj-figure">
      <RelativeTime at={at} />
    </span>
  );
}

/**
 * Today's allowance, and a mark when it is spent.
 *
 * `6 / 6` and `5 / 6` are one glyph apart and are not the same news — the same
 * reason the Teams table marks a ratio at its ceiling.
 */
function Today({ today }: { today: AutonomyRule["today"] }) {
  if (today === null) return <span className="pj-figure pj-figure-none">—</span>;
  const full = today.cap > 0 && today.fired >= today.cap;
  return (
    <span className={full ? "pj-figure pj-figure-full" : "pj-figure"}>
      {today.fired}
      <span className="pj-figure-of"> / {today.cap}</span>
      {full && <span className="pj-said"> — today&apos;s allowance is spent</span>}
    </span>
  );
}

/**
 * What measures this project's work, and when.
 *
 * **Out of `variant="dim"`.** `dim` means "present but not the thing you came
 * for", and this panel carries the most consequential sentence on the page: a
 * queue set to wait for a gate that does not exist refuses every merge, over a
 * key in a gitignored file. That state was drawn in muted body text — quieter
 * than the paragraph above it — while the panel's own comment called it the
 * worst of the three. It is an alert now, which is what the comment always said.
 */
function GatePanel({ command, beforePublish }: { command: string | null; beforePublish: boolean }) {
  const configured = command !== null && command.trim() !== "";
  const contradiction = beforePublish && !configured;

  return (
    <Panel title="Gate">
      {configured ? (
        <code className="pj-gate">{command}</code>
      ) : (
        <p className="pj-note">
          No gate is configured, so nothing measures this project&apos;s work. That is why a job item
          can read <em>passed</em> with no gate status: there was nothing to pass.
        </p>
      )}
      {/* The second moment the same command can run, and the one nothing else on this
          page would reveal. A landing that takes twenty minutes has a reason, and the
          reason is a key in a gitignored file — so this is where it stops being
          invisible. */}
      {contradiction ? (
        <p className="pj-problem" role="alert">
          gate_before_publish is on and no gate command is set, so the queue refuses every merge.
        </p>
      ) : (
        <p className="pj-note">
          {beforePublish
            ? "Merges wait for it. The queue runs it on the merged result and publishes only if it passes; nothing is reverted, because nothing is published first."
            : "Merges do not wait for it: the queue publishes without measuring the tree the two branches make together."}
        </p>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------- the one write -- */

/**
 * The WIP ceiling — the only thing this page writes.
 *
 * `null` is the brake **off** and is never drawn as `0`: the daemon compares
 * `open >= limit`, so a ceiling of zero would mean *never start anything again*
 * while reading like a number somebody chose. The two are one keystroke apart in
 * a form and opposite in effect, so the form keeps them apart — a number field
 * for a ceiling, a separate control for turning the brake off.
 *
 * Plain buttons rather than the two-click interlock: a ceiling is a number that
 * can be typed again in five seconds, and `ConfirmButton` exists for what cannot
 * be undone. Spending the interlock here would spend it everywhere.
 *
 * The reading above the form is a `Meter` and no longer a sentence. It is a
 * count against a ceiling with a state of full, which is the exact thing that
 * primitive draws — including the one distinction this panel exists to defend:
 * `ceiling: null` is a dashed rail reading "no ceiling", which cannot be
 * mistaken for either an empty bar or a full one.
 */
function WipPanel({ projectId, rules }: { projectId: string; rules: ProjectRules }) {
  const setLimit = useSetWipLimit();
  const [draft, setDraft] = useState(rules.wip_limit === null ? "" : String(rules.wip_limit));
  const parsed = Number.parseInt(draft.trim(), 10);
  const valid = Number.isSafeInteger(parsed) && parsed >= 0;

  return (
    <Panel title="Work-in-progress ceiling">
      <p className="pj-note">
        How much unreviewed work this project may be holding before it stops starting more. The brake
        is self-clearing — it releases the moment you review something — so this is the answer to
        &ldquo;how much unanswered work am I willing to have open&rdquo;, not a quota.
      </p>

      <Meter
        label="open and unreviewed"
        value={rules.open_proposals}
        ceiling={rules.wip_limit}
        tone={rules.queue_full ? "pending" : "active"}
      />

      <p className="pj-wip-state">
        {rules.wip_limit === null ? (
          <>
            The brake is <strong>off</strong>: no ceiling at all, which is not the same as a ceiling
            of zero.
          </>
        ) : rules.queue_full ? (
          <>It is currently holding new autonomous work back.</>
        ) : (
          <>Nothing is being held back by it.</>
        )}
      </p>

      <div className="pj-form">
        <label className="pj-field-label" htmlFor="pj-wip">
          Ceiling
        </label>
        <input
          id="pj-wip"
          className="pj-field-input pj-field-number"
          type="number"
          min={0}
          step={1}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <div className="pj-actions">
          <Button
            variant="approve"
            disabled={!valid || setLimit.isPending}
            onClick={() => setLimit.mutate({ projectId, limit: parsed })}
          >
            Set the ceiling
          </Button>
          <Button
            variant="ghost"
            disabled={rules.wip_limit === null || setLimit.isPending}
            onClick={() => {
              setDraft("");
              setLimit.mutate({ projectId, limit: null });
            }}
          >
            Remove the ceiling
          </Button>
        </div>
      </div>

      {draft.trim() !== "" && !valid && (
        <p className="pj-note">
          A ceiling is a whole number, zero or more. The núcleo refuses a negative one outright — it
          would read like a number somebody chose and mean &ldquo;never start anything again&rdquo;.
        </p>
      )}
      {setLimit.isError && <WipError error={setLimit.error} />}
    </Panel>
  );
}

function WipError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the ceiling is unchanged</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_found: "the núcleo has no row for this project, so there is no ceiling to set on it",
        bad_request: "a ceiling cannot be negative",
      }}
    />
  );
}

/* ---------------------------------------------------------------- shared -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="pj-count">{n}</span>;
}
