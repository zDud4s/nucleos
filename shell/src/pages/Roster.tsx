import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { ArrowRight } from "lucide-react";

import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import { useLiveJobs, useLiveRuns } from "../data/fleet";
import { folderOf, gateOf, headline, inAttentionOrder, whereWaiting } from "../data/roster";
import { useProjects, type ProjectSummary } from "../data/system";
import { UI_LOCALE } from "../lib/locale";
import { Count } from "../ui/Count";
import { ErrorNote } from "../ui/ErrorNote";
import { ListFilter } from "../ui/ListFilter";
import { PageHeader } from "../ui/PageHeader";
import { Quiet } from "../ui/Quiet";
import { RefusalNote } from "../ui/RefusalNote";
import { SectionTitle } from "../ui/SectionTitle";
import { Skeleton } from "../ui/Skeleton";
import { StaleNote } from "../ui/StaleNote";
import { Teach } from "../ui/Teach";
import { readState } from "../ui/state-map";
import type { Removed } from "./RemoveProject";
import { plural } from "../lib/plural";

/**
 * Every project the núcleo knows, sorted by what it asks of somebody.
 *
 * **Three groups, one grid.** The order has always been the page's answer — what needs somebody
 * first — and the groups say where the trouble ends:
 *
 * - **Needs you** — a project that is meant to be doing something and cannot, or is waiting on a
 *   decision. On a raised panel, in the tone of the fact that put it there.
 * - **Working** — nothing asked of anybody, but work is in flight. The same panel, so the live
 *   count reads against the ones that need somebody.
 * - **Quiet** — everything else, with no panel, muted text and the mode dot as its only tone,
 *   because a quiet project is looked up by name rather than read about. Switched-off projects live
 *   here whatever their folder says: `rankOf` already stopped shouting about them, and a dormant
 *   project in *Needs you* would teach people that the group can be ignored.
 *
 * Every group is rows on the SAME columns, so one project is compared with another by running an
 * eye down a column rather than by reading a card. It is a list of links and not a `table`: a row
 * is one link to the project, and a link cannot sit inside a table row's cells without the row
 * itself being the target. The column labels sit on the first group's title line.
 *
 * A group with nothing in it is not drawn — an empty *Needs you* heading is a sentence about
 * nothing, and the headline already says when nothing is wrong by not naming anything.
 *
 * Every row opens the project in its workspace, on State: it is the mode that answers the question
 * somebody arrives with.
 *
 * **And removing a project is not on this page any more.** It used to be a `remove` on every row,
 * on the argument that the roster is where somebody compares projects and realises they are done
 * with one. A row that is one link has no room for a second control, and the stronger argument was
 * always the one `DeleteFolder` makes: the way out of a project belongs inside it, reached by
 * somebody who has already opened the one they mean. It lives in State's *Leaving* section now,
 * and the acknowledgement still lands here, because this is where the project is missing from.
 */
