import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  groupWaiting,
  measuredByGenerator,
  parseEvidence,
  useApproveKnowledge,
  useDecideKnowledgeBatch,
  useKnowledge,
  useKnowledgeHistory,
  useRejectKnowledge,
  useRevertKnowledge,
  type Known,
  type KnownKind,
  type KnownLayer,
  type KnownStatus,
} from "../data/knowledge";
import {
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StateBadge,
  Teach,
  Well,
} from "../ui";
import "./learned.css";

/**
 * Learned — what the agent has been told, and what it asked to be told.
 *
 * The page exists because the store behind it was, until this slice, invisible.
 * Something waiting for an answer is a `kind = 'refinement'` proposal, and
 * `proposals::list_pending` filters `kind = 'action-approval'` — so the Waiting
 * queue, which is the page for everything that stopped to ask you something,
 * structurally could not show one. A decision queue nobody can see is a queue
 * where nothing is ever decided, and what was waiting to be decided here is
 * what every later run gets told.
 *
 * **Four lists and not one, in this order.** What is proposed comes
 * first because it is the only part that is a task. What is in force comes
 * second because it is the answer to "why did the agent do that". What is live
 * inside a job comes third because it reaches only that job. What is over comes
 * last and is read rarely — but it is never deleted, because "what did it say
 * before I changed it" is the question a person asks at the exact moment they
 * are considering changing it again.
 *
 * The chain behind a row is fetched only when somebody opens it. Forty rows
 * would otherwise be forty-one requests to answer a question nobody asked.
 */
