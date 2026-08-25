import { useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  REACHABILITY_TEXT,
  joinPath,
  pathSegments,
  pathUpTo,
  readReachability,
  scheduleCapped,
  scheduleNeverFires,
  useProjectCat,
  useProjectDiff,
  useProjectGrep,
  useProjectLs,
  useProjectRules,
  useSetWipLimit,
  type InspectEntry,
  type ProjectRules,
  type RepoTriggerView,
  type ScheduleView,
} from "../data/projects";
import { useProjects, type ProjectSummary } from "../data/system";
import {
  Badge,
  Button,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  Teach,
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
 */

/**
 * Where this inspector lives, now that the workspace owns the shorter path.
 *
 * One function rather than three template literals, because the two callers
 * below drifted apart the moment there was a prefix to forget.
 */
function inspectPath(projectId: string, view: ProjectView): string {
  return `/projects/${projectId}/inspect/${view}`;
}

/** The four views, in the order the tabs read. */
const VIEWS = ["browse", "search", "diff", "rules"] as const;
export type ProjectView = (typeof VIEWS)[number];

const VIEW_LABEL: Record<ProjectView, string> = {
  browse: "Browse",
  search: "Search",
  diff: "Diff",
  rules: "Rules",
};

/**
 * A `$view` param as one of the four.
 *
 * Falls back rather than refusing. TanStack hands route params through as
 * strings with no validation of its own, so `/projects/alpha/brwose` is a path
 * a person can reach by typing — and answering a typo with a dead end teaches
 * nothing. Browse is the right landing: it is the view that needs no input.
 */
export function normaliseView(raw: string | undefined): ProjectView {
  const candidate = (raw ?? "").trim().toLowerCase();
  return (VIEWS as readonly string[]).includes(candidate) ? (candidate as ProjectView) : "browse";
}

export function Projects() {
  const params = useParams({ strict: false }) as { projectId?: string; view?: string };
  const projects = useProjects();

  const rows = projects.data ?? [];
  const projectId = params.projectId ?? null;
  const view = normaliseView(params.view);
  const project = rows.find((row) => row.project_id === projectId);
  const stale = projects.isError && projects.data !== undefined;

  return (
    <>
      <PageHeader title="Projects" headline={headlineFor(rows, projects.data !== undefined)} />

      {/*
        The door to adding one. Here rather than in the rail, because the rail is the design's fixed
        list of places and this is an action taken from the list of what exists — and because the
        first thing anybody does on an empty roster is look at the roster.
      */}
      <p className="pj-add">
        <Link to="/projects/new">Add a project…</Link>
      </p>

      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {projects.isError && projects.data === undefined && <RosterError error={projects.error} />}

      <ProjectPicker rows={rows} answered={projects.data !== undefined} selected={projectId} view={view} />

      {projectId === null && (
        <Teach title="Choose a project">
          <p>
            This page is a window onto one project at a time: what it will do without being asked,
            and what is in its folder right now. Nothing here changes a file — the only thing this
            page writes is the ceiling on how much unreviewed work a project may be holding.
          </p>
        </Teach>
      )}

      {projectId !== null && (
        <>
          <ViewTabs projectId={projectId} view={view} />
          {/* Keyed on the project so a folder, a query and an open file all reset
              when the subject changes. Without the key, switching projects would
              carry one project's path into another's tree and ask for a folder
              that is not there. */}
          <ProjectViews
            key={projectId}
            projectId={projectId}
            projectRoot={project?.project_root ?? null}
            view={view}
          />
        </>
      )}
    </>
  );
}

function headlineFor(rows: ProjectSummary[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "the núcleo knows of no project";
  const rooted = rows.filter((row) => row.project_root !== null).length;
  return rooted === rows.length
    ? `${rows.length} ${rows.length === 1 ? "project" : "projects"}, all with a folder`
    : `${rows.length} ${rows.length === 1 ? "project" : "projects"}, ${rows.length - rooted} without a folder`;
}

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the roster</ErrorNote>;
}

/* ------------------------------------------------------------- the picker -- */

function ProjectPicker({
  rows,
  answered,
  selected,
  view,
}: {
  rows: ProjectSummary[];
  answered: boolean;
  selected: string | null;
  view: ProjectView;
}) {
  if (!answered) return <p className="pj-loading">reading the roster…</p>;
  if (rows.length === 0) {
    return <p className="pj-empty">no project has been registered with the núcleo.</p>;
  }
  return (
    <ul className="pj-picker" aria-label="Projects">
      {rows.map((project) => (
        <ProjectChip
          key={project.project_id}
          project={project}
          active={project.project_id === selected}
          view={view}
        />
      ))}
    </ul>
  );
}

/**
 * One project, with its mode and whether its folder is actually there.
 *
 * The reachability is a real probe — the project's own listing of its root —
 * and not a guess from the presence of a string. A recorded root is a row in a
 * table; the folder it names is on a disk somebody may have reorganised, and
 * the difference between those two is exactly what a person hunting a broken
 * project needs to be told first.
 *
 * A project with no recorded root is not probed at all. The route would answer
 * 404 for it, and that 404 means *you have not named a folder* rather than *the
 * folder is gone* — the two must not read the same.
 */
function ProjectChip({
  project,
  active,
  view,
}: {
  project: ProjectSummary;
  active: boolean;
  view: ProjectView;
}) {
  const probe = useProjectLs(project.project_id, "", project.project_root !== null);
  const reach = readReachability(project.project_root, probe);

  return (
    <li className={active ? "pj-chip pj-chip-active" : "pj-chip"}>
      <Link className="pj-chip-link" to={inspectPath(project.project_id, view)}>
        <span className="pj-chip-name">{project.project_id}</span>
      </Link>
      <Badge tone={project.mode === "active" ? "active" : project.mode === "shadow" ? "shadow" : "off"}>
        {project.mode}
      </Badge>
      <Badge tone={reach === "ok" ? "active" : reach === "gone" ? "danger" : reach === "failed" ? "paused" : "off"}>
        {REACHABILITY_TEXT[reach]}
      </Badge>
    </li>
  );
}

function ViewTabs({ projectId, view }: { projectId: string; view: ProjectView }) {
  return (
    <nav className="pj-tabs" aria-label="Project views">
      {VIEWS.map((candidate) => (
        <Link
          key={candidate}
          className={candidate === view ? "pj-tab pj-tab-active" : "pj-tab"}
          to={inspectPath(projectId, candidate)}
          aria-current={candidate === view ? "page" : undefined}
        >
          {VIEW_LABEL[candidate]}
        </Link>
      ))}
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
      {view === "rules" && <RulesPanel projectId={projectId} />}
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

function BrowsePanel({ projectId, projectRoot }: { projectId: string; projectRoot: string | null }) {
  const [path, setPath] = useState("");
  const [openFile, setOpenFile] = useState<string | null>(null);
  const listing = useProjectLs(projectId, path);
  const entries = listing.data ?? [];

  // Directories first, then files, each alphabetically: a listing sorted only by
  // name buries the folders among the files and makes a deep tree unwalkable.
  const ordered = [...entries].sort(compareEntries);

  return (
    <>
      <Panel title="Folder" aside={<Count n={listing.data?.length} />}>
        <Breadcrumbs
          path={path}
          onGo={(next) => {
            setPath(next);
            setOpenFile(null);
          }}
        />
        {listing.isError && <InspectAbsence error={listing.error} projectRoot={projectRoot} what="that folder" />}
        {!listing.isError && listing.data === undefined && <p className="pj-loading">reading the folder…</p>}
        {listing.data !== undefined && ordered.length === 0 && (
          <p className="pj-empty">this folder is empty.</p>
        )}
        {ordered.length > 0 && (
          <ul className="pj-listing" aria-label="Folder contents">
            {ordered.map((entry) => (
              <li className={entry.is_dir ? "pj-entry pj-entry-dir" : "pj-entry"} key={entry.name}>
                <Button
                  variant="link"
                  onClick={() => {
                    if (entry.is_dir) {
                      setPath(joinPath(path, entry.name));
                      setOpenFile(null);
                      return;
                    }
                    setOpenFile(joinPath(path, entry.name));
                  }}
                >
                  <span className="pj-entry-name">
                    {entry.name}
                    {entry.is_dir ? "/" : ""}
                  </span>
                </Button>
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
          onClose={() => setOpenFile(null)}
        />
      )}
    </>
  );
}

function compareEntries(a: InspectEntry, b: InspectEntry): number {
  if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
  return a.name.localeCompare(b.name);
}

function Breadcrumbs({ path, onGo }: { path: string; onGo: (path: string) => void }) {
  const segments = pathSegments(path);
  return (
    <nav className="pj-crumbs" aria-label="Folder path">
      <Button variant="link" disabled={path === ""} onClick={() => onGo("")}>
        <span className="pj-crumb">the project root</span>
      </Button>
      {segments.map((segment, index) => (
        <Button
          key={`${segment}-${String(index)}`}
          variant="link"
          disabled={index === segments.length - 1}
          onClick={() => onGo(pathUpTo(path, index + 1))}
        >
          <span className="pj-crumb">/ {segment}</span>
        </Button>
      ))}
    </nav>
  );
}

/**
 * One file, as text.
 *
 * Never rendered as markup, and there is no `dangerouslySetInnerHTML` on this
 * page: everything shown here is somebody else's file, and a project under
 * autopilot is a project where an agent may have written it.
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
      {file.isError && <InspectAbsence error={file.error} projectRoot={projectRoot} what="that file" />}
      {!file.isError && file.data === undefined && <p className="pj-loading">reading the file…</p>}
      {file.data !== undefined && file.data === "" && (
        <p className="pj-empty">that file is empty — a real answer, not a failed read.</p>
      )}
      {file.data !== undefined && file.data !== "" && (
        <pre className="pj-file-body">
          <code>{file.data}</code>
        </pre>
      )}
    </Panel>
  );
}

/* ---------------------------------------------------------------- searching -- */

function GrepPanel({ projectId, projectRoot }: { projectId: string; projectRoot: string | null }) {
  const [q, setQ] = useState("");
  const [under, setUnder] = useState("");
  /** What was actually asked for. Typing is not searching — a query per keystroke
      would walk somebody's whole tree once per character. */
  const [asked, setAsked] = useState<{ q: string; path: string } | null>(null);
  const matches = useProjectGrep(projectId, asked?.q ?? "", asked?.path ?? "", asked !== null);
  const rows = matches.data ?? [];

  return (
    <Panel title="Search" aside={<Count n={matches.data?.length} />}>
      <p className="pj-note">
        Plain text, over the files under the folder you name. The núcleo does the walking, so this
        reaches files no editor has open — and it reads, only ever reads.
      </p>
      <div className="pj-form">
        <label className="pj-field-label" htmlFor="pj-grep-q">
          Text to find
        </label>
        <input
          id="pj-grep-q"
          className="pj-field-input"
          type="text"
          value={q}
          spellCheck={false}
          onChange={(event) => setQ(event.target.value)}
        />
        <label className="pj-field-label" htmlFor="pj-grep-path">
          Under this folder (blank is the whole project)
        </label>
        <input
          id="pj-grep-path"
          className="pj-field-input"
          type="text"
          value={under}
          spellCheck={false}
          onChange={(event) => setUnder(event.target.value)}
        />
        <Button
          variant="approve"
          disabled={q.trim() === ""}
          onClick={() => setAsked({ q: q.trim(), path: under.trim() })}
        >
          Search
        </Button>
      </div>

      {asked !== null && matches.isError && (
        <InspectAbsence error={matches.error} projectRoot={projectRoot} what="that search" />
      )}
      {asked !== null && !matches.isError && matches.data === undefined && (
        <p className="pj-loading">searching…</p>
      )}
      {matches.data !== undefined && rows.length === 0 && (
        <p className="pj-empty">nothing in that folder contains it.</p>
      )}
      {rows.length > 0 && (
        <ul className="pj-matches" aria-label="Matches">
          {rows.map((match) => (
            <li className="pj-match" key={`${match.path}:${String(match.line)}`}>
              <span className="pj-match-path">{match.path}</span>
              <span className="pj-match-line">{match.line}</span>
              <code className="pj-match-text">{match.text}</code>
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
      {diff.isError && <InspectAbsence error={diff.error} projectRoot={projectRoot} what="the diff" />}
      {!diff.isError && diff.data === undefined && <p className="pj-loading">reading the tree…</p>}
      {diff.data !== undefined && diff.data.trim() === "" && (
        <p className="pj-empty">nothing is uncommitted — the tree is clean.</p>
      )}
      {diff.data !== undefined && diff.data.trim() !== "" && (
        <pre className="pj-diff">
          <code>{diff.data}</code>
        </pre>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------------ rules -- */

function RulesPanel({ projectId }: { projectId: string }) {
  const rules = useProjectRules(projectId);

  if (rules.isError) {
    return (
      <Panel title="Rules">
        {isApiRefusal(rules.error) ? (
          <RefusalNote refusal={rules.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — nothing is known about this project&apos;s rules</ErrorNote>
        )}
      </Panel>
    );
  }
  if (rules.data === undefined) {
    return (
      <Panel title="Rules">
        <p className="pj-loading">reading the rules…</p>
      </Panel>
    );
  }

  return (
    <>
      <RulesFileState rules={rules.data} />
      <SchedulesPanel schedules={rules.data.schedules} />
      <RepoTriggersPanel triggers={rules.data.repo_triggers} />
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
 */
function RulesFileState({ rules }: { rules: ProjectRules }) {
  return (
    <Panel title="Rules file">
      <div className="pj-rule-head">
        <span className="pj-meta">{rules.project_root ?? "no folder recorded"}</span>
        <Badge
          tone={rules.rules_file === "present" ? "active" : rules.rules_file === "absent" ? "off" : "danger"}
        >
          {rules.rules_file}
        </Badge>
      </div>
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
            empty ruleset. Until it parses, every schedule and repo trigger below is absent — not
            because there are none, but because none could be loaded.
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
    </Panel>
  );
}

function SchedulesPanel({ schedules }: { schedules: ScheduleView[] }) {
  return (
    <Panel title="Scheduled rules" aside={<Count n={schedules.length} />}>
      {schedules.length === 0 && <p className="pj-empty">nothing is scheduled.</p>}
      {schedules.length > 0 && (
        <ul className="pj-rules" aria-label="Scheduled rules">
          {schedules.map((schedule) => (
            <ScheduleRow key={schedule.name} schedule={schedule} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function ScheduleRow({ schedule }: { schedule: ScheduleView }) {
  const broken = scheduleNeverFires(schedule);
  const capped = scheduleCapped(schedule);

  return (
    <li className="pj-rule">
      <div className="pj-rule-head">
        <span className="pj-rule-name">{schedule.name}</span>
        {broken ? (
          <Badge tone="danger">never fires</Badge>
        ) : capped ? (
          <Badge tone="paused">capped for today</Badge>
        ) : (
          <Badge tone="active">armed</Badge>
        )}
        <code className="pj-cron">{schedule.cron}</code>
        {/* `null` is UTC — the scheduler's own default, not an unset field. */}
        <span className="pj-meta">{schedule.timezone ?? "UTC"}</span>
      </div>

      {/* First-class, not a tooltip: an unparseable cron or an unknown timezone
          makes the tick skip this rule 2,880 times a day and log at debug, which
          is how a rule silently never runs. */}
      {broken && (
        <p className="pj-problem" role="alert">
          {schedule.problem}
        </p>
      )}

      <dl className="pj-rule-facts">
        <div className="pj-fact">
          <dt>next</dt>
          <dd>
            {schedule.next_fire_at === null ? (
              broken ? "never" : "not scheduled"
            ) : (
              <RelativeTime at={schedule.next_fire_at} />
            )}
          </dd>
        </div>
        <div className="pj-fact">
          <dt>last</dt>
          <dd>
            {schedule.last_fired_at === null ? "never" : <RelativeTime at={schedule.last_fired_at} />}
          </dd>
        </div>
        <div className="pj-fact">
          <dt>today</dt>
          <dd>
            {schedule.fires_today} of {schedule.daily_cap}
          </dd>
        </div>
      </dl>
      <p className="pj-prompt">{schedule.prompt}</p>
      {schedule.cwd !== null && <p className="pj-meta">in {schedule.cwd}</p>}
    </li>
  );
}

function RepoTriggersPanel({ triggers }: { triggers: RepoTriggerView[] }) {
  return (
    <Panel title="Repo triggers" aside={<Count n={triggers.length} />}>
      {triggers.length === 0 && <p className="pj-empty">no commit starts anything here.</p>}
      {triggers.length > 0 && (
        <ul className="pj-rules" aria-label="Repo triggers">
          {triggers.map((trigger) => (
            <li className="pj-rule" key={trigger.name}>
              <div className="pj-rule-head">
                <span className="pj-rule-name">{trigger.name}</span>
                <Badge tone={trigger.last_sha === null ? "pending" : "active"}>
                  {trigger.last_sha === null ? "no commit seen yet" : "armed"}
                </Badge>
                <code className="pj-cron">{trigger.branch}</code>
              </div>
              {/* Armed with nothing to compare against fires nothing — by design,
                  and worth saying, because "armed" alone reads as "will run". */}
              {trigger.last_sha === null ? (
                <p className="pj-note">
                  This trigger has never been evaluated, so there is no commit to compare a new one
                  against. It fires on the next evaluation after a commit, not on the history behind
                  it.
                </p>
              ) : (
                <p className="pj-meta">last saw {trigger.last_sha.slice(0, 12)}</p>
              )}
              <p className="pj-prompt">{trigger.prompt}</p>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

function GatePanel({ command, beforePublish }: { command: string | null; beforePublish: boolean }) {
  const configured = command !== null && command.trim() !== "";
  return (
    <Panel title="Gate" variant="dim">
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
          invisible. The contradictory state is drawn too, because the queue refuses
          every merge while it holds and a refusal nobody can explain is the worst of
          the three. */}
      <p className="pj-note">
        {!beforePublish
          ? "Merges do not wait for it: the queue publishes without measuring the tree the two branches make together."
          : configured
            ? "Merges wait for it. The queue runs it on the merged result and publishes only if it passes; nothing is reverted, because nothing is published first."
            : "Merges are set to wait for a gate and none is configured, so the queue refuses them."}
      </p>
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

      <p className="pj-wip-state">
        {rules.wip_limit === null ? (
          <>
            The brake is <strong>off</strong>: no ceiling at all, which is not the same as a ceiling
            of zero.
          </>
        ) : (
          <>
            Ceiling <strong>{rules.wip_limit}</strong>, with {rules.open_proposals} open.
          </>
        )}{" "}
        {rules.queue_full
          ? "It is currently holding new autonomous work back."
          : "Nothing is being held back by it."}
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
