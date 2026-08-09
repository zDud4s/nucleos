//! The email pillar's domain core (spec §4): ingestion, the noise gate, and the cursor.
//!
//! This module owns every statement that touches `emails` and `email_cursor` — `storage.rs` stays
//! table-agnostic. Everything here is reachable without a mailbox, a CLI or the sidecar, which is
//! why the rules that decide what happens to a message are pure functions with the I/O around them.
//!
//! The one invariant worth stating up front: a message body is untrusted third-party content, and
//! the system holds it for exactly as long as triage needs it (§7.2).

/// One message as the sidecar delivers it (spec §4.3). Headers arrive lowercased by the sidecar.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct IncomingMessage {
    /// Absent for the rare message with no `Message-ID`; ingestion synthesises one.
    #[serde(default)]
    pub message_id: Option<String>,
    pub uid: i64,
    pub from_addr: String,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// IMAP INTERNALDATE, RFC 3339.
    pub received_at: String,
    #[serde(default)]
    pub body_text: Option<String>,
    #[serde(default)]
    pub has_attachments: bool,
    #[serde(default)]
    pub attachments: Vec<IncomingAttachment>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

/// What a message carries besides its text, described rather than delivered.
///
/// No bytes: the sidecar reports a name, a type and a size — enough for a person to decide whether
/// something is worth opening — and the content is fetched from the mailbox only when asked.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct IncomingAttachment {
    /// Which attachment, from zero, in the order the message carries them.
    pub position: i64,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub size_bytes: i64,
}

/// A message the sidecar looked at but could not deliver (spec §3.3/§4.3). It carries the uid so
/// the cursor can move past it, and the reason so the user learns mail was skipped instead of
/// silently losing it.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SkippedMessage {
    pub uid: i64,
    pub reason: String,
}

/// The stored position in one mailbox.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Cursor {
    pub uidvalidity: i64,
    pub last_uid: i64,
    pub updated_at: String,
}

/// Why a message was classified as noise without spending a run on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseReason {
    /// The universal mailing-list / marketing signature.
    ListUnsubscribe,
    /// `Precedence: bulk | list | junk`.
    Precedence,
    /// `Auto-Submitted` anything other than `no`.
    AutoSubmitted,
    /// A sender that announces itself as unattended.
    NoReplySender,
}

impl NoiseReason {
    /// The user-facing summary stored on the row.
    pub fn summary(self) -> &'static str {
        match self {
            NoiseReason::ListUnsubscribe => "noise: mailing list (List-Unsubscribe)",
            NoiseReason::Precedence => "noise: bulk precedence",
            NoiseReason::AutoSubmitted => "noise: auto-submitted",
            NoiseReason::NoReplySender => "noise: no-reply sender",
        }
    }
}

/// Local parts that identify an unattended sender. Matched WHOLE, never as a prefix: `noreply` is
/// unambiguous, `noreply-team-2026` is somebody's real alias somewhere, and the cost asymmetry of
/// this gate is not symmetric — letting noise through costs tokens, filing a real message as noise
/// costs trust in the pillar (§4.2).
const NO_REPLY_LOCAL_PARTS: &[&str] = &["noreply", "no-reply", "donotreply", "mailer-daemon"];

/// PURE (spec §4.2): does this message announce itself as automated?
///
/// Deliberately conservative — it only catches what declares its own nature in a header a human
/// never sends. It lives in the núcleo rather than the sidecar so the rules are testable in Rust
/// next to the domain, and changing them never means recompiling Go.
///
/// Header lookup is case-insensitive even though the sidecar lowercases keys: a casing bug on the
/// Go side should not be able to switch this gate off silently.
pub fn classify_noise(
    headers: &std::collections::HashMap<String, String>,
    from_addr: &str,
) -> Option<NoiseReason> {
    let header = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };

    if header("list-unsubscribe").is_some() {
        return Some(NoiseReason::ListUnsubscribe);
    }
    if let Some(precedence) = header("precedence")
        && ["bulk", "list", "junk"]
            .iter()
            .any(|kind| precedence.eq_ignore_ascii_case(kind))
    {
        return Some(NoiseReason::Precedence);
    }
    // `Auto-Submitted: no` is the RFC 3834 way of saying "a human sent this", so it is the one
    // value that must NOT count.
    if let Some(auto) = header("auto-submitted")
        && !auto.eq_ignore_ascii_case("no")
    {
        return Some(NoiseReason::AutoSubmitted);
    }

    let local_part = from_addr.split('@').next().unwrap_or_default();
    if NO_REPLY_LOCAL_PARTS
        .iter()
        .any(|candidate| local_part.eq_ignore_ascii_case(candidate))
    {
        return Some(NoiseReason::NoReplySender);
    }
    None
}

/// The `runs.mode` a triage run carries. It is what `create_run_inner` reads to launch the CLI
/// with no tools (§5.5, barrier 1) and what `hooks.rs` reads to deny every tool call (barrier 2),
/// so the two barriers are keyed on the same fact rather than on two independent conditions.
pub const TRIAGE_MODE: &str = "email_triage";

/// Bodies are capped before storage: a triage prompt does not get better with a megabyte of
/// quoted thread, and the cap bounds how much untrusted content sits at rest.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// What an attachment is called when the sender gave it no usable name.
pub const FALLBACK_FILENAME: &str = "attachment.bin";

/// Filesystems stop around here, and a name this long is not a name anyone chose.
const MAX_FILENAME_BYTES: usize = 120;

/// Windows refuses these as filenames whatever the extension, and a program that tries anyway gets
/// an error at a moment it is not expecting one.
const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// PURE: reduces a sender-chosen filename to something safe to put in a header or on a filesystem.
///
/// This string is the least trustworthy value in the whole pillar. It is written by whoever sent
/// the mail, and it ends up in an HTTP header and, later, in a path — two places where the wrong
/// characters stop being text and start being syntax. A carriage return ends a header and starts
/// one of the sender's choosing; `../` walks out of the directory it was meant to stay in.
///
/// So it is rebuilt rather than checked: take the last path segment, drop what a filesystem or a
/// header cannot carry, and if nothing survives, name it ourselves. A name that cannot be made safe
/// is not worth preserving — the file it labels is unchanged either way.
pub fn safe_filename(raw: &str) -> String {
    // Both separators, always: a name arriving from a Unix sender is still going onto this disk.
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");

    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        // `<>:"|?*` are illegal on Windows; the rest are the characters that turn a filename into
        // an argument, a redirect or a second command when something later forgets to quote it.
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0' => '_',
            other => other,
        })
        .collect();

    // Trailing dots and spaces are silently dropped by Windows, so a name ending in one resolves to
    // a DIFFERENT file than the one written — the classic way a guard is stepped around.
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return FALLBACK_FILENAME.to_string();
    }

    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED_WINDOWS_NAMES.contains(&stem.as_str()) {
        return format!("_{trimmed}");
    }

    truncate_utf8(trimmed, MAX_FILENAME_BYTES).to_string()
}

/// A cursor older than this means a pile of already-read mail is about to arrive, which is the
/// only situation the backfill cutoff exists for. Seven days is what separates it from the laptop
/// that was off for the weekend (§4.4).
pub const CURSOR_STALE_DAYS: i64 = 7;

/// Mail older than this, in a batch where the cutoff is armed, is filed rather than triaged.
pub const BACKFILL_AGE_HOURS: i64 = 24;

/// The summary a backfilled row carries, so the reason is visible on the row itself.
pub const BACKFILL_SUMMARY: &str = "não triado (backfill)";

/// What one ingestion did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IngestOutcome {
    pub ingested: i64,
    pub duplicates: i64,
    pub cursor: i64,
}

