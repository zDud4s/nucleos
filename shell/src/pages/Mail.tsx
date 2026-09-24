import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { Paperclip } from "lucide-react";
import { isApiRefusal } from "../data/client";
import {
  awaitingYouCount,
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

/**
 * One derived sentence about the queue — and, before it, about whether anything
 * is reading the queue at all.
 *
 * The order is the answer to "is everything fine?", which is the question this
 * page is opened with far more often than "what is in it": a pillar that is not
 * enabled, not armed, or whose local triage is stopped makes the count beside
 * the point, and all three used to be legible only in the dim configuration
 * panel at the very bottom of the page. The counts are what is left when none
 * of that is wrong, and they lead with the reader's own share of the queue —
 * `urgent` and `action` — rather than with the daemon's backlog.
 *
 * `stale` is not decoration either. When the queue query has failed and rows
 * from an earlier answer are still on screen, this sentence is the page's
 * largest claim about the present, and it must not make one: it says "last
 * known" and hands the rest to {@link StaleNote}.
 */
function headline(
  rows: QueuedEmail[] | undefined,
  config: EmailConfigView | undefined,
  stale: boolean,
): string | undefined {
  if (config !== undefined && !config.enabled) return "mail is not enabled — nothing is fetched";
  if (config !== undefined && !config.armed) {
    return "triage is not armed — mail arrives, nothing reads it";
  }
  const stopped = config === undefined ? null : config.local_triage_disabled;
  if (typeof stopped === "string" && stopped !== "") return `local triage is stopped: ${stopped}`;

  if (rows === undefined) return undefined;
  const said = queueSentence(rows);
  return stale ? `last known: ${said}` : said;
}

function queueSentence(rows: QueuedEmail[]): string {
  if (rows.length === 0) return "the inbound queue is empty";
  const yours = awaitingYouCount(rows);
  const untriaged = untriagedCount(rows);
  const parts = [`${rows.length} in the queue`];
  if (yours > 0) parts.push(`${yours} for you`);
  if (untriaged > 0) parts.push(`${untriaged} not triaged yet`);
  // "needs you" and not the Waiting page's own phrase: that one names a single arithmetic —
  // its six decision lists — and `one-waiting-phrase.test.ts` is the fence that keeps a second
  // page from borrowing it for a different number. This one counts urgent and action mail.
  if (parts.length === 1) parts.push("nothing needs you");
  return parts.join("; ");
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
      <PageHeader title="Mail" headline={headline(rows, config.data, stale)} />

      {/* Mail opens on what arrived; configuration is filled in once, not read first. */}
      <Panel title="Queue" aside={<UntriagedCount rows={rows} />}>
        <MailSearchBar q={q} onSearch={setQ} />
        {stale && <StaleNote dataUpdatedAt={queue.dataUpdatedAt} />}
        {queue.isError && rows === undefined && <QueueError error={queue.error} />}
        <QueueList rows={rows} filtered={q !== undefined} />
      </Panel>

      <TriagePanel />

      <SkippedAbsence />

      {/* The cursor was a panel of its own around one line; it is a fact about the
          mailbox the configuration already names, so it stands in that list. */}
      <ConfigPanel config={config} cursor={cursor.data} cursorLoading={cursor.data === undefined && !cursor.isError} />
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
      <div className="mail-stack">
        <p className="mail-note">
          Ask for one triage batch, now. Whether it ran is answered below — including the times it
          did not, and why, which the daemon always sends as a sentence rather than a failure.
        </p>
        <Button intent="go" disabled={triage.isPending} onClick={() => triage.mutate()}>
          Run triage now
        </Button>
        {triage.data !== undefined && <TriageOutcomeNote outcome={triage.data} />}
        {triage.isError && <TriageRequestError error={triage.error} />}
      </div>
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
      {/* The label is still the control's; it is not drawn because the placeholder and the
          button beside it already say "search" twice, and a third time cost the panel a row. */}
      <Field label="Search" labelHidden>
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
      <Rows label="Mail queue" className="mail-list">
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
            different badges — see this file's header. The badge is the row's ONE
            coloured mark: the sender verdict and the attachment are a word and a
            glyph beside the facts they qualify, never chips stacked on the class. */}
        <span className="mail-row-class">
          <StateBadge domain="email_class" state={row.triage_class} />
        </span>
        <span className="mail-row-from">
          <span className="mail-row-name">{row.from_name ?? row.from_addr}</span>
          {row.sender_verdict !== null && <SenderMark verdict={row.sender_verdict} />}
        </span>
        <Link to={`/mail/${row.id}`} className="mail-row-subject">
          {row.subject ?? "(no subject)"}
        </Link>
        <span className="mail-row-meta">
          {/* `has_attachments` is an i64 0/1 over the wire, not a boolean. */}
          {row.has_attachments === 1 && (
            <Paperclip className="mail-row-clip" size={14} strokeWidth={1.75} role="img" aria-label="has attachments">
              <title>has attachments</title>
            </Paperclip>
          )}
          <RelativeTime at={row.received_at} />
        </span>
        {row.triage_summary !== null && <p className="mail-row-summary">{row.triage_summary}</p>}
      </div>
    </Row>
  );
}

/**
 * A standing decision about the sender, as the word it means rather than the
 * verb that set it. This was a chip, and `pin` wore Acting Green — the tone that
 * says "executing right now", which a sender preference never is. A condition
 * that is not one of the seven tones gets words (DESIGN.md, the Seven Tones
 * Rule). An unfamiliar value is shown as the daemon sent it, in the mono face.
 */
function SenderMark({ verdict }: { verdict: string }) {
  const said = verdict === "pin" ? "pinned" : verdict === "mute" ? "muted" : undefined;
  return <span className="mail-row-verdict">{said ?? <code>{verdict}</code>}</span>;
}

/* -------------------------------------------------------------- configuration -- */

function ConfigPanel({
  config,
  cursor,
  cursorLoading,
}: {
  config: { data: EmailConfigView | undefined; isError: boolean; error: unknown };
  cursor: MailCursor | undefined;
  cursorLoading: boolean;
}) {
  const data = config.data;
  return (
    <Panel title="Configuration" variant="dim">
      {data === undefined && config.isError && <ConfigError error={config.error} />}
      {data === undefined && !config.isError && <p className="mail-loading">reading the configuration…</p>}
      {data !== undefined && (
        <dl className="mail-config">
          <div className="mail-config-fact">
            <dt>account</dt>
            <dd className="mail-config-data">{configAccount(data)}</dd>
          </div>
          <div className="mail-config-fact">
            <dt>mailbox</dt>
            <dd className="mail-config-data">{configMailbox(data)}</dd>
          </div>
          {data.mailbox && (
            <div className="mail-config-fact">
              <dt>sync cursor</dt>
              <dd>
                <CursorFact mailbox={data.mailbox} cursor={cursor} loading={cursorLoading} />
              </dd>
            </div>
          )}
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

/**
 * Where the fetcher has read up to in the named mailbox. `null` is the daemon
 * saying no cursor row exists — never synced — and is not the same as still
 * loading, which is `undefined` while the query has not answered.
 */
function CursorFact({ mailbox, cursor, loading }: { mailbox: string; cursor: MailCursor | undefined; loading: boolean }) {
  if (loading) return <span className="mail-loading">reading the cursor…</span>;
  if (cursor === null) return <>no cursor recorded yet for {mailbox} — it has not been synced.</>;
  if (cursor === undefined) return <>unknown — the cursor could not be read</>;
  return (
    <span className="mail-config-data">
      UID validity {cursor.uidvalidity} · last UID {cursor.last_uid}
    </span>
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
