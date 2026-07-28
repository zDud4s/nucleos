import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchAllAttachments, fetchAttachment, getEmail, getEmailQueue, listMailFiles,
  saveAllAttachments, saveAttachment, triageEmail,
  type ConnectionState, type EmailAttachment, type EmailDetail, type QueuedEmail,
} from "./api";
import {
  base64ToBytes, formatBytes, mailLabel, mailTone, relativeTime, safeDownloadName,
} from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

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

  // Only the top level, and only as suggestions in the folder box. A full browser is a different
  // screen; what this needs is to stop someone retyping "BACMAT" every time.
  const loadFolders = useCallback(async () => {
    if (token === null) return;
    const entries = await listMailFiles(token);
    setFolders((entries ?? []).filter((entry) => entry.is_dir).map((entry) => entry.name));
  }, [token]);

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
    </section>
  );
}

export default Mail;