export function Roster() {
  const projects = useProjects();
  const rows = projects.data ?? [];
  const stale = projects.isError && projects.data !== undefined;

  /*
    The removal this page was navigated here to acknowledge, from the history entry's state (see
    `leftTheRoster` in `RemoveProject.tsx` for why it travels that way). Said only once the roster
    agrees: before the refetch lands the project is still drawn, and "bravo left the roster" above
    a card called bravo is two claims that cannot both be true. A project added back under the same
    name retires the line on its own.
  */
  const left = useRouterState({ select: (state) => state.location.state.leftTheRoster });
  const gone =
    left !== undefined && projects.data !== undefined && !rows.some((row) => row.project_id === left.projectId)
      ? left
      : null;

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
        actions={
          // A button by look, a link by nature: it goes somewhere, so it stays an `<a href>` the
          // keyboard and a middle-click both understand, and wears the same ghost-go recipe as
          // New agent and New team so the three headers read alike.
          <Link className="ui-button ui-button-ghost ui-button-go" to="/projects/new">
            New project
          </Link>
        }
      />

      {/*
        First thing under the headline, because every figure in the headline and every card below is
        the last good read rather than the current one.
      */}
      {stale && <StaleNote dataUpdatedAt={projects.dataUpdatedAt} />}
      {gone !== null && <LeftTheRoster removed={gone} />}

      {/*
        Three explicit branches, never two at once: a roster that failed before it ever answered
        used to draw the error AND "reading the roster…", two claims that cannot both be true.
        Pending is a status; a failure with nothing to show is the error with a way to retry; data
        (fresh or stale) is the page.
      */}
      {projects.data === undefined ? (
        projects.isError ? (
          <RosterError error={projects.error} onRetry={() => void projects.refetch()} />
        ) : (
          <Skeleton label="Loading the projects" lines={3} />
        )
      ) : rows.length === 0 ? (
        <Teach
          title={`No project has been registered with the ${DAEMON_NAME}`}
          action={<Link to="/projects/new">Add a project</Link>}
        >
          Adding one takes a folder and three questions.
        </Teach>
      ) : (
        <Filtered rows={rows} stale={stale} />
      )}
    </>
  );
}

/**
 * The headline's prefix while the roster is stale — "as of 14:02:11: ".
 *
 * The same clock `StaleNote` prints, so the two agree to the second. A roster that has never been
 * read successfully has no data to be stale about, so the zero case is only a guard.
 */
function asOf(dataUpdatedAt: number): string {
  if (dataUpdatedAt <= 0) return "last known: ";
  return `as of ${new Date(dataUpdatedAt).toLocaleTimeString(UI_LOCALE, { hour12: false })}: `;
}

function RosterError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote onRetry={onRetry}>
      Could not load the projects: the {DAEMON_NAME} is not responding, so nothing is known about the
      roster.
    </ErrorNote>
  );
}

/**
 * The groups under a name filter.
 *
 * Twenty-five projects are looked up by name more often than they are read, and a grid cannot be
 * searched by eye. The filter narrows every group at once (a group left empty is not drawn, as
 * ever), `/` focuses it from anywhere that is not already a text field, Escape clears it, and Enter
 * opens the one project left. Its text is held here, above `Groups`, so the groups stay a pure
 * function of their rows.
 *
 * **The toolbar is a short strip, not a full-width bar.** The box has nothing to say that needs the
 * page's width, and the result count beside it is a figure, so it is set as one: bare mono text,
 * like every other count in the app. `ListFilter` is shared and its sheet is unlayered, hence the
 * important variants — they are the only way a utility can outrank it.
 */
function Filtered({ rows, stale }: { rows: ProjectSummary[]; stale: boolean }) {
  const navigate = useNavigate();
  const [q, setQ] = useState("");

  const needle = q.trim().toLowerCase();
  const shown = needle === "" ? rows : rows.filter((row) => row.project_id.toLowerCase().includes(needle));

  return (
    <>
      <div className="mt-1 mb-6 max-w-[30rem] [&_.ui-list-filter-count]:ml-0! [&_.ui-list-filter-count]:font-mono! [&_.ui-list-filter-count]:text-xs!">
        <ListFilter
          noun="projects"
          query={q}
          onQueryChange={setQ}
          resultCount={shown.length}
          onClear={() => setQ("")}
          onOpenOnly={() => {
            void navigate({
              to: "/projects/$projectId/$view",
              params: { projectId: shown[0].project_id, view: "state" },
            });
          }}
        />
      </div>
      {shown.length === 0 ? (
        <Quiet
          says={`No project is named like "${q.trim()}".`}
          action={
            <>
              <button type="button" className="ui-quiet-ask" onClick={() => setQ("")}>
                clear the search
              </button>
              <Link to="/projects/new">add a project</Link>
            </>
          }
        />
      ) : (
        <Groups rows={shown} stale={stale} />
      )}
    </>
  );
}

