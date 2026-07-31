import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchAllAttachments, fetchAttachment, getEmail, getEmailCursor, getEmailQueue, listMailFiles,
  getEmailConfig, requeueEmail, saveAllAttachments, saveAttachment, setSenderVerdict, triageEmail,
  type ConnectionState, type EmailAttachment, type EmailConfig, type EmailCursor,
  type EmailDetail, type QueuedEmail, type SenderVerdict,
} from "./api";
import {
  base64ToBytes, formatBytes, mailLabel, mailTone, relativeTime, requeueFailureMessage,
  safeDownloadName,
} from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";
import Senders from "./Senders";

/**
 * The mailbox to read the cursor for until the daemon says which one it collects from.
 *
 * A fallback for the first render only — `/config/email` reports the configured name and this page
 * asks. It used to be the answer rather than the guess, and a different configured mailbox made the
 * cursor read empty instead of wrong: it said nothing had ever been collected, which is exactly what
 * a healthy but idle mailbox says too.
 */
const DEFAULT_MAILBOX = "INBOX";

/** The mailbox itself, or the people who fill it. */
type MailView = "mailbox" | "senders";

interface SenderStandingProps {
  address: string;
  /** `"pin"`, `"mute"`, or null — the daemon's current standing decision about this sender. */
  verdict: string | null;
  busy: boolean;
  onDecide: (next: SenderVerdict | null) => void;
}

/**
 * The standing decision about whoever sent this message.
 *
 * A pin and a mute are the two rules in `priority.rs` that outrank the model outright — a pinned
 * sender is urgent whatever the classifier thought, a muted one is noise. They have been read on
 * every message since the table existed and, until now, written by nothing: the only way to set the
 * highest-authority rule in triage was to open the database by hand.
 *
 * Each button is its own toggle, so pressing the active one withdraws the decision rather than
 * needing a third "clear" control for a state that is already on screen.
 */
function SenderStanding({ address, verdict, busy, onDecide }: SenderStandingProps) {
  const choice = (value: SenderVerdict, label: string, help: string) => (
    <Button
      size="sm"
      variant={verdict === value ? "approve" : undefined}
      disabled={busy}
      aria-pressed={verdict === value}
      title={help}
      onClick={() => onDecide(verdict === value ? null : value)}
    >
      {label}
    </Button>
  );

  return (
    <span className="sender-standing" title={address}>
      {choice(
        "pin",
        "Always urgent",
        "Every future message from this sender is urgent, whatever the classifier decides.",
      )}
      {choice(
        "mute",
        "Always noise",
        "Every future message from this sender is noise, whatever the classifier decides.",
      )}
    </span>
  );
}

interface OpenMessageProps {
  token: string;
  detail: EmailDetail | null;
  loading: boolean;
  /** Folders that already exist at the root, offered as suggestions rather than as the only choices. */
  folders: string[];
  onFiled: () => void;
}
/**
 * An opened message: its text, and what came with it.
 *
 * The body is rendered as TEXT and never as markup. It is the one thing on screen written by
 * someone outside this machine, and the sidecar already reduced any HTML to plain text — putting it
 * back into the DOM as HTML would undo that and hand a stranger a script tag and a tracking pixel.
 */
