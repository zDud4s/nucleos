import type { ReactNode } from "react";
import { Link, useSearch } from "@tanstack/react-router";
import { countWaitingDecisions, useActionApprovals, useContactMerges, useExclusionRequests, useOpenTeamActions, useRecruitProposals, useTeamActionProposals, useVcsRequests, useVcsWaiting, useWheelRequests } from "../data/waiting";
import { PageHeader, Tip } from "../ui";
import { type Reading, reading } from "./waiting/reading";
import { QUEUES, type QueueText } from "./waiting/shared";
import { WheelRequestSection } from "./waiting/WheelRequests";
import { ActionApprovalSection } from "./waiting/ActionApprovals";
import { TeamActionSection } from "./waiting/TeamActions";
import { RecruitmentSection } from "./waiting/Recruitment";
import { ContactMergeSection } from "./waiting/ContactMerges";
import { ExclusionRequestSection } from "./waiting/ExclusionRequests";
import { GitQueuePanel } from "./waiting/GitQueue";
import "./waiting.css";

/**
 * Waiting — the one place a decision is asked for, whatever asked for it.
 *
 * The page is a single queue with sections rather than a tab per source, and
 * that is the whole design: a person who has to remember that browser handovers
 * live somewhere else from action approvals will find one of them a day late.
 * The sections are the design's §6.4 order, and each one reads **the route that
 * actually serves it** — `GET /proposals` is action-approval only, so it is one
 * section and not the page. `data/waiting.ts` carries the table.
 *
 * Two things here are structural rather than cosmetic.
 *
 * **The ordering freeze.** Every list on this page is refetched on a timer, and
 * every list is worked by clicking a button on a row. A refetch that reorders a
 * list between the arm and the confirm moves a different row under the finger —
 * an approval given to the wrong action, silently. So arming any `ConfirmButton`
 * in a section snapshots that section's order and holds it until nothing in the
 * section is armed. The rows still update; only their sequence is pinned.
 *
 * **Bodies are sentences, not dumps.** A proposal's `tool_input` is JSON in the
 * database, and pasting it into the page as JSON would make the one thing a
 * person has to read — what is about to happen — the least readable thing on the
 * card. It is rendered field by field as plain text. Origins are shown exactly
 * as recorded, punycode included: prettifying `xn--` back to the glyphs it
 * encodes is precisely the attack these decisions exist to catch.
 */

/** The one filter this page's location carries. Empty and absent are the same claim. */
export function validateWaitingSearch(search: Record<string, unknown>) {
  const raw = search.project;
  return { project: typeof raw === "string" && raw !== "" ? raw : undefined };
}

/**
 * One project's share of a queue.
 *
 * A row whose shape carries no `project_id` is dropped and not kept: a contact merge is not
 * about a project, and showing it under a heading that says otherwise would be a wrong claim
 * rather than a generous one. The narrowing note below says what is being left out.
 */
function onlyProject<T>(
  view: Reading<T>,
  projectId: string | undefined,
): Reading<T> {
  if (projectId === undefined || view.rows === undefined) return view;
  const rows = view.rows.filter(
    (row) => (row as { project_id?: string | null }).project_id === projectId,
  );
  return { ...view, rows };
}

