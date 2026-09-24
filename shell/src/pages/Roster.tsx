import { useEffect, useId, useRef, useState } from "react";
import { Link } from "@tanstack/react-router";

import { isApiRefusal } from "../data/client";
import { folderOf, gateOf, headline, inAttentionOrder, type Folder, type Gate } from "../data/roster";
import { useProjects, type ProjectSummary } from "../data/system";
import { UI_LOCALE } from "../lib/locale";
import { Badge } from "../ui/Badge";
import { StateBadge } from "../ui";
import { readState } from "../ui/state-map";
import { Button } from "../ui/Button";
import { Count } from "../ui/Count";
import { ErrorNote } from "../ui/ErrorNote";
import { PageHeader } from "../ui/PageHeader";
import { Quiet } from "../ui/Quiet";
import { RefusalNote } from "../ui/RefusalNote";
import { StaleNote } from "../ui/StaleNote";
import { Teach } from "../ui/Teach";
import { RemoveProject, type Removed } from "./RemoveProject";

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
 * **And no strip of stat cards above the table.** There were four — projects, acting, failing the
 * gate, to review — and every figure in them was already in the headline one line up, which only
 * ever names the facts that are not zero. The strip said them again in four equal boxes, so the
 * largest mass on the page was a repetition, and "4 projects on the roster" weighed exactly what "1
 * failing the gate" weighed. That is the grid of identical cards DESIGN.md names as an
 * anti-reference, and it undid principle 2 — *exceptions dominate; the normal recedes* — which the
 * ORDER of the table had already got right. Keeping only the non-zero exception cards was the other
 * option and was not taken: it would put the same number on screen twice, one of them in a box,
 * for a page whose headline and first row already say who needs somebody.
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

  /*
    The last project that left, so the page can say so. Without it a removal ended in silence: the
    panel shut, the row went on the next poll and focus fell to the body — a deliberate act with no
    answer, which is the thing somebody remembers about it.

    Held here and not in the table, because removing the LAST project unmounts the table and draws
    the empty state instead, and that is exactly the removal that most needs acknowledging.
  */
  const [left, setLeft] = useState<Removed | null>(null);
  // Said only once the roster agrees: before the refetch lands the row is still drawn, and "bravo
  // left the roster" above a row called bravo is two claims that cannot both be true. And a project
  // added back under the same name retires the line on its own.
  const gone = left !== null && !rows.some((row) => row.project_id === left.projectId) ? left : null;

  return (
    <>
      <PageHeader
        title="Projects"
        headline={
          projects.data === undefined
            ? undefined
            : stale
              ? `${asOf(projects.dataUpdatedAt)}${headline(rows)}`
              : headline(rows)
        }
        /*
          The door to adding one, in the header's own slot. It was a line of its own under
          the header — a paragraph containing one link, costing a row of the page — and the
          header has had a place for exactly this on every other screen in the app.
        */
        actions={
          <Link className="text-sm" to="/projects/new">
            Add a project…
          </Link>
        }
      />

      {/*
        First thing under the headline, because every figure in the headline and every row below is
        the last good read rather than the current one. It used to sit under the stat strip, in the
        faintest register the system has, so the eye had read "1 failing the gate" at full weight
        before it could reach the sentence that said the number was old.
      */}
      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {projects.isError && projects.data === undefined && <RosterError error={projects.error} />}

      {gone !== null && <LeftTheRoster removed={gone} />}

      {projects.data === undefined ? (
        <p className="text-sm text-text-faint">reading the roster…</p>
      ) : rows.length === 0 ? (
        <Teach
          title="No project has been registered with the núcleo"
          action={<Link to="/projects/new">Add a project…</Link>}
        >
          Adding one takes a folder and three questions.
        </Teach>
      ) : (
        <Table rows={rows} stale={stale} onRemoved={setLeft} />
      )}
    </>
  );
}

/**
 * The headline's prefix while the roster is stale — "as of 14:02:11 — ".
 *
 * The same clock `StaleNote` prints, so the two agree to the second. A roster that has never been
 * read successfully has no data to be stale about, so the zero case is only a guard.
 */
function asOf(dataUpdatedAt: number): string {
  if (dataUpdatedAt <= 0) return "last known — ";
  return `as of ${new Date(dataUpdatedAt).toTimeString().slice(0, 8)} — `;
}

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the roster</ErrorNote>;
}

/**
 * The answer to a removal, where the row was.
 *
 * `Quiet` and announced: it is one line, it is the reply to something the reader just did, and it
 * carries the one gesture that undoes it. Focus lands on it, because the button that was pressed
 * went away with the panel it was in, and a focus that falls to the body loses somebody's place in
 * the table.
 *
 * "add it back" is a plain link to `/projects/new`. That page does not read a folder from the URL
 * yet, so the path is said here in words rather than handed over — see the report this change was
 * filed with.
 */
