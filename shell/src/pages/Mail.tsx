import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  MAIL_QUEUE_LIMIT,
  untriagedCount,
  useEmailConfig,
  useMailCursor,
  useMailQueue,
  useTriage,
  type EmailConfigView,
  type MailCursor,
  type QueuedEmail,
  type TriageOutcome,
} from "../data/mail";
import {
  Button,
  Count,
  ErrorNote,
  Field,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./mail.css";

/**
 * Mail — the inbound queue, one triage button, and the configuration that
 * decides what the button is even allowed to do.
 *
 * The two facts that shape this page are both in `data/mail.ts`'s header, and
 * neither is the kind of thing this shell usually has to say twice, so it is
 * said again here where it decides what gets drawn:
 *
 * **A triage refusal is a 200, not an error.** `POST /email/triage` never
 * fails for the reasons a person would ask about — the pillar not armed, a
 * batch already running, nothing waiting, a governance gate closed. Every one
 * of those is `{ queued: 0, run_id: null, reason: "…" }`, and `reason` is
 * rendered as a sentence next to the button, `role="status"`, never inside
 * {@link RefusalNote} or {@link ErrorNote} — those are for the daemon not
 * answering at all, which is a different event from the daemon answering no.
 *
 * **A message with no triage class yet is not a message triage read and
 * dismissed.** `triage_class: null` and `triage_class: "noise"` are two
 * different facts about a row, and `ui/state-map.ts`'s `email_class` domain is
 * what keeps the badge from saying the same thing for both.
 */

/** One derived sentence about the queue. */
function headline(rows: QueuedEmail[] | undefined): string | undefined {
  if (rows === undefined) return undefined;
  if (rows.length === 0) return "the inbound queue is empty";
  const untriaged = untriagedCount(rows);
  if (untriaged === 0) return `${rows.length} in the queue, all of it triaged`;
  const noun = untriaged === 1 ? "message" : "messages";
  return `${rows.length} in the queue; ${untriaged} ${noun} not triaged yet`;
}

export function Mail() {
  const [q, setQ] = useState<string | undefined>(undefined);
  const queue = useMailQueue(q);
  const config = useEmailConfig();
  const mailbox = config.data?.mailbox;
  const cursor = useMailCursor(mailbox ?? "", { enabled: mailbox !== undefined });

  const rows = queue.data;
  const stale = queue.isError && rows !== undefined;

  return (
    <>
      <PageHeader title="Mail" headline={headline(rows)} />

      {/* Mail opens on what arrived; configuration is filled in once, not read first. */}
      <Panel title="Queue" aside={<UntriagedCount rows={rows} />}>
        <MailSearchBar q={q} onSearch={setQ} />
        {stale && <StaleNote dataUpdatedAt={queue.dataUpdatedAt} />}
        {queue.isError && rows === undefined && <QueueError error={queue.error} />}
        <QueueList rows={rows} filtered={q !== undefined} />
      </Panel>

      <TriagePanel />

      <CursorPanel mailbox={mailbox} cursor={cursor.data} loading={cursor.data === undefined && !cursor.isError} />

      <SkippedAbsence />

      <ConfigPanel config={config} />
    </>
  );
}

/* -------------------------------------------------------------- the count -- */

/**
 * How many rows have not been triaged yet — never a zero drawn before the
 * queue has actually answered once.
 *
 * `rows === undefined` covers both "still loading" and "the query has never
 * succeeded"; either way there is no count to have an opinion about yet, and a
 * figure that guessed zero would be a claim nobody measured. {@link Count}
 * renders nothing at all for `undefined` for exactly that reason, so the guard
 * is now the argument's type rather than an early return.
 *
 * A *measured* zero is a different fact and does now show. "0 untriaged" is an
 * answer; a heading that loses its count the moment the queue is caught up
 * reads as a count that failed, which is the case `Count` was written around.
 *
 * This was `.mail-count`, a pill in the Awaiting You tone. The rail's
 * `.nav-badge` is the system's only pill-shaped count, and it earns the shape
 * because a shut sidebar has no room for the word the number belongs to — here
 * the word is right there, so the pill was a quantity dressed as a badge.
 */
function UntriagedCount({ rows }: { rows: QueuedEmail[] | undefined }) {
  return (
    <Count
      n={rows === undefined ? undefined : untriagedCount(rows)}
      noun="untriaged"
      plural="untriaged"
    />
  );
}

/* ------------------------------------------------------------- triage form -- */

function TriagePanel() {
  const triage = useTriage();

  return (
    <Panel title="Triage">
      <p className="mail-note">
        Ask for one triage batch, now. Whether it ran is answered below — including the times it
        did not, and why, which the daemon always sends as a sentence rather than a failure.
      </p>
      <Button intent="go" disabled={triage.isPending} onClick={() => triage.mutate()}>
        Run triage now
      </Button>
      {triage.data !== undefined && <TriageOutcomeNote outcome={triage.data} />}
      {triage.isError && <TriageRequestError error={triage.error} />}
    </Panel>
  );
}

/**
 * What the daemon said about the batch that was or was not asked for.
 *
 * `reason` is read as a *value*, never as an error: `queued: 0` with a reason
 * is the ordinary shape of "no batch ran right now", not a fault in the
 * request. `role="status"` throughout — nothing here interrupts a screen
 * reader the way {@link ErrorNote}'s `alert` would, because nothing broke.
 */
function TriageOutcomeNote({ outcome }: { outcome: TriageOutcome }) {
  if (outcome.queued > 0) {
    const noun = outcome.queued === 1 ? "message" : "messages";
    const run = outcome.run_id === null ? "" : ` — run ${outcome.run_id}`;
    return (
      <p className="mail-outcome" role="status">
        queued {outcome.queued} {noun} for triage{run}
      </p>
    );
  }
  return (
    <p className="mail-outcome" role="status">
      {outcome.reason ?? "nothing was queued"}
    </p>
  );
}

/** The request itself did not land — the daemon was not there to answer at all. */
function TriageRequestError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — no triage batch was requested</ErrorNote>;
}

