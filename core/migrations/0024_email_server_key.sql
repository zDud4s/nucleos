-- The dedupe key stops being a header the sender writes.
--
-- `message_id` was `NOT NULL UNIQUE`, and `Message-ID` is composed by whoever sent the mail. Two
-- messages carrying the same one collapse into a single row, and the survivor is whichever the
-- mailbox delivered first: a sender who repeats another message's Message-ID suppresses the other
-- one, silently, as an ignored INSERT that the ingest counts as an ordinary duplicate. Nothing
-- hostile is required either — mailing-list resends and forwards reuse a Message-ID by design.
--
-- `(mailbox, uidvalidity, uid)` is the server's own identity for a message. The UID is assigned by
-- the IMAP server, and `uidvalidity` is the value that says the numbering restarted; between them
-- they name one message on one mailbox, and the sender controls neither. A redelivery after a
-- dropped connection still costs an ignored INSERT, which is what the old constraint was for.
--
-- `message_id` stays, without the UNIQUE, because what the sender claimed is worth keeping on the
-- record — it is simply not an identity.
--
-- Rebuilt rather than altered because SQLite cannot drop a UNIQUE. Both tables are rebuilt, in this
-- order deliberately: with foreign keys on, `DROP TABLE emails` performs an implicit DELETE that
-- fires `email_attachments`' ON DELETE CASCADE, so the children have to be out of the way before
-- the parent goes. The final RENAME is what re-points the surviving child's foreign key.

CREATE TABLE emails_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id      TEXT NOT NULL,
    mailbox         TEXT NOT NULL,
    uidvalidity     INTEGER NOT NULL,
    uid             INTEGER NOT NULL,
    from_addr       TEXT NOT NULL,
    from_name       TEXT,
    subject         TEXT,
    body_text       TEXT,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    received_at     TEXT NOT NULL,
    ingested_at     TEXT NOT NULL,
    triage_class    TEXT,
    triage_summary  TEXT,
    triage_run_id   INTEGER,
    triage_attempts INTEGER NOT NULL DEFAULT 0,
    infra_failures  INTEGER NOT NULL DEFAULT 0,
    triaged_at      TEXT,
    UNIQUE (mailbox, uidvalidity, uid)
);

-- `MIN(id)` keeps the row that arrived first, which is the one the old constraint would have kept
-- had the key been right. Rows sharing a server identity exist only because the old key let them:
-- the same message stored twice under two different Message-IDs.
INSERT INTO emails_new
SELECT id, message_id, mailbox, uidvalidity, uid, from_addr, from_name, subject, body_text,
       has_attachments, received_at, ingested_at, triage_class, triage_summary, triage_run_id,
       triage_attempts, infra_failures, triaged_at
FROM emails
WHERE id IN (SELECT MIN(id) FROM emails GROUP BY mailbox, uidvalidity, uid);

CREATE TABLE email_attachments_new (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    email_id   INTEGER NOT NULL REFERENCES emails_new(id) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    filename   TEXT,
    mime_type  TEXT,
    size_bytes INTEGER NOT NULL,
    UNIQUE (email_id, position)
);

INSERT INTO email_attachments_new
SELECT id, email_id, position, filename, mime_type, size_bytes
FROM email_attachments
WHERE email_id IN (SELECT id FROM emails_new);

DROP TABLE email_attachments;
DROP TABLE emails;

ALTER TABLE emails_new RENAME TO emails;
ALTER TABLE email_attachments_new RENAME TO email_attachments;

CREATE INDEX emails_pending ON emails (ingested_at) WHERE triage_class IS NULL;
CREATE INDEX email_attachments_by_email ON email_attachments (email_id, position);
