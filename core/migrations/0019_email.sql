-- Email pillar v1 (spec §4.1): read-only triage of an IMAP mailbox.
--
-- `emails` holds one row per delivered message. `message_id` is the RFC 5322 natural key and the
-- dedupe boundary, so a redelivery after a dropped connection costs an ignored INSERT, not a
-- duplicate row. A message with no Message-ID (rare but legal) gets a synthetic one built from
-- (uidvalidity, uid) — the only pair that identifies a message on a server whose UIDs may reset.
CREATE TABLE emails (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id      TEXT NOT NULL UNIQUE,
    mailbox         TEXT NOT NULL,
    uidvalidity     INTEGER NOT NULL,
    uid             INTEGER NOT NULL,
    from_addr       TEXT NOT NULL,
    from_name       TEXT,
    subject         TEXT,
    -- Dropped as soon as the row leaves the pending queue (§7.2): untrusted third-party content
    -- lives exactly as long as the triage needs it. `failed` is the one class that keeps it.
    body_text       TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    -- IMAP INTERNALDATE, not the sender's `Date:` header — the latter is written by the sender and
    -- would let hostile mail govern the backfill cutoff and retention (§3.3).
    received_at     TEXT NOT NULL,
    -- The daemon's clock. This is what governs batch age and pruning; see the clock table in §4.1.
    ingested_at     TEXT NOT NULL,
    -- NULL = not yet triaged. Otherwise urgent | action | info | noise | failed, where `failed` is
    -- a terminal state rather than a content class, so a poisonous message is not retried forever.
    triage_class    TEXT,
    triage_summary  TEXT,
    -- Claims the row for one in-flight batch (§5.1). NULL = unclaimed.
    triage_run_id   INTEGER,
    triage_attempts INTEGER NOT NULL DEFAULT 0,
    infra_failures  INTEGER NOT NULL DEFAULT 0,
    triaged_at      TEXT
);

-- The queue the triage loop reads every tick, and the only index it needs: partial on the pending
-- rows, ordered by the column that decides which batch goes next.
CREATE INDEX emails_pending ON emails (ingested_at) WHERE triage_class IS NULL;

-- One row per mailbox. The núcleo owns this, not the sidecar: a cursor that only advances inside
-- the ingestion transaction cannot get ahead of the mail it claims to have stored (§3.2).
CREATE TABLE email_cursor (
    mailbox      TEXT PRIMARY KEY,
    uidvalidity  INTEGER NOT NULL,
    last_uid     INTEGER NOT NULL,
    -- Also the backfill cutoff's age test (§4.4): a cursor older than 7 days means a pile of read
    -- mail is about to arrive, which is the only situation the cutoff exists for.
    updated_at   TEXT NOT NULL
);
