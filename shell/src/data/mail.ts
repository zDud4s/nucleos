import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiBlob, apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The mail pillar: the inbound queue, its config, one mailbox's sync cursor,
 * and the one write this page can make — ask for a triage batch.
 *
 * Every shape and every route below was read off `core/src/http.rs` and the
 * modules it calls, not inferred from a name. Three things are load-bearing
 * and easy to get wrong by analogy with the rest of the shell:
 *
 * **`POST /email/triage` never throws for the reasons this page cares about.**
 * Every refusal — the pillar not armed, a batch already in flight, nothing
 * waiting, a governance gate closed — comes back `200 Json<TriageOutcome>`
 * with `queued: 0` and a sentence in `reason` (`triage.rs:910-917`). Only a
 * panicked task answers with a real 5xx. So `reason` is a *value* on the
 * mutation's success data, never an `ApiRefusal` a page catches — treating it
 * as an error would be inventing a failure the daemon did not report.
 *
 * **A queued row's `triage_class` being NULL is not the same fact as it being
 * `"noise"`.** NULL is triage not having reached this message yet
 * (`email.rs:791`); `"noise"` is triage having read it and found nothing
 * worth surfacing. `ui/state-map.ts`'s `email_class` domain is what keeps a
 * badge from blurring the two.
 *
 * **`GET /email/cursor` requires `mailbox`.** There is no "the" cursor; a
 * caller with no mailbox to ask about has nothing to query, so
 * {@link useMailCursor} takes the parameter as required rather than optional.
 */

/** One row of the inbound queue, exactly as `http.rs`'s `QueuedEmail` serialises. */
export interface QueuedEmail {
  id: number;
  from_addr: string;
  from_name: string | null;
  subject: string | null;
  /** Normalised UTC, `"…Z"`. */
  received_at: string;
  /** NULL = triage has not reached this message yet — see this module's header. */
  triage_class: string | null;
  triage_summary: string | null;
  triaged_at: string | null;
  /** `i64` 0/1 over the wire, NOT a JSON boolean. */
  has_attachments: number;
  /** `"pin"` | `"mute"` | `null` — a standing decision about the sender, if one exists. */
  sender_verdict: string | null;
}

/**
 * What `POST /email/triage` always answers, 200 or not at all.
 *
 * `reason` is non-null on every refusal and null on the one success case.
 * There is no field that says "this succeeded" other than `queued` being
 * greater than zero, or a `run_id` having been assigned — a batch that found
 * nothing to do is not a failure and `reason` says so instead of leaving the
 * page to guess from a zero.
 */
export interface TriageOutcome {
  queued: number;
  run_id: number | null;
  reason: string | null;
}

/** `GET /config/email`, infallible 200 — exactly as `http.rs`'s `EmailConfigView` serialises. */
export interface EmailConfigView {
  enabled: boolean;
  /**
   * `enabled && !armed` is a real, stable state: mail keeps arriving and
   * retention keeps pruning, but no triage batch will ever be asked for. It
   * is not a transitional flicker on the way to `armed`.
   */
  armed: boolean;
  host: string;
  username: string;
  mailbox: string;
  sent_mailbox: string | null;
  poll_interval_secs: number;
  notify_classes: string[];
  digest_hour_utc: number;
  retain_bodies_days: number;
  /** A reason, or null when there is nothing stopping local triage. */
  local_triage_disabled: string | null;
}

/**
 * `GET /email/cursor?mailbox=` — `{uidvalidity, last_uid}`, or the literal
 * JSON `null` when no cursor row exists yet for that mailbox.
 */
export type MailCursor = { uidvalidity: number; last_uid: number } | null;

/**
 * The inbound queue — `GET /email/queue`.
 *
 * **Exactly one filter, `q`, over sender/subject/summary (FTS + LIKE, NOT
 * bodies).** There is no class filter and no direction filter to build a key
 * or a query string for: the route hard-codes `direction = 'inbound'` and
 * `LIMIT 200`, so this hook accepts nothing else to filter by, on purpose —
 * offering a class dropdown here would be a control that talks to a filter
 * the daemon does not have.
 *
 * `keepPreviousData`: a keystroke in the search box changes the key, and a
 * list that blanks between two keys reads as the search having found nothing
 * rather than as the search still being typed.
 */
