import { useState, type ReactNode } from "react";
import { useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useDownloadAttachment,
  useEmail,
  useRequeue,
  useSaveAllAttachments,
  useSaveAttachment,
  useSendReply,
  useSenderVerdict,
  type EmailAttachment,
  type EmailDetail,
  type SavedAttachments,
  type SavedFile,
} from "../data/mail";
import {
  Button,
  ConfirmButton,
  Crumb,
  ErrorNote,
  PageHeader,
  Panel,
  readState,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StateBadge,
} from "../ui";
import "./mail.css";

/**
 * One message: its facts, its body, its attachments, a standing decision
 * about the sender, and a reply.
 *
 * The shape follows `RunDetail.tsx` — a route-parameter guard, a not-found
 * guard, then the real page — for the same reason: `$emailId` is a string
 * anybody can type into a bookmark, and neither guard is a state a person can
 * reach from a link this shell draws itself.
 *
 * Every refusal on this page is read as what it actually is rather than
 * flattened into "something went wrong": a pruned body is not offered a
 * requeue at all, the two indistinguishable 409s on requeue are narrowed by
 * what the page already knows, and a reply's refusal is the daemon's own bare
 * prose, quoted rather than replaced. See `data/mail.ts`'s header for the
 * routes this reads and writes.
 */
export function MailDetail() {
  const params = useParams({ strict: false }) as { emailId?: string };
  const id = Number(params.emailId);
  const valid = Number.isSafeInteger(id) && id > 0;

  if (!valid) return <UnknownEmail raw={params.emailId} />;
  return <KnownEmail id={id} />;
}

function UnknownEmail({ raw }: { raw: string | undefined }) {
  return (
    <>
      <Crumb to="/mail">Mail</Crumb>
      <PageHeader title="Message" />
      <ErrorNote>
        <code>{raw ?? "(nothing)"}</code> is not a message id — messages are numbered.
      </ErrorNote>
    </>
  );
}

function KnownEmail({ id }: { id: number }) {
  const email = useEmail(id);
  const detail = email.data;

  if (detail === undefined) {
    return (
      <>
        <Crumb to="/mail">Mail</Crumb>
        <PageHeader title={`Message ${id}`} />
        {email.isError ? <DetailError error={email.error} /> : <p className="mail-loading">reading message {id}…</p>}
      </>
    );
  }

  return (
    <>
      {/* The way back, once and at the top — it was the last line of the page, under
          the reply form, reachable only by scrolling past everything read here. */}
      <Crumb to="/mail">Mail</Crumb>
      <PageHeader title={detail.subject ?? "(no subject)"} headline={headline(detail)} />

      <FactsPanel email={detail} />
      <BodyPanel email={detail} />
      {detail.has_attachments === 1 && <AttachmentsPanel emailId={id} attachments={detail.attachments} />}
      <SenderVerdict email={detail} />
      <ReplyForm to={detail.from_addr} subject={detail.subject} />
    </>
  );
}

/**
 * One derived sentence about who this is from and where triage got to.
 *
 * In the badge's own words, from the same map: this said "triaged as action"
 * directly above a badge reading "needs a reply, not today", which is one fact
 * under two names on one screen. A class the map does not know is quoted raw.
 */
function headline(email: EmailDetail): string {
  const from = email.from_name ?? email.from_addr;
  return `from ${from} — ${classWords(email.triage_class)}`;
}

function classWords(triageClass: string | null): string {
  if (typeof triageClass !== "string") return "not triaged yet";
  return readState("email_class", triageClass)?.label ?? `triaged as ${triageClass}`;
}

function DetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "there is no message with that number" }} />;
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this message</ErrorNote>;
}

/* ---------------------------------------------------------------- facts -- */