/// PURE (spec §4.4): is the backfill cutoff armed for this batch?
///
/// Armed only when there is a pile of read mail to recover — no cursor at all, a cursor invalidated
/// by a `UIDVALIDITY` change, or a cursor that has not moved in `CURSOR_STALE_DAYS`.
///
/// This rule cost three wrong versions, each of which destroyed mail, so the shape matters more
/// than the brevity. Evaluating it on batch SELECTION turned any pause longer than a day (kill
/// switch, budget, daily cap) into "backfill". Evaluating it on EVERY ingestion looked like the fix
/// until you remember the daemon starts on `LogonTrigger`: a laptop closed for the weekend
/// resyncs on Monday with its cursor intact, and every Saturday message arrives older than 24h —
/// silently discarded, unrecoverably, because retention drops the body in the same transaction.
/// Tying it to the cursor catches exactly the case that justifies it and no other.
pub fn backfill_armed(
    cursor: Option<&Cursor>,
    incoming_uidvalidity: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(cursor) = cursor else {
        return true;
    };
    if cursor.uidvalidity != incoming_uidvalidity {
        return true;
    }
    match chrono::DateTime::parse_from_rfc3339(&cursor.updated_at) {
        // A cursor whose timestamp cannot be read is a cursor whose age is unknown. Treating that
        // as "recent" would be a guess in the direction that floods the queue.
        Err(_) => true,
        Ok(updated_at) => {
            now.signed_duration_since(updated_at.with_timezone(&chrono::Utc))
                > chrono::Duration::days(CURSOR_STALE_DAYS)
        }
    }
}

/// PURE: within an armed batch, is this particular message old enough to file instead of triage?
/// A `received_at` that will not parse is treated as recent — the direction that keeps the message
/// in the queue, since the alternative silently files mail on a formatting error.
pub fn is_backfill_message(received_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(received_at) {
        Err(_) => false,
        Ok(received) => {
            now.signed_duration_since(received.with_timezone(&chrono::Utc))
                > chrono::Duration::hours(BACKFILL_AGE_HOURS)
        }
    }
}

/// PURE: what gets recorded as a message's `Message-ID`. A message with no `Message-ID` is legal,
/// so it gets a synthetic one rather than a NULL.
///
/// Not the dedupe key, despite the name — that is `(mailbox, uidvalidity, uid)` since migration
/// 0024, because a header the sender writes is a claim and not an identity. This value is kept for
/// the record and for threading; nothing decides whether a message already exists by reading it.
pub fn message_key(message_id: Option<&str>, uidvalidity: i64, uid: i64) -> String {
    match message_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => format!("<nucleos-{uidvalidity}-{uid}>"),
    }
}

/// PURE: cap a body at `MAX_BODY_BYTES` without splitting a UTF-8 character.
pub fn truncate_body(body: &str) -> &str {
    truncate_utf8(body, MAX_BODY_BYTES)
}

/// PURE: cut a string to a byte budget without splitting a character in half.
///
/// Shared by the body cap and the filename cap. Two copies of a rule this easy to get subtly wrong
/// is how one of them ends up panicking on the first message with an accent in the wrong place.
fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// How a freshly ingested row enters: either pending, or already resolved without spending a run.
struct EntryClass {
    triage_class: Option<&'static str>,
    summary: Option<String>,
}

impl EntryClass {
    fn pending() -> Self {
        Self {
            triage_class: None,
            summary: None,
        }
    }
}

/// Ingests one delivered batch (spec §4.3), atomically.
///
/// Either every message lands and the cursor advances, or nothing happens and the sidecar
/// redelivers. There is deliberately no intermediate state: a cursor that moved past mail that was
/// not stored is mail lost with no way to notice.
///
/// `now` is a parameter rather than read inside, so the cutoff rules can be tested at any point in
/// time without waiting for one.
///
/// Wide rather than taking a struct, deliberately: every parameter is a distinct field of the
/// sidecar's wire envelope, and bundling them would let a caller inherit a default for one of them
/// — including `retain_bodies_days`, which decides whether a body survives.
/// PURE: whether this message was written by the account whose mailbox is being read.
///
/// Compared through `contacts::normalize_address` so both sides are reduced the same way a stored
/// correspondent is — `Duarte <D@Example.COM>` and `d@example.com` are one person, and a comparison
/// that said otherwise would withhold the owner's own sent mail rather than a stranger's.
///
/// An empty `owner` answers `false`, not `true`. With no address to compare against there is no
/// evidence the owner wrote anything, and this guards a brake: every brake in this tree fails
/// closed, `calendar.rs` being the one documented exception and for the opposite reason.
fn written_by_owner(owner: &str, from_addr: &str) -> bool {
    let owner = crate::contacts::normalize_address(owner);
    !owner.is_empty() && owner == crate::contacts::normalize_address(from_addr)
}

