import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link, useSearch } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import type { AgentRequest } from "../data/agents";
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
  useAwaitingRuns,
  useContactMerges,
  useDecideContactMerge,
  useDismissProposal,
  useApproveVcsRequest,
  useDismissVcsRequest,
  useRefuseVcsRequest,
  useExclusionRequests,
  useOpenTeamActions,
  useRecruitProposals,
  useRefusedActions,
  useDeclineAction,
  useRejectProposal,
  useSkippedItems,
  useTeamActionProposals,
  useVcsRequests,
  useVcsWaiting,
  useWheelRequests,
  type ApprovalOutcome,
  type AwaitingRun,
  type MergeSide,
  type MergeSuggestion,
  type VcsRequestSummary,
  type WheelRequest,
} from "../data/waiting";
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
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  StaleNote,
  StateBadge,
} from "../ui";
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
}

function reading<T>(query: {
  data: T[] | undefined;
  isError: boolean;
  error: unknown;
  dataUpdatedAt: number;
}): Reading<T> {
  return {
    rows: query.data,
    stale: query.isError && query.data !== undefined,
    error: query.isError ? query.error : null,
    dataUpdatedAt: query.dataUpdatedAt,
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
  const skipped = onlyProject(reading(useSkippedItems()), project);
  const refused = onlyProject(reading(useRefusedActions()), project);
  const vcs = onlyProject(reading(useVcsRequests()), project);
  const vcsWaiting = onlyProject(reading(useVcsWaiting()), project);
  const parked = onlyProject(reading(useAwaitingRuns()), project);

  const decisions = countWaitingDecisions({
    wheel: wheel.rows,
    approvals: approvals.rows,
    teamActions: teamActions.rows,
    recruits: recruits.rows,
    merges: merges.rows,
    exclusions: exclusions.rows,
    git: vcsWaiting.rows,
  });
  const records = countOf(skipped.rows, refused.rows);

  /**
   * The design's §6.4 order, with the sections that have something in them first.
   *
   * The order below is that order and is kept exactly — *within* each of the two
   * groups. What moves is emptiness. A queue whose first four headings are
   * one-line absences makes somebody scroll past them to reach the one decision
   * that is actually waiting, which is the opposite of what this page is for; and
   * on a quiet morning nothing moves at all, because the whole list is one group.
   * `sort` is stable, so nothing is reordered relative to its own neighbours.
   *
   * The split is `decisions` and `records` above: a decision is work stopped
   * mid-stride, a record is something the night already settled. Neither is
   * promoted over the other — the design's order already puts the six decisions
   * before the two records, and being stable is what preserves that.
   */
  const sections: { filled: boolean; node: ReactNode }[] = [
    {
      filled: has(wheel.rows),
      node: <WheelRequestSection key="wheel" view={wheel} />,
    },
    {
      filled: has(approvals.rows),
      node: <ActionApprovalSection key="approvals" view={approvals} />,
    },
    {
      filled: has(teamActions.rows),
      node: (
        <TeamActionSection
          key="team-actions"
          view={teamActions}
          executing={openActions.data}
        />
      ),
    },
    {
      filled: has(recruits.rows),
      node: <RecruitmentSection key="recruits" view={recruits} />,
    },
    {
      filled: has(merges.rows),
      node: <ContactMergeSection key="merges" view={merges} />,
    },
    // Never filled: it is a statement about a route the núcleo does not mount,
    // and it says the same thing on every morning there has ever been.
    { filled: false, node: <CalendarEventAbsence key="calendar" /> },
    {
      filled: has(exclusions.rows),
      node: <ExclusionRequestSection key="exclusions" view={exclusions} />,
    },
    {
      filled: has(skipped.rows),
      node: <SkippedItemsPanel key="skipped" view={skipped} />,
    },
    {
      filled: has(refused.rows),
      node: <RefusedActionsPanel key="refused" view={refused} />,
    },
    {
      filled: has(vcs.rows) || has(vcsWaiting.rows),
      node: <GitQueuePanel key="vcs" view={vcs} waiting={vcsWaiting} />,
    },
    {
      filled: has(parked.rows),
      node: <ParkedRunsPanel key="parked" view={parked} />,
    },
  ];

  return (
    <>
      <PageHeader title="Waiting" headline={headlineFor(decisions, records)} />

      {project !== undefined ? (
        <p className="waiting-narrowed" role="status">
          Only {project}. <Link to="/waiting">Show everything</Link>
        </p>
      ) : null}

      <div className="waiting-sections">
        {[...sections]
          .sort((a, b) => Number(b.filled) - Number(a.filled))
          .map((section) => section.node)}
      </div>
    </>
  );
}

/** Whether a list has anything in it. `undefined` — not answered yet — is not "filled". */
function has(rows: unknown[] | undefined): boolean {
  return (rows?.length ?? 0) > 0;
}

/** How many rows across several lists, or `undefined` while none of them has answered. */
function countOf(...lists: (unknown[] | undefined)[]): number | undefined {
  if (lists.every((list) => list === undefined)) return undefined;
  return lists.reduce<number>((total, list) => total + (list?.length ?? 0), 0);
}

/**
 * One derived sentence about the queue.
 *
 * The two counts are kept apart because they ask different things of a person:
 * a decision is work stopped mid-stride, a record is something the night already
 * settled and left a note about. Merging them would make a quiet morning with
 * nine skipped items look like nine things blocking.
 */
function headlineFor(
  decisions: number | undefined,
  records: number | undefined,
): string | undefined {
  if (decisions === undefined) return undefined;
  const first =
    decisions === 0
      ? "nothing is waiting on a decision"
      : `${decisions} waiting on a decision`;
  if (records === undefined || records === 0) return first;
  return `${first}; ${records} to read and put away`;
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
function useOrderFreeze<T>(rows: T[], idOf: (row: T) => number) {
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


/* ------------------------------------------------------------- shared parts -- */

/**
 * One section of the queue, in the shape its data allows. There are three.
 *
 * **With rows in it**, a `Panel` with the count in its corner — a bordered block,
 * because there is something in it to work through.
 *
 * **Empty**, one line under its own heading, with the paragraph that used to sit
 * above the list kept behind "why?". This is the shape that matters: an empty
 * queue is the *normal* state of this page, and eleven panels each explaining an
 * absence made the page longest on the morning nothing was wrong. The reasoning
 * is kept rather than cut — "nothing has asked for the wheel" alone reads as a
 * list that failed to load, and the sentence behind it is what makes the silence
 * a fact instead of a gap.
 *
 * **Not answered yet, or not at all**, the same one line, saying which. A failure
 * gets the daemon's own refusal rather than a sentence of ours.
 *
 * `Section` and `Panel` both render an `h2` carrying the title, so the page's
 * outline — and the eleven headings a composition test counts — is the same
 * whichever shape a section is in.
 */
function Queue({
  title,
  view,
  what,
  says,
  why,
  count,
  aside,
  notes,
  children,
}: {
  title: string;
  view: Reading<unknown>;
  /** What is being read, for the sentence a wait or a failure puts on screen. */
  what: string;
  /** The absence, in the fewest words that are still true. */
  says: string;
  /** What this section is for — the paragraph the one line replaced. */
  why?: ReactNode;
  /** How many rows there are. Zero is the empty shape. */
  count: number;
  /** The heading's corner, when the count in it is not `count`. */
  aside?: ReactNode;
  /** What the last decision did, which outlives the row it was made on. */
  notes?: ReactNode;
  children: ReactNode;
}) {
  if (view.rows === undefined) {
    return (
      <Section label={title}>
        {view.error === null ? (
          <Quiet says={`reading ${what}…`} />
        ) : (
          <ListError error={view.error} what={what} />
        )}
      </Section>
    );
  }

  if (count === 0) {
    return (
      <Section label={title}>
        <Quiet says={says}>{why}</Quiet>
        {view.stale && <StaleNote dataUpdatedAt={view.dataUpdatedAt} />}
        {notes}
      </Section>
    );
  }

  return (
    <Panel title={title} aside={aside ?? <Count n={count} />}>
      {view.stale && <StaleNote dataUpdatedAt={view.dataUpdatedAt} />}
      {children}
      {notes}
    </Panel>
  );
}

function ListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error))
    return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about {what}
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
    "the núcleo accepted the request and found nothing usable inside the proposal to act on",
  internal:
    "the núcleo failed while carrying the decision out — nothing was changed",
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

function DecisionRefusal({
  error,
  trustProse,
}: {
  error: unknown;
  trustProse: boolean;
}) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>
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
  return "the núcleo recorded the decision";
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

interface InputField {
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
function readToolInput(raw: string): InputField[] | null {
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

function ToolInput({ raw }: { raw: string | null }) {
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

/** One thing a turn read before it reached for an action. */
interface ReadEntry {
  tool?: unknown;
  arguments?: unknown;
}

/**
 * Where the idea came from, under the record of what it was.
 *
 * `ToolInput` above answers what was going to happen; this answers who put it in
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
function ReadFrom({ raw }: { raw: string | null }) {
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
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (session) => session.proposal_id,
  );

  return (
    <Queue
      title="Wheel requests"
      view={view}
      what="the open browser sessions"
      count={items.length}
      says="nothing has asked for the wheel"
      why={
        <>
          An agent has met a wall it may not climb and is asking for the window.
          Giving it the wheel opens a real browser on this machine, in the
          profile named on the card; refusing closes the session, and the run
          carries on without that page.
        </>
      }
      notes={
        <DecisionNotes
          outcome={approve.data}
          approveError={approve.isError ? approve.error : null}
          refuseError={reject.isError ? reject.error : null}
        />
      }
    >
      <Rows label="Wheel requests" className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((session) => (
          <Row className="waiting-card" dense={dense(items.length)} key={session.id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id">
                wheel #{session.proposal_id}
              </span>
              <span className="waiting-meta">
                {session.project_id ?? "no project"}
              </span>
              <span className="waiting-meta">
                {session.profile_kind} {session.profile_id}
              </span>
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
              <p className="waiting-reasoning">{session.refusal}</p>
            )}
            <div className="waiting-actions">
              <ConfirmButton
                label={`Give wheel #${session.proposal_id} the window`}
                confirmLabel="Open a real browser here"
                subject={`#${session.proposal_id}`}
                variant="approve"
                disabled={approve.isPending}
                onArmedChange={onArmedChange}
                onConfirm={() => approve.mutate(session.proposal_id)}
              />
              <ConfirmButton
                label={`Refuse wheel #${session.proposal_id}`}
                confirmLabel="Refuse and close the session"
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

/**
 * Which rows a batch decision names.
 *
 * Every identifier up to five, and above five the first three and a count. Five is the page
 * this was written for — `DESIGN.md`'s five byte-identical `git status` cards — and naming
 * them is the whole point: an interlock that says "Let these happen" and nothing else names
 * the action and not the rows, which is the defect the single `subject` was added to fix.
 * Twenty is where listing stops helping. The armed label becomes a paragraph, the
 * announcement a thirty-word sentence that `armWindowFor` then holds the control open for,
 * and neither is how somebody checks a selection — the bar's own line says all twenty, every
 * time the selection changes, which is before anybody arms anything.
 */
function subjectFor(ids: number[]): string {
  const named = ids.map((id) => `#${id}`);
  if (named.length <= 5) return named.join(", ");
  return `${named.slice(0, 3).join(", ")} and ${named.length - 3} more`;
}

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
      title="Action approvals"
      view={view}
      what="the approval queue"
      count={items.length}
      says="nothing is waiting to be let through"
      why={
        <>
          A run that reaches for something outside its permission stops where it
          is and files one of these. An empty queue means either that nothing
          has asked, or that the autopilot is off — it is not a sign that
          anything is stuck.
        </>
      }
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
        {items.length > 0 && (
          <div className="waiting-bulk">
            <Button
              variant="quiet"
              disabled={busy || chosen.length === items.length}
              onClick={() =>
                setSelected(new Set(items.map((proposal) => proposal.id)))
              }
            >
              Select all {items.length}
            </Button>
            {chosen.length > 0 && (
              <>
                <p className="waiting-bulk-said" role="status">
                  {chosen.length} selected —{" "}
                  {chosen.map((proposal) => `#${proposal.id}`).join(", ")}
                </p>
                <ConfirmButton
                  label={`Approve ${chosen.length}`}
                  confirmLabel="Let these happen"
                  subject={subjectFor(chosen.map((proposal) => proposal.id))}
                  variant="approve"
                  disabled={busy}
                  onArmedChange={onArmedChange}
                  onConfirm={() => void decide(batchApprove)}
                />
                <ConfirmButton
                  label={`Reject ${chosen.length}`}
                  confirmLabel="Refuse and end them"
                  subject={subjectFor(chosen.map((proposal) => proposal.id))}
                  variant="ghost"
                  disabled={busy}
                  onArmedChange={onArmedChange}
                  onConfirm={() => void decide(batchReject)}
                />
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setSelected(new Set())}
                >
                  Clear selection
                </Button>
              </>
            )}
          </div>
        )}
        <Rows label="Action approvals" className={dense(items.length) ? "waiting-dense" : undefined}>
          {items.map((proposal) => (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
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
                <span className="waiting-card-id">approval #{proposal.id}</span>
                <span className="waiting-card-title">
                  {proposal.tool_name ?? "an action that names no tool"}
                </span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <dl className="waiting-facts">
                <div className="waiting-fact">
                  <dt>run</dt>
                  <dd>
                    {proposal.run_id === null ? (
                      "no run"
                    ) : (
                      <Link
                        className="waiting-row-link"
                        to={`/runs/${proposal.run_id}`}
                      >
                        run {proposal.run_id}
                      </Link>
                    )}
                  </dd>
                </div>
                <div className="waiting-fact">
                  <dt>project</dt>
                  <dd>{proposal.project_id ?? "no project"}</dd>
                </div>
              </dl>
              <p className="waiting-reasoning">
                {proposal.reasoning.trim() === ""
                  ? "nothing was recorded about why this was asked for"
                  : proposal.reasoning}
              </p>
              <ToolInput raw={proposal.tool_input} />
              <div className="waiting-actions">
                <ConfirmButton
                  label={`Approve #${proposal.id}`}
                  confirmLabel="Let this action happen"
                  subject={`#${proposal.id}`}
                  variant="approve"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label={`Decline only the action #${proposal.id}`}
                  confirmLabel="Refuse this action, keep the run going"
                  subject={`#${proposal.id}`}
                  variant="ghost"
                  disabled={deciding}
                  onArmedChange={onArmedChange}
                  onConfirm={() => decline.mutate(proposal.id)}
                />
                <ConfirmButton
                  label={`Reject #${proposal.id}`}
                  confirmLabel="Refuse and end the run"
                  subject={`#${proposal.id}`}
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
      title="Team actions"
      view={view}
      what="the team's action requests"
      count={items.length}
      says="no team is waiting on an action"
      why={
        <>
          An action a team asked the núcleo to carry out under a propose grant.
          Approving does not do the thing — it lets the núcleo do it on its next
          tick, about ten seconds later.
        </>
      }
    >
      <Rows label="Team actions" className={dense(items.length) ? "waiting-dense" : undefined}>
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
        <span className="waiting-card-id">team action #{proposal.id}</span>
        <span className="waiting-card-title">
          {proposal.tool_name ?? "an action that names no tool"}
        </span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <dl className="waiting-facts">
        <div className="waiting-fact">
          <dt>project</dt>
          <dd>{proposal.project_id ?? "no project"}</dd>
        </div>
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
      <p className="waiting-reasoning">
        {proposal.reasoning.trim() === ""
          ? "nothing was recorded about why"
          : proposal.reasoning}
      </p>
      <TeamActionPayload kind={proposal.tool_name} raw={proposal.tool_input} />
      <div className="waiting-actions">
        <ConfirmButton
          label={`Approve #${proposal.id}`}
          confirmLabel="Let this action happen"
          subject={`#${proposal.id}`}
          variant="approve"
          disabled={approve.isPending}
          onArmedChange={onArmedChange}
          onConfirm={() => approve.mutate(proposal.id)}
        />
        <ConfirmButton
          label={`Reject #${proposal.id}`}
          confirmLabel="Refuse and close the action"
          subject={`#${proposal.id}`}
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
      title="Recruitment"
      view={view}
      what="the recruitment requests"
      count={items.length}
      says="no team has asked for a specialist"
      why={
        <>
          A director asked for a specialist by name. The request is editable
          here before it is granted; saying not now leaves nothing behind — the
          team may ask again.
        </>
      }
    >
      <Rows label="Recruitment" className={dense(items.length) ? "waiting-dense" : undefined}>
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
  useEffect(() => {
    if (form !== null) return;
    setForm(seedRecruitForm(proposal.tool_input));
  }, [form, proposal.tool_input]);

  function field<K extends keyof AgentRequest>(key: K, value: AgentRequest[K]) {
    setForm((current) =>
      current === null ? current : { ...current, [key]: value },
    );
  }

  const idFor = (name: string) => `recruit-${proposal.id}-${name}`;

  return (
    <Row className="waiting-card" dense={dense(count)}>
      <div className="waiting-card-head">
        <span className="waiting-card-id">recruit #{proposal.id}</span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <p className="waiting-reasoning">
        {proposal.reasoning.trim() === ""
          ? "nothing was recorded about why"
          : proposal.reasoning}
      </p>
      {form === null ? (
        <p className="waiting-payload-raw">
          {proposal.tool_input ?? "nothing was proposed"}
        </p>
      ) : (
        // The one card in this queue that is a form. The inset is what says the
        // fields are a thing standing on the row rather than more of the row.
        <Inset>
          <div className="waiting-recruit-field">
            <label className="waiting-recruit-label" htmlFor={idFor("name")}>
              name
            </label>
            <input
              className="waiting-recruit-input"
              id={idFor("name")}
              value={form.name}
              onChange={(event) => field("name", event.target.value)}
            />
          </div>
          <div className="waiting-recruit-field">
            <label
              className="waiting-recruit-label"
              htmlFor={idFor("speciality")}
            >
              speciality
            </label>
            <input
              className="waiting-recruit-input"
              id={idFor("speciality")}
              value={form.speciality}
              onChange={(event) => field("speciality", event.target.value)}
            />
          </div>
          <div className="waiting-recruit-field">
            <label className="waiting-recruit-label" htmlFor={idFor("prompt")}>
              prompt
            </label>
            <textarea
              className="waiting-recruit-textarea"
              id={idFor("prompt")}
              value={form.prompt}
              onChange={(event) => field("prompt", event.target.value)}
            />
          </div>
          <p className="waiting-recruit-hint">
            Engine and tool policy are the two fields a director gets wrong most
            often — they are also what costs money per turn and what widens what
            this agent can reach.
          </p>
          <div className="waiting-recruit-field">
            <label className="waiting-recruit-label" htmlFor={idFor("engine")}>
              engine
            </label>
            <input
              className="waiting-recruit-input"
              id={idFor("engine")}
              value={form.engine}
              onChange={(event) => field("engine", event.target.value)}
            />
          </div>
          <div className="waiting-recruit-field">
            <label className="waiting-recruit-label" htmlFor={idFor("model")}>
              model
            </label>
            <input
              className="waiting-recruit-input"
              id={idFor("model")}
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
            <label
              className="waiting-recruit-label"
              htmlFor={idFor("tool_policy")}
            >
              tool policy
            </label>
            <input
              className="waiting-recruit-input"
              id={idFor("tool_policy")}
              value={form.tool_policy}
              onChange={(event) => field("tool_policy", event.target.value)}
            />
          </div>
        </Inset>
      )}
      <div className="waiting-actions">
        <ConfirmButton
          label={`Hire #${proposal.id}`}
          confirmLabel="Write the agent and add them to the roster"
          subject={`#${proposal.id}`}
          variant="approve"
          disabled={hire.isPending || form === null}
          onArmedChange={onArmedChange}
          onConfirm={() => {
            if (form !== null)
              hire.mutate({ proposalId: proposal.id, hire: form });
          }}
        />
        <Button
          disabled={reject.isPending}
          onClick={() => reject.mutate(proposal.id)}
        >
          Not now
        </Button>
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
      title="Contact merges"
      view={view}
      what="the suggested merges"
      count={items.length}
      says="no two contacts look like the same person"
      why={
        <>
          Two records that look like one person. Merging is a pointer move and
          is undoable one address at a time; saying they are different people is
          recorded too, which is what stops the same suggestion coming back on
          every sweep.
        </>
      }
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
      <Rows label="Contact merges" className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((suggestion) => (
          <Row className="waiting-card" dense={dense(items.length)} key={suggestion.proposal_id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id">
                merge #{suggestion.proposal_id}
              </span>
              <RelativeTime at={suggestion.created_at} />
            </div>
            <p className="waiting-reasoning">{suggestion.reasoning}</p>
            <div className="waiting-merge">
              <MergeSideView side={suggestion.keep} role="kept" />
              <MergeSideView side={suggestion.absorb} role="absorbed" />
            </div>
            {/* Said before the button rather than after the 409: the daemon
                refuses a merge whose two people carry decisions that
                contradict, and the remedy is to settle one of them first. */}
            {verdictsConflict(suggestion) && (
              <ConflictNote>
                These two carry standing decisions that disagree, so the núcleo
                will refuse the merge — settle one of them and decide this
                again.
              </ConflictNote>
            )}
            <div className="waiting-actions">
              <ConfirmButton
                label={`Merge #${suggestion.proposal_id}`}
                confirmLabel="They are one person"
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
                label={`Keep #${suggestion.proposal_id} apart`}
                confirmLabel="They are different people"
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

/* ------------------------------------------------------- 6. calendar events -- */

/**
 * The one section that cannot be built, stated instead of faked.
 *
 * The núcleo files `calendar-event` proposals and decides them through the same
 * approve and reject doors as everything else on this page — `http.rs` dispatches
 * that kind by hand in both. What it does not mount is any route that *lists*
 * them, so this queue has nothing to read. Pointing a hook at a plausible path
 * would turn a missing feature into an unexplainable 404, which is strictly
 * worse than a sentence.
 *
 * It wears the same shape as every other absence on this page — one `Quiet` line
 * under its own heading, the paragraph behind "why?". It was the last panel among
 * ten one-line absences, and a panel is how this page says "there is something
 * here": the odd one out read as a section that had failed rather than as the one
 * section that cannot exist.
 */
function CalendarEventAbsence() {
  return (
    <Section label="Calendar events">
      <Quiet says="the núcleo mounts no route that lists them">
        <p>
          The núcleo can propose a calendar event and can decide one, but it
          mounts no route that lists the pending ones — so this queue cannot
          show them. Nothing is being hidden: what is missing is the door, not
          the record, and opening it is a change to the núcleo rather than to
          this page. Nothing here is in the count above: with no route that
          lists them, there is nothing to count.
        </p>
      </Quiet>
    </Section>
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
      title="Exclusion requests"
      view={view}
      what="the exclusion requests"
      count={items.length}
      says="no job has asked to be kept apart from another"
      why={
        <>
          A request that two jobs of one project never run at the same time.
          Drawing the edge changed nothing; approving it writes the rule, and
          the higher-numbered job is the one that waits.
        </>
      }
      notes={
        <DecisionNotes
          outcome={approve.data}
          approveError={approve.isError ? approve.error : null}
          refuseError={reject.isError ? reject.error : null}
        />
      }
    >
      <Rows label="Exclusion requests" className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => {
          const pair = readExclusionPair(proposal.tool_input);
          return (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">request #{proposal.id}</span>
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
              <p className="waiting-reasoning">
                {proposal.reasoning.trim() === ""
                  ? "nothing was recorded about why"
                  : proposal.reasoning}
              </p>
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
                  confirmLabel="Keep these two apart"
                  subject={`#${proposal.id}`}
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label={`Reject request #${proposal.id}`}
                  confirmLabel="Let them run together"
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

/* ---------------------------------------------------------- 8. skipped items -- */

function lostWorkOf(proposal: Proposal): { title: string; detail: string | null } {
  if (proposal.job_id == null) {
    return { title: "a job this record can no longer trace", detail: null };
  }

  const job = "job " + proposal.job_id + " \u00B7 ";
  if (typeof proposal.item_ordinal === "number") {
    // The column is 0-based; every surface renders item ordinals one-based.
    return {
      title: job + "item " + (proposal.item_ordinal + 1),
      detail: proposal.item_description ?? null,
    };
  }
  if (proposal.run_stage === "review") return { title: job + "the job's review", detail: null };
  if (proposal.run_stage === "plan") return { title: job + "the job's plan", detail: null };
  return { title: job + "an item it no longer names", detail: null };
}

function SkippedItemsPanel({ view }: { view: Reading<Proposal> }) {
  const dismiss = useDismissProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      title="Skipped items"
      view={view}
      what="the skipped items"
      count={items.length}
      says="no job put anything down"
      why={
        <>
          Work a job put down overnight because it needed a decision, and
          carried on without. Nothing resumes from here — the tree moved on
          hours ago — so the only thing left is to read it and put it away.
          Which is why none of it is in the count above: this is a record of a
          decision the night already took, not one held for you.
        </>
      }
      notes={
        <DecisionNotes
          outcome={undefined}
          approveError={null}
          refuseError={dismiss.isError ? dismiss.error : null}
        />
      }
    >
      <Rows label="Skipped items" className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => {
          const work = lostWorkOf(proposal);
          return (
            <Row className="waiting-card" dense={dense(items.length)} key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">item #{proposal.id}</span>
                <span className="waiting-card-title">{work.title}</span>
                {proposal.run_id !== null && (
                  <Link className="waiting-row-link" to={`/runs/${proposal.run_id}`}>
                    run {proposal.run_id}
                  </Link>
                )}
                <span className="waiting-meta">{proposal.project_id ?? "no project"}</span>
                <RelativeTime at={proposal.created_at} />
              </div>
              {work.detail !== null && <p className="waiting-reasoning">{work.detail}</p>}
              <p className="waiting-meta">
                <span>{proposal.tool_name ?? "an action that names no tool"}</span>{" — "}
                <span>
                  {proposal.reasoning.trim() === ""
                    ? "nothing was recorded about why"
                    : proposal.reasoning}
                </span>
              </p>
              <ToolInput raw={proposal.tool_input} />
              <div className="waiting-actions">
                {/* `/dismiss`, never `/reject`: rejecting guards on
                    `action-approval` and would answer 409 for every one of these. */}
                <ConfirmButton
                  label={`Put item #${proposal.id} away`}
                  confirmLabel="I have read it"
                  subject={`#${proposal.id}`}
                  variant="quiet"
                  disabled={dismiss.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => dismiss.mutate(proposal.id)}
                />
              </div>
            </Row>
          );
        })}
      </Rows>
    </Queue>
  );
}

/* --------------------------------------------------------- 9. refused actions -- */

/**
 * What the injection barrier refused — a record, and deliberately buttonless.
 *
 * The núcleo would accept a dismiss for these, and the design still asks for no
 * controls, which is the right call: the turn that reached for this ended long
 * ago, so there is nothing to allow and nothing to release. The only useful
 * response is to go and do the thing yourself, or to decide the agent was wrong
 * to try — and neither of those is a button on this page. A dismiss button here
 * would read as "handled" for something nobody handled.
 */
function RefusedActionsPanel({ view }: { view: Reading<Proposal> }) {
  const rows = view.rows ?? [];

  return (
    <Queue
      title="Refused actions"
      view={view}
      what="the refused actions"
      count={rows.length}
      says="the barrier has refused nothing"
      why={
        <>
          The barrier stopped these before they happened, in turns that have
          since ended. There is nothing here to allow: what they were going to
          do is written out so you can decide whether to do it yourself, and
          under it what the turn had read when it decided to — which is usually
          the half that answers whether the idea was the agent&apos;s or a
          stranger&apos;s. None of it is in the count above: the barrier already
          answered these, and what is left is a record to read.
        </>
      }
    >
      <Rows label="Refused actions" className={dense(rows.length) ? "waiting-dense" : undefined}>
        {rows.map((proposal) => (
          <Row className="waiting-card" dense={dense(rows.length)} key={proposal.id}>
            <div className="waiting-card-head">
              <span className="waiting-card-id">refusal #{proposal.id}</span>
              <span className="waiting-card-title">
                {proposal.tool_name ?? "an action that names no tool"}
              </span>
              <RelativeTime at={proposal.created_at} />
            </div>
            <p className="waiting-reasoning">
              {proposal.reasoning.trim() === ""
                ? "nothing was recorded about why"
                : proposal.reasoning}
            </p>
            <ToolInput raw={proposal.tool_input} />
            <ReadFrom raw={proposal.read_from} />
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}

/* --------------------------------------------------------------- 10. git queue -- */

/**
 * The two statuses that want a person.
 *
 * `escalated` is a *normal outcome* — somebody owns the conflict now, which is
 * the queue working rather than breaking — and `blocked` is terminal without
 * being a failure: the queue will not retry it, and the answer is to fix the
 * tree and submit again. Both need a person; neither is red. `ui/state-map.ts`
 * holds the tones.
 */
/** How much history to show behind the rows that want a person. */
const VCS_RECENT = 5;

function GitQueuePanel({
  view,
  waiting,
}: {
  view: Reading<VcsRequestSummary>;
  waiting: Reading<VcsRequestSummary>;
}) {
  const dismiss = useDismissVcsRequest();
  const approve = useApproveVcsRequest();
  const refuse = useRefuseVcsRequest();
  const rows = view.rows ?? [];
  // What wants a person comes from `/waiting/git`, never from filtering the history: that listing
  // is capped at 200 and keeps every escalation that has since been settled.
  const wanted = waiting.rows ?? [];
  const rest = rows.filter((row) => !wanted.some((open) => open.id === row.id));
  const recent = rest.slice(0, VCS_RECENT);

  return (
    <Queue
      title="Git queue"
      view={view}
      what="the git queue"
      // The queue is empty when it holds nothing at all. A queue that has been
      // through five pushes cleanly is not empty — nothing is waiting on a
      // person, and the corner says so with a zero while the history stays.
      count={Math.max(rows.length, wanted.length)}
      aside={<Count n={wanted.length} />}
      says="nothing has been through the git queue"
      why={
        <>
          Every push, merge and rebase the núcleo has been asked to make. Three
          of the states want a person — escalated, which means somebody owns a
          conflict now, blocked, which the queue will not retry, and held for
          your approval, a merge that changes the test map — and none is a
          failure. One stops waiting once it is settled: a later
          identical request succeeded, a resolution took it, its branch is
          already in the target or gone, or you put it away. It stays in the
          history either way.
        </>
      }
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
      {wanted.length === 0 ? (
        <Quiet says="nothing in the git queue is waiting on you." />
      ) : (
        <Rows label="Git requests waiting on you" className={dense(wanted.length) ? "waiting-dense" : undefined}>
          {wanted.map((row) => (
            <VcsRow key={row.id} row={row} count={wanted.length}>
              <div className="waiting-actions">
                {row.status === "awaiting_owner" ? (
                  <>
                    <ConfirmButton
                      label={`Approve ${row.op} #${row.id}`}
                      confirmLabel="Approve the map change"
                      subject={`#${row.id}`}
                      variant="approve"
                      disabled={approve.isPending || refuse.isPending}
                      onConfirm={() => {
                        refuse.reset();
                        dismiss.reset();
                        approve.mutate(row.id);
                      }}
                    />
                    <ConfirmButton
                      label={`Refuse ${row.op} #${row.id}`}
                      confirmLabel="Refuse the map change"
                      subject={`#${row.id}`}
                      variant="ghost"
                      disabled={approve.isPending || refuse.isPending}
                      onConfirm={() => {
                        approve.reset();
                        dismiss.reset();
                        refuse.mutate(row.id);
                      }}
                    />
                  </>
                ) : (
                  <ConfirmButton
                    label={`Put ${row.op} #${row.id} away`}
                    confirmLabel="Nobody needs to act"
                    subject={`#${row.id}`}
                    variant="quiet"
                    disabled={dismiss.isPending}
                    onConfirm={() => dismiss.mutate(row.id)}
                  />
                )}
              </div>
            </VcsRow>
          ))}
        </Rows>
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
    </Queue>
  );
}

function VcsRow({
  row,
  count,
  children,
}: {
  row: VcsRequestSummary;
  count: number;
  children?: ReactNode;
}) {
  return (
    <Row dense={dense(count)}>
      <div className="waiting-card-head">
        <span className="waiting-card-id">
          {row.op} #{row.id}
        </span>
        <StateBadge domain="vcs" state={row.status} />
        <span className="waiting-meta">{row.project_id}</span>
        <RelativeTime at={row.created_at} />
      </div>
      {/* The badge says the state in one word; the metadata says which repository and where
          the request came from. */}
      <p className="waiting-meta">
        {row.repo_key} — {row.origin}
      </p>
      {/* And what to do about it is its own line, not a third clause on the faint mono one. */}
      {row.status === "blocked" ? (
        <p className="waiting-hint">submit it again</p>
      ) : row.status === "awaiting_owner" ? (
        <p className="waiting-hint">changes the test map — approve to let it land</p>
      ) : null}
      {children}
    </Row>
  );
}

/* -------------------------------------------------------------- 11. parked runs -- */

function ParkedRunsPanel({ view }: { view: Reading<AwaitingRun> }) {
  const rows = view.rows ?? [];

  return (
    <Queue
      title="Parked runs"
      view={view}
      what="the parked runs"
      count={rows.length}
      says="no run is parked"
      why={
        <>
          Worktree runs holding a tree while they wait. The decision that frees
          one is its approval above; giving the tree back without deciding is on
          the run&apos;s own page. A parked run is not in the count above either
          — the approval that frees it is, and counting both would count one
          decision twice.
        </>
      }
    >
      <Rows label="Parked runs" className={dense(rows.length) ? "waiting-dense" : undefined}>
        {rows.map((run) => (
          <Row dense={dense(rows.length)} key={run.id}>
            <div className="waiting-card-head">
              <Link className="waiting-row-link" to={`/runs/${run.id}`}>
                run {run.id}
              </Link>
              <StateBadge domain="run" state="awaiting_approval" />
              <span className="waiting-meta">
                {run.project_id ?? "no project"}
              </span>
              <RelativeTime at={run.created_at} />
            </div>
            <p className="waiting-excerpt">{run.prompt}</p>
            {run.cwd !== null && <p className="waiting-meta">{run.cwd}</p>}
          </Row>
        ))}
      </Rows>
    </Queue>
  );
}