function LeftTheRoster({ removed }: { removed: Removed }) {
  const line = useRef<HTMLDivElement>(null);
  useEffect(() => {
    line.current?.focus();
  }, [removed]);

  const where =
    removed.projectRoot === null ? "it had no folder named" : `its folder is still at ${removed.projectRoot}`;
  const says = removed.forgot
    ? `${removed.projectId} left the roster and its history was deleted — ${where}.`
    : `${removed.projectId} left the roster — ${where}, and its history is kept.`;

  return (
    <div ref={line} tabIndex={-1} className="mb-4">
      <Quiet announce says={says} action={<Link to="/projects/new">add it back</Link>} />
    </div>
  );
}

/* --------------------------------------------------------------- the table -- */

/** How many columns a row spans, for the panel that opens underneath one. */
const COLUMNS = 5;

function Table({
  rows,
  stale,
  onRemoved,
}: {
  rows: ProjectSummary[];
  stale: boolean;
  onRemoved: (removed: Removed) => void;
}) {
  /*
    One open at a time, held here rather than in each row. Two remove panels open at once would put
    two folder paths and two counts on screen with one `remove` button each, which is how somebody
    removes the project they were reading about rather than the one they meant.
  */
  const [leaving, setLeaving] = useState<string | null>(null);

  /*
    A stale roster closes the panel rather than hiding it. The counts and the holds in it were read
    against a roster nobody can vouch for now, and a panel that came back by itself when the poll
    recovered would be offering a decision somebody had stopped looking at. Adjusted during render,
    which is React's own answer for state that follows a prop, rather than an effect that would
    draw the stale panel once before closing it.
  */
  if (stale && leaving !== null) setLeaving(null);

  /*
    A surface in the page column. The header and table share one right edge so the roster reads as
    one page. `--width-column` is reserved for prose columns, where a narrower reading measure helps.

    Muted while stale: the values recede to the register of a thing remembered, and the note above
    says how old they are. Badges keep their own tones — a failed gate an hour ago was still a
    failed gate.
  */
  return (
    <div
      className={`overflow-x-auto rounded-lg border border-border bg-surface${stale ? " text-text-muted" : ""}`}
    >
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border text-left text-xs uppercase tracking-wide text-text-faint">
            <th className="px-3 py-2 font-medium">Project</th>
            {/*
              `To review`, the headline's own words for the same number. The column said `Waiting`
              and the headline said "items to review" about one figure, while the rail's `Waiting`
              counts something else again — three names across two numbers.
            */}
            <th className="whitespace-nowrap px-3 py-2 text-right font-medium">To review</th>
            <th className="px-3 py-2 font-medium">Gate</th>
            <th className="px-3 py-2 font-medium">Folder</th>
            {/*
              The slack goes here, and that is a reading decision rather than a layout one. Four
              columns in a full-width table get spread evenly, which put half a screen between a
              project's name and the number waiting on it — the eye had to travel further to read
              four facts than it did to read seven. Everything that answers *does this need me*
              packs at the left where it can be read in one movement, and the way out sits at the
              far margin, which is also where it is hardest to hit by accident.

              The heading stays while the view is stale and the controls under it go, so the
              columns do not reflow under somebody's eye when a poll fails.
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
              stale={stale}
              leaving={leaving === project.project_id}
              onLeaving={(open) => setLeaving(open ? project.project_id : null)}
              onRemoved={onRemoved}
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
  stale,
  leaving,
  onLeaving,
  onRemoved,
}: {
  project: ProjectSummary;
  stale: boolean;
  leaving: boolean;
  onLeaving: (open: boolean) => void;
  onRemoved: (removed: Removed) => void;
}) {
  const folder = folderOf(project);
  const gate = gateOf(project);
  const panelId = useId();
  /*
    The cell and not the button: `Button` does not take a ref, and the cell holds exactly one
    control. Focus goes back here when the panel closes by any road — `cancel`, Escape, or a
    removal the núcleo accepted — because a panel that unmounts with focus inside it drops focus
    to the body, and somebody on a keyboard loses their place in the table.
  */
  const toggleCell = useRef<HTMLTableCellElement>(null);
  const closePanel = () => {
    onLeaving(false);
    toggleCell.current?.querySelector("button")?.focus();
  };

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
            {/* Not underlined at rest. Twenty-five underlined names down a column is a
                column of rules, and the underline is telling somebody something they
                already know — every name in a roster is the way into that project.
                `base.css` puts it back on hover, which is where it answers a question. */}
            <Link
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
              <StateBadge domain="autopilot" state={project.mode} />
            )}
          </span>
        </th>

        <td className="px-3 py-2 text-right">
          {project.open_review_items === 0 ? (
            <Nothing />
          ) : (
            /* `Count` and not a bare number: this is how many things are in a list, which
               is the one reading the design system already draws — mono and tabular, so a
               column of them lines up on the digit rather than wobbling. */
            <Link
              to="/projects/$projectId/$view"
              params={{ projectId: project.project_id, view: "state" }}
            >
              <Count n={project.open_review_items} />
            </Link>
          )}
        </td>

        <td className="px-3 py-2">
          <GateCell gate={gate} at={project.last_gate_at ?? null} />
        </td>

        <td className="px-3 py-2 whitespace-nowrap">
          <FolderCell folder={folder} root={project.project_root} off={project.mode === "off"} />
        </td>

        <td ref={toggleCell} className="px-3 py-2 text-right">
          {/*
            The way out, on the row where a project is compared with the others — which is where
            somebody realises they are done with one. It is a plain link-weight control and not a red
            button: nothing on a disk moves, and colouring it as destruction would make the act that
            IS destruction, inside the project, have nothing louder left to be.

            It says what pressing it does NOW. While the panel is open that is closing it, so it reads
            `cancel` — the panel's own word for the same act. It used to say `keep` (a second exit
            word beside `cancel`), and before that the fix was to leave it saying `remove`, which put
            two `remove` buttons on screen with opposite effects: this one closed the panel, the one
            inside it took the project off the roster. Two buttons with one name and one effect are
            redundant; two with one name and opposite effects are a trap.

            Gone, not disabled, while the view is stale: the remove would act on a roster nobody can
            vouch for, and the note above the headline is what explains where it went.
          */}
          {!stale && (
            <Button
              variant="quiet"
              aria-expanded={leaving}
              aria-controls={leaving ? panelId : undefined}
              onClick={() => onLeaving(!leaving)}
            >
              {leaving ? "cancel" : "remove"}
            </Button>
          )}
        </td>
      </tr>

      {leaving && (
        <tr className="border-b border-border last:border-b-0">
          <td colSpan={COLUMNS} className="px-3 pb-3">
            <RemoveProject
              id={panelId}
              projectId={project.project_id}
              projectRoot={project.project_root}
              onDone={(removed) => {
                closePanel();
                onRemoved(removed);
              }}
              onCancel={closePanel}
            />
          </td>
        </tr>
      )}
    </>
  );
}

