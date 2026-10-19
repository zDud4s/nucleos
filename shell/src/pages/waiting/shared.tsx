import { useId, useLayoutEffect, useRef, useState, type FocusEvent, type ReactNode } from "react";
import { isApiRefusal, type ApiRefusal } from "../../data/client";
import type { ApprovalOutcome } from "../../data/waiting";
import { Count, ErrorNote, Panel, Quiet, Skeleton, RefusalNote, Section, StaleNote } from "../../ui";
import { DAEMON_NAME } from "../../copy";
import type { Reading } from "./reading";

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
export function dense(count: number): boolean {
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
export function Reasoning({ children }: { children: string }) {
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
export interface QueueText {
  title: string;
  /** The absence, in the fewest words that are still true. */
  says: string;
  /** What this section is for. */
  why: ReactNode;
}

export const QUEUES = {
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
export function Queue({
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

export function DecisionNotes({
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
