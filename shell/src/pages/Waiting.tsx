import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent,
  type ReactNode,
} from "react";
import { Link, useSearch } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import type { AgentEngine, AgentRequest, AgentToolPolicy } from "../data/agents";
import type { Proposal } from "../data/system";
import {
  parseActionPayload,
  teamActionState,
  useApproveTeamAction,
  useHireRecruit,
  type TeamAction,
} from "../data/teams";
import {
  VCS_LIST_LIMIT,
  countWaitingDecisions,
  useActionApprovals,
  useApproveProposal,
  useContactMerges,
  useDecideContactMerge,
  useApproveVcsRequest,
  useDismissVcsRequest,
  useRefuseVcsRequest,
  useExclusionRequests,
  useOpenTeamActions,
  useRecruitProposals,
  useDeclineAction,
  useRejectProposal,
  useTeamActionProposals,
  useVcsRequests,
  useVcsWaiting,
  useWheelRequests,
  type ApprovalOutcome,
  type MergeSide,
  type MergeSuggestion,
  type VcsRequestSummary,
  type WheelRequest,
} from "../data/waiting";
import { useApproveWheel } from "../data/browser";
import {
  Button,
  ConfirmButton,
  ConflictNote,
  Count,
  ErrorNote,
  Inset,
  PageHeader,
  Panel,
  Quiet,
  Skeleton,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  StaleNote,
  StateBadge,
  Tip,
} from "../ui";
import "./waiting.css";
import { DAEMON_NAME } from "../copy";

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

/* ------------------------------------------------------------- the reading -- */

/**
 * One list, plus what is known about how fresh it is.
 *
 * The page reads and the sections render, which keeps ten `useQuery` calls in
 * one place where the page-level headline can count them, and keeps each section
 * a function of its data.
 */
interface Reading<T> {
  rows: T[] | undefined;
  /** The rows on screen are no longer the daemon's, but they are better than a blank. */
  stale: boolean;
  /** Non-null only while the query is failing; `null` while it is fine. */
  error: unknown;
  dataUpdatedAt: number;
  /** The first read has not finished: nothing is known yet, which is not the same as empty. */
  pending: boolean;
  /** Tries the read again; offered beside a failure that left nothing to show. */
  retry: () => void;
}

function reading<T>(query: {
  data: T[] | undefined;
  isError: boolean;
  isPending: boolean;
  error: unknown;
  dataUpdatedAt: number;
  refetch: () => unknown;
}): Reading<T> {
  return {
    rows: query.data,
    stale: query.isError && query.data !== undefined,
    error: query.isError ? query.error : null,
    dataUpdatedAt: query.dataUpdatedAt,
    pending: query.isPending,
    retry: () => void query.refetch(),
  };
}

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

  return (
    <>
      <PageHeader title="Waiting" headline={headlineFor(decisions)} />

      {project !== undefined ? (
        <p className="waiting-narrowed" role="status">
          Only {project}. <Link to="/waiting">Show everything</Link>
        </p>
      ) : null}

      <div className="waiting-sections">
        {[...sections]
          .sort((a, b) => Number(has(b.rows)) - Number(has(a.rows)))
          .map((section) => section.node)}
        <NothingWaiting queues={quiet} />
      </div>
    </>
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

/* ------------------------------------------------------- the ordering freeze -- */

/**
 * Hold a section's order still while any of its cards is armed.
 *
 * The snapshot is taken on the **first** arm and released when the last armed
 * control disarms — counted, because two cards can be armed at once and the
 * second one disarming must not unfreeze the list under the first. Rows that
 * arrive while the freeze is on go to the end in the order the daemon sent them,
 * which is the only honest place for a row nobody has seen yet.
 *
 * Nothing here is a render-phase side effect: the snapshot is state, written
 * from an event handler, so the component stays a pure function of its props.
 */
export function useOrderFreeze<T>(rows: T[], idOf: (row: T) => number) {
  const [snapshot, setSnapshot] = useState<number[] | null>(null);
  const armedCount = useRef(0);

  function onArmedChange(armed: boolean) {
    armedCount.current = Math.max(0, armedCount.current + (armed ? 1 : -1));
    if (armedCount.current === 0) {
      setSnapshot(null);
      return;
    }
    setSnapshot((current) => (current === null ? rows.map(idOf) : current));
  }

  return { items: inSnapshotOrder(rows, idOf, snapshot), onArmedChange };
}

function inSnapshotOrder<T>(
  rows: T[],
  idOf: (row: T) => number,
  snapshot: number[] | null,
): T[] {
  if (snapshot === null) return rows;
  const rank = new Map(snapshot.map((id, index) => [id, index]));
  // `sort` is stable, so everything the snapshot never saw keeps the daemon's
  // own order behind everything it did.
  return [...rows].sort(
    (a, b) =>
      (rank.get(idOf(a)) ?? Number.MAX_SAFE_INTEGER) -
      (rank.get(idOf(b)) ?? Number.MAX_SAFE_INTEGER),
  );
}

/**
 * Dense above three.
 *
 * A queue of two is read; a queue of ten is worked through. The tighter row is
 * what keeps the tenth one on the same screen as the first, and the clamp on the
 * reasoning is what stops one verbose model costing the other nine their place.
 */
function dense(count: number): boolean {
  return count > 3;
}

/**
 * Why the agent says it wants this, with a way out of the clamp.
 *
 * A dense list clamps the reasoning to three lines (`waiting.css`), and a clamp with no
 * way past it hides the one thing the person is deciding on: model text that was cut at
 * the sentence that said what the action would actually touch. So the clamp is
 * honest — a "Show all" appears when, and only when, the text is really longer than what is
 * showing, measured on the element rather than guessed from a character count (the width
 * decides how many lines a sentence takes, and the width is not known until layout).
 *
 * `overflows` is **kept while expanded**: once the clamp is lifted the text no longer
 * overflows, and a button that vanished on the click that opened it could not close it
 * again. The measure is taken in a layout effect so the first paint already has the button
 * where it belongs, and it re-runs when the text changes under a poll.
 */
function Reasoning({ children }: { children: string }) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [overflows, setOverflows] = useState(false);
  const id = useId();

  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null || expanded) return;
    setOverflows(element.scrollHeight > element.clientHeight + 1);
  }, [children, expanded]);

  return (
    <div className="waiting-reasoning-block">
      <p
        ref={ref}
        id={id}
        className="waiting-reasoning"
        data-expanded={expanded ? "true" : undefined}
      >
        {children}
      </p>
      {overflows && (
        <button
          type="button"
          className="waiting-more"
          aria-expanded={expanded}
          aria-controls={id}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded ? "Show less" : "Show all"}
        </button>
      )}
    </div>
  );
}


/* ------------------------------------------------------------- shared parts -- */

/**
 * What a decision section is called, and what it is for.
 *
 * Kept in one table and not inside each section, because the page needs the title and the
 * reasoning of the sections that are **empty** — and an empty section renders nothing, so
 * the text cannot live in the component that did not render. The sentences are the ones the
 * one-line absences used to carry behind "why?", moved rather than cut: "nothing has
 * asked for browser access" alone reads as a list that failed to load, and the sentence
 * behind it is what makes the silence a fact instead of a gap.
 */
interface QueueText {
  title: string;
  /** The absence, in the fewest words that are still true. */
  says: string;
  /** What this section is for. */
  why: ReactNode;
}