/* --------------------------------------------------------------- the queue -- */

function MailSearchBar({ q, onSearch }: { q: string | undefined; onSearch: (q: string | undefined) => void }) {
  return (
    <form
      className="mail-search"
      role="search"
      aria-label="Search the mail queue"
      onSubmit={(event) => {
        event.preventDefault();
        const typed = new FormData(event.currentTarget).get("q");
        const text = typeof typed === "string" ? typed.trim() : "";
        onSearch(text === "" ? undefined : text);
      }}
    >
      <Field label="Search">
        <input
          name="q"
          defaultValue={q ?? ""}
          key={q ?? ""}
          aria-label="Search sender, subject or summary"
          placeholder="sender, subject or summary"
        />
      </Field>
      <div className="mail-search-actions">
        <Button type="submit">Search</Button>
        {q !== undefined && <Button onClick={() => onSearch(undefined)}>Clear</Button>}
      </div>
    </form>
  );
}

function QueueError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the queue</ErrorNote>;
}

function QueueList({ rows, filtered }: { rows: QueuedEmail[] | undefined; filtered: boolean }) {
  if (rows === undefined) return <p className="mail-loading">reading the queue…</p>;

  if (rows.length === 0) {
    return (
      <Teach title={filtered ? "Nothing matches that search" : "The inbound queue is empty"}>
        {filtered ? (
          <p>Clear the search above to see the whole queue — an empty match is not an empty inbox.</p>
        ) : (
          <p>
            Nothing has arrived, or everything that has has already been read out of the queue.
            Retention keeps a rolling window of mail, not a history — this list is what is here now.
          </p>
        )}
      </Teach>
    );
  }

  // Hairline-ruled and not a column of cards: this list is read by scanning
  // down it, not by picking messages out of it — the argument `.ui-rows` now
  // carries for all four lists that had grown it byte for byte.
  return (
    <>
      <Rows label="Mail queue">
        {rows.map((row) => (
          <MailRow key={row.id} row={row} />
        ))}
      </Rows>
      {rows.length >= MAIL_QUEUE_LIMIT && (
        <p className="mail-ceiling">
          showing the newest {MAIL_QUEUE_LIMIT} — narrow the search to reach further back
        </p>
      )}
    </>
  );
}

function MailRow({ row }: { row: QueuedEmail }) {
  return (
    <Row>
      <div className="mail-row-head">
        {/* NULL and "noise" are two different facts and must read as two
            different badges — see this file's header. */}
        <span className="mail-row-badges">
          <StateBadge domain="email_class" state={row.triage_class} />
          {/* `has_attachments` is an i64 0/1 over the wire, not a boolean. */}
          {row.has_attachments === 1 && <span className="mail-attachment">attachment</span>}
          {row.sender_verdict !== null && (
            <span className={`mail-verdict mail-verdict-${row.sender_verdict}`}>{row.sender_verdict}</span>
          )}
        </span>
        <span className="mail-row-from">{row.from_name ?? row.from_addr}</span>
        <Link to={`/mail/${row.id}`} className="mail-row-subject">
          {row.subject ?? "(no subject)"}
        </Link>
        <RelativeTime at={row.received_at} />
        {row.triage_summary !== null && <p className="mail-row-summary">{row.triage_summary}</p>}
      </div>
    </Row>
  );
}