/**
 * The answer to a removal, on the page the project is now missing from.
 *
 * `Quiet` and announced: it is one line, it is the reply to something the reader just did, and it
 * carries the one gesture that undoes it. Focus lands on it, because the button that was pressed
 * went away with the page it was on, and a focus that falls to the body loses somebody's place.
 *
 * "add it back" is a plain link to `/projects/new`. That page does not read a folder from the URL
 * yet, so the path is said here in words rather than handed over.
 */
function LeftTheRoster({ removed }: { removed: Removed }) {
  const line = useRef<HTMLDivElement>(null);
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => {
    setDismissed(false);
    line.current?.focus();
  }, [removed]);
  if (dismissed) return null;

  const where =
    removed.projectRoot === null ? "it had no folder named" : `its folder is still at ${removed.projectRoot}`;
  const says = removed.forgot
    ? `${removed.projectId} left the roster and its history was deleted: ${where}.`
    : `${removed.projectId} left the roster: ${where}, and its history is kept.`;

  return (
    <div ref={line} tabIndex={-1} className="mb-4">
      <Quiet
        announce
        says={says}
        action={
          <>
            <Link to="/projects/new">add it back</Link>
            <button type="button" className="ui-quiet-ask" onClick={() => setDismissed(true)}>
              dismiss
            </button>
          </>
        }
      />
    </div>
  );
}

/* --------------------------------------------------------------- the groups -- */

/**
 * The columns every row and every group title share, from 52rem of container up.
 *
 * mode · project · to review · gate · in flight · last run · open. One literal so the title line
 * and the rows cannot drift apart, and a literal (not built from parts) so Tailwind can see it.
 * A container query and not the app's 60rem viewport fold: the sidebar takes a share of the
 * window, so the viewport's number is not the list's, and the seven columns need about 52rem of
 * their own to leave the name a readable measure.
 */
const WIDE_COLUMNS = "@min-[52rem]:grid-cols-[4.75rem_minmax(0,1fr)_7.5rem_5rem_6.5rem_8.25rem_3.5rem]";

/** Below that, a row is two lines: the project and its mode, then the facts it has. */
const NARROW_COLUMNS = "grid-cols-[minmax(0,1fr)_auto_auto]";

/**
 * The work in flight for one project, as a phrase, or `null` when there is none.
 *
 * Jobs and runs are named apart rather than summed. A job's runs are live runs too, and nothing in
 * either listing links one to the other, so a single number would count the same work twice.
 */
function inFlight(jobs: number, runs: number): string | null {
  const parts: string[] = [];
  if (jobs > 0) parts.push(plural(jobs, "job"));
  if (runs > 0) parts.push(plural(runs, "run"));
  if (parts.length === 0) return null;
  return parts.join(" and ");
}

/** Whether a project belongs in *Needs you*: on, and something only a person can clear. */
function needsYou(project: ProjectSummary): boolean {
  if (project.mode === "off") return false;
  const gate = gateOf(project);
  return (
    folderOf(project) !== "ok" || gate === "failed" || gate === "errored" || project.open_review_items > 0
  );
}

/** The order *Quiet* is drawn in: running, then watching, then switched off. */
const QUIET_STATES = ["active", "shadow", "off"];

/** A mode this page has never heard of goes last rather than vanishing. */
function quietRank(mode: string): number {
  const at = QUIET_STATES.indexOf(mode);
  return at === -1 ? QUIET_STATES.length : at;
}

/** How loud a group's rows are: the first two carry tone, the last carries only its mode dot. */
type Tier = "needs" | "working" | "quiet";