const QUEUES = {
  wheel: {
    title: "Browser access requests",
    says: "nothing has asked for browser access",
    why: (
      <>
        An agent has met a wall it may not climb and is asking for the window.
        A session the shell can drive may be approved to be driven here, in the
        shell, or opened as a real browser window on this machine; one it
        cannot is only offered the window. Either way it runs in the profile
        named on the card. Declining closes the session, and the run carries
        on without that page.
      </>
    ),
  },
  approvals: {
    title: "Action approvals",
    says: "nothing is waiting to be let through",
    why: (
      <>
        A run that reaches for something outside its permission stops where it
        is and files one of these. An empty queue means either that nothing
        has asked, or that the autopilot is off — it is not a sign that
        anything is stuck.
      </>
    ),
  },
  teamActions: {
    title: "Team actions",
    says: "no team is waiting on an action",
    why: (
      <>
        An action a team asked the daemon to carry out under a propose grant.
        Approving does not do the thing — it lets the daemon do it on its next
        tick, about ten seconds later.
      </>
    ),
  },
  recruits: {
    title: "Recruitment",
    says: "no team has asked for a specialist",
    why: (
      <>
        A director asked for a specialist by name. The request is editable
        here before it is granted; declining leaves nothing behind — the
        team may ask again.
      </>
    ),
  },
  merges: {
    title: "Contact merges",
    says: "no two contacts look like the same person",
    why: (
      <>
        Two records that look like one person. Merging is a pointer move and
        is undoable one address at a time; saying they are different people is
        recorded too, which is what stops the same suggestion coming back on
        every sweep.
      </>
    ),
  },
  exclusions: {
    title: "Keep jobs apart?",
    says: "no job has asked to be kept apart from another",
    why: (
      <>
        A request that two jobs of one project never run at the same time.
        Drawing the edge changed nothing; approving it writes the rule, and
        the higher-numbered job is the one that waits.
      </>
    ),
  },
  git: {
    title: "Git queue",
    says: "nothing in the git queue needs your approval",
    why: (
      <>
        Every push, merge and rebase the daemon has been asked to make. One
        state wants your approval: a merge that changes the test map, held
        until you decide. Escalated requests (somebody owns a conflict now) and
        blocked ones (the queue will not retry) want a person to read them, and
        none is a failure; they, and the recent history, are behind the
        disclosure at the bottom of the panel. A request stops waiting once it
        is settled: a later identical request succeeded, a resolution took it,
        its branch is already in the target or gone, or you put it away.
      </>
    ),
  },
} satisfies Record<string, QueueText>;

/**
 * One section of the queue, in the shape its data allows. There are three, and one of them
 * is nothing.
 *
 * **With rows in it**, a `Panel` with the count in its corner — a bordered block,
 * because there is something in it to work through.
 *
 * **Empty**, nothing at all: the page lists the empty sections' titles in one line
 * (`NothingWaiting`) and keeps their reasoning behind a disclosure there. The exception is a
 * section that was full a moment ago in this same visit — the last card was just decided.
 * That one keeps its heading and one line, because it carries the receipt of the decision
 * ("run 44 is going again"), which outlives the row it was made on, and because it is where
 * focus goes when the last card leaves.
 *
 * **Not answered yet, or not at all**, one line under its own heading, saying which. A failure
 * gets the daemon's own refusal rather than a sentence of ours.
 *
 * **Focus.** A decision removes the card it was made on, and with it the button that had
 * focus, so focus falls to `<body>` and a keyboard user starts again from the top of the
 * page. Here it is moved to the heading of the card that took the removed one's place, or to
 * the section's own title when none is left. Only when focus really was inside this section
 * and really did fall: a poll that drops a row while somebody is typing elsewhere must not
 * take their cursor.
 *
 * `Section` and `Panel` both render an `h2` carrying the title, so the page's
 * outline is the same whichever shape a section is in.
 */
function Queue({
  title,
  view,
  what,
  says,
  count,
  aside,
  notes,
  children,
}: {
  title: string;
  view: Reading<unknown>;
  /** What is being read, for the sentence a wait or a failure puts on screen. */
  what: string;
  /** The absence, for the moment a section has just been emptied. */
  says: string;
  /** How many rows there are. Zero is the empty shape. */
  count: number;
  /** The heading's corner, when the count in it is not `count`. */
  aside?: ReactNode;
  /** What the last decision did, which outlives the row it was made on. */
  notes?: ReactNode;
  children: ReactNode;
}) {
  const root = useRef<HTMLDivElement>(null);
  const [hadRows, setHadRows] = useState(false);
  if (count > 0 && !hadRows) setHadRows(true);

  const focusHeld = useRef(false);
  const focusedAt = useRef<number | null>(null);
  const previous = useRef(count);

  function headings(): HTMLElement[] {
    return Array.from(
      root.current?.querySelectorAll<HTMLElement>("[data-waiting-heading]") ?? [],
    );
  }

  function noteFocus(event: FocusEvent<HTMLDivElement>) {
    focusHeld.current = true;
    const card = (event.target as HTMLElement).closest("li");
    const heading = card?.querySelector<HTMLElement>("[data-waiting-heading]");
    const at = heading === null || heading === undefined ? -1 : headings().indexOf(heading);
    focusedAt.current = at === -1 ? null : at;
  }

  function noteBlur(event: FocusEvent<HTMLDivElement>) {
    // Focus going to another element is the person moving on. Focus going to nothing is also
    // what a removed or disabled button looks like, and that is exactly the case to catch.
    if (event.relatedTarget !== null && !root.current?.contains(event.relatedTarget as Node))
      focusHeld.current = false;
  }

  // `[count]` alone, on purpose: the effect is about the list *shrinking*, `headings` is
  // redefined on every render, and listing it would re-run this on renders that removed nothing.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useLayoutEffect(() => {
    const before = previous.current;
    previous.current = count;
    if (count >= before || !focusHeld.current || root.current === null) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body && root.current.contains(active)) return;
    focusHeld.current = false;
    const cards = headings();
    const target =
      cards.length > 0
        ? cards[Math.min(focusedAt.current ?? 0, cards.length - 1)]
        : root.current.querySelector<HTMLElement>("h2");
    if (target === null || target === undefined) return;
    if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
    target.focus();
  }, [count]);

  let body: ReactNode = null;
  if (view.rows === undefined) {
    body = (
      <Section label={title}>
        {view.error === null ? (
          <Skeleton label={`Loading ${what}`} />
        ) : (
          <ListError error={view.error} what={what} onRetry={view.retry} />
        )}
      </Section>
    );
  } else if (count === 0) {
    body = hadRows ? (
      <Section label={title}>
        <Quiet says={says} />
        {view.stale && <StaleNote dataUpdatedAt={view.dataUpdatedAt} />}
        {notes}
      </Section>
    ) : null;
  } else {
    body = (
      <Panel title={title} aside={aside ?? <Count n={count} />}>
        {view.stale && <StaleNote dataUpdatedAt={view.dataUpdatedAt} />}
        {children}
        {notes}
      </Panel>
    );
  }

  return (
    <div
      className="waiting-queue"
      ref={root}
      onFocusCapture={noteFocus}
      onBlurCapture={noteBlur}
    >
      {body}
    </div>
  );
}

function ListError({
  error,
  what,
  onRetry,
}: {
  error: unknown;
  what: string;
  onRetry: () => void;
}) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote onRetry={onRetry}>
      the {DAEMON_NAME} is not responding — nothing is known about {what}
    </ErrorNote>
  );
}

/**
 * What the decision doors say when they say no.
 *
 * The 409 is the one worth writing copy for: on this page it almost always means
 * somebody — or something — answered while the card sat on screen, and the row
 * is about to disappear on its own. "Something about this has already changed",
 * the shared floor's sentence, describes that correctly but leaves the person
 * wondering whether they have to do anything.
 */
const DECISION_SENTENCES: Record<string, string> = {
  conflict:
    "this one was already answered — the list clears it on the next read, and nothing further is needed",
  not_found: "that proposal is gone; there is nothing left to decide",
  unprocessable:
    "the daemon accepted the request and found nothing usable inside the proposal to act on",
  internal:
    "the daemon failed while carrying the decision out — nothing was changed",
};

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so a
 * bare 409 arrives carrying the word "Conflict" — which is the code spelled with
 * a capital letter and explains nothing. Four words is the line between a status
 * word ("Unprocessable Entity") and a sentence the daemon wrote on purpose
 * ("these two people carry standing decisions that disagree — …").
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

export function DecisionRefusal({
  error,
  trustProse,
}: {
  error: unknown;
  trustProse: boolean;
}) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the {DAEMON_NAME} is not responding — nothing was decided</ErrorNote>
    );
  }
  const sentences = trustProse
    ? { ...DECISION_SENTENCES, ...daemonProse(error) }
    : DECISION_SENTENCES;
  return <RefusalNote refusal={error} sentences={sentences} />;
}

/**
 * What an approval did, said afterwards.
 *
 * `queued` first: a team action's approval is not a result at all, it is a
 * promise the núcleo keeps on its next tick, and it must never be mistaken for
 * one of the outcomes below. `closed` follows and is a **success** that
 * changed nothing, and the sentence the daemon sends with it is better than
 * anything derivable from the other fields. Reading it as a failure would send
 * somebody hunting for a rule that was right not to be written.
 */
function outcomeSentence(outcome: ApprovalOutcome): string {
  if (typeof outcome.queued === "string" && outcome.queued !== "")
    return outcome.queued;
  if (typeof outcome.closed === "string" && outcome.closed !== "")
    return outcome.closed;
  if (typeof outcome.resume_run_id === "number")
    return `run ${outcome.resume_run_id} is going again`;
  if (typeof outcome.exclusion_id === "number")
    return `rule ${outcome.exclusion_id} is in force`;
  if (outcome.merged === true) return "the two are one contact now";
  if (typeof outcome.event_id === "number")
    return `event ${outcome.event_id} is in the calendar`;
  return "the daemon recorded the decision";
}

