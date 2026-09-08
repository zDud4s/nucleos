import { useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
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
import { Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, RelativeTime, StateBadge } from "../ui";
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
      <PageHeader title="Message" />
      <ErrorNote>
        <code>{raw ?? "(nothing)"}</code> is not a message id — messages are numbered.
      </ErrorNote>
      <Link to="/mail">Back to the queue</Link>
    </>
  );
}

function KnownEmail({ id }: { id: number }) {
  const email = useEmail(id);
  const detail = email.data;

  if (detail === undefined) {
    return (
      <>
        <PageHeader title={`Message ${id}`} />
        {email.isError ? <DetailError error={email.error} /> : <p className="mail-loading">reading message {id}…</p>}
        <Link to="/mail">Back to the queue</Link>
      </>
    );
  }

  return (
    <>
      <PageHeader title={detail.subject ?? "(no subject)"} headline={headline(detail)} />

      <FactsPanel email={detail} />
      <BodyPanel email={detail} />
      {detail.has_attachments === 1 && <AttachmentsPanel emailId={id} attachments={detail.attachments} />}
      <SenderVerdict email={detail} />
      <ReplyForm to={detail.from_addr} subject={detail.subject} />

      <Link to="/mail">Back to the queue</Link>
    </>
  );
}

/** One derived sentence about who this is from and where triage got to. */
function headline(email: EmailDetail): string {
  const from = email.from_name ?? email.from_addr;
  const state = typeof email.triage_class !== "string" ? "not triaged yet" : `triaged as ${email.triage_class}`;
  return `from ${from} — ${state}`;
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
        {email.model_class !== null && <Fact label="Model said">{email.model_class}</Fact>}
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
      <ul className="ui-rows mail-detail-attachments">
        {attachments.map((attachment) => (
          <AttachmentRow key={attachment.position} emailId={emailId} attachment={attachment} />
        ))}
      </ul>
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

  return (
    <li className="ui-rows-row mail-detail-attachment">
      <span className="mail-detail-attachment-name">{senderName}</span>
      <span className="mail-detail-attachment-meta">
        {attachment.mime_type ?? "unknown type"} · {formatBytes(attachment.size_bytes)}
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
    </li>
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

  return (
    <Panel title="This sender" variant="dim">
      <p className="mail-note">
        A standing decision about <strong>{email.from_addr}</strong> — pin them to keep seeing their mail
        in full, or mute them to stop it being surfaced. This is separate from the triage class above,
        which is about this one message.
      </p>
      <div className="mail-detail-verdict-actions">
        <Button
          variant="approve"
          disabled={verdict.isPending}
          onClick={() => verdict.mutate({ address: email.from_addr, verdict: "pin" })}
        >
          Pin
        </Button>
        <Button disabled={verdict.isPending} onClick={() => verdict.mutate({ address: email.from_addr, verdict: "mute" })}>
          Mute
        </Button>
        <Button disabled={verdict.isPending} onClick={() => verdict.mutate({ address: email.from_addr, verdict: null })}>
          Clear
        </Button>
      </div>
      {verdict.isSuccess && (
        <p className="mail-outcome" role="status">
          recorded
        </p>
      )}
      {verdict.isError && <VerdictError error={verdict.error} />}
    </Panel>
  );
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
      <form className="mail-detail-reply" onSubmit={(event) => event.preventDefault()}>
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