/** A gate run's time, in the one locale the window formats dates in. */
const GATE_AT = new Intl.DateTimeFormat(UI_LOCALE, { dateStyle: "medium", timeStyle: "short" });

function lastRun(at: string): string {
  const when = new Date(at);
  // A timestamp this shell cannot parse is still a fact; said raw rather than as "Invalid Date".
  return `last run ${Number.isNaN(when.getTime()) ? at : GATE_AT.format(when)}`;
}

/**
 * The four things a gate can have said, in the map's tones and the map's words.
 *
 * `errored` is info-blue and not red on purpose: the gate could not run — a missing command, a
 * worktree that had gone — and that says nothing at all about the code. Drawing it as a failure
 * sends somebody to read a diff when the thing to fix is a path. The badge says "gate not measured",
 * the map's label, rather than the wire's `errored`, which is exactly the word that reads as a
 * failure.
 */
function GateCell({ gate, at }: { gate: Gate; at: string | null }) {
  if (gate === "none") {
    // The map's own sentence for an absent gate, said to a screen reader as well as in the title:
    // a lone dash is not something a keyboard can hover.
    const absent = readState("gate", null)?.label ?? "no gate reading";
    return (
      <span className="text-text-faint" title={absent}>
        <span aria-hidden="true">—</span>
        <span className="sr-only">{absent}</span>
      </span>
    );
  }
  const reading = readState("gate", gate);
  if (reading === null) return <span className="text-text-faint">{gate}</span>;
  return (
    <Badge tone={reading.tone} title={at === null ? undefined : lastRun(at)}>
      {reading.label}
    </Badge>
  );
}

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
 *
 * The words come from the map even where the tone does not: the dormant case renders
 * `readState("folder", "unset")`'s label as plain text. `ok` has no map row: a healthy folder is
 * the absence of a fact, and the old row was a reading nothing rendered.
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
  const reading = readState("folder", folder);
  if (folder === "unset" && off) {
    return (
      <span className="text-text-faint" title="this project is switched off and has no folder">
        {reading === null ? folder : reading.label}
      </span>
    );
  }
  if (reading === null) return <span className="text-text-faint">{folder}</span>;
  return (
    <Badge
      tone={reading.tone}
      title={
        folder === "missing"
          ? `${root} is not on this disk`
          : "no folder has been named for this project"
      }
    >
      {reading.label}
    </Badge>
  );
}
