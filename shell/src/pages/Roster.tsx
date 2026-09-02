import { useState } from "react";
import { Link } from "@tanstack/react-router";

import { isApiRefusal } from "../data/client";
import { folderOf, gateOf, headline, inAttentionOrder, type Folder, type Gate } from "../data/roster";
import { useProjects, type ProjectSummary } from "../data/system";
import { Badge, type BadgeTone } from "../ui/Badge";
import { Button } from "../ui/Button";
import { ErrorNote } from "../ui/ErrorNote";
import { PageHeader } from "../ui/PageHeader";
import { RefusalNote } from "../ui/RefusalNote";
import { StaleNote } from "../ui/StaleNote";
import { RemoveProject } from "./RemoveProject";

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
 * **The columns are the readings that differ between projects**, and nothing that does not — four
 * of them, down from seven, because fourteen of the twenty-four cells that were not a name said
 * nothing at all. What is waiting, what the gate last said and whether the folder is still there
 * are the three that decide whether a project needs somebody today, and nothing else on the page
 * answered that question.
 *
 * The table had already written the rule down and half-applied it: *a column of identical badges is
 * a column of noise*, so `Mode` drew the word `shadow` in grey. With twenty-four of twenty-five
 * saying shadow, the whole column was that — so the mode stops being a column and becomes a mark on
 * the name, drawn only when it departs from the default. `Folder` had exactly the same problem —
 * `ok`, three times out of four — and keeps its column because the other two states are the reason
 * the column exists.
 *
 * **Two columns left for a reason stronger than being empty: they are already somewhere better.**
 * The shadow-exit bar and the ceiling are both drawn in the project's own Settings panel, with room
 * for the sentence that makes them mean something. Here they were jargon in a narrow column, blank
 * three times out of four, and neither answers *does this one need me today*.
 *
 * **And a project can leave.** One could be added and none could go, so this page only ever grew: a
 * folder somebody moved, a repository they finished with, a project added to try something once —
 * all of them still here, still polled, still ordered by trouble. `remove` is on the row because
 * the row is where a project is compared with the others, which is where somebody realises they are
 * done with one. It touches nothing on a disk, and the panel says so before it offers the button.
 * Deleting a folder is a second act and is not on this page at all: it belongs inside the project,
 * reached by somebody who is already there.
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

/** How many columns a row spans, for the panel that opens underneath one. */
const COLUMNS = 5;

function Table({ rows }: { rows: ProjectSummary[] }) {
  /*
    One open at a time, held here rather than in each row. Two remove panels open at once would put
    two folder paths and two counts on screen with one `remove` button each, which is how somebody
    removes the project they were reading about rather than the one they meant.
  */
  const [leaving, setLeaving] = useState<string | null>(null);

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border text-left text-xs uppercase tracking-wide text-text-faint">
            <th className="px-3 py-2 font-medium">Project</th>
            <th className="px-3 py-2 text-right font-medium">Waiting</th>
            <th className="px-3 py-2 font-medium">Gate</th>
            <th className="px-3 py-2 font-medium">Folder</th>
            {/*
              The slack goes here, and that is a reading decision rather than a layout one. Four
              columns in a full-width table get spread evenly, which put half a screen between a
              project's name and the number waiting on it — the eye had to travel further to read
              four facts than it did to read seven. Everything that answers *does this need me*
              packs at the left where it can be read in one movement, and the way out sits at the
              far margin, which is also where it is hardest to hit by accident.
            */}
            <th className="w-full px-3 py-2 text-right font-medium">
              <span className="sr-only">Leaving</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {inAttentionOrder(rows).map((project) => (
            <Row
              key={project.project_id}
              project={project}
              leaving={leaving === project.project_id}
              onLeaving={(open) => setLeaving(open ? project.project_id : null)}
            />
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

function Row({
  project,
  leaving,
  onLeaving,
}: {
  project: ProjectSummary;
  leaving: boolean;
  onLeaving: (open: boolean) => void;
}) {
  const folder = folderOf(project);
  const gate = gateOf(project);

  return (
    <>
      <tr className="border-b border-border last:border-b-0 align-middle">
        <th scope="row" className="px-3 py-2 text-left font-normal">
          {/*
            Into the workspace, and specifically into Estado: it is the mode that answers the
            question somebody arrives with. The inspector this page used to open into is a different
            question, reached from inside the Código mode.
          */}
          <span className="inline-flex items-baseline gap-2">
            <Link
              className="underline underline-offset-2"
              to="/projects/$projectId/$view"
              params={{ projectId: project.project_id, view: "state" }}
            >
              {project.project_id}
            </Link>
            {/*
              The mode, as a mark on the name rather than a column of its own. The table already
              wrote this rule down for one case — a column of identical badges is a column of
              noise, so `shadow` was drawn in grey — and with twenty-four of twenty-five saying
              shadow the whole column was that. What is left is what departs from the default,
              beside the name it is a fact about.
            */}
            {project.mode !== "shadow" && (
              <Badge tone={project.mode === "active" ? "active" : "off"}>{project.mode}</Badge>
            )}
          </span>
        </th>

        <td className="px-3 py-2 text-right tabular-nums">
          {project.open_proposals === 0 ? (
            <Nothing />
          ) : (
            <Link
              className="underline underline-offset-2"
              to="/projects/$projectId/$view"
              params={{ projectId: project.project_id, view: "state" }}
            >
              {project.open_proposals}
            </Link>
          )}
        </td>

        <td className="px-3 py-2">
          <GateCell gate={gate} at={project.last_gate_at ?? null} />
        </td>

        <td className="px-3 py-2 whitespace-nowrap">
          <FolderCell folder={folder} root={project.project_root} off={project.mode === "off"} />
        </td>

        <td className="px-3 py-2 text-right">
          {/*
            The way out, on the row where a project is compared with the others — which is where
            somebody realises they are done with one. It is a plain link-weight control and not a red
            button: nothing on a disk moves, and colouring it as destruction would make the act that
            IS destruction, inside the project, have nothing louder left to be.
          */}
          <Button variant="quiet" aria-expanded={leaving} onClick={() => onLeaving(!leaving)}>
            {leaving ? "keep" : "remove"}
          </Button>
        </td>
      </tr>

      {leaving && (
        <tr className="border-b border-border last:border-b-0">
          <td colSpan={COLUMNS} className="px-3 pb-3">
            <RemoveProject
              projectId={project.project_id}
              projectRoot={project.project_root}
              onDone={() => onLeaving(false)}
              onCancel={() => onLeaving(false)}
            />
          </td>
        </tr>
      )}
    </>
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

/**
 * Where the project's folder is, and how loudly to say it.
 *
 * `off` is not decoration on that second question, it is the whole of it. A project nobody has
 * pointed anywhere is a real gap while the project is meant to be doing something, and is simply
 * what switched off looks like otherwise — `rankOf` already stopped treating the two alike, and a
 * badge here on a dormant project would go on shouting the thing the order stopped shouting. Said
 * either way, in the same words; coloured only when it is a fault.
 *
 * A folder that is *gone* stays a fault whatever the mode: it was named, something moved it, and
 * that is a fact about a disk rather than about the autopilot.
 */
function FolderCell({
  folder,
  root,
  off,
}: {
  folder: Folder;
  root: string | null;
  off: boolean;
}) {
  if (folder === "ok") {
    return (
      <span className="text-text-faint" title={root ?? undefined}>
        ok
      </span>
    );
  }
  if (folder === "unset" && off) {
    return (
      <span className="text-text-faint" title="this project is switched off and has no folder">
        not named
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
