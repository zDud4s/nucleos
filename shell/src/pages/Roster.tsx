import { useEffect, useRef, type ReactNode } from "react";
import { Link, useRouterState } from "@tanstack/react-router";
import { ArrowRight } from "lucide-react";

import { isApiRefusal } from "../data/client";
import { useLiveJobs, useLiveRuns } from "../data/fleet";
import { folderOf, gateOf, headline, inAttentionOrder, whereWaiting } from "../data/roster";
import { useProjects, type ProjectSummary } from "../data/system";
import { UI_LOCALE } from "../lib/locale";
import { Count } from "../ui/Count";
import { ErrorNote } from "../ui/ErrorNote";
import { PageHeader } from "../ui/PageHeader";
import { Quiet } from "../ui/Quiet";
import { RefusalNote } from "../ui/RefusalNote";
import { SectionTitle } from "../ui/SectionTitle";
import { StaleNote } from "../ui/StaleNote";
import { Teach } from "../ui/Teach";
import { readState } from "../ui/state-map";
import type { Removed } from "./RemoveProject";
import "./projects.css";

/**
 * Every project the núcleo knows, sorted by what it asks of somebody.
 *
 * **Three groups, not one ordered table.** The table before this already put the projects that
 * needed somebody at the top, and that order was right; what it could not do was say where the
 * trouble ended. Twenty-five rows of equal weight, four columns each, and the reader had to scan
 * every one to learn that only the first three mattered. So the order became a partition:
 *
 * - **Needs you** — a project that is meant to be doing something and cannot, or is waiting on a
 *   decision. Drawn as cards, because each one carries the sentences that say *why*, and the reason
 *   is the thing somebody reads before they open it.
 * - **Working** — nothing asked of anybody, but work is in flight. Same card, one reason.
 * - **Quiet** — everything else, alphabetically and compact, because a quiet project is looked
 *   up by name rather than read about. Switched-off projects live here whatever their folder says:
 *   `rankOf` already stopped shouting about them, and a dormant project in *Needs you* would teach
 *   people that the group can be ignored.
 *
 * A group with nothing in it is not drawn — an empty *Needs you* heading is a sentence about
 * nothing, and the headline already says when nothing is wrong by not naming anything.
 *
 * Every card and item opens the project in its workspace, on State: it is the mode that answers the
 * question somebody arrives with.
 *
 * **And removing a project is not on this page any more.** It used to be a `remove` on every row,
 * on the argument that the roster is where somebody compares projects and realises they are done
 * with one. Cards that are one link each have no row to put a second control on, and the stronger
 * argument was always the one `DeleteFolder` makes: the way out of a project belongs inside it,
 * reached by somebody who has already opened the one they mean. It lives in State's *Leaving*
 * section now, and the acknowledgement still lands here, because this is where the project is
 * missing from.
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
        <Groups rows={rows} stale={stale} />
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

/* --------------------------------------------------------------- the groups -- */

/**
 * What kind of fact a reason is, which `projects.css` turns into its tone's colour.
 *
 * Named for the fact rather than the tone on purpose: tones are `state-map.ts`'s to hand out, to
 * badges, and these are not badges — they are swatches beside a sentence, and one of them departs
 * from the map on purpose (a folder nobody named is grey on a badge and amber here, because on this
 * page it only ever appears for a project that is meant to be running).
 */
type Mark = "review" | "fault" | "unmeasured" | "unnamed" | "flight";

/** One sentence on a card: why it is there, marked with the kind of fact it is. */
interface Reason {
  mark: Mark;
  says: string;
  title?: string;
}

/**
 * The work in flight for one project, as a phrase, or `null` when there is none.
 *
 * Jobs and runs are named apart rather than summed. A job's runs are live runs too, and nothing in
 * either listing links one to the other, so a single number would count the same work twice.
 */
function inFlight(jobs: number, runs: number): string | null {
  const parts: string[] = [];
  if (jobs > 0) parts.push(`${jobs} ${jobs === 1 ? "job" : "jobs"}`);
  if (runs > 0) parts.push(`${runs} ${runs === 1 ? "run" : "runs"}`);
  if (parts.length === 0) return null;
  return `${parts.join(" and ")} in flight`;
}

/** Whether a project belongs in *Needs you*: on, and something only a person can clear. */
function needsYou(project: ProjectSummary): boolean {
  if (project.mode === "off") return false;
  const gate = gateOf(project);
  return (
    folderOf(project) !== "ok" || gate === "failed" || gate === "errored" || project.open_review_items > 0
  );
}

function reasonsFor(project: ProjectSummary, flight: string | null): Reason[] {
  const reasons: Reason[] = [];
  const folder = folderOf(project);
  const gate = gateOf(project);
  if (project.open_review_items > 0) {
    // The split when the daemon gave one: "13 to review" pointed at the proposals page for a
    // project whose every waiting item was a shadow decision.
    const detail = whereWaiting(project);
    reasons.push({
      mark: "review",
      says: detail === null ? `${project.open_review_items} to review` : `${detail} to review`,
    });
  }
  if (gate === "failed") {
    reasons.push({ mark: "fault", says: "gate failed", title: lastRun(project.last_gate_at ?? null) });
  }
  // Info and not danger: the gate could not run — a missing command, a worktree that had gone — and
  // that says nothing at all about the code.
  if (gate === "errored") {
    reasons.push({ mark: "unmeasured", says: "gate could not run", title: lastRun(project.last_gate_at ?? null) });
  }
  if (folder === "missing") {
    reasons.push({ mark: "fault", says: "folder gone", title: `${project.project_root} is not on this disk` });
  }
  if (folder === "unset") reasons.push({ mark: "unnamed", says: "no folder named" });
  if (flight !== null) reasons.push({ mark: "flight", says: flight });
  return reasons;
}