function Groups({ rows, stale }: { rows: ProjectSummary[]; stale: boolean }) {
  const jobs = useLiveJobs();
  const runs = useLiveRuns();

  /*
    Live work per project. An unanswered listing counts as none rather than holding the page: what
    is in flight decides only between *Working* and *Quiet*, and a roster that waited on the fleet
    to draw anything would make the slower question block the more important one.
  */
  const flightOf = (id: string): string | null =>
    inFlight(
      (jobs.data ?? []).filter((job) => job.project_id === id).length,
      (runs.data ?? []).filter((run) => run.project_id === id).length,
    );

  const needs = inAttentionOrder(rows).filter(needsYou);
  const working = inAttentionOrder(rows).filter(
    (project) => project.mode !== "off" && !needsYou(project) && flightOf(project.project_id) !== null,
  );
  const placed = new Set([...needs, ...working].map((project) => project.project_id));
  /*
    By mode first — what it is doing when left alone, so the switched-off ones sink to the foot —
    and by name inside each, because within a mode a quiet project is still looked up rather than
    read about. The mode is a column now, so it is no longer a sub-heading.
  */
  const quiet = rows
    .filter((project) => !placed.has(project.project_id))
    .sort((a, b) => quietRank(a.mode) - quietRank(b.mode) || a.project_id.localeCompare(b.project_id));

  const groups: { label: string; tier: Tier; projects: ProjectSummary[] }[] = [
    { label: "Needs you", tier: "needs", projects: needs },
    { label: "Working", tier: "working", projects: working },
    { label: "Quiet", tier: "quiet", projects: quiet },
  ];

  /*
    Muted while stale: the values recede to the register of a thing remembered, and the note above
    says how old they are. The tones of the facts stay — a failed gate an hour ago was still a
    failed gate.
  */
  return (
    <div className={`@container flex flex-col gap-8${stale ? " text-text-muted" : ""}`}>
      {groups
        .filter((group) => group.projects.length > 0)
        .map((group, i) => (
          <Group key={group.label} label={group.label} n={group.projects.length} tier={group.tier} labelled={i === 0}>
            {group.projects.map((project) => (
              <Row
                key={project.project_id}
                project={project}
                tier={group.tier}
                flight={flightOf(project.project_id)}
              />
            ))}
          </Group>
        ))}
    </div>
  );
}

/** The labels over the four fact columns; mode and project are the title's, and open is a verb. */
const COLUMN_HEADS = ["To review", "Gate", "In flight", "Last run"];

/**
 * A labelled region: its title with the count beside it, then its rows.
 *
 * `SectionTitle` rather than `Section`: `Section` takes its heading as a string, and the count has
 * to sit inside the heading to sit beside it. The region keeps the plain label as its name, so a
 * screen reader hears "Needs you", and the count is read as part of the heading under it.
 *
 * The first group's title line doubles as the column heads. They are `aria-hidden`: each cell
 * carries its own words for a screen reader (see `Row`), and a header row over links would be read
 * as a list of labels that belong to nothing.
 */
function Group({
  label,
  n,
  tier,
  labelled,
  children,
}: {
  label: string;
  n: number;
  tier: Tier;
  labelled: boolean;
  children: ReactNode;
}) {
  return (
    <section aria-label={label} className="flex flex-col gap-2">
      <div className={`grid items-baseline gap-x-3 px-3 ${WIDE_COLUMNS}`}>
        <div className="@min-[52rem]:col-span-2">
          <SectionTitle level={2}>
            {label} <Count n={n} />
          </SectionTitle>
        </div>
        {labelled &&
          COLUMN_HEADS.map((head) => (
            <span
              key={head}
              aria-hidden="true"
              className="hidden text-xs font-medium tracking-wide text-text-faint uppercase @min-[52rem]:block"
            >
              {head}
            </span>
          ))}
      </div>
      {/* No preflight in this app, so the list resets its own bullets and margins. */}
      <ul className={`m-0 list-none p-0 ${tier === "quiet" ? "" : "rounded-lg bg-surface"}`}>{children}</ul>
    </section>
  );
}