export function Learned() {
  const knowledge = useKnowledge();
  const approve = useApproveKnowledge();
  const reject = useRejectKnowledge();
  const batch = useDecideKnowledgeBatch();
  const revert = useRevertKnowledge();

  const [layer, setLayer] = useState<KnownLayer | "all">("all");
  const [scope, setScope] = useState("all");

  const rows = knowledge.data;
  const scopes = scopeOptions(rows ?? []);
  const scoped = rows?.filter((row) =>
    scope === "all"
      ? true
      : scope === "machine"
        ? row.scope_kind === "machine"
        : row.scope_id === scope,
  );
  const filtered = scoped?.filter((row) => layer === "all" || row.layer === layer);
  const waiting = filtered?.filter((row) => row.status === "proposed") ?? [];
  const waitingGroups = groupWaiting(waiting);
  const inForce = filtered?.filter((row) => row.status === "active") ?? [];
  const live = filtered?.filter((row) => row.status === "live") ?? [];
  const over = filtered?.filter((row) => OVER.has(row.status)) ?? [];
  const measured = measuredByGenerator(scoped ?? []);
  const measuredTotal = measured.reduce((sum, group) => sum + group.total, 0);

  // The three mutations share one error slot on purpose: they are three answers
  // to the same question, only one is ever in flight, and a refusal from any of
  // them is about the row the person just touched.
  const refusal = approve.error ?? reject.error ?? revert.error;
  const deciding = approve.isPending || reject.isPending || batch.isPending;

  return (
    <>
      <PageHeader title="Learned" headline={headline(filtered)} />

      {knowledge.isError && (
        <ErrorNote>
          the núcleo did not answer — nothing is known about what it has learned
        </ErrorNote>
      )}
      {refusal !== null && <DecisionRefusal error={refusal} />}

      {rows !== undefined && rows.length === 0 && (
        <Teach title="Nothing has been learned yet">
          This is where supplemental instructions, facts about a project, and
          reusable ways of working are kept once you have approved them — and
          where you take one back. Nothing reaches a prompt until you say so,
          so an empty layer means the agent is running on its standing brief
          alone.
        </Teach>
      )}

      {rows !== undefined && rows.length > 0 && (
        <div className="learned-filters">
          <div role="group" aria-label="Layer">
            {LAYER_FILTERS.map(([value, label]) => (
              <Button
                key={value}
                variant="quiet"
                aria-pressed={layer === value}
                onClick={() => setLayer(value)}
              >
                {label}
              </Button>
            ))}
          </div>
          <label>
            Scope{" "}
            <select
              aria-label="Scope"
              value={scope}
              onChange={(event) => setScope(event.target.value)}
            >
              <option value="all">All scopes</option>
              {scopes.map((option) => (
                <option key={option.key} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
        </div>
      )}

      {waiting.length > 0 && (
        <Panel title="Waiting for you" aside={<Count n={waiting.length} />}>
          <p className="learned-lede">
            Declared, and reaching nothing until you answer. Approving adds it
            to every later run in its scope; refusing keeps the refusal on the
            record rather than erasing the question.
          </p>
          {waitingGroups.map((group) => {
            const resultBelongsHere = sameProposalIds(
              batch.variables?.proposalIds,
              group.proposalIds,
            );
            return (
              <section
                key={group.key}
                className="learned-group"
                aria-label={`${group.scope}, ${group.source}`}
              >
                <div className="learned-group-head">
                  <span>{group.scope}</span>
                  <span>{group.source}</span>
                  <Count n={group.rows.length} />
                  {group.proposalIds.length > 1 && (
                    <>
                      <ConfirmButton
                        label={`Approve all ${group.proposalIds.length}`}
                        confirmLabel={`Let all ${group.proposalIds.length} into every later prompt`}
                        variant="approve"
                        onConfirm={() =>
                          batch.mutate({
                            action: "approve",
                            proposalIds: group.proposalIds,
                          })
                        }
                        disabled={deciding}
                      />
                      <Button
                        onClick={() =>
                          batch.mutate({
                            action: "reject",
                            proposalIds: group.proposalIds,
                          })
                        }
                        disabled={deciding}
                      >
                        Refuse all {group.proposalIds.length}
                      </Button>
                    </>
                  )}
                </div>
                {resultBelongsHere &&
                  !batch.isPending &&
                  batch.data !== undefined &&
                  batch.data.failed.length > 0 && (
                    <ErrorNote>
                      {batch.data.done} decided; {batch.data.failed.length} could not be — they
                      were already decided, or are gone. The list clears on the next read.
                    </ErrorNote>
                  )}
                {resultBelongsHere && batch.isError && (
                  <DecisionRefusal error={batch.error} />
                )}
                <Rows label={`${group.scope}, ${group.source}`}>
                  {group.rows.map((row) => (
                    <KnownRow
                      key={row.id}
                      row={row}
                      decisions={
                        row.proposal_id === null ? (
                          // A proposed row whose question is gone cannot be decided from here, and a
                          // button that 404s is worse than none: it invites a click that teaches the
                          // person the app is broken when the daemon is merely inconsistent. Said in
                          // the row, and said as `Quiet`: what is missing here is the decision, which
                          // is the one-line absence that component is for. It was set faint, which is
                          // the rung for metadata standing beside content — here the sentence is all
                          // the row has to say in place of its two buttons.
                          <Quiet says="no question to answer — decide in the daemon" />
                        ) : (
                          <>
                            <Button
                              variant="approve"
                              onClick={() =>
                                approve.mutate(row.proposal_id as number)
                              }
                              disabled={deciding}
                            >
                              Approve
                            </Button>
                            <Button
                              onClick={() =>
                                reject.mutate(row.proposal_id as number)
                              }
                              disabled={deciding}
                            >
                              Refuse
                            </Button>
                          </>
                        )
                      }
                    />
                  ))}
                </Rows>
              </section>
            );
          })}
        </Panel>
      )}

      {inForce.length > 0 && (
        <Panel title="In force" aside={<Count n={inForce.length} />}>
          <p className="learned-lede">
            Appended to the brief of every node in scope — never replacing it. A
            machine-wide note reaches every project; a project's note reaches
            only that project.
          </p>
          <Rows label="In force">
            {[...inForce].sort(byKindThenId).map((row) => (
              <KnownRow
                key={row.id}
                row={row}
                decisions={
                  <ConfirmButton
                    label="Revert"
                    confirmLabel="It no longer applies"
                    variant="quiet"
                    onConfirm={() => revert.mutate(row.id)}
                    disabled={revert.isPending}
                  />
                }
              />
            ))}
          </Rows>
        </Panel>
      )}

      {live.length > 0 && (
        <Panel title="Live in a job" aside={<Count n={live.length} />}>
          <p className="learned-lede">
            Facts a run left for the rest of its own job. They reach only that
            job&apos;s later nodes and are closed when it ends.
          </p>
          <Rows label="Live in a job">
            {[...live].sort(byKindThenId).map((row) => (
              <KnownRow key={row.id} row={row} />
            ))}
          </Rows>
        </Panel>
      )}

      {over.length > 0 && (
        <Panel title="No longer in force" aside={<Count n={over.length} />}>
          <p className="learned-lede">
            Kept, not deleted. What was refused, what was taken back, and what a
            later text replaced.
          </p>
          <Rows label="No longer in force">
            {[...over]
              .sort((left, right) => right.id - left.id)
              .map((row) => (
                <KnownRow key={row.id} row={row} />
              ))}
          </Rows>
        </Panel>
      )}

      {measured.length > 0 && (
        <Panel title="Measured, by generator" aside={<Count n={measuredTotal} />}>
          <Rows label="Measured, by generator">
            {measured.map((group) => (
              <Row key={group.scope} className="learned-measured-row">
                <span className="learned-scope">{group.scope}</span>
                {group.byGenerator.map((entry) => (
                  <span key={entry.generator}>
                    {entry.generator}: {entry.count}
                  </span>
                ))}
              </Row>
            ))}
          </Rows>
        </Panel>
      )}
    </>
  );
}

const LAYER_FILTERS: readonly (readonly [KnownLayer | "all", string])[] = [
  ["all", "All"],
  ["semantic", "Facts"],
  ["episodic", "Measured"],
  ["procedural", "How-to"],
  ["working", "Working"],
];

/** The statuses that mean "was decided, and is not applying now". */
const OVER: ReadonlySet<KnownStatus> = new Set<KnownStatus>([
  "rejected",
  "reverted",
  "superseded",
  "archived",
  "closed",
  "expired",
]);

/**
 * One thing known, with whatever can be done to it.
 *
 * `decisions` is a slot rather than a status check inside the row: what a person
 * may do to a row is a property of the list it is in — you approve what
 * is waiting, revert what is in force, and do nothing at all to what is over.
 */
function KnownRow({ row, decisions }: { row: Known; decisions?: ReactNode }) {
  const [chainId, setChainId] = useState<number | null>(null);
  const evidence = parseEvidence(row.evidence);

  return (
    <Row className="learned-row">
      <div className="learned-head">
        <StateBadge domain="knowledge" state={row.kind} />
        <span className="learned-scope">
          {row.scope_id ?? "this machine"}
        </span>
        <span className="learned-layer">{row.layer}</span>
        <span className="learned-source">{row.source}</span>
        {row.observations !== null && (
          <span className="learned-count">
            measured {row.observations} time{row.observations === 1 ? "" : "s"}
          </span>
        )}
        <span className="learned-when">
          <RelativeTime at={row.activated_at ?? row.created_at} />
        </span>
      </div>

      <p className="learned-title">{row.title}</p>
      <p className="learned-body">{row.body}</p>

      {evidence.length > 0 && (
        <ul className="learned-evidence">
          {evidence.map((ref, index) => (
            <li key={`${ref.t}-${String(ref.id)}-${index}`}>
              {ref.t === "run" ? (
                <Link to="/runs/$runId" params={{ runId: String(ref.id) }}>
                  run {ref.id}
                </Link>
              ) : ref.t === "knowledge" && knowledgeId(ref.id) !== null ? (
                <Button
                  variant="quiet"
                  aria-expanded={chainId === knowledgeId(ref.id)}
                  onClick={() => {
                    const id = knowledgeId(ref.id);
                    if (id !== null) setChainId(chainId === id ? null : id);
                  }}
                >
                  knowledge {ref.id}
                </Button>
              ) : (
                <span>
                  {ref.t} {ref.id}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="learned-foot">
        <Button
          variant="quiet"
          aria-expanded={chainId === row.id}
          onClick={() => setChainId(chainId === row.id ? null : row.id)}
        >
          {row.supersedes === null ? "History" : "What it replaced"}
        </Button>
        {decisions}
      </div>

      {chainId !== null && <Chain id={chainId} />}
    </Row>
  );
}

function knowledgeId(id: number | string): number | null {
  const numeric = typeof id === "number" ? id : Number(id);
  return Number.isFinite(numeric) ? numeric : null;
}

/**
 * What a row replaced, what replaced it, and every decision it has been through.
 *
 * A component of its own so the query lives and dies with the disclosure: an
 * `enabled: false` query on a closed row would still occupy a cache entry per
 * row, and the point of not fetching is that nothing is asked for.
 */
function Chain({ id }: { id: number }) {
  const history = useKnowledgeHistory(id);

  if (history.isError) {
    return (
      <ErrorNote>
        the núcleo did not answer — this one&apos;s history is not known
      </ErrorNote>
    );
  }
  if (history.data === undefined)
    return <p className="learned-chain-loading">reading…</p>;

  const { events, replaced, replaced_by: replacedBy } = history.data;

  return (
    // A well and not a box: the chain is the same row seen further back in time,
    // and the rung below the row is what the system calls a recess cut into a
    // surface. `reads` because every line of it is somebody's writing, which the
    // well's default mono face would deny; `capped` because the decisions grow by
    // one each time a person answers something.
    <Well as="div" reads capped>
      {replacedBy !== null && (
        <p className="learned-chain-line">
          Replaced by <strong>{replacedBy.title}</strong>.
        </p>
      )}
      {replaced.length > 0 && (
        <>
          <p className="learned-chain-line">
            What it replaced, most recent first:
          </p>
          <ul className="learned-chain-list">
            {replaced.map((older) => (
              <li key={older.id}>
                <span className="learned-title">{older.title}</span>
                <span className="learned-body">{older.body}</span>
              </li>
            ))}
          </ul>
        </>
      )}
      {/* The one genuine absence on this page: a chain with no chain in it. One
          line, nothing to teach, and `Quiet`'s muted rung rather than the faint
          one — in an empty region the sentence is the content. */}
      {replaced.length === 0 && replacedBy === null && (
        <Quiet says="This one replaced nothing and nothing has replaced it." />
      )}
      {events.length > 0 && (
        <ul className="learned-events">
          {events.map((event) => (
            <li key={event.id}>
              <span className="learned-event-status">{event.to_status}</span>
              <span className="learned-event-note">{event.note ?? ""}</span>
              <RelativeTime at={event.at} />
            </li>
          ))}
        </ul>
      )}
    </Well>
  );
}

/** Kind first, then id — the order a node reads them, so the screen matches the prompt. */
function byKindThenId(left: Known, right: Known): number {
  const order = KIND_ORDER[left.kind] - KIND_ORDER[right.kind];
  return order !== 0 ? order : left.id - right.id;
}

function scopeOptions(rows: Known[]) {
  const options = new Map<
    string,
    { key: string; value: string; label: string }
  >();
  for (const row of rows) {
    const key = `${row.scope_kind}:${row.scope_id ?? ""}`;
    options.set(key, {
      key,
      value: row.scope_kind === "machine" ? "machine" : (row.scope_id ?? "machine"),
      label: row.scope_id ?? "this machine",
    });
  }
  return [...options.values()].sort(
    (left, right) => left.label.localeCompare(right.label) || left.key.localeCompare(right.key),
  );
}

/** `refine::Kind`'s own order: an instruction changes what a node does, a fact what it believes. */
const KIND_ORDER: Record<KnownKind, number> = {
  prompt: 0,
  memory: 1,
  skill: 2,
  subagent: 3,
};


/**
 * What the decision doors say when they say no.
 *
 * The 409 is the one worth writing copy for: it almost always means the row was
 * answered while it sat on screen — by the other window, or by a later text
 * superseding this one — and the list clears itself on the next read.
 */
const DECISION_SENTENCES: Record<string, string> = {
  conflict:
    "this one was already decided, or is no longer in force — the list clears it on the next read",
  not_found: "that one is gone; there is nothing left to decide",
  unprocessable:
    "the núcleo found nothing usable inside the proposal to act on",
  internal:
    "the núcleo failed while carrying the decision out — nothing was changed",
};

function DecisionRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>
    );
  }
  return <RefusalNote refusal={error} sentences={DECISION_SENTENCES} />;
}

/**
 * One derived sentence about the store.
 *
 * What is *waiting* leads, because it is the only part of this page that is a
 * task. A count of what is in force follows, because "the agent has been told
 * eleven things" is the fact a person wants before they read any of them.
 */
function headline(rows: Known[] | undefined): string | undefined {
  if (rows === undefined) return undefined;
  const waiting = rows.filter((row) => row.status === "proposed").length;
  const inForce = rows.filter((row) => row.status === "active").length;
  const measured = rows.filter(
    (row) =>
      row.status === "active" &&
      row.layer === "episodic" &&
      row.source === "consolidator",
  ).length;

  if (rows.length === 0)
    return "the agent is running on its standing brief alone";
  const held =
    inForce === 1 ? "one note is in force" : `${inForce} notes are in force`;
  const proposed =
    waiting === 0
      ? "nothing proposed"
      : `${waiting} note${waiting === 1 ? "" : "s"} proposed`;
  const base = `${held}; ${proposed}`;
  if (measured > 0) return `${base}; ${measured} measured`;
  // "note", which is the word the clause before it already uses. The store is
  // called knowledge and the page is called Learned; neither is a word to count
  // out loud, and "3 refinements proposed" is the old table's name surviving in
  // the one place a person reads it.
  return base;
}

function sameProposalIds(left: number[] | undefined, right: number[]): boolean {
  if (left === undefined || left.length !== right.length) return false;
  const orderedLeft = [...left].sort((a, b) => a - b);
  const orderedRight = [...right].sort((a, b) => a - b);
  return orderedLeft.every((id, index) => id === orderedRight[index]);
}