/** What opening the card is for — the first thing in it somebody can act on. */
function verbFor(project: ProjectSummary): string {
  const gate = gateOf(project);
  if (project.open_review_items > 0) return "Review";
  if (gate === "failed" || gate === "errored") return "See the gate";
  if (folderOf(project) !== "ok") return "Point at the folder";
  return "Open";
}

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
  const quiet = rows.filter((project) => !placed.has(project.project_id));
  /*
    By state first — what it is doing when left alone — and by name inside each, because within a
    state a quiet project is still looked up rather than read about. Running before watching before
    switched off; a mode this page has never heard of goes last rather than vanishing.
  */
  const byState = [...QUIET_STATES, ...new Set(quiet.map((project) => project.mode))]
    .filter((mode, i, all) => all.indexOf(mode) === i)
    .map((mode) => ({
      mode,
      projects: quiet
        .filter((project) => project.mode === mode)
        .sort((a, b) => a.project_id.localeCompare(b.project_id)),
    }))
    .filter((state) => state.projects.length > 0);

  /*
    Muted while stale: the values recede to the register of a thing remembered, and the note above
    says how old they are. The tones of the reasons stay — a failed gate an hour ago was still a
    failed gate.
  */
  return (
    <div className={`rs-groups${stale ? " text-text-muted" : ""}`}>
      {needs.length > 0 && (
        <Group label="Needs you" n={needs.length}>
          <div className="rs-cards">
            {needs.map((project) => {
              const flight = flightOf(project.project_id);
              return (
                <Card
                  key={project.project_id}
                  project={project}
                  reasons={reasonsFor(project, flight)}
                  verb={verbFor(project)}
                />
              );
            })}
          </div>
        </Group>
      )}

      {working.length > 0 && (
        <Group label="Working" n={working.length}>
          <div className="rs-cards">
            {working.map((project) => (
              <Card
                key={project.project_id}
                project={project}
                reasons={reasonsFor(project, flightOf(project.project_id))}
                verb="Open"
              />
            ))}
          </div>
        </Group>
      )}

      {quiet.length > 0 && (
        <Group label="Quiet" n={quiet.length}>
          <div className="rs-quiet-states">
            {byState.map(({ mode, projects }) => {
              const label = readState("autopilot", mode)?.label ?? mode;
              return (
                <div key={mode} role="group" aria-label={label} className="rs-quiet-state">
                  <h3 className="rs-quiet-label">
                    <ModeDot mode={mode} />
                    {label} <Count n={projects.length} />
                  </h3>
                  <div className="rs-quiet">
                    {projects.map((project) => (
                      <Link
                        key={project.project_id}
                        className="rs-quiet-item"
                        to="/projects/$projectId/$view"
                        params={{ projectId: project.project_id, view: "state" }}
                      >
                        <span className="rs-quiet-name">{project.project_id}</span>
                      </Link>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        </Group>
      )}
    </div>
  );
}

/**
 * A labelled region with its count beside the heading.
 *
 * `SectionTitle` rather than `Section`: `Section` takes its heading as a string, and the count has
 * to sit inside the heading to sit beside it. The region keeps the plain label as its name, so a
 * screen reader hears "Needs you", and the count is read as part of the heading under it.
 */
function Group({ label, n, children }: { label: string; n: number; children: ReactNode }) {
  return (
    <section aria-label={label} className="rs-group">
      <SectionTitle level={2}>
        {label} <Count n={n} />
      </SectionTitle>
      {children}
    </section>
  );
}

/** The order the states of *Quiet* are drawn in: running, then watching, then switched off. */
const QUIET_STATES = ["active", "shadow", "off"];

/** The project's mode in the width of a dot — the rail's mark, so the two read alike. */
function ModeDot({ mode }: { mode: string }) {
  return <span className="rs-dot" data-mode={mode} aria-hidden="true" />;
}

function Card({ project, reasons, verb }: { project: ProjectSummary; reasons: Reason[]; verb: string }) {
  return (
    <Link
      className="rs-card"
      to="/projects/$projectId/$view"
      params={{ projectId: project.project_id, view: "state" }}
    >
      <span className="rs-card-top">
        <ModeDot mode={project.mode} />
        <span className="rs-card-name">{project.project_id}</span>
        <span className="rs-card-mode">{readState("autopilot", project.mode)?.label ?? project.mode}</span>
      </span>
      <ul className="rs-card-reasons">
        {reasons.map((reason) => (
          <li key={reason.says} title={reason.title}>
            <span className="rs-mark" data-mark={reason.mark} aria-hidden="true" />
            {reason.says}
          </li>
        ))}
      </ul>
      <span className="rs-card-act">
        {verb}
        <ArrowRight size={14} aria-hidden="true" />
      </span>
    </Link>
  );
}

/** A gate run's time, in the one locale the window formats dates in. */
const GATE_AT = new Intl.DateTimeFormat(UI_LOCALE, { dateStyle: "medium", timeStyle: "short" });

function lastRun(at: string | null): string | undefined {
  if (at === null) return undefined;
  const when = new Date(at);
  // A timestamp this shell cannot parse is still a fact; said raw rather than as "Invalid Date".
  return `last run ${Number.isNaN(when.getTime()) ? at : GATE_AT.format(when)}`;
}