function OpenMessage({ token, detail, loading, folders, onFiled }: OpenMessageProps) {
  const [saving, setSaving] = useState<number | null>(null);
  const [filing, setFiling] = useState<number | null>(null);
  const [bulk, setBulk] = useState<"saving" | "filing" | null>(null);
  const [folder, setFolder] = useState("");
  const [failed, setFailed] = useState<string | null>(null);
  const [filed, setFiled] = useState<string | null>(null);

  // One flag for every action on this message: two downloads at once would race the same folder
  // input, and a per-file button pressed mid-bulk would fetch the message a second time.
  const busy = saving !== null || filing !== null || bulk !== null;

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
    // The bytes never touched a disk on the way here; the browser's own download is what puts them
    // somewhere.
    offer(blob, file.filename);
  }

  // Puts one blob on disk under a name made safe here, because downloading through a blob is the
  // only way to send the bearer token and it skips the `Content-Disposition` the daemon built.
  function offer(blob: Blob, filename: string | null) {
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = safeDownloadName(filename);
    link.click();
    URL.revokeObjectURL(url);
  }

  // All of them, from ONE trip to the mailbox. A loop over the single fetch would pull the whole
  // message once per attachment — eight files meant downloading all eight, eight times.
  async function saveAll() {
    if (detail === null) return;
    setBulk("saving");
    setFailed(null);
    setFiled(null);
    const all = await fetchAllAttachments(token, detail.id);
    setBulk(null);
    if (all === null) {
      setFailed("Could not fetch these files. The message may have been deleted from the mailbox.");
      return;
    }
    for (const attachment of all) {
      offer(new Blob([base64ToBytes(attachment.content_base64)]), attachment.filename);
    }
  }

  async function fileAll() {
    if (detail === null) return;
    setBulk("filing");
    setFailed(null);
    setFiled(null);
    const stored = await saveAllAttachments(token, detail.id, folder);
    setBulk(null);
    if (stored === null) {
      setFailed("Could not file these. Check the folder name.");
      return;
    }
    setFiled(
      `${stored.length} ficheiro${stored.length === 1 ? "" : "s"} → ${folder === "" ? "mail/" : `mail/${folder}/`}`,
    );
    onFiled();
  }

  // Filing writes into the mail folder, which is the one place a stranger's bytes land on this
  // disk — and it happens because someone typed a folder and pressed a button.
  async function file(attachment: EmailAttachment) {
    if (detail === null) return;
    setFiling(attachment.position);
    setFailed(null);
    setFiled(null);
    const stored = await saveAttachment(token, detail.id, attachment.position, folder);
    setFiling(null);
    if (stored === null) {
      setFailed("Could not file this one. Check the folder name.");
      return;
    }
    // Reporting the stored name rather than the sender's, because they differ whenever the name
    // had to be made safe or collided with something already there.
    setFiled(`${stored} → ${folder === "" ? "mail/" : `mail/${folder}/`}`);
    onFiled();
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
        <>
          <label className="mf-folder">
            Arquivar em <span className="mf-root">mail/</span>
            <input
              type="text"
              list="mail-folders"
              value={folder}
              placeholder="(raiz)"
              onChange={(event) => setFolder(event.target.value)}
            />
            <datalist id="mail-folders">
              {folders.map((name) => <option key={name} value={name} />)}
            </datalist>
          </label>
          {/* All-at-once above the list, per-file beside each row: the same two actions at two
              scales, so choosing one file is never harder than choosing every file. */}
          <div className="mf-bulk">
            <Button size="sm" disabled={busy} onClick={() => void saveAll()}>
              {bulk === "saving"
                ? "A obter…"
                : `Descarregar ${detail.attachments.length} ficheiro${detail.attachments.length === 1 ? "" : "s"}`}
            </Button>
            <Button size="sm" disabled={busy} onClick={() => void fileAll()}>
              {bulk === "filing" ? "A arquivar…" : "Arquivar todos"}
            </Button>
          </div>
          <ul className="mail-files">
            {detail.attachments.map((attachment) => (
              <li key={attachment.position}>
                <span className="mf-name">{attachment.filename ?? "(sem nome)"}</span>
                <span className="mf-meta">
                  {attachment.mime_type ?? "tipo desconhecido"} ·{" "}
                  {formatBytes(attachment.size_bytes)}
                </span>
                <Button
                  size="sm"
                  disabled={busy}
                  onClick={() => void save(attachment)}
                >
                  {saving === attachment.position ? "A obter…" : "Descarregar"}
                </Button>
                <Button
                  size="sm"
                  disabled={busy}
                  onClick={() => void file(attachment)}
                >
                  {filing === attachment.position ? "A arquivar…" : "Arquivar"}
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}
      {filed !== null && <p className="gate-note">Arquivado como {filed}</p>}
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
  const [folders, setFolders] = useState<string[]>([]);
  const [cursor, setCursor] = useState<EmailCursor | null>(null);
  const [requeuing, setRequeuing] = useState<number | null>(null);
  const [requeueNote, setRequeueNote] = useState<string | null>(null);
  /** The sender whose standing decision is being written, so only their buttons go quiet. */
  const [deciding, setDeciding] = useState<string | null>(null);
  /**
   * The outcome of the last standing decision, tied to the sender it was about.
   *
   * Held with its address rather than as a bare string, because the same note would otherwise print
   * under every message from every sender at once — the queue draws one row per message, and a busy
   * correspondent has several.
   */
  const [verdictNote, setVerdictNote] = useState<{ address: string; text: string } | null>(null);
  const [config, setConfig] = useState<EmailConfig | null>(null);
  const [view, setView] = useState<MailView>("mailbox");
  // The configured mailbox once the daemon has said which it is, and the default until then.
  const mailbox = config?.mailbox ?? DEFAULT_MAILBOX;

  // Only the top level, and only as suggestions in the folder box. A full browser is a different
  // screen; what this needs is to stop someone retyping "BACMAT" every time.
  const loadFolders = useCallback(async () => {
    if (token === null) return;
    const entries = await listMailFiles(token);
    setFolders((entries ?? []).filter((entry) => entry.is_dir).map((entry) => entry.name));
  }, [token]);

  /**
   * The daemon's mail settings, read once.
   *
   * Which mailbox is collected from decides which cursor to ask for, so it is fetched before the
   * cursor is meaningful — and it is read once rather than per poll because `state.rs` resolves it
   * at startup and never changes it: editing `.ai/email.yaml` means restarting the daemon.
   */
  useEffect(() => {
    if (token === null || connection !== "connected") return;
    let cancelled = false;
    void (async () => {
      const next = await getEmailConfig(token);
      if (!cancelled) setConfig(next);
    })();
    return () => { cancelled = true; };
  }, [connection, token]);

  const refresh = useCallback(
    async (background = false) => {
      if (token === null || connection !== "connected") return;
      if (!background) setLoading(true);
      // The cursor rides along with the queue: it answers "has collection stalled?", which is only
      // ever asked while looking at how much is waiting.
      const [next, nextCursor] = await Promise.all([
        getEmailQueue(token),
        getEmailCursor(token, mailbox),
      ]);
      setQueue(next);
      setCursor(nextCursor);
      if (!background) setLoading(false);
    },
    [connection, mailbox, token],
  );

  useEffect(() => {
    if (unavailable) {
      setQueue(null);
      setLoading(true);
      return;
    }
    void refresh();
    void loadFolders();
  }, [loadFolders, refresh, unavailable]);

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

  /**
   * Sends a message back to be read again.
   *
   * Offered on anything already classified, not only on `failed`, because the daemon's own
   * eligibility rule is "the body is still there" — which covers the verdict that was simply wrong
   * just as well as the one that errored.
   */
  async function requeue(id: number) {
    if (token === null) return;
    setRequeuing(id);
    setRequeueNote(null);
    const outcome = await requeueEmail(token, id);
    setRequeuing(null);
    if (outcome !== true) {
      setRequeueNote(requeueFailureMessage(outcome));
      return;
    }
    setRequeueNote("Back in the queue — it will be read on the next pass.");
    void refresh(true);
  }

  /**
   * Records — or withdraws — a standing decision about a sender.
   *
   * The note says what this did and did NOT do. A pin governs classification from here on; the mail
   * already on screen keeps the class it was given, and "Read again" is what applies the new
   * decision to it. Leaving that unsaid is how someone pins a sender, sees the message still marked
   * noise, and concludes the button is broken.
   */
  async function decide(address: string, next: SenderVerdict | null) {
    if (token === null) return;
    setDeciding(address);
    setVerdictNote(null);
    const result = await setSenderVerdict(token, address, next);
    setDeciding(null);
    if (!result.ok) {
      setVerdictNote({
        address,
        // 404 is not a fault here: it is what the daemon says about an address it has no contact
        // for, which cannot normally happen from this list — the message in front of you IS the
        // arrival that creates one — so it means the two have gone out of step.
        text: result.status === 404
          ? "The núcleo has no contact for this address. Reload the mailbox and try again."
          : "Could not record that decision.",
      });
      return;
    }
    setVerdictNote({
      address,
      text: next === null
        ? "Decision withdrawn. New mail from this sender goes back to being classified on its merits."
        : next === "pin"
          ? "New mail from this sender will be urgent. Use “Read again” to re-read what is already here."
          : "New mail from this sender will be noise. Use “Read again” to re-read what is already here.",
    });
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
        <span title={`Where collection got to in ${mailbox}. The cursor lives in the núcleo, so collection resumes here after a restart.`}>
          {mailbox} cursor{" "}
          <b>{cursor === null ? "nothing collected yet" : `uid ${cursor.last_uid}`}</b>
        </span>
        {config !== null && config.username !== "" && (
          <span title={config.host}>{config.username}</span>
        )}
      </div>
      {/* Enabled is not armed. The hook barrier is proven at startup, and until it is, mail is
          collected and expired but never read — a mailbox that fills up while the button does
          nothing, which looks like a broken button rather than a refused pillar. */}
      {config !== null && config.enabled && !config.armed && (
        <ErrorNote>
          Collection is on but triage is not armed: the núcleo could not prove, at startup, that a
          mail body can never reach a tool. Mail is still being collected and still expires on
          schedule; nothing is being read.
        </ErrorNote>
      )}
      {config?.local_triage_disabled != null && (
        <ErrorNote>
          Local triage was asked for and could not be provided, so nothing is being read rather than
          being sent to a remote model — {config.local_triage_disabled}
        </ErrorNote>
      )}
      <nav className="subnav" aria-label="Mail views">
        {(["mailbox", "senders"] as MailView[]).map((option) => (
          <button
            type="button"
            key={option}
            className="subtab"
            aria-current={view === option ? "page" : undefined}
            onClick={() => setView(option)}
          >
            {option}
          </button>
        ))}
      </nav>
      {/* The senders list is where a standing decision can be found again. Pinning happens on a
          message, and once that message leaves the queue the only trace of the pin is its effect. */}
      {view === "senders" && token !== null && <Senders token={token} />}
      {view === "mailbox" && (
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
        {requeueNote !== null && <p className="gate-note">{requeueNote}</p>}
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
                  {/* Outside the row button, not inside it: a button nested in a button is invalid
                      markup, and clicking it would toggle the message open as well. */}
                  <div className="a-actions">
                    {mail.triage_class !== null && (
                      <Button
                        size="sm"
                        disabled={requeuing !== null}
                        title="Clears the verdict and puts the message back in the queue to be read again."
                        onClick={() => void requeue(mail.id)}
                      >
                        {requeuing === mail.id ? "Requeuing…" : "Read again"}
                      </Button>
                    )}
                    <SenderStanding
                      address={mail.from_addr}
                      verdict={mail.sender_verdict}
                      busy={deciding === mail.from_addr}
                      onDecide={(next) => void decide(mail.from_addr, next)}
                    />
                  </div>
                  {verdictNote !== null && verdictNote.address === mail.from_addr && (
                    <p className="gate-note">{verdictNote.text}</p>
                  )}
                  {openId === mail.id && token !== null && (
                    <OpenMessage
                      token={token}
                      detail={detail}
                      loading={opening}
                      folders={folders}
                      onFiled={() => void loadFolders()}
                    />
                  )}
                </article>
              ))}
      </Panel>
      )}
    </section>
  );
}

export default Mail;