function DecisionNotes({
  outcome,
  approveError,
  refuseError,
}: {
  outcome: ApprovalOutcome | undefined;
  approveError: unknown;
  refuseError: unknown;
}) {
  return (
    <>
      {outcome !== undefined && (
        <p className="waiting-outcome" role="status">
          {outcomeSentence(outcome)}
        </p>
      )}
      {/* The approve door sends prose with its refusals; the refuse and dismiss
          doors answer with a bare status, so there is nothing of theirs to trust. */}
      {approveError !== null && (
        <DecisionRefusal error={approveError} trustProse />
      )}
      {refuseError !== null && (
        <DecisionRefusal error={refuseError} trustProse={false} />
      )}
    </>
  );
}

/* ---------------------------------------------------------------- tool input -- */

export interface InputField {
  name: string;
  value: string;
}

/**
 * A proposal's arguments, as fields rather than as JSON.
 *
 * `null` out means the text was not an object — a bare command string, most
 * often — and the caller shows it as the one unnamed line it is. Braces, quotes
 * and indentation are dropped on purpose: the person reading this is deciding
 * whether an action may happen, and every character that is punctuation rather
 * than content is a character between them and that decision.
 */
export function readToolInput(raw: string): InputField[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return null;
  return Object.entries(parsed as Record<string, unknown>).map(
    ([name, value]) => ({
      name,
      value: plainValue(value),
    }),
  );
}

/** One value, flattened to a line of text. Nested structure is described, never dumped. */
function plainValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === null) return "nothing";
  if (Array.isArray(value)) return value.map(plainValue).join(", ");
  if (typeof value === "object") {
    return Object.entries(value as Record<string, unknown>)
      .map(([name, nested]) => `${name} ${plainValue(nested)}`)
      .join("; ");
  }
  return String(value);
}