/* -------------------------------------------------------------- configuration -- */

function ConfigPanel({ config }: { config: { data: EmailConfigView | undefined; isError: boolean; error: unknown } }) {
  const data = config.data;
  return (
    <Panel title="Configuration" variant="dim">
      {data === undefined && config.isError && <ConfigError error={config.error} />}
      {data === undefined && !config.isError && <p className="mail-loading">reading the configuration…</p>}
      {data !== undefined && (
        <dl className="mail-config">
          <div className="mail-config-fact">
            <dt>account</dt>
            <dd>
              {configAccount(data)}
            </dd>
          </div>
          <div className="mail-config-fact">
            <dt>mailbox</dt>
            <dd>{configMailbox(data)}</dd>
          </div>
          <div className="mail-config-fact">
            <dt>state</dt>
            <dd>{configState(data)}</dd>
          </div>
          <div className="mail-config-fact">
            <dt>local triage</dt>
            <dd>{configLocalTriage(data)}</dd>
          </div>
        </dl>
      )}
    </Panel>
  );
}

/**
 * `enabled && !armed` is a real, standing state and not a transition: mail
 * keeps arriving and retention keeps pruning, and no triage batch is ever
 * asked for until somebody arms the pillar.
 */
function configState(data: EmailConfigView): string {
  if (!data.enabled) return "not enabled — nothing is fetched";
  if (!data.armed) return "enabled but not armed — mail arrives, triage does not run";
  return "enabled and armed";
}

function configAccount(data: EmailConfigView): string {
  if (!data.username && !data.host) return "no account configured";
  return [data.username, data.host].filter(Boolean).join("@");
}

function configMailbox(data: EmailConfigView): string {
  if (!data.mailbox) return "no mailbox named";
  return data.mailbox;
}

function configLocalTriage(data: EmailConfigView): string {
  const disabled = (data as { local_triage_disabled?: unknown }).local_triage_disabled;
  if (disabled === null) return "nothing is wrong";
  if (typeof disabled === "string") return `disabled: ${disabled}`;
  return "unknown";
}

function ConfigError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the mail configuration</ErrorNote>;
}

/* -------------------------------------------------------------- sync cursor -- */

function CursorPanel({
  mailbox,
  cursor,
  loading,
}: {
  mailbox: string | undefined;
  cursor: MailCursor | undefined;
  loading: boolean;
}) {
  if (mailbox === undefined) return null;
  return (
    <Panel title="Sync cursor" variant="dim">
      {loading && <p className="mail-loading">reading the cursor…</p>}
      {!loading && cursor === null && (
        <p className="mail-note">no cursor recorded yet for {mailbox} — it has not been synced.</p>
      )}
      {!loading && cursor !== null && cursor !== undefined && (
        <p className="mail-note">
          {mailbox}: UID validity {cursor.uidvalidity}, last UID {cursor.last_uid}
        </p>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------ skipped mail -- */

/**
 * A message the fetcher could not parse is never a row in this queue — it is
 * a feed line, kind `email_fetch_skipped`, and nothing else. Said out loud
 * rather than pointed at a route: `GET /proposals/skipped-items` is job
 * items, not mail, and inventing a mail route for this would be worse than
 * the sentence.
 */
function SkippedAbsence() {
  return (
    <Panel title="Skipped messages" variant="dim">
      {/* The absence in one line, with the paragraph that used to stand here
          kept verbatim behind it. This panel says nothing but "there is nothing
          to show and here is why", which is the whole of what `Quiet` is for:
          the reasoning is worth keeping and is not worth the four lines it
          costs every reader who already knows it. */}
      <Quiet says="a message the fetcher could not parse is never a row in this queue.">
        <p>
          A message the fetcher could not parse does not appear above — it is written only as a line
          on the feed, kind <code className="mail-kind">email_fetch_skipped</code>, with the reason
          in its text. Open <Link to="/feed">the feed</Link> and filter by that kind to read them.
        </p>
      </Quiet>
    </Panel>
  );
}
