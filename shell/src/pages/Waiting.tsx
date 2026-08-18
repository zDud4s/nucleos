import { useRef, useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import type { Proposal } from "../data/system";
import {
  VCS_LIST_LIMIT,
  useActionApprovals,
  useApproveProposal,
  useAwaitingRuns,
  useContactMerges,
  useDecideContactMerge,
  useDismissProposal,
  useExclusionRequests,
  useRefusedActions,
  useRejectProposal,
  useSkippedItems,
  useVcsRequests,
  useWheelRequests,
  type ApprovalOutcome,
  type AwaitingRun,
  type MergeSide,
  type MergeSuggestion,
  type VcsRequestSummary,
  type WheelRequest,
} from "../data/waiting";
import {
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
  Teach,
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

export function Waiting() {
  const wheel = reading(useWheelRequests());
  const approvals = reading(useActionApprovals());
  const merges = reading(useContactMerges());
  const exclusions = reading(useExclusionRequests());
  const skipped = reading(useSkippedItems());
  const refused = reading(useRefusedActions());
  const vcs = reading(useVcsRequests());
  const parked = reading(useAwaitingRuns());

  const decisions = countOf(wheel.rows, approvals.rows, merges.rows, exclusions.rows);
  const records = countOf(skipped.rows, refused.rows);

  return (
    <>
      <PageHeader title="Waiting" headline={headlineFor(decisions, records)} />

      <div className="waiting-sections">
        <WheelRequestSection view={wheel} />
        <ActionApprovalSection view={approvals} />
        <TeamSectionsAbsence />
        <ContactMergeSection view={merges} />
        <CalendarEventAbsence />
        <ExclusionRequestSection view={exclusions} />
        <SkippedItemsPanel view={skipped} />
        <RefusedActionsPanel view={refused} />
        <GitQueuePanel view={vcs} />
        <ParkedRunsPanel view={parked} />
      </div>
    </>
  );
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
function headlineFor(decisions: number | undefined, records: number | undefined): string | undefined {
  if (decisions === undefined) return undefined;
  const first = decisions === 0 ? "nothing is waiting on a decision" : `${decisions} waiting on a decision`;
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

function inSnapshotOrder<T>(rows: T[], idOf: (row: T) => number, snapshot: number[] | null): T[] {
  if (snapshot === null) return rows;
  const rank = new Map(snapshot.map((id, index) => [id, index]));
  // `sort` is stable, so everything the snapshot never saw keeps the daemon's
  // own order behind everything it did.
  return [...rows].sort(
    (a, b) =>
      (rank.get(idOf(a)) ?? Number.MAX_SAFE_INTEGER) - (rank.get(idOf(b)) ?? Number.MAX_SAFE_INTEGER),
  );
}

/**
 * Dense above three.
 *
 * A queue of two is read; a queue of ten is worked through. The tighter card is
 * what keeps the tenth one on the same screen as the first.
 */
function listClass(count: number): string {
  return count > 3 ? "waiting-list waiting-list-dense" : "waiting-list";
}

/* ------------------------------------------------------------- shared parts -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="waiting-count">{n}</span>;
}

/** Stale, failed or not yet answered — the three things a list can be besides ready. */
function ReadingNotes({ view, what }: { view: Reading<unknown>; what: string }) {
  return (
    <>
      {view.stale && <StaleNote dataUpdatedAt={view.dataUpdatedAt} />}
      {view.error !== null && view.rows === undefined && <ListError error={view.error} what={what} />}
      {view.error === null && view.rows === undefined && (
        <p className="waiting-loading">reading {what}…</p>
      )}
    </>
  );
}

function ListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about {what}</ErrorNote>;
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
  internal: "the núcleo failed while carrying the decision out — nothing was changed",
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

function DecisionRefusal({ error, trustProse }: { error: unknown; trustProse: boolean }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>;
  }
  const sentences = trustProse
    ? { ...DECISION_SENTENCES, ...daemonProse(error) }
    : DECISION_SENTENCES;
  return <RefusalNote refusal={error} sentences={sentences} />;
}

/**
 * What an approval did, said afterwards.
 *
 * `closed` first, and deliberately: it is a **success** that changed nothing,
 * and the sentence the daemon sends with it is better than anything derivable
 * from the other fields. Reading it as a failure would send somebody hunting for
 * a rule that was right not to be written.
 */
function outcomeSentence(outcome: ApprovalOutcome): string {
  if (typeof outcome.closed === "string" && outcome.closed !== "") return outcome.closed;
  if (typeof outcome.resume_run_id === "number") return `run ${outcome.resume_run_id} is going again`;
  if (typeof outcome.exclusion_id === "number") return `rule ${outcome.exclusion_id} is in force`;
  if (outcome.merged === true) return "the two are one contact now";
  if (typeof outcome.event_id === "number") return `event ${outcome.event_id} is in the calendar`;
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
      {approveError !== null && <DecisionRefusal error={approveError} trustProse />}
      {refuseError !== null && <DecisionRefusal error={refuseError} trustProse={false} />}
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
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
  return Object.entries(parsed as Record<string, unknown>).map(([name, value]) => ({
    name,
    value: plainValue(value),
  }));
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

/* ------------------------------------------------------- 1. wheel requests -- */

function WheelRequestSection({ view }: { view: Reading<WheelRequest> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(rows, (session) => session.proposal_id);

  return (
    <Panel title="Wheel requests" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        An agent has met a wall it may not climb and is asking for the window. Giving it the wheel
        opens a real browser on this machine, in the profile named on the card; refusing closes the
        session, and the run carries on without that page.
      </p>
      <ReadingNotes view={view} what="the open browser sessions" />
      {view.rows !== undefined && items.length === 0 && (
        <p className="waiting-empty">nothing has asked for the wheel.</p>
      )}
      {items.length > 0 && (
        <ul className={listClass(items.length)} aria-label="Wheel requests">
          {items.map((session) => (
            <li className="waiting-card" key={session.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">wheel #{session.proposal_id}</span>
                <span className="waiting-meta">{session.project_id ?? "no project"}</span>
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
              {session.refusal !== null && <p className="waiting-reasoning">{session.refusal}</p>}
              <div className="waiting-actions">
                <ConfirmButton
                  label={`Give wheel #${session.proposal_id} the window`}
                  confirmLabel="Open a real browser here"
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(session.proposal_id)}
                />
                <ConfirmButton
                  label={`Refuse wheel #${session.proposal_id}`}
                  confirmLabel="Refuse and close the session"
                  disabled={reject.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => reject.mutate(session.proposal_id)}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      <DecisionNotes
        outcome={approve.data}
        approveError={approve.isError ? approve.error : null}
        refuseError={reject.isError ? reject.error : null}
      />
    </Panel>
  );
}

/* ----------------------------------------------------- 2. action approvals -- */

function ActionApprovalSection({ view }: { view: Reading<Proposal> }) {
  const approve = useApproveProposal();
  const reject = useRejectProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(rows, (proposal) => proposal.id);

  return (
    <Panel title="Action approvals" aside={<Count n={view.rows?.length} />}>
      <ReadingNotes view={view} what="the approval queue" />
      {view.rows !== undefined && items.length === 0 && (
        <Teach title="Nothing is waiting to be let through">
          <p>
            A run that reaches for something outside its permission stops where it is and files one
            of these. An empty queue means either that nothing has asked, or that the autopilot is
            off — it is not a sign that anything is stuck.
          </p>
        </Teach>
      )}
      {items.length > 0 && (
        <ul className={listClass(items.length)} aria-label="Action approvals">
          {items.map((proposal) => (
            <li className="waiting-card" key={proposal.id}>
              <div className="waiting-card-head">
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
                      <Link className="waiting-row-link" to={`/runs/${proposal.run_id}`}>
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
                  variant="approve"
                  disabled={approve.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => approve.mutate(proposal.id)}
                />
                <ConfirmButton
                  label={`Reject #${proposal.id}`}
                  confirmLabel="Refuse and end the run"
                  disabled={reject.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => reject.mutate(proposal.id)}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      <DecisionNotes
        outcome={approve.data}
        approveError={approve.isError ? approve.error : null}
        refuseError={reject.isError ? reject.error : null}
      />
    </Panel>
  );
}

/* --------------------------------------------- 3 & 4. teams and recruitment -- */

/**
 * The design's sections three and four, absent and saying so.
 *
 * A team's own action approvals and its recruitment requests belong in this
 * queue and are not in it, because the núcleo mounts no team routes at all — the
 * tables exist, the doors do not. Rendering an empty section for them would
 * claim they are quiet; rendering nothing at all would lose the fact that the
 * design asks for them. So the absence is a sentence, and it names what it is
 * waiting on.
 */
function TeamSectionsAbsence() {
  return (
    <Panel title="Team decisions" variant="dim">
      <p className="waiting-absence">
        Two sections are missing here by design, not by oversight: a team&apos;s own action approvals
        and its recruitment requests. The núcleo mounts no team routes, so there is nothing to read
        and nothing to answer — these arrive with the Teams slice, which is what the sidebar&apos;s
        Teams entry is held on too.
      </p>
    </Panel>
  );
}

/* ------------------------------------------------------- 5. contact merges -- */

function MergeSideView({ side, role }: { side: MergeSide; role: string }) {
  return (
    <div className="waiting-side">
      <p className="waiting-side-role">{role}</p>
      <p className="waiting-side-name">{side.display_name ?? "no name recorded"}</p>
      <ul className="waiting-addresses">
        {side.addresses.map((address) => (
          <li key={address}>{address}</li>
        ))}
      </ul>
      <p className="waiting-meta">{side.messages_in} messages in</p>
      <p className="waiting-verdict">
        {side.verdict === null ? "no standing decision" : `standing decision: ${side.verdict}`}
      </p>
    </div>
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
  const { items, onArmedChange } = useOrderFreeze(rows, (suggestion) => suggestion.proposal_id);

  return (
    <Panel title="Contact merges" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        Two records that look like one person. Merging is a pointer move and is undoable one address
        at a time; saying they are different people is recorded too, which is what stops the same
        suggestion coming back on every sweep.
      </p>
      <ReadingNotes view={view} what="the suggested merges" />
      {view.rows !== undefined && items.length === 0 && (
        <p className="waiting-empty">no two contacts look like the same person.</p>
      )}
      {items.length > 0 && (
        <ul className={listClass(items.length)} aria-label="Contact merges">
          {items.map((suggestion) => (
            <li className="waiting-card" key={suggestion.proposal_id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">merge #{suggestion.proposal_id}</span>
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
                <p className="waiting-conflict">
                  These two carry standing decisions that disagree, so the núcleo will refuse the
                  merge — settle one of them and decide this again.
                </p>
              )}
              <div className="waiting-actions">
                <ConfirmButton
                  label={`Merge #${suggestion.proposal_id}`}
                  confirmLabel="They are one person"
                  variant="approve"
                  disabled={decide.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    decide.mutate({ proposalId: suggestion.proposal_id, verdict: "approve" })
                  }
                />
                <ConfirmButton
                  label={`Keep #${suggestion.proposal_id} apart`}
                  confirmLabel="They are different people"
                  disabled={decide.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() =>
                    decide.mutate({ proposalId: suggestion.proposal_id, verdict: "reject" })
                  }
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      {/* One mutation for both verdicts, so the refusal is shown once — and its
          prose is trusted, because the 409 here is the refusal that names the two
          decisions that disagree. */}
      <DecisionNotes
        outcome={decide.data}
        approveError={decide.isError ? decide.error : null}
        refuseError={null}
      />
    </Panel>
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
 */
function CalendarEventAbsence() {
  return (
    <Panel title="Calendar events" variant="dim">
      <p className="waiting-absence">
        The núcleo can propose a calendar event and can decide one, but it mounts no route that
        lists the pending ones — so this queue cannot show them. Nothing is being hidden: what is
        missing is the door, not the record, and opening it is a change to the núcleo rather than to
        this page.
      </p>
    </Panel>
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
  const { items, onArmedChange } = useOrderFreeze(rows, (proposal) => proposal.id);

  return (
    <Panel title="Exclusion requests" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        A request that two jobs of one project never run at the same time. Drawing the edge changed
        nothing; approving it writes the rule, and the higher-numbered job is the one that waits.
      </p>
      <ReadingNotes view={view} what="the exclusion requests" />
      {view.rows !== undefined && items.length === 0 && (
        <p className="waiting-empty">no job has asked to be kept apart from another.</p>
      )}
      {items.length > 0 && (
        <ul className={listClass(items.length)} aria-label="Exclusion requests">
          {items.map((proposal) => {
            const pair = readExclusionPair(proposal.tool_input);
            return (
              <li className="waiting-card" key={proposal.id}>
                <div className="waiting-card-head">
                  <span className="waiting-card-id">request #{proposal.id}</span>
                  <span className="waiting-card-title">
                    {pair.low === null || pair.high === null
                      ? "a pair this request does not name"
                      : `jobs ${pair.low} and ${pair.high}`}
                  </span>
                  <span className="waiting-meta">{proposal.project_id ?? "no project"}</span>
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
                    variant="approve"
                    disabled={approve.isPending}
                    onArmedChange={onArmedChange}
                    onConfirm={() => approve.mutate(proposal.id)}
                  />
                  <ConfirmButton
                    label={`Reject request #${proposal.id}`}
                    confirmLabel="Let them run together"
                    disabled={reject.isPending}
                    onArmedChange={onArmedChange}
                    onConfirm={() => reject.mutate(proposal.id)}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
      <DecisionNotes
        outcome={approve.data}
        approveError={approve.isError ? approve.error : null}
        refuseError={reject.isError ? reject.error : null}
      />
    </Panel>
  );
}

/* ---------------------------------------------------------- 8. skipped items -- */

function SkippedItemsPanel({ view }: { view: Reading<Proposal> }) {
  const dismiss = useDismissProposal();
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(rows, (proposal) => proposal.id);

  return (
    <Panel title="Skipped items" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        Work a job put down overnight because it needed a decision, and carried on without. Nothing
        resumes from here — the tree moved on hours ago — so the only thing left is to read it and
        put it away.
      </p>
      <ReadingNotes view={view} what="the skipped items" />
      {view.rows !== undefined && items.length === 0 && (
        <p className="waiting-empty">no job put anything down.</p>
      )}
      {items.length > 0 && (
        <ul className={listClass(items.length)} aria-label="Skipped items">
          {items.map((proposal) => (
            <li className="waiting-card" key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">item #{proposal.id}</span>
                <span className="waiting-card-title">
                  {proposal.tool_name ?? "an action that names no tool"}
                </span>
                <span className="waiting-meta">{proposal.project_id ?? "no project"}</span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <p className="waiting-reasoning">
                {proposal.reasoning.trim() === "" ? "nothing was recorded about why" : proposal.reasoning}
              </p>
              <ToolInput raw={proposal.tool_input} />
              <div className="waiting-actions">
                {/* `/dismiss`, never `/reject`: rejecting guards on
                    `action-approval` and would answer 409 for every one of these. */}
                <ConfirmButton
                  label={`Put item #${proposal.id} away`}
                  confirmLabel="I have read it"
                  disabled={dismiss.isPending}
                  onArmedChange={onArmedChange}
                  onConfirm={() => dismiss.mutate(proposal.id)}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      <DecisionNotes
        outcome={undefined}
        approveError={null}
        refuseError={dismiss.isError ? dismiss.error : null}
      />
    </Panel>
  );
}

/* --------------------------------------------------------- 9. refused actions -- */

/**
 * What the injection barrier refused — a record, and deliberately buttonless.
 *
 * The núcleo would accept a dismiss for these, and the design still asks for no
 * controls, which is the right call: the turn that reached for this ended long
 * ago, so there is nothing to allow and nothing to release. The only useful
 * response is to go and do the thing yourself, or to decide the errand was wrong
 * to try — and neither of those is a button on this page. A dismiss button here
 * would read as "handled" for something nobody handled.
 */
function RefusedActionsPanel({ view }: { view: Reading<Proposal> }) {
  const rows = view.rows ?? [];

  return (
    <Panel title="Refused actions" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        The barrier stopped these before they happened, in turns that have since ended. There is
        nothing here to allow: what they were going to do is written out so you can decide whether
        to do it yourself.
      </p>
      <ReadingNotes view={view} what="the refused actions" />
      {view.rows !== undefined && rows.length === 0 && (
        <p className="waiting-empty">the barrier has refused nothing.</p>
      )}
      {rows.length > 0 && (
        <ul className={listClass(rows.length)} aria-label="Refused actions">
          {rows.map((proposal) => (
            <li className="waiting-card" key={proposal.id}>
              <div className="waiting-card-head">
                <span className="waiting-card-id">refusal #{proposal.id}</span>
                <span className="waiting-card-title">
                  {proposal.tool_name ?? "an action that names no tool"}
                </span>
                {/* The one listing that joins the errand in: "send_email" without
                    the errand is the verb with the subject missing. */}
                <span className="waiting-meta">{proposal.errand_name ?? "no errand"}</span>
                <RelativeTime at={proposal.created_at} />
              </div>
              <p className="waiting-reasoning">
                {proposal.reasoning.trim() === "" ? "nothing was recorded about why" : proposal.reasoning}
              </p>
              <ToolInput raw={proposal.tool_input} />
            </li>
          ))}
        </ul>
      )}
    </Panel>
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
const VCS_WANTS_A_PERSON = ["escalated", "blocked"];

/** How much history to show behind the rows that want a person. */
const VCS_RECENT = 5;

function GitQueuePanel({ view }: { view: Reading<VcsRequestSummary> }) {
  const rows = view.rows ?? [];
  const wanted = rows.filter((row) => VCS_WANTS_A_PERSON.includes(row.status));
  const rest = rows.filter((row) => !VCS_WANTS_A_PERSON.includes(row.status));
  const recent = rest.slice(0, VCS_RECENT);

  return (
    <Panel title="Git queue" aside={<Count n={wanted.length} />}>
      <ReadingNotes view={view} what="the git queue" />
      {view.rows !== undefined && wanted.length === 0 && (
        <p className="waiting-empty">nothing in the git queue is waiting on you.</p>
      )}
      {wanted.length > 0 && (
        <ul className={listClass(wanted.length)} aria-label="Git requests waiting on you">
          {wanted.map((row) => (
            <VcsRow key={row.id} row={row} />
          ))}
        </ul>
      )}
      {recent.length > 0 && (
        <>
          <p className="waiting-subhead">recently through the queue</p>
          <ul className="waiting-list" aria-label="Recent git requests">
            {recent.map((row) => (
              <VcsRow key={row.id} row={row} />
            ))}
          </ul>
        </>
      )}
      {/* Nothing prunes `vcs_requests`, so this listing is the whole history and
          not a backlog. Arriving at the cap says the daemon has been running a
          while — it is not a finding and is not drawn as one. */}
      {rows.length >= VCS_LIST_LIMIT && (
        <p className="waiting-ceiling">
          the listing stops at {VCS_LIST_LIMIT} rows — this is the queue&apos;s whole history, kept
          rather than pruned, so a full listing only means the daemon has been running a while
        </p>
      )}
    </Panel>
  );
}

function VcsRow({ row }: { row: VcsRequestSummary }) {
  return (
    <li className="waiting-row">
      <div className="waiting-row-head">
        <span className="waiting-card-id">
          {row.op} #{row.id}
        </span>
        <StateBadge domain="vcs" state={row.status} />
        <span className="waiting-meta">{row.project_id}</span>
        <RelativeTime at={row.created_at} />
      </div>
      <p className="waiting-meta">
        {row.repo_key} — {row.origin}
      </p>
    </li>
  );
}

/* -------------------------------------------------------------- 11. parked runs -- */

function ParkedRunsPanel({ view }: { view: Reading<AwaitingRun> }) {
  const rows = view.rows ?? [];

  return (
    <Panel title="Parked runs" aside={<Count n={view.rows?.length} />}>
      <p className="waiting-note">
        Worktree runs holding a tree while they wait. The decision that frees one is its approval
        above; giving the tree back without deciding is on the run&apos;s own page.
      </p>
      <ReadingNotes view={view} what="the parked runs" />
      {view.rows !== undefined && rows.length === 0 && (
        <p className="waiting-empty">no run is parked.</p>
      )}
      {rows.length > 0 && (
        <ul className={listClass(rows.length)} aria-label="Parked runs">
          {rows.map((run) => (
            <li className="waiting-row" key={run.id}>
              <div className="waiting-row-head">
                <Link className="waiting-row-link" to={`/runs/${run.id}`}>
                  run {run.id}
                </Link>
                <StateBadge domain="run" state="awaiting_approval" />
                <span className="waiting-meta">{run.project_id ?? "no project"}</span>
                <RelativeTime at={run.created_at} />
              </div>
              <p className="waiting-excerpt">{run.prompt}</p>
              {run.cwd !== null && <p className="waiting-meta">{run.cwd}</p>}
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}