export function ToolInput({ raw }: { raw: string | null }) {
  if (raw === null || raw.trim() === "") return null;
  const fields = readToolInput(raw);
  // An object with no fields in it is an action that carried no arguments, and
  // rendering `{}` would be the one dump this whole function exists to avoid.
  if (fields !== null && fields.length === 0) return null;
  const lines: InputField[] = fields ?? [{ name: "input", value: raw.trim() }];
  return (
    <dl className="waiting-input">
      {lines.map((line) => (
        <div className="waiting-input-line" key={line.name}>
          <dt className="waiting-input-name">{line.name}</dt>
          <dd className="waiting-input-value">{line.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/* ---------------------------------------------------------------- the provenance -- */

/** One thing a turn read before it reached for an action. */
interface ReadEntry {
  tool?: unknown;
  arguments?: unknown;
}

/**
 * Where the idea came from, under the record of what it was.
 *
 * `ToolInput` answers what was going to happen; this answers who put it in
 * the turn's head, and only the second one decides. "Email accounts and ask them
 * to change the bank details" reads identically whether the owner asked for it
 * or a page did, so a card carrying only the first question gets answered by
 * guessing.
 *
 * **Absent renders as nothing, never as "read nothing".** `null` covers two
 * cases the row cannot tell apart — a refusal that fired on whose work it is
 * rather than on what was read, and a recording that failed — and a card that
 * printed "this turn read nothing" would be wrong in the second one, in the
 * direction that makes a contaminated action look clean.
 */
export function ReadFrom({ raw }: { raw: string | null }) {
  if (raw === null || raw.trim() === "") return null;
  let entries: ReadEntry[];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed) || parsed.length === 0) return null;
    entries = parsed as ReadEntry[];
  } catch {
    return null;
  }
  return (
    <div className="waiting-provenance">
      <p className="waiting-provenance-head">this turn had read</p>
      <ul className="waiting-provenance-list">
        {entries.map((entry, index) => {
          const tool =
            typeof entry.tool === "string" ? entry.tool : "something unnamed";
          // The arguments are stored as the daemon received them, so the url that
          // decides the question is in there and nowhere else. Flattened through
          // the same reader the action's own input uses, for the same reason:
          // braces are characters between a person and a decision.
          const detail =
            typeof entry.arguments === "string"
              ? (readToolInput(entry.arguments) ?? [])
                  .map((field) => field.value)
                  .join(", ")
              : "";
          return (
            <li className="waiting-provenance-line" key={`${tool}-${index}`}>
              <span className="waiting-provenance-tool">{tool}</span>
              {detail !== "" && (
                <span className="waiting-provenance-detail">{detail}</span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/* ------------------------------------------------------- 1. wheel requests -- */

function WheelRequestSection({ view }: { view: Reading<WheelRequest> }) {
  const approve = useApproveWheel();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (session) => session.proposal_id,
  );

  return (
    <Queue
      title={QUEUES.wheel.title}
      view={view}
      what="the open browser sessions"
      count={items.length}
      says={QUEUES.wheel.says}
      // The receipt rides in the notes and not in the children: approving the last card
      // empties the list, and the link to the session is the one thing the person needs next.
      notes={
        <>
          {approve.isSuccess && approve.variables?.seat === "shell" && (
            <p className="waiting-outcome" role="status">
              the shell has the wheel — <Link to="/web">drive it on Web</Link>
            </p>
          )}
          <DecisionNotes
            outcome={undefined}
            approveError={approve.isError ? approve.error : null}
            refuseError={reject.isError ? reject.error : null}
          />
        </>
      }
    >
      <Rows label={QUEUES.wheel.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((session) => (
          <Row className="waiting-card" dense={dense(items.length)} key={session.id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>
                wheel #{session.proposal_id}
              </span>
              <span className="waiting-meta">
                {session.project_id ?? "no project"}
              </span>
              <span className="waiting-meta">
                {session.profile_kind} {session.profile_id}
              </span>
              {session.run_id !== null && (
                <Link
                  className="waiting-row-link"
                  to="/runs/$runId"
                  params={{ runId: String(session.run_id) }}
                >
                  run {session.run_id}
                </Link>
              )}
              <RelativeTime at={session.opened_at} />
            </div>
            <dl className="waiting-facts">
              <div className="waiting-fact">
                <dt>asked for</dt>
                {/* Verbatim, punycode and all. A host prettified out of its
                    `xn--` form is exactly the lookalike this decision exists
                    to catch, and the shell must never be the thing that hides
                    it. */}
                <dd className="waiting-url">{session.requested_url}</dd>
              </div>
              {session.final_url !== session.requested_url && (
                <div className="waiting-fact">
                  <dt>ended at</dt>
                  <dd className="waiting-url">{session.final_url}</dd>
                </div>
              )}
              <div className="waiting-fact">
                <dt>rule</dt>
                <dd>{session.rule}</dd>
              </div>
            </dl>
            {session.refusal !== null && (
              <Reasoning>{session.refusal}</Reasoning>
            )}
            <div className="waiting-actions">
              {session.shell_eligible && (
                <ConfirmButton
                  label={`Approve wheel #${session.proposal_id}`}
                  confirmLabel="Approve and drive it here in the shell"
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "shell" })
                  }
                />
              )}
              {session.shell_eligible ? (
                <ConfirmButton
                  label={`Open real window for wheel #${session.proposal_id}`}
                  confirmLabel="Open a real browser here"
                  variant="ghost"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "window" })
                  }
                />
              ) : (
                <ConfirmButton
                  label={`Open real window for wheel #${session.proposal_id}`}
                  confirmLabel="Open a real browser here"
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    approve.mutate({ proposalId: session.proposal_id, sessionId: session.id, seat: "window" })
                  }
                />
              )}
              <ConfirmButton
                label={`Decline wheel #${session.proposal_id}`}
                confirmLabel="Decline and close the session"
                subject={`#${session.proposal_id}`}
                variant="ghost"
                disabled={reject.isPending}
                onArmedChange={onArmedChange}
                onConfirm={() => reject.mutate(session.proposal_id)}
              />
            </div>
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}

/* ----------------------------------------------------- 2. action approvals -- */

/* --------------------------------------------- the batch on the approvals -- */

function ActionApprovalSection({ view }: { view: Reading<Proposal> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const decline = useDeclineAction();
  /**
   * A second pair of doors for the batch, deliberately.
   *
   * `DecisionNotes` below reads the first pair, and it is a note about the last *single*
   * decision — what it did, or why it did not. A batch that failed on one row out of five
   * would overwrite that with its own refusal, on a note that belongs to a different
   * gesture. The batch reports per row instead, beside the row.
   */
  const batchApprove = useApproveProposal();
  const batchReject = useRejectProposal();
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [failed, setFailed] = useState<Map<number, unknown>>(new Map());
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );
  /**
   * The selection, read through the list rather than out of the Set.
   *
   * A row can be decided somewhere else while this page is open — the run resumed, another
   * window approved it — and it leaves the queue on the next read. A count taken from the Set
   * would keep counting it, and the interlock would name a decision nobody can take.
   */
  const chosen = items.filter((proposal) => selected.has(proposal.id));
  const busy = batchApprove.isPending || batchReject.isPending;
  // One decision at a time per queue: approve, decline and reject each continue or end the same
  // run, and two in flight would leave the second to lose a race the daemon settles silently.
  const deciding = approve.isPending || decline.isPending || reject.isPending;

  function toggle(id: number, on: boolean) {
    setSelected((current) => {
      const next = new Set(current);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  /**
   * One call per row, one at a time, in the order they are on screen.
   *
   * The ids are taken before the first call and not read again: every decision invalidates
   * the queue `onSettled`, so the list can refetch under the loop, and a batch that re-read
   * it would be deciding a different set from the one that was named on the button. Sequential
   * and not `Promise.all` for the same reason `Files.tsx` is: the núcleo answers each of these
   * with a transaction of its own, and a failure has to be attributable to a row.
   */
  async function decide(door: {
    mutateAsync: (id: number) => Promise<unknown>;
  }) {
    const ids = chosen.map((proposal) => proposal.id);
    setFailed(new Map());
    const failures = new Map<number, unknown>();
    for (const id of ids) {
      try {
        await door.mutateAsync(id);
      } catch (error) {
        failures.set(id, error);
      }
    }
    setFailed(failures);
    // What went through is gone on the next read; what did not is still a decision to take.
    setSelected(new Set(failures.keys()));
  }

  return (
    <Queue
      title={QUEUES.approvals.title}
      view={view}
      what="the approval queue"
      count={items.length}
      says={QUEUES.approvals.says}
      notes={
        <DecisionNotes
          // A decline continues the run too, so its outcome is the note the owner reads;
          // whichever of the two was sent last is the one that answers.
          outcome={
            decline.submittedAt > approve.submittedAt ? decline.data : approve.data
          }
          approveError={approve.isError ? approve.error : null}
          refuseError={
            reject.isError ? reject.error : decline.isError ? decline.error : null
          }
        />
      }
    >
      <>
        {/* The bar is drawn whole whenever there is more than one row to pick from, and
            nothing in it changes size as boxes are ticked: the buttons carry no count and no
            subject (both would resize them, and the Decline a slip lands on with them), the
            selection line is one truncating line, and an empty selection disables rather than
            removes. Approve opens the bar and Decline closes it, so the two never sit side by
            side. */}
        {items.length > 1 && (
          <div className="waiting-bulk">
            <div className="waiting-bulk-head">
              {/* A count and nothing else. The identifiers are already on screen, as the
                  ticked boxes on the cards below; listing them here grew the line with every
                  tick and ran out of room past a handful. */}
              <p className="waiting-bulk-said" role="status">
                {chosen.length} of {items.length} selected
              </p>
              <Button
                variant="quiet"
                disabled={busy || chosen.length === items.length}
                onClick={() =>
                  setSelected(new Set(items.map((proposal) => proposal.id)))
                }
              >
                Select all
              </Button>
              <Button
                variant="quiet"
                disabled={busy || chosen.length === 0}
                onClick={() => setSelected(new Set())}
              >
                Clear
              </Button>
            </div>
            <div className="waiting-actions-split">
              <ConfirmButton
                label="Approve"
                confirmLabel="Approve the selected"
                variant="approve"
                disabled={busy || chosen.length === 0}
                onArmedChange={onArmedChange}
                onConfirm={() => void decide(batchApprove)}
              />
              <ConfirmButton
                label="Decline"
                confirmLabel="Decline and end their runs"
                variant="ghost"
                disabled={busy || chosen.length === 0}
                onArmedChange={onArmedChange}
                onConfirm={() => void decide(batchReject)}
              />
            </div>
          </div>
        )}
        <Rows label={QUEUES.approvals.title} className={dense(items.length) ? "waiting-dense" : undefined}>
          {items.map((proposal) => (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                {/* The box is the smallest target on the card and the one used most when a
                    queue is worked through, so the label around it is what is clickable:
                    28 by 28 CSS pixels, with the box centred in it (`waiting.css`). */}
                <label className="waiting-select-hit">
                  <input
                    className="waiting-select"
                    type="checkbox"
                    aria-label={`Select approval #${proposal.id}`}
                    checked={selected.has(proposal.id)}
                    disabled={busy}
                    onChange={(event) =>
                      toggle(proposal.id, event.target.checked)
                    }
                  />
                </label>
                <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>approval #{proposal.id}</span>
                <span className="waiting-card-title">
                  {proposal.tool_name ?? "an action that names no tool"}
                </span>
                {/* Where it came from is two short facts, so they sit in the head as plain
                    text: a labelled grid ("RUN", "PROJECT") spent a row on words that say
                    what their values already say. */}
                {proposal.run_id === null ? (
                  <span className="waiting-meta">no run</span>
                ) : (
                  <Link
                    className="waiting-row-link"
                    to="/runs/$runId"
                    params={{ runId: String(proposal.run_id) }}
                  >
                    run {proposal.run_id}
                  </Link>
                )}
                <span className="waiting-meta">
                  {proposal.project_id ?? "no project"}
                </span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <ApprovalContext proposal={proposal} />
              <div className="waiting-actions-split">
                <ConfirmButton
                  label="Approve"
                  confirmLabel="Approve and let this action happen"
                  variant="approve"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label="Decline, run continues"
                  confirmLabel="Decline action and keep the run going"
                  variant="ghost"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => decline.mutate(proposal.id)}
                />
                <ConfirmButton
                  label="Decline, stop run"
                  confirmLabel="Decline and end the run"
                  variant="ghost"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => reject.mutate(proposal.id)}
                />
              </div>
              {failed.has(proposal.id) && (
                <DecisionRefusal error={failed.get(proposal.id)} trustProse />
              )}
            </Row>
          ))}
        </Rows>
      </>
    </Queue>
  );
}

/** One step a run took, as `run_recent` carries it. */
interface RecentStep {
  name: string | null;
  detail: string | null;
}

/** The run's last steps, or none when the text is absent or not the array it should be. */
export function readRecentSteps(raw: string | null | undefined): RecentStep[] {
  if (raw === null || raw === undefined || raw.trim() === "") return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (step): step is RecentStep => typeof step === "object" && step !== null,
    );
  } catch {
    return [];
  }
}

/**
 * How a run's prompt opens, as the one paragraph that says what the run is for.
 *
 * The daemon cuts the prompt at 600 characters; the first paragraph is where a task, a chat
 * message or a resolver's brief says what it is, and the rest is instructions about how.
 */
export function taskLine(prompt: string | null | undefined): string | null {
  if (prompt === null || prompt === undefined) return null;
  const first = prompt.trim().split(/\n\s*\n/)[0]?.trim() ?? "";
  return first === "" ? null : first;
}

/**
 * What a person needs beside the command to answer an approval.
 *
 * The command alone says what would happen, and the classifier's reason says only why it was
 * stopped — "unrecognized shell commands require approval" is true of every second line an agent
 * writes. Whether it SHOULD happen turns on what the run is for, where it is standing, what it
 * was doing just before, and what it had read; so the card says those, in that order, under the
 * command it is asking to run.
 */
function ApprovalContext({ proposal }: { proposal: Proposal }) {
  const task = taskLine(proposal.run_prompt);
  const steps = readRecentSteps(proposal.run_recent);
  const status = proposal.run_status ?? null;
  return (
    <>
      <div>
        <p className="waiting-approval-label">held because</p>
        <Reasoning>
          {proposal.reasoning.trim() === ""
            ? "nothing was recorded about why this was stopped"
            : proposal.reasoning}
        </Reasoning>
      </div>
      <ToolInput raw={proposal.tool_input} />
      {proposal.run_cwd && (
        <p className="waiting-meta" title={proposal.run_cwd}>
          runs in <code>{proposal.run_cwd}</code>
        </p>
      )}
      {status !== null && status !== "awaiting_approval" && (
        <p className="waiting-notice" role="status">
          The run is {status.replace(/_/g, " ")}, not waiting on this — approving resumes nothing.
        </p>
      )}
      {(task !== null || steps.length > 0) && (
        <div className="waiting-provenance">
          {task !== null && (
            <>
              <p className="waiting-provenance-head">the run was asked to</p>
              <Reasoning>{task}</Reasoning>
            </>
          )}
          {steps.length > 0 && (
            <>
              <p className="waiting-provenance-head">just before, it ran</p>
              <ul
                className="waiting-provenance-list"
                aria-label={`Steps before approval #${proposal.id}`}
              >
                {steps.map((step, index) => (
                  <li className="waiting-provenance-line" key={index}>
                    <span className="waiting-provenance-tool">
                      {step.name ?? "a step"}
                    </span>
                    {step.detail && (
                      <span className="waiting-provenance-detail">
                        {step.detail}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
      <ReadFrom raw={proposal.read_from} />
    </>
  );
}

/* --------------------------------------------------------- 3. team actions -- */

/**
 * What a department has asked the núcleo to carry out under a `propose` grant.
 *
 * Approving is not the action itself — the núcleo carries it out on its next
 * tick, about ten seconds later — so the card shows two facts side by side
 * rather than collapsing them: the human decision, and separately, whether the
 * núcleo has gotten to it yet.
 */
function TeamActionSection({
  view,
  executing,
}: {
  view: Reading<Proposal>;
  executing: TeamAction[] | undefined;
}) {
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      title={QUEUES.teamActions.title}
      view={view}
      what="the team's action requests"
      count={items.length}
      says={QUEUES.teamActions.says}
    >
      <Rows label={QUEUES.teamActions.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => (
          <TeamActionCard
            key={proposal.id}
            proposal={proposal}
            count={items.length}
            executing={executing}
            onArmedChange={onArmedChange}
          />
        ))}
      </Rows>
    </Queue>
  );
}

/** One team action's payload, per `tool_name` — the three kinds a grant can ever name. */
function payloadFields(
  kind: string | null,
  parsed: Record<string, unknown>,
): InputField[] {
  const field = (value: unknown): string =>
    value === undefined ? "not given" : plainValue(value);
  switch (kind) {
    case "send_email":
      return [
        { name: "to", value: field(parsed.to) },
        { name: "subject", value: field(parsed.subject) },
        { name: "body", value: field(parsed.body) },
      ];
    case "file_document":
      return [
        { name: "path", value: field(parsed.path) },
        { name: "title", value: field(parsed.title) },
      ];
    case "calendar_event":
      return [
        { name: "title", value: field(parsed.title) },
        { name: "start", value: field(parsed.start) },
        { name: "duration", value: field(parsed.duration) },
      ];
    default:
      return Object.entries(parsed).map(([name, value]) => ({
        name,
        value: plainValue(value),
      }));
  }
}

/**
 * A team action's payload, as fields a person can actually decide on.
 *
 * `tool_input` is the daemon's canonical re-serialisation, so parsing it a
 * second time can fail on data that is not ours to fix — the raw string is the
 * fallback, never a blank card.
 */
function TeamActionPayload({
  kind,
  raw,
}: {
  kind: string | null;
  raw: string | null;
}) {
  if (raw === null || raw.trim() === "") return null;
  const parsed = parseActionPayload(raw);
  if (parsed === null) {
    return <p className="waiting-payload-raw">{raw}</p>;
  }
  const fields = payloadFields(kind, parsed);
  if (fields.length === 0) return null;
  return (
    <dl className="waiting-input">
      {fields.map((line) => (
        <div className="waiting-input-line" key={line.name}>
          <dt className="waiting-input-name">{line.name}</dt>
          <dd className="waiting-input-value">{line.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/**
 * Which department asked, who in it, and what it was working on.
 *
 * A team action or a recruit names what would happen and why the asker wants it, but not who the
 * asker is or what job it is doing — and "send this email" is a different decision coming from
 * the director of a reconciliation than from a specialist halfway through one item of it.
 */
function DepartmentContext({ proposal }: { proposal: Proposal }) {
  const team = proposal.team_name ?? null;
  const runId = proposal.team_run_id ?? null;
  const task = taskLine(proposal.team_request);
  if (team === null && runId === null) return null;
  return (
    <>
      <p className="waiting-meta">
        asked by {proposal.asked_by ?? "an agent the daemon no longer names"}
        {proposal.asked_for ? (
          <>
            , working on <q>{proposal.asked_for}</q>
          </>
        ) : null}
        {" — "}
        {runId !== null ? (
          <Link className="waiting-row-link" to="/team-runs/$runId" params={{ runId }}>
            {team ?? "a department"}
          </Link>
        ) : (
          team
        )}
      </p>
      {task !== null && (
        <div className="waiting-provenance">
          <p className="waiting-provenance-head">the department was asked to</p>
          <Reasoning>{task}</Reasoning>
        </div>
      )}
    </>
  );
}

function TeamActionCard({
  proposal,
  count,
  executing,
  onArmedChange,
}: {
  proposal: Proposal;
  /** How many rows the list has, which is what decides how tight this one is. */
  count: number;
  executing: TeamAction[] | undefined;
  onArmedChange: (armed: boolean) => void;
}) {
  const approve = useApproveTeamAction();
  const reject = useRejectProposal();
  // `GET /team-actions` lists only `pending` and `working`, so an action that
  // moved on between polls simply is not in this list — absent is not a state.
  const action = executing?.find((row) => row.proposal_id === proposal.id);

  return (
    <Row className="waiting-card" dense={dense(count)}>
      <div className="waiting-card-head">
        <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>team action #{proposal.id}</span>
        <span className="waiting-card-title">
          {proposal.tool_name ?? "an action that names no tool"}
        </span>
        <span className="waiting-meta">
          {proposal.project_id ?? "no project"}
        </span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <DepartmentContext proposal={proposal} />
      <dl className="waiting-facts">
        <div className="waiting-fact">
          <dt>execution</dt>
          <dd>
            {action === undefined ? (
              <span className="waiting-meta">
                the execution state is not known yet
              </span>
            ) : (
              <StateBadge
                domain="team_action"
                state={teamActionState(action)}
              />
            )}
          </dd>
        </div>
      </dl>
      <Reasoning>
        {proposal.reasoning.trim() === ""
          ? "nothing was recorded about why"
          : proposal.reasoning}
      </Reasoning>
      <TeamActionPayload kind={proposal.tool_name} raw={proposal.tool_input} />
      <div className="waiting-actions-split">
        <ConfirmButton
          label="Approve"
          confirmLabel="Approve and let this action happen"
          variant="approve"
          disabled={approve.isPending}
          onArmedChange={onArmedChange}
          onConfirm={() => approve.mutate(proposal.id)}
        />
        <ConfirmButton
          label="Decline"
          confirmLabel="Decline and close the action"
          variant="ghost"
          disabled={reject.isPending}
          onArmedChange={onArmedChange}
          onConfirm={() => reject.mutate(proposal.id)}
        />
      </div>
      <DecisionNotes
        outcome={approve.data}
        approveError={approve.isError ? approve.error : null}
        refuseError={reject.isError ? reject.error : null}
      />
    </Row>
  );
}

/* ---------------------------------------------------------- 4. recruitment -- */

/**
 * The specialists directors ask for, editable at the moment of decision.
 *
 * Unlike everything else in this queue, this is the one approval a person may
 * correct before granting it — `agent::validate` runs over what was approved,
 * not over what was proposed.
 */
function RecruitmentSection({ view }: { view: Reading<Proposal> }) {
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      title={QUEUES.recruits.title}
      view={view}
      what="the recruitment requests"
      count={items.length}
      says={QUEUES.recruits.says}
    >
      <Rows label={QUEUES.recruits.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => (
          <RecruitmentCard
            key={proposal.id}
            proposal={proposal}
            count={items.length}
            onArmedChange={onArmedChange}
          />
        ))}
      </Rows>
    </Queue>
  );
}

/** What the catalogue accepts for `engine` and `tool_policy` — the same lists `Agents.tsx` offers. */
const RECRUIT_ENGINES: AgentEngine[] = ["claude", "codex", "local"];
const RECRUIT_TOOL_POLICIES: AgentToolPolicy[] = ["mcp_only", "none"];

/**
 * A select's options, keeping whatever the director proposed even when it is not one of them.
 *
 * A director can propose a value the daemon refuses (`read_only` is the usual one). Dropping it
 * would show the first option as if it had been proposed; keeping it, marked, shows what was
 * asked for and lets the hire's refusal say why.
 */
function OptionsWith({ current, known }: { current: string; known: string[] }) {
  return (
    <>
      {!known.includes(current) && (
        <option value={current}>
          {current === "" ? "none proposed" : `${current} (not accepted)`}
        </option>
      )}
      {known.map((value) => (
        <option key={value} value={value}>
          {value}
        </option>
      ))}
    </>
  );
}

/** Every editable field of a recruitment, seeded once from the proposed `AgentRequest`. */
function seedRecruitForm(raw: string | null): AgentRequest | null {
  if (raw === null) return null;
  const parsed = parseActionPayload(raw);
  if (parsed === null) return null;
  const text = (value: unknown): string =>
    typeof value === "string" ? value : "";
  return {
    name: text(parsed.name),
    speciality: text(parsed.speciality),
    prompt: text(parsed.prompt),
    engine: text(parsed.engine),
    model: typeof parsed.model === "string" ? parsed.model : null,
    tool_policy: text(parsed.tool_policy),
  };
}

function RecruitmentCard({
  proposal,
  count,
  onArmedChange,
}: {
  proposal: Proposal;
  /** How many rows the list has, which is what decides how tight this one is. */
  count: number;
  onArmedChange: (armed: boolean) => void;
}) {
  const hire = useHireRecruit();
  const reject = useRejectProposal();
  // Seed once: a poll tick landing mid-edit must not overwrite what the person
  // has already typed, so the effect only ever sets the form while it is null.
  const [form, setForm] = useState<AgentRequest | null>(null);
  // The proposal text the form was seeded from. A later poll can bring a different one (the
  // director revised the request); the form stays as typed, and this is what says it is behind.
  const [seededFrom, setSeededFrom] = useState<string | null>(null);
  useEffect(() => {
    if (form !== null) return;
    setForm(seedRecruitForm(proposal.tool_input));
    setSeededFrom(proposal.tool_input);
  }, [form, proposal.tool_input]);
  const proposalChanged = form !== null && proposal.tool_input !== seededFrom;
  const proposed = useMemo(() => seedRecruitForm(seededFrom), [seededFrom]);

  function loadChangedProposal() {
    setForm(seedRecruitForm(proposal.tool_input));
    setSeededFrom(proposal.tool_input);
    // The reader asked for the new values: put them where the edit starts.
    window.requestAnimationFrame(() =>
      document.getElementById(`recruit-${proposal.id}-name`)?.focus(),
    );
  }

  function field<K extends keyof AgentRequest>(key: K, value: AgentRequest[K]) {
    setForm((current) =>
      current === null ? current : { ...current, [key]: value },
    );
  }

  const idFor = (name: string) => `recruit-${proposal.id}-${name}`;
  // The caution about engine and tool policy used to be a paragraph between the fields, on
  // every card, all the time. It is help for two fields, so it belongs to those two fields:
  // read out when either takes focus, and on hover for anybody with a pointer.
  const carefulId = idFor("careful");
  const carefulChanged =
    form !== null &&
    proposed !== null &&
    (form.engine !== proposed.engine || form.tool_policy !== proposed.tool_policy);
  const careful =
    "Engine and tool policy are the two fields a director gets wrong most often — they are also what costs money per turn and what widens what this agent can reach.";

  return (
    <Row className="waiting-card" dense={dense(count)}>
      <div className="waiting-card-head">
        <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>recruit #{proposal.id}</span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <DepartmentContext proposal={proposal} />
      <Reasoning>
        {proposal.reasoning.trim() === ""
          ? "nothing was recorded about why"
          : proposal.reasoning}
      </Reasoning>
      {form === null ? (
        <p className="waiting-payload-raw">
          {proposal.tool_input ?? "nothing was proposed"}
        </p>
      ) : (
        // The one card in this queue that is a form. The inset is what says the
        // fields are a thing standing on the row rather than more of the row.
        <Inset>
          {proposalChanged && (
            <p className="waiting-notice waiting-recruit-changed" role="status">
              The director changed this proposal after you opened it. The fields below still hold
              the earlier values.{" "}
              <Button variant="quiet" onClick={loadChangedProposal}>
                Load the new proposal
              </Button>
            </p>
          )}
          <div className="waiting-recruit">
            <div className="waiting-recruit-form">
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("name")}>
                  Name
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("name")}
                  value={form.name}
                  onChange={(event) => field("name", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field waiting-recruit-span-2">
                <label className="waiting-recruit-label" htmlFor={idFor("speciality")}>
                  Speciality
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("speciality")}
                  value={form.speciality}
                  onChange={(event) => field("speciality", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field waiting-recruit-span-all">
                <label className="waiting-recruit-label" htmlFor={idFor("prompt")}>
                  Prompt
                </label>
                <textarea
                  className="waiting-recruit-textarea"
                  id={idFor("prompt")}
                  rows={4}
                  value={form.prompt}
                  onChange={(event) => field("prompt", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("engine")}>
                  Engine
                </label>
                <select
                  className="waiting-recruit-input waiting-recruit-input-careful"
                  id={idFor("engine")}
                  aria-describedby={carefulId}
                  value={form.engine}
                  onChange={(event) => field("engine", event.target.value)}
                >
                  <OptionsWith current={form.engine} known={RECRUIT_ENGINES} />
                </select>
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("model")}>
                  Model
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("model")}
                  placeholder="engine default"
                  value={form.model ?? ""}
                  onChange={(event) =>
                    field(
                      "model",
                      event.target.value === "" ? null : event.target.value,
                    )
                  }
                />
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("tool_policy")}>
                  Tool policy
                </label>
                <select
                  className="waiting-recruit-input waiting-recruit-input-careful"
                  id={idFor("tool_policy")}
                  aria-describedby={carefulId}
                  value={form.tool_policy}
                  onChange={(event) => field("tool_policy", event.target.value)}
                >
                  <OptionsWith current={form.tool_policy} known={RECRUIT_TOOL_POLICIES} />
                </select>
              </div>
              {/* Visible, not a `title`: this is what costs money per turn, and a tooltip waits,
                  never shows on touch and is not read by most readers. It is also the two
                  fields' description. */}
              <p
                className={
                  carefulChanged
                    ? "waiting-recruit-careful waiting-recruit-careful-changed waiting-recruit-span-all"
                    : "waiting-recruit-careful waiting-recruit-span-all"
                }
                id={carefulId}
              >
                {careful}
                {carefulChanged && " Changed from what the director proposed."}
              </p>
            </div>
          </div>
        </Inset>
      )}
      <div className="waiting-actions-split">
        <ConfirmButton
          label="Hire"
          confirmLabel="Hire and add them to the roster"
          variant="approve"
          disabled={hire.isPending || form === null}
          onArmedChange={onArmedChange}
          onConfirm={() => {
            if (form !== null)
              hire.mutate({ proposalId: proposal.id, hire: form });
          }}
        />
        <ConfirmButton
          label="Decline"
          confirmLabel="Decline — the team may ask again"
          variant="ghost"
          disabled={reject.isPending}
          onArmedChange={onArmedChange}
          onConfirm={() => reject.mutate(proposal.id)}
        />
      </div>
      {hire.isSuccess && hire.data !== undefined && (
        <p className="waiting-outcome" role="status">
          agent {hire.data.agent_id} is hired and on the roster
        </p>
      )}
      {hire.isError && <DecisionRefusal error={hire.error} trustProse />}
      {reject.isError && (
        <DecisionRefusal error={reject.error} trustProse={false} />
      )}
    </Row>
  );
}

/* ------------------------------------------------------- 5. contact merges -- */

/**
 * One side of a suggested merge, under the word for what happens to it.
 *
 * `kept` and `absorbed` are a heading and not a field label — the whole point of
 * showing both sides is that the merge is not symmetrical, and which of the two
 * survives is the first thing a reader has to know. `Section` is the heading
 * rank; the label rank it used to be written in is what a `dt` gets, and at 11px
 * the two are told apart by 0.06em of tracking and nothing else.
 */
function MergeSideView({ side, role }: { side: MergeSide; role: string }) {
  return (
    <Section label={role} level={3}>
      <div className="waiting-side">
        <p className="waiting-side-name">
          {side.display_name ?? "no name recorded"}
        </p>
        <ul className="waiting-addresses">
          {side.addresses.map((address) => (
            <li key={address}>{address}</li>
          ))}
        </ul>
        <p className="waiting-meta">{side.messages_in} messages in</p>
        <p className="waiting-verdict">
          {side.verdict === null
            ? "no standing decision"
            : `standing decision: ${side.verdict}`}
        </p>
      </div>
    </Section>
  );
}

/** Two standing decisions that disagree, which is the 409 this card can predict. */
function verdictsConflict(suggestion: MergeSuggestion): boolean {
  const keep = suggestion.keep.verdict;
  const absorb = suggestion.absorb.verdict;
  return keep !== null && absorb !== null && keep !== absorb;
}

function ContactMergeSection({ view }: { view: Reading<MergeSuggestion> }) {
  const decide = useDecideContactMerge();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (suggestion) => suggestion.proposal_id,
  );

  return (
    <Queue
      title={QUEUES.merges.title}
      view={view}
      what="the suggested merges"
      count={items.length}
      says={QUEUES.merges.says}
      /* One mutation for both verdicts, so the refusal is shown once — and its
         prose is trusted, because the 409 here is the refusal that names the two
         decisions that disagree. */
      notes={
        <DecisionNotes
          outcome={decide.data}
          approveError={decide.isError ? decide.error : null}
          refuseError={null}
        />
      }
    >
      <Rows label={QUEUES.merges.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((suggestion) => (
          <Row className="waiting-card" dense={dense(items.length)} key={suggestion.proposal_id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>
                merge #{suggestion.proposal_id}
              </span>
              <RelativeTime at={suggestion.created_at} />
            </div>
            <Reasoning>{suggestion.reasoning}</Reasoning>
            <div className="waiting-merge">
              <MergeSideView side={suggestion.keep} role="kept" />
              <MergeSideView side={suggestion.absorb} role="absorbed" />
            </div>
            {/* Said before the button rather than after the 409: the daemon
                refuses a merge whose two people carry decisions that
                contradict, and the remedy is to settle one of them first. */}
            {verdictsConflict(suggestion) && (
              <ConflictNote>
                These two carry standing decisions that disagree, so the daemon
                will refuse the merge — settle one of them and decide this
                again.
              </ConflictNote>
            )}
            <div className="waiting-actions">
              <ConfirmButton
                label={`Merge #${suggestion.proposal_id}`}
                confirmLabel="Merge — they are one person"
                subject={`#${suggestion.proposal_id}`}
                variant="approve"
                disabled={decide.isPending}
                onArmedChange={onArmedChange}
                onConfirm={() =>
                  decide.mutate({
                    proposalId: suggestion.proposal_id,
                    verdict: "approve",
                  })
                }
              />
              <ConfirmButton
                label={`Decline merge #${suggestion.proposal_id}`}
                confirmLabel="Decline — they are different people"
                subject={`#${suggestion.proposal_id}`}
                variant="quiet"
                disabled={decide.isPending}
                onArmedChange={onArmedChange}
                onConfirm={() =>
                  decide.mutate({
                    proposalId: suggestion.proposal_id,
                    verdict: "reject",
                  })
                }
              />
            </div>
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}

/* --------------------------------------------------- 7. exclusion requests -- */

interface ExclusionPair {
  low: number | null;
  high: number | null;
  paths: string[];
}

/** The pair an exclusion request names, out of the JSON `exclusion::propose` wrote. */
function readExclusionPair(raw: string | null): ExclusionPair {
  const empty: ExclusionPair = { low: null, high: null, paths: [] };
  if (raw === null || raw.trim() === "") return empty;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return empty;
  }
  if (typeof parsed !== "object" || parsed === null) return empty;
  const body = parsed as Record<string, unknown>;
  const paths = Array.isArray(body.paths)
    ? body.paths.filter((path): path is string => typeof path === "string")
    : [];
  return {
    low: typeof body.job_low === "number" ? body.job_low : null,
    high: typeof body.job_high === "number" ? body.job_high : null,
    paths,
  };
}

function ExclusionRequestSection({ view }: { view: Reading<Proposal> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      title={QUEUES.exclusions.title}
      view={view}
      what="the exclusion requests"
      count={items.length}
      says={QUEUES.exclusions.says}
      notes={
        <DecisionNotes
          outcome={approve.data}
          approveError={approve.isError ? approve.error : null}
          refuseError={reject.isError ? reject.error : null}
        />
      }
    >
      <Rows label={QUEUES.exclusions.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => {
          const pair = readExclusionPair(proposal.tool_input);
          return (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>request #{proposal.id}</span>
                <span className="waiting-card-title">
                  {pair.low === null || pair.high === null
                    ? "a pair this request does not name"
                    : `jobs ${pair.low} and ${pair.high}`}
                </span>
                <span className="waiting-meta">
                  {proposal.project_id ?? "no project"}
                </span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <Reasoning>
                {proposal.reasoning.trim() === ""
                  ? "nothing was recorded about why"
                  : proposal.reasoning}
              </Reasoning>
              {(proposal.job_low_task || proposal.job_high_task) && (
                <dl className="waiting-input" aria-label={`Jobs in request #${proposal.id}`}>
                  {[
                    [pair.low, proposal.job_low_task],
                    [pair.high, proposal.job_high_task],
                  ].map(([job, task]) => (
                    <div className="waiting-input-line" key={String(job)}>
                      <dt className="waiting-input-name">job {job ?? "?"}</dt>
                      <dd className="waiting-input-value">
                        {taskLine(task as string | null) ?? "the daemon no longer has this job"}
                      </dd>
                    </div>
                  ))}
                </dl>
              )}
              {pair.paths.length > 0 && (
                <ul className="waiting-paths">
                  {pair.paths.map((path) => (
                    <li key={path}>{path}</li>
                  ))}
                </ul>
              )}
              <div className="waiting-actions">
                <ConfirmButton
                  label={`Approve request #${proposal.id}`}
                  confirmLabel="Approve — keep these two apart"
                  subject={`#${proposal.id}`}
                  variant="approve"
                  disabled={approve.isPending || pair.low === null || pair.high === null}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label={`Decline request #${proposal.id}`}
                  confirmLabel="Decline — let them run together"
                  subject={`#${proposal.id}`}
                  variant="ghost"
                  disabled={reject.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => reject.mutate(proposal.id)}
                />
              </div>
            </Row>
          );
        })}
      </Rows>
    </Queue>
  );
}

/* --------------------------------------------------------------- 7. git queue -- */

/**
 * The git queue: one status asks for a decision, and the rest is for reading.
 *
 * `awaiting_owner` is the only row this panel can be *decided* on — a merge that changes the
 * test map, held until somebody approves or declines it. `escalated` is a *normal outcome*
 * (somebody owns the conflict now, which is the queue working rather than breaking) and
 * `blocked` is terminal without being a failure (the queue will not retry it; the answer is to
 * fix the tree and submit again). Both still want a person, in that they are counted in the
 * headline, but all anyone can do is read them and put them away — so they sit behind the
 * disclosure at the bottom with the history, and not between the decision and the person
 * making it. `ui/state-map.ts` holds the tones.
 */
/** How much history to show behind the rows that want a person. */
const VCS_RECENT = 5;

function GitQueuePanel({
  history,
  waiting,
}: {
  history: Reading<VcsRequestSummary>;
  waiting: Reading<VcsRequestSummary>;
}) {
  const dismiss = useDismissVcsRequest();
  const approve = useApproveVcsRequest();
  const refuse = useRefuseVcsRequest();
  // What wants a person comes from `/waiting/git`, never from filtering the history: that listing
  // is capped at 200 and keeps every escalation that has since been settled.
  const wanted = waiting.rows ?? [];
  // One order for both groups: arming any control pins the sequence, so a poll that reorders
  // the list cannot move another request under the finger that is about to confirm.
  const { items, onArmedChange } = useOrderFreeze(wanted, (row) => row.id);
  const owed = items.filter((row) => row.status === "awaiting_owner");
  const toRead = items.filter((row) => row.status !== "awaiting_owner");
  const rows = history.rows ?? [];
  const rest = rows.filter((row) => !wanted.some((open) => open.id === row.id));
  const recent = rest.slice(0, VCS_RECENT);
  const behind = toRead.length + recent.length;

  return (
    <Queue
      title={QUEUES.git.title}
      view={waiting}
      what="the git queue"
      says={QUEUES.git.says}
      count={wanted.length}
      aside={<Count n={wanted.length} />}
      notes={
        <DecisionNotes
          outcome={undefined}
          approveError={null}
          refuseError={
            approve.isError
              ? approve.error
              : refuse.isError
                ? refuse.error
                : dismiss.isError
                  ? dismiss.error
                  : null
          }
        />
      }
    >
      {owed.length === 0 ? (
        <Quiet says="nothing in the git queue needs your approval." />
      ) : (
        <Rows label="Git requests waiting on you" className={dense(owed.length) ? "waiting-dense" : undefined}>
          {owed.map((row) => (
            <VcsRow key={row.id} row={row} count={owed.length} decision>
              <div className="waiting-actions-split">
                <ConfirmButton
                  label="Approve"
                  confirmLabel="Approve the map change"
                  variant="approve"
                  disabled={approve.isPending || refuse.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => {
                    refuse.reset();
                    dismiss.reset();
                    approve.mutate(row.id);
                  }}
                />
                <ConfirmButton
                  label="Decline"
                  confirmLabel="Decline the map change"
                  variant="ghost"
                  disabled={approve.isPending || refuse.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => {
                    approve.reset();
                    dismiss.reset();
                    refuse.mutate(row.id);
                  }}
                />
              </div>
            </VcsRow>
          ))}
        </Rows>
      )}
      {/* Everything that is read rather than decided, below what is, under its own headings.
          It sat behind a disclosure until the owner dropped it (2026-10-09); the list is
          short by construction — the escalations plus at most `VCS_RECENT` rows. */}
      {(behind > 0 || rows.length >= VCS_LIST_LIMIT) && (
        <>
          {toRead.length > 0 && (
            <div className="waiting-recent">
              <Section label="escalated or blocked" level={3}>
                <Rows label="Git requests to put away">
                  {toRead.map((row) => (
                    <VcsRow key={row.id} row={row} count={toRead.length}>
                      <div className="waiting-actions">
                        <ConfirmButton
                          label={`Put ${row.op} #${row.id} away`}
                          confirmLabel="Nobody needs to act"
                          subject={`#${row.id}`}
                          variant="quiet"
                          disabled={dismiss.isPending}
                          onArmedChange={onArmedChange}
                          onConfirm={() => dismiss.mutate(row.id)}
                        />
                      </div>
                    </VcsRow>
                  ))}
                </Rows>
              </Section>
            </div>
          )}
          {recent.length > 0 && (
            <div className="waiting-recent">
              <Section label="recently through the queue" level={3}>
                <Rows label="Recent git requests">
                  {recent.map((row) => (
                    <VcsRow key={row.id} row={row} count={recent.length} />
                  ))}
                </Rows>
              </Section>
            </div>
          )}
          {/* Nothing prunes `vcs_requests`, so this listing is the whole history and
              not a backlog. Arriving at the cap says the daemon has been running a
              while — it is not a finding and is not drawn as one. */}
          {rows.length >= VCS_LIST_LIMIT && (
            <p className="waiting-ceiling">
              the listing stops at {VCS_LIST_LIMIT} rows — this is the queue&apos;s
              whole history, kept rather than pruned, so a full listing only means
              the daemon has been running a while
            </p>
          )}
        </>
      )}
    </Queue>
  );
}

function VcsRow({
  row,
  count,
  decision,
  children,
}: {
  row: VcsRequestSummary;
  count: number;
  /** A request somebody is being asked to decide: a card, with a heading focus can land on. */
  decision?: boolean;
  children?: ReactNode;
}) {
  const conflicts = conflictedFiles(row.output_tail);
  const next = gitNextStep(row);
  // The queue's own reason, when nothing more specific says why. A conflict's reason is written
  // for the agent that asked ("not yours to fix from here") and the files say it better.
  const reason = conflicts.length === 0 ? row.failure_reason : null;
  return (
    <Row className={decision === true ? "waiting-card" : undefined} dense={dense(count)}>
      <div className="waiting-card-head">
        <span
          className="waiting-card-id"
          tabIndex={decision === true ? -1 : undefined}
          data-waiting-heading={decision === true ? "" : undefined}
        >
          {row.op} #{row.id}
        </span>
        <StateBadge
          domain="vcs"
          state={row.status === "escalated" && next !== null && !next.yours ? "resolving" : row.status}
        />
        <span className="waiting-meta">{row.project_id}</span>
        <RelativeTime at={row.created_at} />
      </div>
      {/* Which branches, first: "merge #573" alone does not say what would be merged. */}
      {row.source != null && row.target != null && (
        <p className="waiting-git-branches">
          <code>{row.source}</code> into <code>{row.target}</code>
        </p>
      )}
      <p className="waiting-meta" title={row.repo_key}>
        {repoLabel(row.repo_key)} — asked from {row.origin}
      </p>
      {conflicts.length > 0 && (
        <div className="waiting-git-conflicts">
          <span className="waiting-meta">conflicts in</span>
          <ul aria-label={`Conflicts in ${row.op} #${row.id}`}>
            {conflicts.map((file) => (
              <li key={file}>
                <code>{file}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      {reason != null && reason !== "" && <Reasoning>{reason}</Reasoning>}
      {/* And what to do about it is its own line, not a third clause on the faint mono one. */}
      {next !== null && (
        <p className={next.yours ? "waiting-hint waiting-hint-yours" : "waiting-hint"}>
          {next.before}
          {next.run !== undefined && (
            <Link className="waiting-row-link" to="/runs/$runId" params={{ runId: String(next.run) }}>
              run #{next.run}
            </Link>
          )}
          {next.after}
        </p>
      )}
      {children}
    </Row>
  );
}

/** The repository key without Windows' `\\?\` verbatim prefix, which only git needs. */
function repoLabel(key: string): string {
  return key.replace(/^\\\\\?\\/, "");
}

/**
 * The files git named in a conflict, read from its own output.
 *
 * `CONFLICT (content): Merge conflict in a.rs` gives `a.rs`; the other kinds
 * (`modify/delete`, `rename/rename`, …) have no fixed shape, so their whole message is kept rather
 * than guessed at.
 */
export function conflictedFiles(tail: string | null | undefined): string[] {
  if (tail == null) return [];
  const found: string[] = [];
  for (const match of tail.matchAll(/^CONFLICT \([^)]*\): (.+)$/gm)) {
    const file = match[1].replace(/^Merge conflict in /, "").trim();
    if (!found.includes(file)) found.push(file);
  }
  return found;
}

/**
 * What happens next on a git request, and whether that is the reader's to do.
 *
 * Split around the run it names so the run can be a link. `yours` is the line that asks the
 * person to act; every other line says who is acting instead, which is what lets somebody leave an
 * escalation alone without wondering whether it is stuck.
 */
export function gitNextStep(
  row: VcsRequestSummary,
): { before: string; run?: number; after: string; yours: boolean } | null {
  if (row.status === "blocked") return { before: "submit it again", after: "", yours: true };
  if (row.status === "awaiting_owner")
    return { before: "changes the test map — approve to let it land", after: "", yours: true };
  if (row.status !== "escalated" || row.op !== "merge") return null;
  if (row.from_resolution === true)
    return {
      before:
        "this merge was itself a resolution landing and conflicted again, so no agent is given it — resolve it by hand, or put it away if the merge no longer matters",
      after: "",
      yours: true,
    };
  const run = row.resolution_run_id;
  if (run == null)
    return {
      before: "an agent is given this conflict within a minute — nothing to do yet",
      after: "",
      yours: false,
    };
  switch (row.resolution_status) {
    case "running":
      return { before: "", run, after: " is resolving it — nothing to do yet", yours: false };
    case "awaiting_approval":
      return { before: "", run, after: " is resolving it and waits on your approval", yours: true };
    case "completed": {
      // The resolver hands a finished resolution to the queue only when its gate passed (or never
      // ran) and its worktree is still there (`resolver::land_finished`); otherwise nothing lands
      // it, and the row is the person's.
      const gate = row.resolution_gate_status;
      if (gate != null && gate !== "passed")
        return {
          before: "",
          run,
          after: ` finished its resolution but its gate ${gate}, so the queue will not land it — open the run, land it by hand, or put it away if the merge no longer matters`,
          yours: true,
        };
      if (row.resolution_worktree_gone === true)
        return {
          before: "",
          run,
          after:
            " finished its resolution but its worktree is gone, so the queue cannot land it — resolve it by hand, or put it away if the merge no longer matters",
          yours: true,
        };
      return {
        before: "",
        run,
        after:
          " finished its resolution; the queue lands it and this clears on its own — if it stays, open the run to see why",
        yours: false,
      };
    }
    case "failed":
    case "cancelled":
    case "interrupted":
    case "timed_out":
    case "superseded":
      return {
        before: "",
        run,
        after: ` ended (${row.resolution_status}) without resolving it — resolve it by hand, or put it away if the merge no longer matters`,
        yours: true,
      };
    default:
      return { before: "an agent was given it as ", run, after: "", yours: false };
  }
}
