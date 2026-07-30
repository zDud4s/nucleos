import { useCallback, useEffect, useRef, useState } from "react";
import {
  getProjectCat, getProjectDiff, getProjectGrep, getProjectLs, getProjects,
  type ConnectionState, type InspectEntry, type InspectMatch, type ProjectSummary,
} from "./api";
import { breadcrumbs, joinPath, parentPath } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

type View = "browse" | "search" | "diff";

/**
 * Why a read was refused, in the words that say what to do next.
 *
 * The rootless project — the other thing the daemon answers 404 for — never reaches here: the
 * roster already reports `project_root`, so that case is decided before a request is made rather
 * than inferred back out of a status code that means two different things.
 */
function inspectFailure(status: number): string {
  if (status === 404) return "Not found. The path is gone, or was renamed since this listing.";
  if (status === 400) return "That path is outside the project. The daemon refuses to read past the root.";
  if (status === 413) return "Too large to read. The daemon caps how much of a file it will hand over.";
  return "Could not read that.";
}

interface BrowserProps {
  token: string;
  projectId: string;
}

/** The tree, one directory at a time, with the file it opened last shown beside it. */
function Browser({ token, projectId }: BrowserProps) {
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<InspectEntry[] | null>(null);
  const [listFailure, setListFailure] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [openFile, setOpenFile] = useState<string | null>(null);
  const [text, setText] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  // A slow read that lands after something else was opened must not paint the wrong file.
  const readRequest = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void (async () => {
      const result = await getProjectLs(token, projectId, path);
      if (cancelled) return;
      setEntries(result.ok ? result.value : null);
      setListFailure(result.ok ? null : inspectFailure(result.status));
      setLoading(false);
    })();
    return () => { cancelled = true; };
  }, [path, projectId, token]);

  async function open(entry: InspectEntry) {
    const full = joinPath(path, entry.name);
    if (entry.is_dir) {
      setPath(full);
      return;
    }
    readRequest.current = full;
    setOpenFile(full);
    setText(null);
    setFailed(null);
    setReading(true);
    const result = await getProjectCat(token, projectId, full);
    if (readRequest.current !== full) return;
    setReading(false);
    if (!result.ok) {
      setFailed(inspectFailure(result.status));
      return;
    }
    setText(result.value);
  }

  const trail = breadcrumbs(path);

  return (
    <div className="grid">
      <div className="stack">
        <Panel title="Files" aside={entries === null ? undefined : `${entries.length} entries`}>
          <nav className="crumbs" aria-label="Path">
            {trail.map((crumb, index) => (
              <span key={crumb.path}>
                {index > 0 && <span className="c-sep">/</span>}
                <button
                  type="button"
                  className="crumb"
                  aria-current={crumb.path === path ? "location" : undefined}
                  onClick={() => setPath(crumb.path)}
                >
                  {crumb.label}
                </button>
              </span>
            ))}
          </nav>
          {path !== "" && (
            <Button size="sm" onClick={() => setPath(parentPath(path))}>Up one level</Button>
          )}
          {loading && entries === null && <p className="a-note">Loading…</p>}
          {!loading && listFailure !== null && <ErrorNote>{listFailure}</ErrorNote>}
          {entries !== null && entries.length === 0 && (
            <Teach title="Nothing here.">
              This directory is empty, or everything in it is past the size the daemon will scan.
            </Teach>
          )}
          <ul className="tree">
            {(entries ?? []).map((entry) => (
              <li key={entry.name}>
                <button
                  type="button"
                  className={entry.is_dir ? "t-entry t-dir" : "t-entry"}
                  aria-current={openFile === joinPath(path, entry.name) ? "true" : undefined}
                  onClick={() => void open(entry)}
                >
                  <span className="t-icon">{entry.is_dir ? "▸" : "·"}</span>
                  {entry.name}
                </button>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
      <div className="stack">
        <Panel title={openFile ?? "No file open"} flat={openFile !== null}>
          {openFile === null ? (
            <Teach title="Open a file to read it.">
              The daemon reads it out of the project root and caps how much it will hand over, so a
              large file arrives truncated rather than not at all.
            </Teach>
          ) : reading ? (
            <p className="a-note">Reading…</p>
          ) : failed !== null ? (
            <ErrorNote>{failed}</ErrorNote>
          ) : (
            // Text, never markup: this is a file from a repository, which is content this shell did
            // not write and has no business executing.
            <pre className="file-body">{text}</pre>
          )}
        </Panel>
      </div>
    </div>
  );
}

interface SearchProps {
  token: string;
  projectId: string;
}

/** grep across the tree. The daemon walks it and caps both file size and result count. */
function Search({ token, projectId }: SearchProps) {
  const [q, setQ] = useState("");
  const [scope, setScope] = useState("");
  const [matches, setMatches] = useState<InspectMatch[] | null>(null);
  const [searchFailure, setSearchFailure] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);

  async function run() {
    setSearching(true);
    const result = await getProjectGrep(token, projectId, q.trim(), scope.trim());
    setSearching(false);
    setMatches(result.ok ? result.value : null);
    setSearchFailure(result.ok ? null : inspectFailure(result.status));
  }

  return (
    <Panel title="Search" aside={matches === null ? undefined : `${matches.length} matches`}>
      <form
        className="filters"
        onSubmit={(event) => {
          event.preventDefault();
          if (q.trim() === "" || searching) return;
          void run();
        }}
      >
        <label className="wide">
          Pattern
          <input value={q} placeholder="what to look for" onChange={(event) => setQ(event.target.value)} />
        </label>
        <label>
          Under
          <input value={scope} placeholder="(whole project)" onChange={(event) => setScope(event.target.value)} />
        </label>
        <div className="form-actions">
          <Button type="submit" disabled={q.trim() === "" || searching}>
            {searching ? "Searching…" : "Search"}
          </Button>
        </div>
      </form>
      {!searching && searchFailure !== null && <ErrorNote>{searchFailure}</ErrorNote>}
      {matches !== null && matches.length === 0 && (
        <Teach title="No matches.">
          The walk skips files past a size ceiling, so a hit inside a build artifact will not appear
          here — that ceiling is what keeps one large file from deciding the cost of a search.
        </Teach>
      )}
      <ul className="matches">
        {(matches ?? []).map((match, index) => (
          <li key={`${match.path}:${match.line}:${index}`}>
            <span className="m-path">{match.path}</span>
            <span className="m-line">{match.line}</span>
            <pre className="m-text">{match.text}</pre>
          </li>
        ))}
      </ul>
    </Panel>
  );
}

interface DiffProps {
  token: string;
  projectId: string;
}

/** What the project has that is not committed — the shape of a run's work, before it lands. */
function Diff({ token, projectId }: DiffProps) {
  const [diff, setDiff] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setFailed(null);
    const result = await getProjectDiff(token, projectId);
    setLoading(false);
    if (!result.ok) {
      setFailed(inspectFailure(result.status));
      setDiff(null);
      return;
    }
    setDiff(result.value);
  }, [projectId, token]);

  useEffect(() => { void load(); }, [load]);

  return (
    <Panel title="Uncommitted diff" aside={<Button size="sm" onClick={() => void load()}>Refresh</Button>}>
      {loading && <p className="a-note">Reading the working tree…</p>}
      {!loading && failed !== null && <ErrorNote>{failed}</ErrorNote>}
      {!loading && failed === null && (diff === null || diff.trim() === "") && (
        <Teach title="The working tree is clean.">
          Nothing is uncommitted. A run working in a worktree does not show up here — it has its own
          copy, which is the point of that mode.
        </Teach>
      )}
      {!loading && failed === null && diff !== null && diff.trim() !== "" && (
        <pre className="file-body diff-body">{diff}</pre>
      )}
    </Panel>
  );
}

