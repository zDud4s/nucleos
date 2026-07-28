import { useCallback, useEffect, useState } from "react";
import {
  getEmailQueue, triageEmail,
  type ConnectionState, type QueuedEmail,
} from "./api";
import { mailLabel, mailTone, relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

interface MailProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * The mailbox, in the order a mailbox is read: newest arrival first.
 *
 * Its own tab rather than a panel inside Autopilot, because the two answer different questions.
 * Autopilot asks what the núcleo wants to do to your code; this asks what other people have sent
 * you. Sharing a screen made the second one a footnote of the first, capped at eight rows.
 *
 * Collecting mail is automatic because reading a mailbox costs nothing. Classifying it costs a run,
 * so it happens only when asked, and the button at the top is the asking. That is why the count of
 * what is waiting leads the page: the decision on offer is whether it is worth spending on yet.
 */
function Mail({ token, connection }: MailProps) {
  const unavailable = connection !== "connected" || token === null;
  const [queue, setQueue] = useState<QueuedEmail[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const refresh = useCallback(
    async (background = false) => {
      if (token === null || connection !== "connected") return;
      if (!background) setLoading(true);
      const next = await getEmailQueue(token);
      setQueue(next);
      if (!background) setLoading(false);
    },
    [connection, token],
  );

  useEffect(() => {
    if (unavailable) {
      setQueue(null);
      setLoading(true);
      return;
    }
    void refresh();
  }, [refresh, unavailable]);

  // The same silent 3s cadence as the rest of the shell: a background refresh keeps the previous
  // rows on screen until fresh ones land, so a list you are reading never blinks.
  useEffect(() => {
    if (unavailable) return;
    const id = setInterval(() => void refresh(true), 3000);
    return () => clearInterval(id);
  }, [refresh, unavailable]);

  async function triage() {
    if (token === null) return;
    setBusy(true);
    setNote(null);
    const outcome = await triageEmail(token);
    setBusy(false);
    if (outcome === null) {
      setNote("The daemon did not answer.");
      return;
    }
    setNote(
      outcome.run_id === null
        ? outcome.reason ?? "Nothing to do."
        // Deliberately does not promise the feed: a feed row IS a notification, so it is written
        // only for the classes in `notify_classes`, which starts empty. This list is the one place
        // a verdict always lands.
        : `Reading ${outcome.queued} message${outcome.queued === 1 ? "" : "s"} — the verdicts appear here when the run finishes.`,
    );
    void refresh(true);
  }

  const waiting = (queue ?? []).filter((mail) => mail.triage_class === null);

  if (unavailable) {
    return (
      <section className="mail">
        <Teach title="Mail is waiting for the daemon.">
          Connect to the daemon to load the mailbox. Nothing was missed: the cursor lives in the
          núcleo, so collection resumes where it stopped.
        </Teach>
      </section>
    );
  }

  return (
    <section className="mail">
      <h1 className="headline">
        {waiting.length === 0
          ? <>Nothing waiting. <span className="ok">The mailbox is read.</span></>
          : <><em>{waiting.length} message{waiting.length === 1 ? "" : "s"}</em> waiting to be read.</>}
      </h1>
      <div className="statusline">
        <span>{queue?.length ?? 0} in the mailbox</span>
        <span>collection is automatic · <b>reading costs a run</b></span>
      </div>
      <Panel
        title="Mailbox"
        aside={waiting.length > 0 ? `${waiting.length} waiting` : "nothing waiting"}
      >
        <Button onClick={triage} disabled={busy || waiting.length === 0}>
          {busy
            ? "Reading…"
            : waiting.length === 0
              ? "Nothing to read"
              : `Read ${waiting.length} now`}
        </Button>
        {note !== null && <p className="gate-note">{note}</p>}
        {loading && queue === null && <p className="a-note">Loading…</p>}
        {!loading && queue === null
          ? <ErrorNote>Could not load the mailbox from the daemon.</ErrorNote>
          : queue !== null && queue.length === 0
            ? <Teach title="No mail yet.">
                Messages appear here as they arrive. Nothing is classified until you ask.
              </Teach>
            : (queue ?? []).map((mail) => (
                // Server order, not re-sorted here: the daemon sorts by arrival and truncates at
                // its own limit, so re-sorting a truncated page would only invent a second opinion.
                <article className="feed-item" key={mail.id}>
                  <div className="f-meta">
                    <time dateTime={mail.received_at} title={mail.received_at}>
                      {relativeTime(mail.received_at)}
                    </time>
                    <Badge tone={mailTone(mail.triage_class)}>{mailLabel(mail.triage_class)}</Badge>
                  </div>
                  <p className="f-body">
                    <b>{mail.from_name ?? mail.from_addr}</b> — {mail.subject ?? "(no subject)"}
                  </p>
                  {mail.triage_summary !== null && (
                    <p className="f-body">{mail.triage_summary}</p>
                  )}
                </article>
              ))}
      </Panel>
    </section>
  );
}

export default Mail;