#[allow(clippy::too_many_arguments)]
pub async fn ingest_batch(
    pool: &sqlx::SqlitePool,
    direction: crate::contacts::MessageDirection,
    mailbox: &str,
    uidvalidity: i64,
    max_uid_examined: i64,
    skipped: &[SkippedMessage],
    messages: &[IncomingMessage],
    owner_address: &str,
    retain_bodies_days: u8,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<IngestOutcome> {
    let now_str = now.to_rfc3339();
    let direction_value = match direction {
        crate::contacts::MessageDirection::Inbound => "inbound",
        crate::contacts::MessageDirection::Outbound => "outbound",
    };
    let mut tx = pool.begin().await?;

    let stored: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT uidvalidity, last_uid, updated_at FROM email_cursor WHERE mailbox = ?",
    )
    .bind(mailbox)
    .fetch_optional(tx.as_mut())
    .await?;
    let cursor = stored.map(|(uidvalidity, last_uid, updated_at)| Cursor {
        uidvalidity,
        last_uid,
        updated_at,
    });
    let armed = backfill_armed(cursor.as_ref(), uidvalidity, now);

    let mut ingested = 0i64;
    let mut duplicates = 0i64;
    let mut highest_delivered = 0i64;
    let mut foreign_in_sent = 0i64;

    for message in messages {
        highest_delivered = highest_delivered.max(message.uid);
        let key = message_key(message.message_id.as_deref(), uidvalidity, message.uid);

        // The class is decided BEFORE the insert rather than patched in afterwards. A second
        // UPDATE keyed on the same row would also reach a redelivered message that an in-flight
        // batch had already claimed, and race that run's verdict (§4.2).
        let entry = if let Some(reason) = classify_noise(&message.headers, &message.from_addr) {
            EntryClass {
                triage_class: Some("noise"),
                summary: Some(reason.summary().to_string()),
            }
        } else if armed && is_backfill_message(&message.received_at, now) {
            EntryClass {
                triage_class: Some("info"),
                summary: Some(BACKFILL_SUMMARY.to_string()),
            }
        } else {
            EntryClass::pending()
        };

        // A row classified here never enters the queue, so in the steady state it never needs its
        // body. But that is only true at `retain_bodies_days == 0`: the default keeps bodies so the
        // calibration week (§9) has a real corpus to judge the classifier against, and dropping
        // them at classification would make that week one-way — a wrong call could never be
        // reviewed. With retention on, periodic pruning is what clears them (§7.2).
        let body = match direction {
            crate::contacts::MessageDirection::Outbound => None,
            crate::contacts::MessageDirection::Inbound => match entry.triage_class {
                Some(_) if retain_bodies_days == 0 => None,
                _ => message.body_text.as_deref().map(truncate_body),
            },
        };
        let to_addrs = match direction {
            crate::contacts::MessageDirection::Inbound => None,
            crate::contacts::MessageDirection::Outbound => {
                // As in classify_noise, a casing bug on the Go side must not switch this off silently.
                message
                    .headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("to"))
                    .map(|(_, value)| value.as_str())
            }
        };
        // The names of files the user sent are what they wrote, not who they wrote to, and nothing
        // reads them. SentMessage omits them, but refusing both the flag and attachment rows here
        // keeps that guarantee if a future client sends them anyway. The flag must describe what is
        // stored; claiming attachments while recording none would be a worse lie than either answer.
        let has_attachments = match direction {
            crate::contacts::MessageDirection::Inbound => message.has_attachments,
            crate::contacts::MessageDirection::Outbound => false,
        };
        let triaged_at = entry.triage_class.map(|_| now_str.as_str());

        let result = sqlx::query(
            "INSERT OR IGNORE INTO emails
                 (message_id, mailbox, uidvalidity, uid, from_addr, from_name, subject, body_text,
                  has_attachments, received_at, ingested_at, triage_class, triage_summary,
                  triaged_at, direction, to_addrs)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&key)
        .bind(mailbox)
        .bind(uidvalidity)
        .bind(message.uid)
        .bind(&message.from_addr)
        .bind(message.from_name.as_deref())
        .bind(message.subject.as_deref())
        .bind(body)
        .bind(i64::from(has_attachments))
        .bind(&message.received_at)
        .bind(&now_str)
        .bind(entry.triage_class)
        .bind(entry.summary.as_deref())
        .bind(triaged_at)
        .bind(direction_value)
        .bind(to_addrs)
        .execute(tx.as_mut())
        .await?;

        if result.rows_affected() == 1 {
            match direction {
                crate::contacts::MessageDirection::Inbound => {
                    crate::contacts::record_inbound(
                        &mut tx,
                        &message.from_addr,
                        message.from_name.as_deref(),
                        &message.received_at,
                    )
                    .await?;
                }
                crate::contacts::MessageDirection::Outbound => {
                    // Only this account's OWN sent mail is evidence that this account has written
                    // to somebody. `sent_mailbox` is owner configuration and nothing validates what
                    // it points at (THREAT_MODEL, known gap 6): aimed at a shared or archive
                    // folder, every message in it would latch `outbound_ever` for recipients this
                    // account never wrote to.
                    //
                    // That is not a cosmetic error in a contact list. `priority.rs`'s
                    // `first-contact` rule reads exactly this field to decide whether an `urgent`
                    // from a stranger may keep its class, so a wrongly latched row retires a brake
                    // — silently, and for precisely the senders the brake exists to hold back.
                    if written_by_owner(owner_address, &message.from_addr) {
                        let recipients = to_addrs
                            .into_iter()
                            .flat_map(crate::contacts::split_address_list)
                            .filter(|address| !address.trim().is_empty())
                            .collect::<Vec<_>>();
                        crate::contacts::record_outbound(
                            &mut tx,
                            &recipients,
                            &message.received_at,
                        )
                        .await?;
                    } else {
                        // The message itself is still stored — it IS in the mailbox being read, and
                        // dropping it would lose mail. What is withheld is the CLAIM about who
                        // corresponds with whom.
                        foreign_in_sent += 1;
                    }
                }
            }
            ingested += 1;
            // Only for a row this batch actually created. A duplicate already has its attachments,
            // and re-inserting them would either collide on the UNIQUE or silently double a list
            // the user reads as "what came with this message".
            let email_id: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
                .fetch_one(tx.as_mut())
                .await?;
            if matches!(direction, crate::contacts::MessageDirection::Inbound) {
                for attachment in &message.attachments {
                    sqlx::query(
                        "INSERT OR IGNORE INTO email_attachments
                             (email_id, position, filename, mime_type, size_bytes)
                         VALUES (?, ?, ?, ?, ?)",
                    )
                    .bind(email_id)
                    .bind(attachment.position)
                    .bind(attachment.filename.as_deref())
                    .bind(attachment.mime_type.as_deref())
                    .bind(attachment.size_bytes)
                    .execute(tx.as_mut())
                    .await?;
                }
            }
        } else {
            duplicates += 1;
        }
    }

    // Mail the sidecar could not read still has to be visible, or it disappears leaving nothing but
    // a line in the sidecar's log. Inside the transaction, because the cursor is about to move past
    // it (§4.3).
    for skip in skipped {
        crate::feed::append_on(
            tx.as_mut(),
            None,
            "email_fetch_skipped",
            &format!(
                "skipped uid {} in {}: {}",
                skip.uid,
                mailbox,
                skip.reason.trim()
            ),
            None,
        )
        .await?;
    }

    // A `sent_mailbox` pointing somewhere it should not is the one email misconfiguration that
    // cannot be seen from its effects: the mail still arrives, the queue still fills, and the only
    // symptom is a brake in `priority.rs` that stops engaging. So it is reported the same way an
    // unreadable message is — a feed row, in the same transaction as the cursor that moved past it.
    // Once per batch rather than once per message: a first sync of a shared folder is one mistake,
    // not four hundred.
    if foreign_in_sent > 0 {
        crate::feed::append_on(
            tx.as_mut(),
            None,
            "email_sent_mailbox_foreign",
            &format!(
                "{foreign_in_sent} message{} in {mailbox} {} not written by this account, so \
                 the recipients were not recorded as people you have written to — check \
                 `sent_mailbox` in .ai/email.yaml",
                if foreign_in_sent == 1 { "" } else { "s" },
                if foreign_in_sent == 1 { "was" } else { "were" },
            ),
            None,
        )
        .await?;
    }

    // The cursor advances over what was EXAMINED, not only over what was delivered — that is what
    // unblocks a message the sidecar cannot parse (§4.3). A batch with no messages and a higher
    // `max_uid_examined` is therefore a legitimate, meaningful call.
    let advanced = max_uid_examined.max(highest_delivered);
    let next_uid = match &cursor {
        // A UIDVALIDITY change means the server's uid space was rebuilt: the old position is not a
        // position any more, so it is replaced rather than advanced.
        Some(cursor) if cursor.uidvalidity == uidvalidity => cursor.last_uid.max(advanced),
        _ => advanced,
    };

    sqlx::query(
        "INSERT INTO email_cursor (mailbox, uidvalidity, last_uid, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(mailbox) DO UPDATE SET
             uidvalidity = excluded.uidvalidity,
             last_uid = excluded.last_uid,
             updated_at = excluded.updated_at",
    )
    .bind(mailbox)
    .bind(uidvalidity)
    .bind(next_uid)
    .bind(&now_str)
    .execute(tx.as_mut())
    .await?;

    tx.commit().await?;
    Ok(IngestOutcome {
        ingested,
        duplicates,
        cursor: next_uid,
    })
}

/// A run is terminal when it can no longer write anything: everything except `running` and
/// `awaiting_approval`. Written as a SQL fragment because both the requeue guard and the triage
/// loop's single-flight check ask the same question, and two spellings of it would drift.
/// (A triage run never reaches `awaiting_approval` — it has no tools to trigger an approval.)
///
/// Built from [`crate::runs::TERMINAL_RUN_STATUSES`] rather than spelled out here, because a
/// hand-written third copy of that list is precisely how `superseded` went missing from the
/// worktree GC: this fragment had it, `worktree::GC_CANDIDATES_SQL` did not, and nothing compared
/// the two. One list, quoted for SQL in one place.
pub static RUN_IS_TERMINAL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let statuses = crate::runs::TERMINAL_RUN_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!("status IN ({statuses})")
});

/// Why a requeue was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequeueError {
    UnknownEmail,
    /// The body was already dropped, so there is nothing left to re-triage (§7.2).
    BodyPurged,
    /// A batch currently holds this row (§5.1).
    ClaimedByRun(i64),
}

/// Returns a classified message to the pending queue (spec §4.3).
///
/// Eligibility is `body_text IS NOT NULL` rather than `class = 'failed'`, because that predicate is
/// literally the condition for a re-triage to be possible — and it covers the row the classifier
/// merely got wrong just as well as the one that failed.
///
/// `ingested_at` is deliberately untouched: the message did not re-arrive, and that column governs
/// queue order and pruning.
pub async fn requeue(pool: &sqlx::SqlitePool, id: i64) -> Result<(), RequeueError> {
    let row: Option<(Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT body_text, triage_run_id FROM emails WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|_| RequeueError::UnknownEmail)?;
    let (body, claim) = row.ok_or(RequeueError::UnknownEmail)?;
    if body.is_none() {
        return Err(RequeueError::BodyPurged);
    }

    // Without this guard a requeue mid-batch clears the claim, the next tick launches a second run
    // over the same message, and both write a verdict — §5.1's mutual exclusion reopened from
    // behind.
    if let Some(run_id) = claim {
        // `AssertSqlSafe` because sqlx only accepts `&'static str` otherwise. The interpolated
        // fragment is built in this file from a fixed list of status literals, and the run id stays
        // a bound parameter, so nothing caller-supplied reaches the SQL text (same justification as
        // `shadow.rs`).
        let terminal: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT 1 FROM runs WHERE id = ? AND {}",
            RUN_IS_TERMINAL.as_str()
        )))
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| RequeueError::ClaimedByRun(run_id))?;
        if terminal.is_none() {
            return Err(RequeueError::ClaimedByRun(run_id));
        }
    }

    sqlx::query(
        "UPDATE emails
            SET triage_class = NULL, triage_run_id = NULL, triage_summary = NULL,
                triaged_at = NULL, triage_attempts = 0, infra_failures = 0
          WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|_| RequeueError::UnknownEmail)?;
    Ok(())
}

/// Reads the cursor for one mailbox. `None` means this mailbox was never synchronised, which is
/// also one of the three conditions that arm the backfill cutoff (§4.4).
pub async fn get_cursor(pool: &sqlx::SqlitePool, mailbox: &str) -> sqlx::Result<Option<Cursor>> {
    let row: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT uidvalidity, last_uid, updated_at FROM email_cursor WHERE mailbox = ?",
    )
    .bind(mailbox)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(uidvalidity, last_uid, updated_at)| Cursor {
        uidvalidity,
        last_uid,
        updated_at,
    }))
}