/** The project's mode as a dot, the rail's mark, so the two read alike. */
const MODE_DOT: Record<string, string> = {
  active: "bg-tone-active-fg",
  shadow: "bg-tone-shadow-fg",
  // Hollow: known, and doing nothing on purpose.
  off: "border-[1.5px] border-tone-off-fg",
};

// Active is the one mode whose word stays neutral: Acting Green on a word is spent nowhere in the
// app (`badge-authorship.test.ts`), so the dot beside it carries the tone alone.
const MODE_TEXT: Record<string, string> = {
  active: "text-text",
  shadow: "text-tone-shadow-fg",
  off: "text-tone-off-fg",
};

/**
 * The mode as a dot and its word. In *Quiet* the word is muted and only the dot keeps its tone, so
 * a column of them is a column of marks rather than of coloured words.
 */
function ModeMark({ mode, tone }: { mode: string; tone: boolean }) {
  return (
    <span
      data-mode={mode}
      className="col-start-2 row-start-1 flex items-center gap-2 text-sm @min-[52rem]:col-auto @min-[52rem]:row-auto"
    >
      <span aria-hidden="true" className={`size-2 shrink-0 rounded-pill ${MODE_DOT[mode] ?? "bg-text-faint"}`} />
      <span className={tone ? (MODE_TEXT[mode] ?? "text-text-muted") : "text-text-muted"}>
        {readState("autopilot", mode)?.label ?? mode}
      </span>
    </span>
  );
}

/** A gate run's time, in the one locale the window formats dates in. */
const GATE_AT = new Intl.DateTimeFormat(UI_LOCALE, { dateStyle: "medium", timeStyle: "short" });

function when(at: string): string {
  const date = new Date(at);
  // A timestamp this shell cannot parse is still a fact; said raw rather than as "Invalid Date".
  return Number.isNaN(date.getTime()) ? at : GATE_AT.format(date);
}

/**
 * The words a cell carries for a reader who has no column head: visible below the fold, where the
 * row is two lines and the heads are gone, and screen-reader-only above it, where the head says it.
 */
const CELL_WORD = "text-text-faint @min-[52rem]:sr-only";

/** A cell that is empty above the fold to keep the columns aligned, and is not drawn below it. */
function Gap() {
  return <span aria-hidden="true" className="hidden @min-[52rem]:block" />;
}

/**
 * What a project says about its folder, under its name: the path when it is fine, and the fact in
 * words when it is not.
 *
 * The tone follows who is asking. For a project that is meant to be running, a folder that is gone
 * is a fault (red) and one nobody named is unfinished (amber — grey on a badge, but here it only
 * ever appears for a project that is meant to be running). For a switched-off project neither is a
 * fault, it is what switched off looks like, so both are said in the grey of switched off.
 */
function FolderLine({ project }: { project: ProjectSummary }) {
  const folder = folderOf(project);
  const off = project.mode === "off";
  if (folder === "ok") {
    return <span className="block truncate font-mono text-xs text-text-faint">{project.project_root}</span>;
  }
  if (folder === "missing") {
    const detail = `${project.project_root} is not on this disk`;
    return (
      <span className="flex gap-2 text-xs">
        <span title={detail} className={`shrink-0 font-medium ${off ? "text-tone-off-fg" : "text-tone-danger-fg"}`}>
          folder gone
        </span>
        <span className="truncate font-mono text-text-faint">{detail}</span>
      </span>
    );
  }
  return (
    <span className={`block text-xs font-medium ${off ? "text-tone-off-fg" : "text-tone-pending-fg"}`}>
      no folder named
    </span>
  );
}

/**
 * One project: a single link to its State, laid on the shared columns.
 *
 * The whole row is the link, and nothing inside it is interactive, so there is no nested control to
 * break. Hover is one rung up the neutral ladder, never a tint — the tones belong to the facts, and
 * a row that went amber on hover would be saying something about the project that is not true. The
 * focus ring is the document's own.
 *
 * The verb at the end says "Open" and nothing else: the row goes to the project's State, so a verb
 * like "Review" or "See the gate" would promise a destination the link does not reach. The facts
 * say what is wrong; the verb only says what the click does.
 */