export function useMailQueue(q?: string) {
  const trimmed = q?.trim();
  const query = trimmed === undefined || trimmed === "" ? "" : `?q=${encodeURIComponent(trimmed)}`;
  return useQuery({
    queryKey: keys.mail.queue(trimmed === "" ? undefined : trimmed),
    queryFn: () => apiFetch<QueuedEmail[]>(`/email/queue${query}`),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** How many rows `GET /email/queue` returns at most — `http.rs`'s hard-coded `LIMIT 200`. */
export const MAIL_QUEUE_LIMIT = 200;

/** The e-mail pillar's own configuration. Admin scope, read-only from this page. */
export function useEmailConfig() {
  return useQuery({
    queryKey: keys.mail.config,
    queryFn: () => apiFetch<EmailConfigView>("/config/email"),
    refetchInterval: POLL.slow,
  });
}

/**
 * One mailbox's IMAP sync cursor.
 *
 * `mailbox` is required by the route and by this hook — there is nothing to
 * ask about without one. `POLL.queue`, the same cadence as the row list this
 * cursor advances alongside.
 */
export function useMailCursor(mailbox: string, options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: keys.mail.cursor(mailbox),
    queryFn: () => apiFetch<MailCursor>(`/email/cursor?mailbox=${encodeURIComponent(mailbox)}`),
    refetchInterval: POLL.queue,
    enabled: options.enabled !== false,
  });
}

/**
 * Ask for a triage batch.
 *
 * No request body — the route takes none. `reason` on the answer is read by
 * the page as a sentence, never as this mutation's `isError`: see this
 * module's header for why a 200 carrying `reason` is not a failure.
 *
 * Invalidates the whole `mail` namespace rather than one queue key: a batch
 * that actually ran changes `triage_class` on rows the queue already holds
 * under whatever filter was active, and it is the daemon's `run_id`, not this
 * page, that decides which rows moved.
 */
export function useTriage() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<TriageOutcome>("/email/triage", { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.mail.all });
    },
  });
}

/**
 * How many rows in a queue reading have not been triaged yet.
 *
 * Pure, and the one thing this file exports that is not a hook — the sidebar
 * badge and the page's own headline both need this count and neither should
 * have to mount a second query to get it. `NULL` is the only reading that
 * counts: a row that came back `"noise"` was read and dismissed, which is a
 * different fact from one triage has not reached at all.
 */
export function untriagedCount(rows: QueuedEmail[]): number {
  return rows.filter((row) => row.triage_class === null).length;
}

/**
 * How many rows in a queue reading are the reader's own to answer.
 *
 * `urgent` and `action` are the two classes triage assigns when a person has to
 * do something; everything else is the machine reporting. Kept beside
 * {@link untriagedCount} because the difference between the two is the whole
 * point: one counts the owner's backlog, the other counts the daemon's.
 */
export function awaitingYouCount(rows: QueuedEmail[]): number {
  return rows.filter((row) => row.triage_class === "urgent" || row.triage_class === "action").length;
}

/* ---------------------------------------------------------------- detail -- */

/**
 * One attachment's metadata, exactly as `GET /email/{id}` flattens it onto
 * `EmailDetailResponse.attachments`.
 *
 * `filename` here is read out of the message's own MIME parts at ingest time —
 * it is the sender's name for the file, not what a save writes to disk. Never
 * conflate the two: {@link SavedFile.filename} is the DAEMON's name, chosen
 * after sanitisation and de-collision, and the two can legitimately differ for
 * the same attachment.
 */
export interface EmailAttachment {
  /** A zero-based index in message order — NOT a filename, and not stable across messages. */
  position: number;
  filename: string | null;
  mime_type: string | null;
  size_bytes: number;
}

/**
 * One message in full — `GET /email/{id}`.
 *
 * A superset of {@link QueuedEmail}'s facts, read off `http.rs`'s flattened
 * `EmailDetail` plus the `attachments` array it appends. Two fields do not
 * exist on the queue row: `model_class` is what the model itself said, before
 * any rule adjusted it, and `priority_rule` is which rule (if any) overrode
 * that into `triage_class`.
 *
 * **`body_text: null` means retention pruned it, not that the message never
 * had one.** This is the field {@link useRequeue} eligibility reads — never
 * `triage_class`, since a merely misclassified message is exactly as
 * requeueable as one triage never reached; only a purged body makes requeuing
 * impossible to act on.
 */
export interface EmailDetail {
  id: number;
  from_addr: string;
  from_name: string | null;
  subject: string | null;
  received_at: string;
  triage_class: string | null;
  triage_summary: string | null;
  triaged_at: string | null;
  model_class: string | null;
  priority_rule: string | null;
  body_text: string | null;
  has_attachments: number;
  attachments: EmailAttachment[];
  /**
   * `"pin"` | `"mute"` | `null` — the standing decision about this sender, the
   * same field {@link QueuedEmail} carries and matched by the same address
   * normalisation (`core/src/http.rs`'s `get_email`). The message page is where
   * that decision is changed, and a control that cannot say which way it is set
   * can only be pressed blind.
   */
  sender_verdict: string | null;
}

/** One message, in full. No `keepPreviousData`: a stale detail under the wrong id is worse than a loading state. */
export function useEmail(id: number) {
  return useQuery({
    queryKey: keys.mail.detail(id),
    queryFn: () => apiFetch<EmailDetail>(`/email/${id}`),
    refetchInterval: POLL.queue,
  });
}

