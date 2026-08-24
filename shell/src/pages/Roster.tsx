import { Link } from "@tanstack/react-router";

import { isApiRefusal } from "../data/client";
import { folderOf, gateOf, headline, inAttentionOrder, type Folder, type Gate } from "../data/roster";
import { useProjects, type ProjectSummary } from "../data/system";
import { Badge, type BadgeTone } from "../ui/Badge";
import { ErrorNote } from "../ui/ErrorNote";
import { PageHeader } from "../ui/PageHeader";
import { RefusalNote } from "../ui/RefusalNote";
import { StaleNote } from "../ui/StaleNote";

/**
 * Every project the núcleo knows, and how each one is doing.
 *
 * **A table, and its own page.** What was here was a wall of chips — one pill per project, each
 * repeating the word `shadow` that twenty-four of twenty-five share, with the two readings that
 * actually differ buried among them — and underneath it, on the same screen, a file browser for
 * whichever project you had clicked. Two jobs on one page, and the one people came for was the
 * loser.
 *
 * So the inspector kept `/projects/{id}/inspect/{view}` and this kept `/projects`. The roster
 * answers *how are they all doing* and the workspace answers *what is happening in this one*,
 * which is why a row leads to `estado` and not back into a file tree.
 *
 * **The columns are the readings that differ between projects**, and nothing that does not. The
 * mode is here because one row in twenty-five says `active` and that is worth finding; it is drawn
 * as a badge only when it is not the shadow everything else is, because a column of identical
 * badges is a column of noise. What is waiting, what the gate last said and whether the folder is
 * still there are the three that decide whether a project needs somebody today.
 */
export function Roster() {
  const projects = useProjects();
  const rows = projects.data ?? [];
  const stale = projects.isError && projects.data !== undefined;

  return (
    <>
      <PageHeader
        title="Projects"
        headline={projects.data === undefined ? undefined : headline(rows)}
      />

      {/*
        The door to adding one, kept where it was: the rail is the design's fixed list of places,
        and this is an action taken from the list of what exists.
      */}
      <p className="mb-4">
        <Link className="text-sm underline underline-offset-2" to="/projects/new">
          Add a project…
        </Link>
      </p>

      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {projects.isError && projects.data === undefined && <RosterError error={projects.error} />}

      {projects.data === undefined ? (
        <p className="text-sm text-text-faint">reading the roster…</p>
      ) : rows.length === 0 ? (
        <p className="text-sm text-text-muted">
          No project has been registered with the núcleo. Adding one takes a folder and three
          questions.
        </p>
      ) : (
        <Table rows={rows} />
      )}
    </>
  );
}

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the roster</ErrorNote>;
}

/* --------------------------------------------------------------- the table -- */