interface ProjectsProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * The project inspector: read the tree, search it, and see what is uncommitted.
 *
 * Read-only by construction — the daemon exposes ls, cat, grep and diff and nothing that writes. It
 * exists so a proposal about a file can be checked against the file, without leaving for an editor
 * and losing the queue you were working through.
 */
/**
 * Whether the selected project can be inspected at all, right now.
 *
 * `gone` is the state that made this worth hoisting out of the views: a root is recorded when a
 * project enters shadow mode and never re-checked, so a root that was a temporary directory can
 * simply stop existing. The daemon then answers 404 to every inspect route, which — read from
 * inside a file browser — looks exactly like clicking a stale folder.
 */
type Reach =
  | { state: "checking" }
  | { state: "ok" }
  | { state: "gone" }
  | { state: "failed"; status: number };

function Projects({ token, connection }: ProjectsProps) {
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<View>("browse");
  const [loading, setLoading] = useState(true);
  const [reach, setReach] = useState<Reach>({ state: "checking" });

  useEffect(() => {
    if (token === null || connection !== "connected") {
      setProjects(null);
      setLoading(true);
      return;
    }
    let cancelled = false;
    void (async () => {
      const next = await getProjects(token);
      if (cancelled) return;
      setProjects(next);
      setLoading(false);
      // Pick a project rather than making an empty screen the default — but pick one that can
      // actually be read. A project in `off` mode stores no root (`autopilot.rs` only writes one
      // for shadow and active), so defaulting to the first row alphabetically lands on a dead
      // screen whenever the alphabet puts an off project first.
      setSelected((current) =>
        current ?? next?.find((project) => project.project_root !== null)?.project_id
          ?? next?.[0]?.project_id ?? null,
      );
    })();
    return () => { cancelled = true; };
  }, [connection, token]);

  const selectedProject =
    projects?.find((project) => project.project_id === selected) ?? null;
  const readable = (projects ?? []).filter((project) => project.project_root !== null).length;
  const recordedRoot = selectedProject?.project_root ?? null;

  /**
   * One listing of the root decides whether the three views are worth showing.
   *
   * Hoisted here rather than left to each view, because otherwise browse, search and diff each
   * discover the same dead root separately and each phrases it as its own local failure — and the
   * one that discovers it first is whichever tab happens to be open.
   */
  useEffect(() => {
    if (token === null || selected === null || recordedRoot === null) return;
    let cancelled = false;
    setReach({ state: "checking" });
    void (async () => {
      const result = await getProjectLs(token, selected, "");
      if (cancelled) return;
      setReach(
        result.ok
          ? { state: "ok" }
          : result.status === 404
            ? { state: "gone" }
            : { state: "failed", status: result.status },
      );
    })();
    return () => { cancelled = true; };
  }, [recordedRoot, selected, token]);

  // Below every hook: an early return above them would make the set of hooks this component runs
  // depend on the connection, which React forbids.
  if (connection !== "connected" || token === null) {
    return (
      <section className="projects-page">
        <Teach title="The inspector is waiting for the daemon.">
          Connect to the daemon to read a project&apos;s tree. Files are read through the núcleo, so
          the shell never touches the disk itself.
        </Teach>
      </section>
    );
  }

  return (
    <section className="projects-page">
      <h1 className="headline">
        {selected === null
          ? <>No project to inspect yet.</>
          // Not "Reading X" when there is nothing to read — the headline should not assert
          // something the panel below it is about to contradict.
          : selectedProject?.project_root === null
            ? <><em>{selected}</em> is off, so there is nothing to read.</>
            : reach.state === "gone"
              ? <><em>{selected}</em> points at a root that is gone.</>
              : <>Reading <em>{selected}</em>.</>}
      </h1>
      <div className="statusline">
        <span>{projects?.length ?? 0} projects · {readable} readable</span>
        <span>read-only · <b>ls, cat, grep, diff</b></span>
      </div>
      {loading && projects === null && <p className="a-note">Loading…</p>}
      {!loading && projects === null && (
        <ErrorNote>Could not load projects from the daemon.</ErrorNote>
      )}
      {projects !== null && projects.length === 0 && (
        <Teach title="No projects yet.">
          A project appears here once autopilot knows about it. Bring one aboard in shadow mode
          first — the inspector reads the root that step records.
        </Teach>
      )}
      {projects !== null && projects.length > 0 && (
        <>
          <div className="picker">
            {projects.map((project) => (
              <button
                type="button"
                key={project.project_id}
                className={project.project_root === null ? "pick pick-rootless" : "pick"}
                aria-current={selected === project.project_id ? "true" : undefined}
                title={project.project_root ?? "No root recorded — nothing to inspect."}
                onClick={() => setSelected(project.project_id)}
              >
                {project.project_id}
                <Badge tone={project.mode === "active" ? "active" : project.mode === "shadow" ? "shadow" : "off"}>
                  {project.mode}
                </Badge>
              </button>
            ))}
          </div>
          {/* A project with no root is a KNOWN state, not a failure, and the roster already says
              which one it is — so this is decided here rather than by firing four requests that
              can only come back 404 and then guessing at the reason from the status code. */}
          {selectedProject !== null && selectedProject.project_root === null ? (
            <Teach title={`${selectedProject.project_id} has no root to read.`}>
              A root is recorded when a project is put into shadow or active mode, and cleared when
              it goes back to off — so an off project has no tree for the inspector to open. Put it
              in shadow mode from Autopilot and it becomes readable here.
            </Teach>
          ) : reach.state === "gone" ? (
            // A recorded root that is not on disk. Named separately from "not found" because the
            // fix is not to click elsewhere — the project is pointing at a path that is gone, and
            // only Autopilot can repoint it.
            <Teach title="The root recorded for this project is no longer on disk.">
              <code className="dead-root">{recordedRoot}</code>
              The root is recorded once, when the project enters shadow mode, and never re-checked —
              so a root under a temporary directory stops existing the moment that directory is
              cleaned up. Nothing here can repoint it: set the project to off, then back to shadow
              from Autopilot with a root that exists.
            </Teach>
          ) : reach.state === "failed" ? (
            <ErrorNote>
              The daemon refused to read this project&apos;s root ({reach.status}).
            </ErrorNote>
          ) : reach.state === "checking" ? (
            <p className="a-note">Checking the project root…</p>
          ) : selected !== null && (
            <>
              <nav className="subnav" aria-label="Inspector views">
                {(["browse", "search", "diff"] as View[]).map((option) => (
                  <button
                    type="button"
                    key={option}
                    className="subtab"
                    aria-current={view === option ? "page" : undefined}
                    onClick={() => setView(option)}
                  >
                    {option}
                  </button>
                ))}
              </nav>
              {/* Keyed by project so switching projects remounts each view with its own state
                  rather than carrying a path or a search across the boundary. */}
              {view === "browse" && <Browser key={selected} token={token} projectId={selected} />}
              {view === "search" && <Search key={selected} token={token} projectId={selected} />}
              {view === "diff" && <Diff key={selected} token={token} projectId={selected} />}
            </>
          )}
        </>
      )}
    </section>
  );
}

export default Projects;