#[cfg(test)]
mod tests {
    /// The account whose mailbox these tests read: the address every fixture message is from, so a `Sent` folder holding them is this account's own.
    ///
    /// `ingest_batch` compares it against each sent message's `From`, so a value that did not
    /// match would stop the outbound fixtures recording anything and quietly hollow out every
    /// assertion about `outbound_ever` below.
    const OWNER: &str = "ana@company.com";

    use super::*;
    use sqlx::Row;

    pub(crate) async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn migration_creates_the_email_tables() {
        let pool = test_pool().await;
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'email%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        // The four `emails_fts_*` entries are FTS5's own storage for the index 0058 declares, not
        // tables anything here writes to. They are listed rather than filtered out because this
        // assertion is a schema pin: the next person to add or drop an email table should have to
        // say so here, and that includes the day somebody decides the index is not worth its space.
        assert_eq!(
            tables,
            vec![
                "email_attachments",
                "email_cursor",
                "emails",
                "emails_fts",
                "emails_fts_config",
                "emails_fts_data",
                "emails_fts_docsize",
                "emails_fts_idx",
            ]
        );
    }

    /// The partial index is what makes the pending queue cheap to read every tick; without the
    /// WHERE clause it would be an ordinary index over the whole table.
    #[tokio::test]
    async fn the_pending_index_is_partial() {
        let pool = test_pool().await;
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'emails_pending'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            sql.contains("WHERE triage_class IS NULL"),
            "the pending index must stay partial: {sql}"
        );
    }

    #[tokio::test]
    async fn an_email_row_round_trips_every_column() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, from_name,
                                 subject, body_text, has_attachments, received_at, ingested_at,
                                 triage_class, triage_summary, triage_run_id, triage_attempts,
                                 infra_failures, triaged_at)
             VALUES ('<a@b>', 'INBOX', 12, 34, 'x@y', 'X', 'subj', 'body', 1,
                     '2026-07-28T10:00:00+00:00', '2026-07-28T10:00:01+00:00',
                     'urgent', 'summary', 7, 2, 1, '2026-07-28T10:05:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let row = sqlx::query("SELECT * FROM emails WHERE message_id = '<a@b>'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("mailbox"), "INBOX");
        assert_eq!(row.get::<i64, _>("uidvalidity"), 12);
        assert_eq!(row.get::<i64, _>("uid"), 34);
        assert_eq!(row.get::<String, _>("from_addr"), "x@y");
        assert_eq!(
            row.get::<Option<String>, _>("from_name").as_deref(),
            Some("X")
        );
        assert_eq!(
            row.get::<Option<String>, _>("subject").as_deref(),
            Some("subj")
        );
        assert_eq!(
            row.get::<Option<String>, _>("body_text").as_deref(),
            Some("body")
        );
        assert_eq!(row.get::<i64, _>("has_attachments"), 1);
        assert_eq!(
            row.get::<String, _>("received_at"),
            "2026-07-28T10:00:00+00:00"
        );
        assert_eq!(
            row.get::<Option<String>, _>("triage_class").as_deref(),
            Some("urgent")
        );
        assert_eq!(row.get::<Option<i64>, _>("triage_run_id"), Some(7));
        assert_eq!(row.get::<i64, _>("triage_attempts"), 2);
        assert_eq!(row.get::<i64, _>("infra_failures"), 1);
    }

    /// Defaults matter here: a freshly ingested row must be *pending*, not accidentally counted as
    /// having already failed or been attempted.
    #[tokio::test]
    async fn a_minimal_row_defaults_to_pending() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, received_at,
                                 ingested_at)
             VALUES ('<c@d>', 'INBOX', 1, 2, 'a@b', '2026-07-28T10:00:00+00:00',
                     '2026-07-28T10:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let row = sqlx::query("SELECT * FROM emails WHERE message_id = '<c@d>'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(row.get::<Option<String>, _>("triage_class").is_none());
        assert!(row.get::<Option<i64>, _>("triage_run_id").is_none());
        assert_eq!(row.get::<i64, _>("triage_attempts"), 0);
        assert_eq!(row.get::<i64, _>("infra_failures"), 0);
        assert_eq!(row.get::<i64, _>("has_attachments"), 0);
    }

    async fn insert_email(
        pool: &sqlx::SqlitePool,
        message_id: &str,
        mailbox: &str,
        uidvalidity: i64,
        uid: i64,
    ) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr,
                                 received_at, ingested_at)
             VALUES (?, ?, ?, ?, 'a@b', '2026-07-28T10:00:00+00:00', '2026-07-28T10:00:00+00:00')",
        )
        .bind(message_id)
        .bind(mailbox)
        .bind(uidvalidity)
        .bind(uid)
        .execute(pool)
        .await
    }

    /// `Message-ID` is a header the sender composes, so it was an identity anyone could claim. Two
    /// messages carrying the same one used to collapse into one row, and the one that lost was
    /// whichever the mailbox delivered second — a way to suppress someone else's mail that costs a
    /// forged header. Mailing-list resends and forwards do it without meaning to.
    #[tokio::test]
    async fn two_messages_claiming_the_same_message_id_are_both_stored() {
        let pool = test_pool().await;
        insert_email(&pool, "<dup@x>", "INBOX", 1, 2).await.unwrap();
        insert_email(&pool, "<dup@x>", "INBOX", 1, 3).await.unwrap();

        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, 2);
    }

    /// What the dedupe is actually for: the same message arriving twice, which is what a redelivery
    /// after a dropped connection looks like. The server's own UID is the thing that says so.
    #[tokio::test]
    async fn the_same_message_on_the_server_can_only_be_stored_once() {
        let pool = test_pool().await;
        insert_email(&pool, "<a@x>", "INBOX", 1, 2).await.unwrap();
        // A different Message-ID does not make it a different message.
        assert!(insert_email(&pool, "<b@x>", "INBOX", 1, 2).await.is_err());
    }

    /// The other two thirds of the key. A UID means nothing on its own: mailboxes number
    /// independently, and `uidvalidity` changing is the server saying the numbering restarted.
    #[tokio::test]
    async fn a_uid_only_identifies_a_message_within_its_mailbox_and_uidvalidity() {
        let pool = test_pool().await;
        insert_email(&pool, "<a@x>", "INBOX", 1, 2).await.unwrap();
        insert_email(&pool, "<b@x>", "Archive", 1, 2)
            .await
            .expect("the same uid in another mailbox is another message");
        insert_email(&pool, "<c@x>", "INBOX", 2, 2)
            .await
            .expect("a uidvalidity reset renumbers from scratch");
    }

    /// Migration 0024 rebuilds two tables to drop a UNIQUE, which SQLite cannot do in place. A fresh
    /// database never exercises that — `migrate!()` runs it against empty tables — so the data path
    /// is tested here, against the schema as it stood at 0023 and with foreign keys ON, which is
    /// what makes `DROP TABLE emails` dangerous: the implicit DELETE fires the attachments' cascade.
    #[tokio::test]
    async fn the_rebuild_keeps_the_mail_and_its_attachments() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();

        sqlx::raw_sql(include_str!("../migrations/0019_email.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0021_email_attachments.sql"))
            .execute(&pool)
            .await
            .unwrap();

        // Two rows the OLD key allowed and the new one does not: one message stored twice under two
        // Message-IDs. The earlier row wins, and its attachment has to come through with it.
        insert_email(&pool, "<first@x>", "INBOX", 1, 7)
            .await
            .unwrap();
        insert_email(&pool, "<second@x>", "INBOX", 1, 7)
            .await
            .unwrap();
        insert_email(&pool, "<other@x>", "INBOX", 1, 8)
            .await
            .unwrap();
        for email_id in [1, 2, 3] {
            sqlx::query(
                "INSERT INTO email_attachments (email_id, position, filename, mime_type, size_bytes)
                 VALUES (?, 0, 'r.pdf', 'application/pdf', 10)",
            )
            .bind(email_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        sqlx::raw_sql(include_str!("../migrations/0024_email_server_key.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let surviving: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, message_id FROM emails ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            surviving,
            vec![(1, "<first@x>".to_owned()), (3, "<other@x>".to_owned())],
            "the earlier of the two rows sharing a server identity is the one kept"
        );

        // The cascade must not have taken these with it, and the orphan of the dropped row must not
        // have survived either.
        let attachments: Vec<i64> =
            sqlx::query_scalar("SELECT email_id FROM email_attachments ORDER BY email_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(attachments, vec![1, 3]);

        // And the constraint that was the point of the rebuild.
        assert!(
            insert_email(&pool, "<third@x>", "INBOX", 1, 8)
                .await
                .is_err()
        );
        insert_email(&pool, "<first@x>", "INBOX", 1, 9)
            .await
            .expect("a repeated Message-ID is no longer a collision");

        // The foreign key survived the rename, rather than being left pointing at `emails_new`.
        assert!(
            sqlx::query(
                "INSERT INTO email_attachments (email_id, position, size_bytes)
                 VALUES (9999, 0, 1)"
            )
            .execute(&pool)
            .await
            .is_err(),
            "email_attachments must still reference emails"
        );
    }

    fn headers(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn each_automation_header_is_noise() {
        /// headers, sender, expected verdict.
        type NoiseCase<'a> = (&'a [(&'a str, &'a str)], &'a str, NoiseReason);

        let cases: &[NoiseCase] = &[
            (
                &[("list-unsubscribe", "<mailto:x@y>")],
                "team@company.com",
                NoiseReason::ListUnsubscribe,
            ),
            (
                &[("precedence", "bulk")],
                "team@company.com",
                NoiseReason::Precedence,
            ),
            (
                &[("precedence", "List")],
                "team@company.com",
                NoiseReason::Precedence,
            ),
            (
                &[("precedence", "junk")],
                "team@company.com",
                NoiseReason::Precedence,
            ),
            (
                &[("auto-submitted", "auto-generated")],
                "team@company.com",
                NoiseReason::AutoSubmitted,
            ),
            (&[], "noreply@company.com", NoiseReason::NoReplySender),
            (&[], "no-reply@company.com", NoiseReason::NoReplySender),
            (&[], "donotreply@company.com", NoiseReason::NoReplySender),
            (&[], "mailer-daemon@company.com", NoiseReason::NoReplySender),
        ];
        for (hdrs, from, expected) in cases {
            assert_eq!(
                classify_noise(&headers(hdrs), from),
                Some(*expected),
                "{from} with {hdrs:?} should be {expected:?}"
            );
        }
    }

    /// The whole point of the gate is that it never touches mail a person actually sent.
    #[test]
    fn a_personal_email_is_not_noise() {
        let hdrs = headers(&[
            ("from", "Ana <ana@company.com>"),
            ("subject", "lunch?"),
            ("message-id", "<abc@company.com>"),
        ]);
        assert_eq!(classify_noise(&hdrs, "ana@company.com"), None);
    }

    /// RFC 3834's way of saying a human sent it — the one value that must not trip the gate.
    #[test]
    fn auto_submitted_no_is_not_noise() {
        assert_eq!(
            classify_noise(&headers(&[("auto-submitted", "no")]), "ana@company.com"),
            None
        );
    }

    #[test]
    fn the_sender_local_part_matches_case_insensitively() {
        assert_eq!(
            classify_noise(&headers(&[]), "NoReply@Company.COM"),
            Some(NoiseReason::NoReplySender)
        );
    }

    /// Whole-local-part matching, not prefix: a real alias that merely starts the same way is a
    /// person's mail, and filing it as noise is the expensive direction of this gate's error.
    #[test]
    fn a_sender_that_merely_starts_like_no_reply_is_not_noise() {
        assert_eq!(
            classify_noise(&headers(&[]), "noreply-team@company.com"),
            None
        );
        assert_eq!(
            classify_noise(&headers(&[]), "noreplying@company.com"),
            None
        );
    }

    /// The sidecar promises lowercase keys; a casing bug there must not silently disable the gate.
    #[test]
    fn header_lookup_survives_unexpected_casing() {
        assert_eq!(
            classify_noise(&headers(&[("List-Unsubscribe", "<mailto:x@y>")]), "a@b"),
            Some(NoiseReason::ListUnsubscribe)
        );
    }

    /// An unusual precedence value is not one of the three the spec names, and inventing a fourth
    /// would widen the gate past what a message actually declared.
    #[test]
    fn an_unknown_precedence_value_is_not_noise() {
        assert_eq!(
            classify_noise(&headers(&[("precedence", "first-class")]), "a@b"),
            None
        );
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-07-28T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn ago(hours: i64) -> String {
        (now() - chrono::Duration::hours(hours)).to_rfc3339()
    }

    fn message(uid: i64) -> IncomingMessage {
        IncomingMessage {
            message_id: Some(format!("<m{uid}@x>")),
            uid,
            from_addr: "ana@company.com".into(),
            from_name: Some("Ana".into()),
            subject: Some("hello".into()),
            received_at: ago(1),
            body_text: Some("body".into()),
            has_attachments: false,
            attachments: Vec::new(),
            headers: Default::default(),
        }
    }

    fn message_with_attachment(uid: i64) -> IncomingMessage {
        IncomingMessage {
            has_attachments: true,
            attachments: vec![IncomingAttachment {
                position: 0,
                filename: Some("cotacao.pdf".into()),
                mime_type: Some("application/pdf".into()),
                size_bytes: 4096,
            }],
            ..message(uid)
        }
    }

    /// The sender writes this string. It ends up in an HTTP header and, later, in a path — two
    /// places where the wrong character stops being text and becomes syntax.
    #[test]
    fn a_filename_is_rebuilt_rather_than_trusted() {
        // A carriage return ends a header and starts one the sender chose.
        assert_eq!(
            safe_filename("relatorio\r\nSet-Cookie: x=1.docx"),
            "relatorioSet-Cookie_ x=1.docx"
        );
        // Path traversal, both separators, because a Unix sender still writes to this disk.
        assert_eq!(
            safe_filename("../../.ssh/authorized_keys"),
            "authorized_keys"
        );
        assert_eq!(
            safe_filename(r"..\..\Windows\System32\evil.dll"),
            "evil.dll"
        );
        // Nothing usable left is named by us rather than guessed at.
        assert_eq!(safe_filename(""), FALLBACK_FILENAME);
        assert_eq!(safe_filename("   "), FALLBACK_FILENAME);
        assert_eq!(safe_filename(".."), FALLBACK_FILENAME);
        assert_eq!(safe_filename("../"), FALLBACK_FILENAME);
        // Windows drops trailing dots and spaces silently, so `x.docx.` resolves to a DIFFERENT
        // file than the one written — the classic way past a check on the extension.
        assert_eq!(safe_filename("relatorio.docx. "), "relatorio.docx");
        // Reserved device names fail at open() rather than at write().
        assert_eq!(safe_filename("CON.txt"), "_CON.txt");
        assert_eq!(safe_filename("nul"), "_nul");
        // Ordinary names, including accents, survive untouched.
        assert_eq!(
            safe_filename("MÉDIAS_ESPERADAS_e_percentis.docx"),
            "MÉDIAS_ESPERADAS_e_percentis.docx"
        );
    }

    /// Cutting at a byte budget must not split a character, or the name is invalid UTF-8 and the
    /// first accented attachment takes the process down.
    #[test]
    fn a_long_filename_is_cut_on_a_character_boundary() {
        let long = format!("{}.docx", "é".repeat(200));
        let cut = safe_filename(&long);
        assert!(cut.len() <= 120, "still {} bytes", cut.len());
        assert!(cut.chars().all(|c| c == 'é'));
    }

    #[tokio::test]
    async fn an_attachment_is_described_alongside_its_message() {
        let pool = test_pool().await;
        ingest(&pool, &[message_with_attachment(1)], 1).await;

        let row: (i64, String, i64) = sqlx::query_as(
            "SELECT position, filename, size_bytes FROM email_attachments
              JOIN emails ON emails.id = email_attachments.email_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (0, "cotacao.pdf".to_string(), 4096));
    }

    /// A redelivered message must not grow a second copy of its own attachment list — the list is
    /// read as "what came with this message", and a duplicate would read as two files.
    #[tokio::test]
    async fn a_redelivered_message_does_not_duplicate_its_attachments() {
        let pool = test_pool().await;
        ingest(&pool, &[message_with_attachment(1)], 1).await;
        ingest(&pool, &[message_with_attachment(1)], 1).await;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_attachments")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    async fn seed_cursor(pool: &sqlx::SqlitePool, uidvalidity: i64, last_uid: i64, age_days: i64) {
        sqlx::query(
            "INSERT INTO email_cursor (mailbox, uidvalidity, last_uid, updated_at)
             VALUES ('INBOX', ?, ?, ?)",
        )
        .bind(uidvalidity)
        .bind(last_uid)
        .bind((now() - chrono::Duration::days(age_days)).to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn ingest(
        pool: &sqlx::SqlitePool,
        messages: &[IncomingMessage],
        max_uid_examined: i64,
    ) -> IngestOutcome {
        ingest_batch(
            pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            1,
            max_uid_examined,
            &[],
            messages,
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap()
    }

    async fn class_of(pool: &sqlx::SqlitePool, message_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT triage_class FROM emails WHERE message_id = ?")
            .bind(message_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ingest_stores_a_message_and_advances_the_cursor() {
        let pool = test_pool().await;
        let outcome = ingest(&pool, &[message(10)], 10).await;
        assert_eq!(
            outcome,
            IngestOutcome {
                ingested: 1,
                duplicates: 0,
                cursor: 10
            }
        );
        assert_eq!(
            class_of(&pool, "<m10@x>").await,
            None,
            "must arrive pending"
        );
    }

    #[tokio::test]
    async fn a_redelivered_message_is_a_duplicate_not_an_error() {
        let pool = test_pool().await;
        ingest(&pool, &[message(10)], 10).await;
        let outcome = ingest(&pool, &[message(10)], 10).await;
        assert_eq!(outcome.ingested, 0);
        assert_eq!(outcome.duplicates, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn a_ingestao_acumula_o_contacto() {
        let pool = test_pool().await;
        let sender = "primeiro.contacto@example.com";
        let mut inbound = message(20);
        inbound.from_addr = sender.into();

        ingest(&pool, &[inbound], 20).await;

        let profile = crate::contacts::profile_for(&pool, sender).await.unwrap();
        assert!(
            profile.is_some(),
            "ingesting an inbound message must create the sender's contact profile"
        );
        let profile = profile.unwrap();
        assert_eq!(profile.messages_in, 1);
        assert!(!profile.outbound_ever);
    }

    #[tokio::test]
    async fn um_email_duplicado_nao_conta_duas_vezes() {
        let pool = test_pool().await;
        let sender = "redelivery@example.com";
        let mut inbound = message(21);
        inbound.from_addr = sender.into();

        let first = ingest(&pool, std::slice::from_ref(&inbound), 21).await;
        let duplicate = ingest(&pool, std::slice::from_ref(&inbound), 21).await;
        assert_eq!(first.ingested, 1);
        assert_eq!(duplicate.ingested, 0);
        assert_eq!(duplicate.duplicates, 1);

        let profile = crate::contacts::profile_for(&pool, sender).await.unwrap();
        assert!(
            profile.is_some(),
            "the first delivery must create the sender's contact profile"
        );
        let profile = profile.unwrap();
        assert_eq!(
            profile.messages_in, 1,
            "a redelivery ignored by the email insert must not inflate contact history"
        );
        assert!(!profile.outbound_ever);
    }

    #[tokio::test]
    async fn enviados_nunca_guardam_corpo() {
        let pool = test_pool().await;
        let secret = "rascunho confidencial que nunca deve entrar na base";
        let mut sent = message(22);
        sent.body_text = Some(secret.into());

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            22,
            &[],
            std::slice::from_ref(&sent),
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();
        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            1,
            22,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let outbound: (String, Option<String>) =
            sqlx::query_as("SELECT direction, body_text FROM emails WHERE mailbox = 'Sent'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let inbound: (String, Option<String>) =
            sqlx::query_as("SELECT direction, body_text FROM emails WHERE mailbox = 'INBOX'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(
            (outbound, inbound),
            (
                ("outbound".to_string(), None),
                ("inbound".to_string(), Some(secret.to_string()))
            )
        );
    }

    #[tokio::test]
    async fn enviados_marcam_o_trinco_sem_contar() {
        let pool = test_pool().await;
        let first = "primeiro.destinatario@example.com";
        let second = "segundo.destinatario@example.com";
        let to_header = format!("{first}, {second}");
        let mut sent = message(23);
        sent.headers.insert("to".into(), to_header);

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            23,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let to_addrs: Option<String> =
            sqlx::query_scalar("SELECT to_addrs FROM emails WHERE mailbox = 'Sent'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let first_profile = crate::contacts::profile_for(&pool, first)
            .await
            .unwrap()
            .map(|profile| (profile.messages_in, profile.outbound_ever));
        let second_profile = crate::contacts::profile_for(&pool, second)
            .await
            .unwrap()
            .map(|profile| (profile.messages_in, profile.outbound_ever));
        let stored_both = to_addrs
            .as_deref()
            .is_some_and(|stored| stored.contains(first) && stored.contains(second));

        assert_eq!(
            (stored_both, first_profile, second_profile),
            (true, Some((0, true)), Some((0, true)))
        );
    }

    #[tokio::test]
    async fn um_enviado_nao_guarda_o_que_anexaste() {
        let pool = test_pool().await;
        let recipient = "destinatario@example.com";
        let mut sent = message_with_attachment(24);
        sent.headers.insert("to".into(), recipient.into());

        // Even if the sidecar sends attachment details anyway, the núcleo must refuse to store them.
        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            24,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let attachment_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM email_attachments")
            .fetch_one(&pool)
            .await
            .unwrap();
        let has_attachments: i64 = sqlx::query_scalar("SELECT has_attachments FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        let outbound_ever = crate::contacts::profile_for(&pool, recipient)
            .await
            .unwrap()
            .map(|profile| profile.outbound_ever);

        assert_eq!(
            (attachment_count, has_attachments, outbound_ever),
            (0, 0, Some(true))
        );
    }

    /// A `sent_mailbox` aimed somewhere it should not be must not invent correspondents.
    ///
    /// THREAT_MODEL known gap 6: nothing validates what `sent_mailbox` points at, and pointed at a
    /// shared or archive folder every message in it would latch `outbound_ever` for people this
    /// account never wrote to. That is not a cosmetic error — `priority.rs`'s `first-contact` rule
    /// reads that field to decide whether an `urgent` from a stranger keeps its class, so the
    /// misconfiguration retires a brake for exactly the senders the brake exists to hold back.
    ///
    /// The message is still STORED: it is in the mailbox being read, and dropping it would lose
    /// mail. What is withheld is the claim about who corresponds with whom.
    #[tokio::test]
    async fn uma_mensagem_de_outra_pessoa_na_pasta_de_enviados_nao_inventa_correspondentes() {
        let pool = test_pool().await;
        let recipient = "estranho@example.com";
        let mut sent = message(30);
        // Somebody else's sent mail, sitting in the folder `sent_mailbox` names.
        sent.from_addr = "outra.pessoa@company.com".into();
        sent.headers.insert("to".into(), recipient.into());

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            30,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails WHERE mailbox = 'Sent'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            stored, 1,
            "the message itself is mail and must still be kept"
        );

        let profile = crate::contacts::profile_for(&pool, recipient)
            .await
            .unwrap();
        assert!(
            profile.is_none_or(|profile| !profile.outbound_ever),
            "a message this account did not write must not record that it wrote to the recipient"
        );

        // And it must not be silent, which is the whole complaint in known gap 6: the mail arrives,
        // the queue fills, and the only symptom is a brake that stops engaging.
        let told: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feed WHERE kind = 'email_sent_mailbox_foreign'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(told, 1, "a misconfigured sent_mailbox has to be visible");
    }

    /// The other half, without which the test above passes by breaking the feature.
    ///
    /// A guard comparing raw strings would refuse the owner's own mail the moment the header
    /// carried a display name or different casing — and every `outbound_ever` assertion in this
    /// module would still pass, because they would all be asserting on a code path that no longer
    /// runs. Both sides go through `normalize_address` for that reason.
    #[tokio::test]
    async fn o_proprio_envio_conta_mesmo_com_nome_e_capitalizacao_diferentes() {
        let pool = test_pool().await;
        let recipient = "destinatario@example.com";
        let mut sent = message(31);
        sent.from_addr = "Ana Pereira <Ana@Company.COM>".into();
        sent.headers.insert("to".into(), recipient.into());

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            31,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let outbound_ever = crate::contacts::profile_for(&pool, recipient)
            .await
            .unwrap()
            .map(|profile| profile.outbound_ever);
        assert_eq!(
            outbound_ever,
            Some(true),
            "the owner's own sent mail still records a correspondent"
        );

        let told: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feed WHERE kind = 'email_sent_mailbox_foreign'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(told, 0, "nothing is wrong here, so nothing is reported");
    }

    // The noise gate above looks headers up case-insensitively because a casing bug on the Go side
    // should not be able to switch that gate off silently. Recipients need the same protection,
    // especially because a hand-written demo fixture leaves casing to a human.
    #[tokio::test]
    async fn um_cabecalho_to_com_outra_capitalizacao_ainda_conta() {
        let pool = test_pool().await;
        let first = "primeiro.destinatario@example.com";
        let second = "segundo.destinatario@example.com";
        let to_header = format!("{first}, {second}");
        let mut sent = message(24);
        sent.headers.insert("To".into(), to_header);

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            24,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let to_addrs: Option<String> =
            sqlx::query_scalar("SELECT to_addrs FROM emails WHERE mailbox = 'Sent'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let first_profile = crate::contacts::profile_for(&pool, first)
            .await
            .unwrap()
            .map(|profile| (profile.messages_in, profile.outbound_ever));
        let second_profile = crate::contacts::profile_for(&pool, second)
            .await
            .unwrap()
            .map(|profile| (profile.messages_in, profile.outbound_ever));
        let stored_both = to_addrs
            .as_deref()
            .is_some_and(|stored| stored.contains(first) && stored.contains(second));

        assert_eq!(
            (stored_both, first_profile, second_profile),
            (true, Some((0, true)), Some((0, true)))
        );
    }

    #[tokio::test]
    async fn um_nome_com_virgula_nao_inventa_um_contacto() {
        let pool = test_pool().await;
        let mut sent = message(25);
        sent.headers
            .insert("to".into(), r#""Silva, Maria" <maria@example.com>"#.into());

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Outbound,
            "Sent",
            1,
            25,
            &[],
            &[sent],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let contact_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_addresses")
            .fetch_one(&pool)
            .await
            .unwrap();
        let address: String = sqlx::query_scalar("SELECT address FROM contact_addresses")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!((contact_count, address.as_str()), (1, "maria@example.com"));
    }

    #[tokio::test]
    async fn a_message_without_an_id_gets_a_synthetic_one() {
        let pool = test_pool().await;
        let mut m = message(77);
        m.message_id = None;
        ingest(&pool, &[m], 77).await;
        let stored: String = sqlx::query_scalar("SELECT message_id FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, "<nucleos-1-77>");
    }

    #[tokio::test]
    async fn an_oversized_body_is_truncated() {
        let pool = test_pool().await;
        let mut m = message(11);
        m.body_text = Some("x".repeat(MAX_BODY_BYTES + 500));
        ingest(&pool, &[m], 11).await;
        let body: String = sqlx::query_scalar("SELECT body_text FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(body.len(), MAX_BODY_BYTES);
    }

    /// A gated row never enters the queue, so it never needs the body — and dropping it here is
    /// what makes the gate the pillar's main reducer of untrusted content at rest.
    #[tokio::test]
    async fn a_noise_row_is_stored_classified_without_a_run() {
        let pool = test_pool().await;
        let mut m = message(12);
        m.headers
            .insert("list-unsubscribe".into(), "<mailto:x@y>".into());
        ingest(&pool, &[m], 12).await;

        let row = sqlx::query("SELECT * FROM emails WHERE message_id = '<m12@x>'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            row.get::<Option<String>, _>("triage_class").as_deref(),
            Some("noise")
        );
        assert!(
            row.get::<Option<String>, _>("body_text").is_some(),
            "with retention on, classification keeps the body for the calibration week (§7.2)"
        );
        assert!(row.get::<Option<i64>, _>("triage_run_id").is_none());
        assert!(row.get::<Option<String>, _>("triaged_at").is_some());
    }

    /// The steady state (`retain_bodies_days: 0`): a row that never enters the queue never needs
    /// its body, and dropping it here is what makes the gate the pillar's main reducer of untrusted
    /// content at rest.
    #[tokio::test]
    async fn with_retention_off_a_classified_row_keeps_no_body() {
        let pool = test_pool().await;
        let mut noisy = message(14);
        noisy
            .headers
            .insert("list-unsubscribe".into(), "<mailto:x@y>".into());
        let mut old = message(15);
        old.received_at = ago(72);

        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            1,
            15,
            &[],
            &[noisy, old],
            OWNER,
            0,
            now(),
        )
        .await
        .unwrap();

        for message_id in ["<m14@x>", "<m15@x>"] {
            let body: Option<String> =
                sqlx::query_scalar("SELECT body_text FROM emails WHERE message_id = ?")
                    .bind(message_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(body.is_none(), "{message_id} should have dropped its body");
        }
    }

    /// Spec §4.2's race: a redelivery must not re-run the gate over a row an in-flight batch has
    /// already claimed, because that write competes with the run's verdict.
    #[tokio::test]
    async fn a_redelivery_never_touches_an_already_claimed_row() {
        let pool = test_pool().await;
        ingest(&pool, &[message(13)], 13).await;
        sqlx::query(
            "UPDATE emails SET triage_run_id = 99, triage_class = 'urgent',
                               triage_summary = 'the verdict' WHERE message_id = '<m13@x>'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut redelivered = message(13);
        redelivered
            .headers
            .insert("precedence".into(), "bulk".into());
        ingest(&pool, &[redelivered], 13).await;

        let row = sqlx::query("SELECT * FROM emails WHERE message_id = '<m13@x>'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            row.get::<Option<String>, _>("triage_class").as_deref(),
            Some("urgent")
        );
        assert_eq!(row.get::<Option<i64>, _>("triage_run_id"), Some(99));
        assert_eq!(
            row.get::<Option<String>, _>("triage_summary").as_deref(),
            Some("the verdict")
        );
    }

    #[tokio::test]
    async fn a_failure_mid_batch_leaves_no_rows_and_no_cursor() {
        let pool = test_pool().await;
        // `received_at` is NOT NULL, so a message carrying a NULL for it fails the insert. The
        // batch has a good message before it, which must not survive the rollback.
        let mut poisoned = message(21);
        poisoned.received_at = String::new();
        sqlx::query(
            "CREATE TRIGGER reject_empty BEFORE INSERT ON emails
                     WHEN NEW.received_at = '' BEGIN SELECT RAISE(ABORT, 'bad date'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            1,
            21,
            &[],
            &[message(20), poisoned],
            OWNER,
            14,
            now(),
        )
        .await;
        assert!(result.is_err(), "the batch must fail as a whole");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no message may survive a failed batch");
        assert_eq!(get_cursor(&pool, "INBOX").await.unwrap(), None);
    }

    /// "I looked at a message, could not read it, move on" — the case that unblocks a corrupted
    /// message instead of retrying it forever.
    #[tokio::test]
    async fn an_empty_batch_still_advances_the_cursor() {
        let pool = test_pool().await;
        let outcome = ingest(&pool, &[], 55).await;
        assert_eq!(outcome.cursor, 55);
        assert_eq!(
            get_cursor(&pool, "INBOX").await.unwrap().unwrap().last_uid,
            55
        );
    }

    #[tokio::test]
    async fn a_skipped_message_is_announced_in_the_feed() {
        let pool = test_pool().await;
        ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            1,
            60,
            &[SkippedMessage {
                uid: 59,
                reason: "malformed MIME".into(),
            }],
            &[],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();

        let (kind, summary): (String, String) =
            sqlx::query_as("SELECT kind, summary FROM feed ORDER BY id DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "email_fetch_skipped");
        assert!(summary.contains("59"), "{summary}");
        assert!(summary.contains("malformed MIME"), "{summary}");
    }

    #[tokio::test]
    async fn a_uidvalidity_change_replaces_the_cursor() {
        let pool = test_pool().await;
        seed_cursor(&pool, 1, 900, 0).await;
        let outcome = ingest_batch(
            &pool,
            crate::contacts::MessageDirection::Inbound,
            "INBOX",
            2,
            5,
            &[],
            &[],
            OWNER,
            14,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.cursor, 5, "a rebuilt uid space is not a rewind");
        let cursor = get_cursor(&pool, "INBOX").await.unwrap().unwrap();
        assert_eq!(cursor.uidvalidity, 2);
        assert_eq!(cursor.last_uid, 5);
    }

    // ---- §4.4, the four cases that separate the wrong versions of the cutoff from the right one.

    #[tokio::test]
    async fn without_a_cursor_old_mail_is_filed_as_backfill() {
        let pool = test_pool().await;
        let mut m = message(30);
        m.received_at = ago(72);
        ingest(&pool, &[m], 30).await;

        let row = sqlx::query("SELECT * FROM emails WHERE message_id = '<m30@x>'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            row.get::<Option<String>, _>("triage_class").as_deref(),
            Some("info")
        );
        assert_eq!(
            row.get::<Option<String>, _>("triage_summary").as_deref(),
            Some(BACKFILL_SUMMARY)
        );
    }

    /// The weekend-offline case an earlier version destroyed: the daemon starts on LogonTrigger, so
    /// Monday's first sync carries Saturday's mail with an intact cursor. It must stay triageable.
    #[tokio::test]
    async fn a_recent_cursor_keeps_old_mail_in_the_queue() {
        let pool = test_pool().await;
        seed_cursor(&pool, 1, 5, 2).await;
        let mut m = message(31);
        m.received_at = ago(72);
        ingest(&pool, &[m], 31).await;
        assert_eq!(class_of(&pool, "<m31@x>").await, None);
    }

    /// The month-long absence: the cursor is intact but thousands of messages are about to arrive,
    /// and triaging them would drain the budget and notify three-week-old urgencies.
    #[tokio::test]
    async fn a_stale_cursor_arms_the_cutoff_again() {
        let pool = test_pool().await;
        seed_cursor(&pool, 1, 5, 30).await;
        let mut m = message(32);
        m.received_at = ago(72);
        ingest(&pool, &[m], 32).await;
        assert_eq!(class_of(&pool, "<m32@x>").await.as_deref(), Some("info"));
    }

    /// The cutoff is decided at ingestion and never re-evaluated, so a message that waits in the
    /// queue does not become backfill just by ageing there.
    #[tokio::test]
    async fn a_message_ingested_recently_never_becomes_backfill_later() {
        let pool = test_pool().await;
        let m = message(33);
        ingest(&pool, &[m], 33).await;
        assert_eq!(class_of(&pool, "<m33@x>").await, None);

        // Two days later the row is still exactly what it was: pending, with its body.
        let body: Option<String> =
            sqlx::query_scalar("SELECT body_text FROM emails WHERE uid = 33")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(body.as_deref(), Some("body"));
        assert_eq!(class_of(&pool, "<m33@x>").await, None);
    }

    #[tokio::test]
    async fn an_armed_batch_still_triages_fresh_mail() {
        let pool = test_pool().await;
        let mut fresh = message(34);
        fresh.received_at = ago(2);
        ingest(&pool, &[fresh], 34).await;
        assert_eq!(class_of(&pool, "<m34@x>").await, None);
    }

    #[test]
    fn an_unreadable_cursor_timestamp_arms_the_cutoff() {
        let cursor = Cursor {
            uidvalidity: 1,
            last_uid: 5,
            updated_at: "not a date".into(),
        };
        assert!(backfill_armed(Some(&cursor), 1, now()));
    }

    /// The opposite direction on the message side: an unreadable `received_at` keeps the message in
    /// the queue rather than filing it on a formatting error.
    #[test]
    fn an_unreadable_received_at_is_not_backfill() {
        assert!(!is_backfill_message("not a date", now()));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let body = "é".repeat(MAX_BODY_BYTES);
        let cut = truncate_body(&body);
        assert!(cut.len() <= MAX_BODY_BYTES);
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }

    async fn seed_classified(
        pool: &sqlx::SqlitePool,
        body: Option<&str>,
        claim: Option<i64>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, body_text,
                                 received_at, ingested_at, triage_class, triage_summary,
                                 triaged_at, triage_attempts, infra_failures, triage_run_id)
             VALUES ('<q@x>', 'INBOX', 1, 1, 'a@b', ?, '2026-07-28T10:00:00+00:00',
                     '2026-07-28T10:00:00+00:00', 'failed', 'gave up', '2026-07-28T10:05:00+00:00',
                     2, 3, ?)",
        )
        .bind(body)
        .bind(claim)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_run(pool: &sqlx::SqlitePool, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('proj', 'triage', ?, 'email_triage', '2026-07-28T10:00:00+00:00')",
        )
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn requeue_clears_the_verdict_without_moving_ingested_at() {
        let pool = test_pool().await;
        let id = seed_classified(&pool, Some("body"), None).await;
        requeue(&pool, id).await.unwrap();

        let row = sqlx::query("SELECT * FROM emails WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(row.get::<Option<String>, _>("triage_class").is_none());
        assert!(row.get::<Option<String>, _>("triage_summary").is_none());
        assert!(row.get::<Option<String>, _>("triaged_at").is_none());
        assert!(row.get::<Option<i64>, _>("triage_run_id").is_none());
        assert_eq!(row.get::<i64, _>("triage_attempts"), 0);
        assert_eq!(row.get::<i64, _>("infra_failures"), 0);
        assert_eq!(
            row.get::<String, _>("ingested_at"),
            "2026-07-28T10:00:00+00:00",
            "the message did not re-arrive, so queue order must not move"
        );
    }

    /// Eligibility is the body, not the class: the misclassified `info` row is exactly as worth
    /// re-triaging as the one that failed.
    #[tokio::test]
    async fn a_misclassified_row_is_as_requeueable_as_a_failed_one() {
        let pool = test_pool().await;
        let id = seed_classified(&pool, Some("body"), None).await;
        sqlx::query("UPDATE emails SET triage_class = 'info' WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(requeue(&pool, id).await, Ok(()));
    }

    #[tokio::test]
    async fn a_purged_body_cannot_be_requeued() {
        let pool = test_pool().await;
        let id = seed_classified(&pool, None, None).await;
        assert_eq!(requeue(&pool, id).await, Err(RequeueError::BodyPurged));
    }

    #[tokio::test]
    async fn an_unknown_email_cannot_be_requeued() {
        let pool = test_pool().await;
        assert_eq!(requeue(&pool, 4242).await, Err(RequeueError::UnknownEmail));
    }

    /// The guard that carries this route: clearing a live claim would let the next tick launch a
    /// second run over the same message, and both would write a verdict.
    #[tokio::test]
    async fn a_row_claimed_by_a_live_run_is_refused() {
        let pool = test_pool().await;
        let run_id = seed_run(&pool, "running").await;
        let id = seed_classified(&pool, Some("body"), Some(run_id)).await;
        assert_eq!(
            requeue(&pool, id).await,
            Err(RequeueError::ClaimedByRun(run_id))
        );
    }

    #[tokio::test]
    async fn a_row_claimed_by_a_finished_run_is_released() {
        let pool = test_pool().await;
        for status in [
            "completed",
            "failed",
            "timed_out",
            "cancelled",
            "interrupted",
            "superseded",
        ] {
            let run_id = seed_run(&pool, status).await;
            let id = seed_classified(&pool, Some("body"), Some(run_id)).await;
            assert_eq!(requeue(&pool, id).await, Ok(()), "status {status}");
            sqlx::query("DELETE FROM emails WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
    }

    /// An `awaiting_approval` run is not terminal. A triage run never gets there, but the predicate
    /// is shared with the triage loop's single-flight check, so it has to be right for both.
    #[tokio::test]
    async fn a_row_claimed_by_a_paused_run_is_refused() {
        let pool = test_pool().await;
        let run_id = seed_run(&pool, "awaiting_approval").await;
        let id = seed_classified(&pool, Some("body"), Some(run_id)).await;
        assert_eq!(
            requeue(&pool, id).await,
            Err(RequeueError::ClaimedByRun(run_id))
        );
    }

    /// Spec §4.3: a feed entry is a notification, and notifying someone of their own click is noise.
    #[tokio::test]
    async fn requeue_writes_no_feed_row() {
        let pool = test_pool().await;
        let id = seed_classified(&pool, Some("body"), None).await;
        requeue(&pool, id).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn an_unsynchronised_mailbox_has_no_cursor() {
        let pool = test_pool().await;
        assert_eq!(get_cursor(&pool, "INBOX").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_stored_cursor_round_trips() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO email_cursor (mailbox, uidvalidity, last_uid, updated_at)
             VALUES ('INBOX', 9, 41, '2026-07-28T10:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            get_cursor(&pool, "INBOX").await.unwrap(),
            Some(Cursor {
                uidvalidity: 9,
                last_uid: 41,
                updated_at: "2026-07-28T10:00:00+00:00".to_string(),
            })
        );
    }
}