function Table({ rows }: { rows: ProjectSummary[] }) {
  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border text-left text-xs uppercase tracking-wide text-text-faint">
            <th className="px-3 py-2 font-medium">Project</th>
            <th className="px-3 py-2 font-medium">Mode</th>
            <th className="px-3 py-2 text-right font-medium">Waiting</th>
            <th className="px-3 py-2 font-medium">Gate</th>
            <th className="px-3 py-2 font-medium">Folder</th>
            <th className="px-3 py-2 font-medium">Shadow bar</th>
            <th className="px-3 py-2 text-right font-medium">Ceiling</th>
          </tr>
        </thead>
        <tbody>
          {inAttentionOrder(rows).map((project) => (
            <Row key={project.project_id} project={project} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** A cell with nothing to say, drawn the same way everywhere so absence is legible as absence. */
function Nothing() {
  return <span className="text-text-faint">—</span>;
}

function Row({ project }: { project: ProjectSummary }) {
  const folder = folderOf(project);
  const gate = gateOf(project);

  return (
    <tr className="border-b border-border last:border-b-0 align-middle">
      <th scope="row" className="px-3 py-2 text-left font-normal">
        {/*
          Into the workspace, and specifically into Estado: it is the mode that answers the
          question somebody arrives with. The inspector this page used to open into is a different
          question, reached from inside the Código mode.
        */}
        <Link
          className="underline underline-offset-2"
          to="/projects/$projectId/$view"
          params={{ projectId: project.project_id, view: "estado" }}
        >
          {project.project_id}
        </Link>
      </th>

      <td className="px-3 py-2">
        {/*
          Only when it is not the shadow that everything else is. Twenty-four identical badges
          teach an eye to skip the column that holds the one that is not.
        */}
        {project.mode === "shadow" ? (
          <span className="text-text-faint">shadow</span>
        ) : (
          <Badge tone={project.mode === "active" ? "active" : "off"}>{project.mode}</Badge>
        )}
      </td>

      <td className="px-3 py-2 text-right tabular-nums">
        {project.open_proposals === 0 ? (
          <Nothing />
        ) : (
          <Link
            className="underline underline-offset-2"
            to="/projects/$projectId/$view"
            params={{ projectId: project.project_id, view: "estado" }}
          >
            {project.open_proposals}
          </Link>
        )}
      </td>

      <td className="px-3 py-2">
        <GateCell gate={gate} at={project.last_gate_at ?? null} />
      </td>

      <td className="px-3 py-2">
        <FolderCell folder={folder} root={project.project_root} />
      </td>

      <td className="px-3 py-2 tabular-nums">
        {/*
          The shadow-exit bar, which is what says whether this project could be trusted to act. Not
          drawn for a project already acting: it is the criterion for a decision that has been
          taken, and repeating it there reads as if the promotion could still be refused.
        */}
        {project.mode === "active" ? (
          <Nothing />
        ) : project.classes_total === 0 ? (
          <span className="text-text-faint" title="nothing has been exercised yet">
            —
          </span>
        ) : (
          <span className={project.promotable ? "text-tone-active-fg" : undefined}>
            {project.classes_ready}/{project.classes_total}
          </span>
        )}
      </td>

      <td className="px-3 py-2 text-right tabular-nums">
        {project.wip_limit === null ? (
          <span className="text-text-faint" title="no ceiling — the brake is off">
            off
          </span>
        ) : project.queue_full ? (
          <Badge tone="paused" title="new work is being deferred until something is reviewed">
            {project.wip_limit} full
          </Badge>
        ) : (
          project.wip_limit
        )}
      </td>
    </tr>
  );
}

/**
 * The four things a gate can have said, in four tones and four words.
 *
 * `errored` is amber and not red on purpose: the gate could not run — a missing command, a worktree
 * that had gone — and that says nothing at all about the code. Drawing it as a failure sends
 * somebody to read a diff when the thing to fix is a path.
 */
const GATE_TONE: Record<Exclude<Gate, "none">, BadgeTone> = {
  passed: "active",
  failed: "danger",
  errored: "paused",
};

function GateCell({ gate, at }: { gate: Gate; at: string | null }) {
  if (gate === "none") {
    return (
      <span className="text-text-faint" title="this project has no gate command, so nothing ran">
        —
      </span>
    );
  }
  return (
    <Badge tone={GATE_TONE[gate]} title={at === null ? undefined : `last run ${at}`}>
      {gate}
    </Badge>
  );
}

const FOLDER_WORD: Record<Folder, string> = {
  ok: "ok",
  missing: "gone",
  unset: "not named",
};

function FolderCell({ folder, root }: { folder: Folder; root: string | null }) {
  if (folder === "ok") {
    return (
      <span className="text-text-faint" title={root ?? undefined}>
        ok
      </span>
    );
  }
  return (
    <Badge
      tone={folder === "missing" ? "danger" : "off"}
      title={
        folder === "missing"
          ? `${root} is not on this disk`
          : "no folder has been named for this project"
      }
    >
      {FOLDER_WORD[folder]}
    </Badge>
  );
}