export function Waiting() {
  const { project } = useSearch({ strict: false }) as { project?: string };

  const wheel = onlyProject(reading(useWheelRequests()), project);
  const approvals = onlyProject(reading(useActionApprovals()), project);
  const teamActions = onlyProject(reading(useTeamActionProposals()), project);
  const recruits = onlyProject(reading(useRecruitProposals()), project);
  // A lookup and not a section: `openActions` answers a question about a row that is already
  // on screen, so narrowing it would only hide the answer.
  const openActions = useOpenTeamActions();
  const merges = onlyProject(reading(useContactMerges()), project);
  const exclusions = onlyProject(reading(useExclusionRequests()), project);
  const vcs = onlyProject(reading(useVcsRequests()), project);
  const vcsWaiting = onlyProject(reading(useVcsWaiting()), project);

  const decisions = countWaitingDecisions({
    wheel: wheel.rows,
    approvals: approvals.rows,
    teamActions: teamActions.rows,
    recruits: recruits.rows,
    merges: merges.rows,
    exclusions: exclusions.rows,
    git: vcsWaiting.rows,
  });

  /**
   * The design's §6.4 order, with the sections that have something in them first.
   *
   * The order below is that order and is kept exactly. What moves is emptiness: a
   * queue whose first four headings are absences makes somebody scroll past them to
   * reach the one decision that is actually waiting, which is the opposite of what
   * this page is for. `sort` is stable, so nothing is reordered relative to its own
   * neighbours.
   *
   * **Every section here asks for a decision.** Skipped items and refused actions are
   * records of what the night and the barrier already settled, and they live on the
   * Autopilot page now; a parked run is the run side of an approval that is already
   * on this page; and the calendar absence only ever said that a route does not
   * exist. A queue that mixes things to decide with things to read is a queue whose
   * count nobody trusts.
   *
   * `rows` is what decides whether a section is *empty* — answered, and nothing in it
   * — as opposed to not answered yet, which is neither filled nor quiet.
   */
  const sections: {
    queue: QueueText;
    rows: unknown[] | undefined;
    node: ReactNode;
  }[] = [
    {
      queue: QUEUES.wheel,
      rows: wheel.rows,
      node: <WheelRequestSection key="wheel" view={wheel} />,
    },
    {
      queue: QUEUES.approvals,
      rows: approvals.rows,
      node: <ActionApprovalSection key="approvals" view={approvals} />,
    },
    {
      queue: QUEUES.teamActions,
      rows: teamActions.rows,
      node: (
        <TeamActionSection
          key="team-actions"
          view={teamActions}
          executing={openActions.data}
        />
      ),
    },
    {
      queue: QUEUES.recruits,
      rows: recruits.rows,
      node: <RecruitmentSection key="recruits" view={recruits} />,
    },
    {
      queue: QUEUES.merges,
      rows: merges.rows,
      node: <ContactMergeSection key="merges" view={merges} />,
    },
    {
      queue: QUEUES.exclusions,
      rows: exclusions.rows,
      node: <ExclusionRequestSection key="exclusions" view={exclusions} />,
    },
    {
      queue: QUEUES.git,
      rows: vcsWaiting.rows,
      node: <GitQueuePanel key="vcs" history={vcs} waiting={vcsWaiting} />,
    },
  ];
  const quiet = sections
    .filter((section) => section.rows !== undefined && section.rows.length === 0)
    .map((section) => section.queue);
  const ordered = [...sections].sort((a, b) => Number(has(b.rows)) - Number(has(a.rows)));
  const filled = ordered
    .filter((section) => has(section.rows))
    .map((section) => ({ queue: section.queue, count: section.rows?.length ?? 0 }));

  return (
    <>
      <PageHeader title="Waiting" headline={headlineFor(decisions)} />

      {project !== undefined ? (
        <p className="waiting-narrowed" role="status">
          Only {project}. <Link to="/waiting">Show everything</Link>
        </p>
      ) : null}

      <WaitingIndex queues={filled} />

      <div className="waiting-sections">
        {ordered.map((section) => section.node)}
        <NothingWaiting queues={quiet} />
      </div>
    </>
  );
}

/**
 * What is waiting, by kind, as links to where it is.
 *
 * The headline says how many decisions there are and nothing about what sort, so the page had to
 * be scrolled to find out; this is the map. Only the queues with something in them are named
 * (the ones without are the sentence at the bottom), in the order the sections are on the page,
 * so the line reads top to bottom as the page does. Plain anchors: the router keeps its location in
 * memory, so a fragment link only moves the scroll, which is all it is for.
 */
function WaitingIndex({ queues }: { queues: { queue: QueueText; count: number }[] }) {
  if (queues.length === 0) return null;
  return (
    <nav className="waiting-index" aria-label="Queues with something waiting">
      <ul className="waiting-index-list">
        {queues.map(({ queue, count }) => (
          <li key={queue.id}>
            <a className="waiting-index-link" href={`#${queue.id}`}>
              <span className="waiting-index-count">{count}</span> {queue.index(count)}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/**
 * The sections with nothing in them, as one line.
 *
 * An empty queue is the *normal* state of this page, and a heading plus a sentence for each of
 * six absences made the page longest on the morning nothing was wrong. What an absence
 * needs is to be a fact and not a gap — "nothing has asked for browser access" alone reads as a
 * list that failed to load — so the names are one sentence. The reasoning behind each rides on
 * its name: a `Tip`, shown on hover, focus or press, closed by Escape, and read out as the
 * name's description. It was a disclosure under
 * the line, dropped at the owner's call (2026-10-09).
 */
function NothingWaiting({ queues }: { queues: QueueText[] }) {
  if (queues.length === 0) return null;
  return (
    <p className="waiting-quiet-line">
      Nothing waiting in:{" "}
      {queues.map((queue, index) => (
        <span key={queue.title}>
          {index > 0 && ", "}
          {/* A `Tip`, not `title` and not a CSS-only hover: it opens on focus and on a press
              (touch), Escape closes it, and it is the name's description, so it is also read
              out. `persist` keeps the text in the document while closed. */}
          <Tip className="waiting-quiet-name" tip={queue.why} persist>
            {queue.title}
          </Tip>
        </span>
      ))}
    </p>
  );
}

/** Whether a list has anything in it. `undefined` — not answered yet — is not "filled". */
function has(rows: unknown[] | undefined): boolean {
  return (rows?.length ?? 0) > 0;
}

/**
 * One derived sentence about the queue.
 *
 * Only decisions are counted, and only decisions are said. Records the night settled used to
 * ride along as "N to read and put away", which made a quiet morning with nine skipped items
 * read as a page with something on it; they are on the Autopilot page now.
 */
function headlineFor(decisions: number | undefined): ReactNode {
  if (decisions === undefined) return undefined;
  if (decisions === 0) return "nothing is waiting on a decision";
  // The system's awaiting tone, so the one count that asks for a person is not neutral.
  return <span className="ui-awaiting">{decisions} waiting on a decision</span>;
}