function FactsPanel({ email }: { email: EmailDetail }) {
  const requeue = useRequeue(email.id);
  // Eligibility is `body_text IS NOT NULL`, never the triage class — a merely
  // misclassified message is exactly as requeueable as one triage never
  // reached. See `data/mail.ts`'s `EmailDetail` header.
  const eligible = email.body_text !== null;

  return (
    <Panel
      title="This message"
      aside={
        eligible ? (
          <ConfirmButton
            label="Requeue for triage"
            confirmLabel="Requeue it now"
            variant="quiet"
            disabled={requeue.isPending}
            onConfirm={() => requeue.mutate()}
          />
        ) : undefined
      }
    >
      <dl className="mail-detail-facts">
        <Fact label="From">
          {email.from_name ?? email.from_addr}
          {email.from_name !== null && <span className="mail-detail-address"> {email.from_addr}</span>}
        </Fact>
        <Fact label="Received">
          <RelativeTime at={email.received_at} />
        </Fact>
        <Fact label="Triage">
          <StateBadge domain="email_class" state={email.triage_class} />
        </Fact>
        {/* Only when the two disagree. Said whenever it was present, it printed the badge's
            own words a second time directly beside the badge — one fact under two names. What
            is worth reading is the disagreement, and the rule that settled it. */}
        {email.model_class !== null && email.model_class !== email.triage_class && (
          <Fact label="Model said">
            {classWords(email.model_class)}
            <span className="mail-detail-address"> {email.model_class}</span>
          </Fact>
        )}
        {email.priority_rule !== null && <Fact label="Overridden by">{email.priority_rule}</Fact>}
      </dl>

      {!eligible && (
        <p className="mail-detail-pruned">
          the body was pruned by retention — there is nothing left here for triage to read again, so
          requeuing is not offered
        </p>
      )}
      {requeue.isError && <RequeueError error={requeue.error} />}
      {requeue.isSuccess && (
        <p className="mail-outcome" role="status">
          requeued for triage
        </p>
      )}
    </Panel>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="mail-detail-fact">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function RequeueError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          // `eligible` above already ruled out the pruned-body 409 — a 409
          // that still arrives here can only be the other, indistinguishable
          // one: a live triage run already holds this message.
          conflict: "a triage run is already holding this message — try again once it finishes",
          not_found: "this message is no longer in the queue — it may have been swept",
        }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing was requeued</ErrorNote>;
}

/* ----------------------------------------------------------------- body -- */

function BodyPanel({ email }: { email: EmailDetail }) {
  return (
    <Panel title="Body">
      {typeof email.body_text !== "string" ? (
        <p className="mail-detail-pruned">
          this message's body was pruned by retention — only the facts above remain
        </p>
      ) : (
        <pre className="mail-detail-body">{email.body_text}</pre>
      )}
    </Panel>
  );
}

/* ---------------------------------------------------------- attachments -- */

function AttachmentsPanel({ emailId, attachments }: { emailId: number; attachments: EmailAttachment[] }) {
  const saveAll = useSaveAllAttachments(emailId);

  return (
    <Panel
      title="Attachments"
      aside={
        attachments.length > 1 ? (
          <Button disabled={saveAll.isPending} onClick={() => saveAll.mutate()}>
            Save all
          </Button>
        ) : undefined
      }
    >
      <Rows label="Attachments">
        {attachments.map((attachment) => (
          <AttachmentRow key={attachment.position} emailId={emailId} attachment={attachment} />
        ))}
      </Rows>
      {saveAll.isSuccess && <SavedAllNote result={saveAll.data} />}
      {saveAll.isError && <AttachmentError error={saveAll.error} what="not everything could be saved" />}
    </Panel>
  );
}

function SavedAllNote({ result }: { result: SavedAttachments }) {
  const where = result.folder === "" ? "the files root" : result.folder;
  const noun = result.filenames.length === 1 ? "file" : "files";
  return (
    <p className="mail-outcome" role="status">
      saved {result.filenames.length} {noun} to {where}: {result.filenames.join(", ")}
    </p>
  );
}

