import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchAttachment, getEmail, getEmailQueue, triageEmail,
  type ConnectionState, type EmailAttachment, type EmailDetail, type QueuedEmail,
} from "./api";
import { formatBytes, mailLabel, mailTone, relativeTime, safeDownloadName } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

interface OpenMessageProps { token: string; detail: EmailDetail | null; loading: boolean; }
/**
 * An opened message: its text, and what came with it.
 *
 * The body is rendered as TEXT and never as markup. It is the one thing on screen written by
 * someone outside this machine, and the sidecar already reduced any HTML to plain text — putting it
 * back into the DOM as HTML would undo that and hand a stranger a script tag and a tracking pixel.
 */
function OpenMessage({ token, detail, loading }: OpenMessageProps) {
  const [saving, setSaving] = useState<number | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  if (loading) return <p className="a-note">A abrir…</p>;
  if (detail === null) return <ErrorNote>Could not open this message.</ErrorNote>;

  async function save(file: EmailAttachment) {
    if (detail === null) return;
    setSaving(file.position);
    setFailed(null);
    const blob = await fetchAttachment(token, detail.id, file.position);
    setSaving(null);
    if (blob === null) {
      setFailed("Could not fetch this file. It may have been deleted from the mailbox since.");
      return;
    }
    // The bytes never touched a disk on the way here, so the browser's own download is what puts
    // them somewhere — under a name made safe on this side, because going through a blob skips the
    // `Content-Disposition` the daemon took care to build.
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = safeDownloadName(file.filename);
    link.click();
    URL.revokeObjectURL(url);
  }

  return (
    <div className="mail-open">
      {detail.body_text === null
        ? <Teach title="This message no longer has a body.">
            Bodies are kept for a set number of days and then pruned, so the verdict outlives the
            text it was based on. Nothing was lost from the mailbox itself.
          </Teach>
        : <pre className="mail-body">{detail.body_text}</pre>}
      {detail.attachments.length > 0 && (
        <ul className="mail-files">
          {detail.attachments.map((file) => (
            <li key={file.position}>
              <span className="mf-name">{file.filename ?? "(sem nome)"}</span>
              <span className="mf-meta">
                {file.mime_type ?? "tipo desconhecido"} · {formatBytes(file.size_bytes)}
              </span>
              <Button
                size="sm"
                disabled={saving !== null}
                onClick={() => void save(file)}
              >
                {saving === file.position ? "A obter…" : "Guardar"}
              </Button>
            </li>
          ))}
        </ul>
      )}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </div>
  );
}

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
  const [openId, setOpenId] = useState<number | null>(null);
  const [detail, setDetail] = useState<EmailDetail | null>(null);
  const [opening, setOpening] = useState(false);
  const openRequest = useRef<number | null>(null);

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

  // Bodies are fetched one at a time, when opened. The list's 3s refresh never touches this, so a
  // message you are reading does not reload underneath you.
  async function toggle(id: number) {
    if (openId === id) {
      openRequest.current = null;
      setOpenId(null);
      setDetail(null);
      return;
    }
    openRequest.current = id;
    setOpenId(id);
    setDetail(null);
    if (token === null) return;
    setOpening(true);
    const next = await getEmail(token, id);
    // A slow fetch that lands after something else was opened must not paint the wrong message
    // into the open one. Checked against a ref because the state read in this closure is stale.
    if (openRequest.current !== id) return;
    setOpening(false);
    setDetail(next);
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
                  {/* A button rather than a clickable div, so the mailbox is reachable from the
                      keyboard and announces itself as expandable. */}
                  <button
                    type="button"
                    className="mail-row"
                    aria-expanded={openId === mail.id}
                    onClick={() => void toggle(mail.id)}
                  >
                    <div className="f-meta">
                      <time dateTime={mail.received_at} title={mail.received_at}>
                        {relativeTime(mail.received_at)}
                      </time>
                      <Badge tone={mailTone(mail.triage_class)}>
                        {mailLabel(mail.triage_class)}
                      </Badge>
                      {mail.has_attachments > 0 && <span className="mf-clip" title="tem anexos">📎</span>}
                    </div>
                    <p className="f-body">
                      <b>{mail.from_name ?? mail.from_addr}</b> — {mail.subject ?? "(no subject)"}
                    </p>
                    {mail.triage_summary !== null && (
                      <p className="f-body">{mail.triage_summary}</p>
                    )}
                  </button>
                  {openId === mail.id && token !== null && (
                    <OpenMessage token={token} detail={detail} loading={opening} />
                  )}
                </article>
              ))}
      </Panel>
    </section>
  );
}

export default Mail;