/**
 * What a save answers — `SavedFile` from `files.rs`.
 *
 * `filename` is the name the attachment was ACTUALLY written under: sanitised
 * by `email::safe_filename` and de-collided by `available_name` against
 * whatever else is already in the folder. A page that shows the sender's name
 * here instead would be showing a promise the filesystem may not have kept.
 */
export interface SavedFile {
  filename: string;
  folder: string;
}

/**
 * Save one attachment to the files folder.
 *
 * No `folder` parameter: this page always saves to the root (`""`, which
 * `#[serde(default)]` on the daemon's side already means), and the mutation's
 * whole answer is `SavedFile` — the name actually written, for the page to
 * show instead of the sender's.
 */
export function useSaveAttachment(emailId: number) {
  return useMutation({
    mutationFn: (position: number) =>
      apiFetch<SavedFile>(`/email/${emailId}/attachments/${position}/save`, {
        method: "POST",
        body: JSON.stringify({}),
      }),
    retry: false,
  });
}

/** What `POST /email/{id}/attachments/save-all` answers — every name post-sanitisation, in message order. */
export interface SavedAttachments {
  folder: string;
  filenames: string[];
}

/** Save every attachment on the message in one call. */
export function useSaveAllAttachments(emailId: number) {
  return useMutation({
    mutationFn: () =>
      apiFetch<SavedAttachments>(`/email/${emailId}/attachments/save-all`, {
        method: "POST",
        body: JSON.stringify({}),
      }),
    retry: false,
  });
}

/**
 * Fetch one attachment's bytes and hand them to the browser as a download.
 *
 * `apiBlob`, not `apiFetch` — see its own doc comment. There is no daemon-side
 * name for these bytes to show: the download's suggested filename is
 * whatever the caller passes, typically the sender's `EmailAttachment.filename`
 * — cosmetic only, unlike {@link SavedFile.filename} above, which is a fact
 * about what actually exists on disk.
 */
export function useDownloadAttachment(emailId: number) {
  return useMutation({
    mutationFn: async ({ position, suggestedName }: { position: number; suggestedName: string }) => {
      const blob = await apiBlob(`/email/${emailId}/attachments/${position}`);
      const url = URL.createObjectURL(blob);
      try {
        const link = document.createElement("a");
        link.href = url;
        link.download = suggestedName;
        document.body.appendChild(link);
        link.click();
        document.body.removeChild(link);
      } finally {
        URL.revokeObjectURL(url);
      }
    },
    retry: false,
  });
}

/**
 * Ask for this one message to be triaged again.
 *
 * `204` on success, so `apiFetch<void>` is safe unlike the cases elsewhere in
 * this shell that need `apiText` — `apiFetch` already exempts 204/205 from
 * JSON parsing.
 *
 * **The two 409s this route can answer — a purged body and a run already
 * holding the message — are indistinguishable on the wire**, same status, no
 * body, no discriminator. This page never lets the second one surface as a
 * mystery: eligibility is decided from `body_text` before the button is even
 * offered (see {@link EmailDetail}'s header), so a 409 that does arrive here
 * can only be the run-in-progress case, and the page's own copy says so
 * instead of repeating the daemon's silence.
 */
export function useRequeue(emailId: number) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<void>(`/email/${emailId}/requeue`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.mail.all });
    },
  });
}

/** What `POST /email/send` accepts — one recipient, always required, along with a subject and a body. */
export interface ReplyInput {
  to: string;
  subject: string;
  body: string;
}

/**
 * Send a reply.
 *
 * **There is no SMTP pre-flight.** `GET /config/email` carries no field that
 * says whether a submission host is configured, so this form cannot be
 * disabled in advance with a reason — it is always open, and a daemon with
 * nowhere to send from answers 503 only after the attempt. Every refusal this
 * route makes is bare prose the daemon wrote on purpose
 * (`"a recipient must not contain a line break"`, `"no submission host is
 * configured — set smtp_host in .ai/email.yaml"`, …) and is worth showing
 * verbatim rather than translated into shell copy.
 */
export function useSendReply() {
  return useMutation({
    mutationFn: (input: ReplyInput) =>
      apiFetch<void>("/email/send", { method: "POST", body: JSON.stringify(input) }),
    retry: false,
  });
}

/** What `POST /contacts/verdict` accepts. `verdict: null` withdraws a standing decision — the key must still be present. */
export interface SenderVerdictInput {
  address: string;
  verdict: "pin" | "mute" | null;
}

/**
 * Record, change or withdraw a standing decision about a sender.
 *
 * Invalidates both `mail` (a pin or mute changes how the queue reads, via
 * `QueuedEmail.sender_verdict`) and `contacts` — the two namespaces the design
 * splits this fact across.
 */
export function useSenderVerdict() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: SenderVerdictInput) =>
      apiFetch<void>("/contacts/verdict", { method: "POST", body: JSON.stringify(input) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.mail.all });
      void queryClient.invalidateQueries({ queryKey: keys.contacts.all });
    },
  });
}