function Row({ project, tier, flight }: { project: ProjectSummary; tier: Tier; flight: string | null }) {
  const gate = gateOf(project);
  const lastGate = project.last_gate_at ?? null;
  const split = whereWaiting(project);
  const quiet = tier === "quiet";
  const hasFacts = project.open_review_items > 0 || gate !== "none" || flight !== null || lastGate !== null;

  return (
    <li
      className={
        quiet ? "border-t border-border" : "border-t border-border first:rounded-t-lg first:border-t-0 last:rounded-b-lg"
      }
    >
      <Link
        className={`group grid items-baseline gap-x-3 gap-y-1 px-3 py-3 text-[color:inherit] no-underline transition-colors hover:no-underline ${NARROW_COLUMNS} ${WIDE_COLUMNS} ${
          quiet ? "rounded-md hover:bg-surface" : "rounded-[inherit] hover:bg-surface-raised"
        }`}
        to="/projects/$projectId/$view"
        params={{ projectId: project.project_id, view: "state" }}
      >
        <ModeMark mode={project.mode} tone={!quiet} />

        <span className="col-start-1 row-start-1 min-w-0 @min-[52rem]:col-auto @min-[52rem]:row-auto">
          <span
            data-name=""
            className={`block truncate font-display leading-snug ${
              quiet ? "text-base font-medium text-text-muted group-hover:text-text" : "text-md font-semibold"
            }`}
          >
            {project.project_id}
          </span>
          <FolderLine project={project} />
        </span>

        {/* The facts: cells of their own above the fold, one wrapping line below it. */}
        <span
          className={`col-span-3 col-start-1 flex-wrap items-baseline gap-x-4 gap-y-1 text-sm @min-[52rem]:contents ${
            hasFacts ? "flex" : "hidden"
          }`}
        >
          {project.open_review_items > 0 ? (
            <span className="min-w-0">
              <span className="font-mono tabular-nums text-tone-pending-fg">{project.open_review_items}</span>
              <span className={CELL_WORD}> to review</span>
              {split !== null && <span className="block text-xs text-text-faint">{split}</span>}
            </span>
          ) : (
            <Gap />
          )}

          {gate === "none" ? (
            <Gap />
          ) : (
            <span className="min-w-0">
              <span className={CELL_WORD}>gate </span>
              {gate === "failed" ? (
                <span data-tone="danger" className="text-tone-danger-fg">
                  failed
                </span>
              ) : gate === "errored" ? (
                // Info and not danger: the gate could not run — a missing command, a worktree that
                // had gone — and that says nothing at all about the code.
                <span data-tone="info" className="text-tone-info-fg">
                  could not run
                </span>
              ) : (
                <span className="text-text-faint">passed</span>
              )}
            </span>
          )}

          {flight !== null ? (
            <span className="min-w-0">
              {/* The green is a mark beside the count, never the count's own ink — the same
                  split the mode column makes. */}
              <span
                aria-hidden="true"
                className="mr-1.5 inline-block size-[0.4rem] rounded-full bg-tone-active-fg align-middle"
              />
              <span className="text-text">{flight}</span>
              <span className={CELL_WORD}> in flight</span>
            </span>
          ) : (
            <Gap />
          )}

          {lastGate !== null ? (
            <span className="min-w-0 font-mono text-xs tabular-nums text-text-muted">
              <span className={CELL_WORD}>last run </span>
              {when(lastGate)}
            </span>
          ) : (
            <Gap />
          )}
        </span>

        <span className="col-start-3 row-start-1 flex items-center justify-end gap-1 text-sm text-text-muted group-hover:text-text @min-[52rem]:col-auto @min-[52rem]:row-auto">
          <span>Open</span>
          <ArrowRight size={14} aria-hidden="true" />
        </span>
      </Link>
    </li>
  );
}