function AttachmentRow({ emailId, attachment }: { emailId: number; attachment: EmailAttachment }) {
  const save = useSaveAttachment(emailId);
  const download = useDownloadAttachment(emailId);
  const senderName = attachment.filename ?? `attachment ${attachment.position}`;

  // One row of the panel's hairline-ruled list, not a box of its own: name, type
  // and size, then both actions on the same line, and any outcome spanning the
  // row underneath.
  return (
    <Row className="mail-detail-attachment">
      <span className="mail-detail-attachment-name">{senderName}</span>
      <span className="mail-detail-attachment-meta">
        <span className="mail-detail-attachment-type" title={attachment.mime_type ?? undefined}>
          {attachment.mime_type ?? "unknown type"}
        </span>
        <span aria-hidden="true">·</span>
        <span>{formatBytes(attachment.size_bytes)}</span>
      </span>
      <Button
        disabled={download.isPending}
        onClick={() => download.mutate({ position: attachment.position, suggestedName: senderName })}
      >
        Download
      </Button>
      <Button disabled={save.isPending} onClick={() => save.mutate(attachment.position)}>
        Save to files
      </Button>
      {/* The name shown here is the one the DAEMON wrote it under — sanitised
          and de-collided, and never assumed to be `senderName` above; the two
          can legitimately differ for the same attachment. */}
      {save.isSuccess && <SavedOneNote result={save.data} />}
      {save.isError && <AttachmentError error={save.error} what="that attachment could not be saved" />}
      {download.isError && <AttachmentError error={download.error} what="that attachment could not be downloaded" />}
    </Row>
  );
}

function SavedOneNote({ result }: { result: SavedFile }) {
  const where = result.folder === "" ? "the files root" : result.folder;
  return (
    <p className="mail-outcome" role="status">
      saved as <code>{result.filename}</code> in {where}
    </p>
  );
}

function AttachmentError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ unavailable: "no files folder is configured — nothing can be saved to disk" }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

/* -------------------------------------------------------- sender verdict -- */

/**
 * A standing decision about the sender, separate from this message's own
 * triage class — one is about a person, the other is about one thing they
 * sent.
 */
function SenderVerdict({ email }: { email: EmailDetail }) {
  const verdict = useSenderVerdict();
  const standing = email.sender_verdict;

  return (
    <Panel title="This sender" variant="dim">
      <p className="mail-note">{standingSentence(email.from_addr, standing)}</p>
      <div className="mail-detail-verdict-actions" role="group" aria-label="Standing decision about this sender">
        {/* `aria-pressed` and a rung of the neutral ladder, never a tone: pinning a sender is a
            preference, and Acting Green — which this button wore — is the colour of something
            executing right now. The same reasoning took the chip off the queue row. */}
        <Button
          aria-pressed={standing === "pin"}
          disabled={verdict.isPending}
          onClick={() => verdict.mutate({ address: email.from_addr, verdict: "pin" })}
        >
          Pin
        </Button>
        {/* The one action here with a consequence you would not see: muted mail stops being
            surfaced, and nothing on this page would ever say so again. It asks twice, naming
            who it is about. Pin and Clear are both one press — either is undone by the other. */}
        <span className="mail-detail-verdict-mute" data-set={standing === "mute" ? "true" : undefined}>
          <ConfirmButton
            label="Mute"
            confirmLabel="Mute them now"
            /* The address goes to the ear, not into the label: `subject` is a short row
               identifier — `#101`, a run id — and it is composed into the armed label AS
               TEXT, so an e-mail address there reserves the width of the whole address on a
               button that reads "Mute". The panel's own sentence already names the sender for
               the eye. */
            sayAs={`Mute ${email.from_addr} — their mail stops being surfaced`}
            variant="ghost"
            disabled={verdict.isPending || standing === "mute"}
            onConfirm={() => verdict.mutate({ address: email.from_addr, verdict: "mute" })}
          />
        </span>
        {standing !== null && (
          <Button
            disabled={verdict.isPending}
            onClick={() => verdict.mutate({ address: email.from_addr, verdict: null })}
          >
            Clear
          </Button>
        )}
      </div>
      {verdict.isSuccess && (
        <p className="mail-outcome" role="status">
          {verdictRecorded(verdict.variables?.verdict)}
        </p>
      )}
      {verdict.isError && <VerdictError error={verdict.error} />}
    </Panel>
  );
}

/**
 * What is standing now, in words — the fact the three buttons act on.
 *
 * The page could not say this at all before: `EmailDetail` carried no verdict,
 * so a sender the queue had just shown as "pinned" opened onto three buttons
 * with nothing marked, and the only way to know what you were about to undo was
 * to remember the row you came from.
 */
