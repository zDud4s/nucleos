import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
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