function standingSentence(address: string, verdict: string | null): ReactNode {
  if (verdict === "pin") {
    return (
      <>
        <code>{address}</code> is pinned — their mail keeps being surfaced in full. This is separate
        from the triage class above, which is about this one message.
      </>
    );
  }
  if (verdict === "mute") {
    return (
      <>
        <code>{address}</code> is muted — their mail stops being surfaced. This is separate from the
        triage class above, which is about this one message.
      </>
    );
  }
  return (
    <>
      No standing decision about <code>{address}</code> — pin them to keep seeing their mail in full,
      or mute them to stop it being surfaced. This is separate from the triage class above, which is
      about this one message.
    </>
  );
}

/** What the press just recorded — "recorded" alone did not say which of three buttons it was. */
function verdictRecorded(verdict: "pin" | "mute" | null | undefined): string {
  if (verdict === "pin") return "recorded — this sender is pinned";
  if (verdict === "mute") return "recorded — this sender is muted";
  if (verdict === null) return "recorded — no standing decision about this sender";
  return "recorded";
}

function VerdictError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "no mail has ever arrived from that address" }} />;
  }
  return <ErrorNote>the núcleo did not answer — that verdict was not recorded</ErrorNote>;
}

/* ---------------------------------------------------------------- reply -- */

/**
 * Reply to the sender.
 *
 * **There is no pre-flight.** The form is always open — see `useSendReply`'s
 * doc comment — and a daemon with nowhere to send from answers 503 only after
 * Send is pressed. That refusal, and every other one this route makes, is
 * bare prose the daemon wrote on purpose, and {@link daemonProse} is what
 * lets it through verbatim instead of being replaced by this shell's own
 * generic copy.
 */
function ReplyForm({ to, subject }: { to: string; subject: string | null }) {
  const send = useSendReply();
  const [body, setBody] = useState("");
  // The daemon may omit a field the type says is always present; absent is not null.
  const replySubject = typeof subject !== "string" ? "Re:" : subject.startsWith("Re:") ? subject : `Re: ${subject}`;

  return (
    <Panel title="Reply">
      <p className="mail-note">
        There is no pre-flight check here — the form is always open. If the daemon has nowhere to send
        this from, that refusal arrives after Send is pressed, named below, not before.
      </p>
      <form className="mail-detail-reply mail-stack" onSubmit={(event) => event.preventDefault()}>
        {/* To and Subject are fixed by the message being answered. They stay
            inputs, for their names, but are drawn as the facts they are — a
            bordered box reads as a place to type. */}
        <label className="mail-detail-reply-field">
          <span>To</span>
          <input value={to} readOnly aria-label="Reply recipient" />
        </label>
        <label className="mail-detail-reply-field">
          <span>Subject</span>
          <input value={replySubject} readOnly aria-label="Reply subject" />
        </label>
        <label className="mail-detail-reply-field">
          <span>Message</span>
          <textarea
            rows={6}
            value={body}
            aria-label="Reply body"
            onChange={(event) => setBody(event.target.value)}
          />
        </label>
        <ConfirmButton
          label="Send"
          confirmLabel="Send it now"
          variant="approve"
          intent="go"
          disabled={body.trim() === "" || send.isPending}
          onConfirm={() =>
            send.mutate(
              { to, subject: replySubject, body: body.trim() },
              { onSuccess: () => setBody("") },
            )
          }
        />
      </form>
      {send.isSuccess && (
        <p className="mail-outcome" role="status">
          sent
        </p>
      )}
      {send.isError && <SendError error={send.error} />}
    </Panel>
  );
}

function SendError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing was sent</ErrorNote>;
}

/**
 * Turn a refusal's own detail into a page-specific sentence, keyed by its
 * code — the pattern `RunDetail.tsx` uses for the same reason: `RefusalNote`
 * prefers page copy over its shared floor, so handing back `{ [code]: prose }`
 * is what makes the daemon's exact words win over this shell's generic
 * reading of the same status.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const prose = refusal.detail.trim();
  return prose === "" || prose === refusal.code ? {} : { [refusal.code]: prose };
}
