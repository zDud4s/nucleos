-- Lets a person find mail by what it was about, months after the body is gone.
--
-- The column list is the whole design decision, and what is ABSENT from it matters more than what
-- is present. `body_text` is not indexed and must never be: triage sets it to NULL once it is done
-- (see `triage.rs`, and the comment on 0019 that says why — untrusted third-party content lives
-- exactly as long as the triage needs it). An FTS index over the body would hold its terms after
-- the body itself is gone, which is that retention decision quietly undone by a search feature.
--
-- What is indexed is what survives triage: who wrote, what they called it, and the summary a LOCAL
-- model wrote about it. Those are already kept for ever in the row, so indexing them adds no
-- retention question that the table does not already answer.
--
-- external-content table over `emails`, whose `id` is an INTEGER PRIMARY KEY and therefore a rowid
-- alias.
CREATE VIRTUAL TABLE IF NOT EXISTS emails_fts USING fts5(
    subject,
    from_name,
    from_addr,
    triage_summary,
    content='emails',
    content_rowid='id',
    tokenize='unicode61'
);

INSERT INTO emails_fts (rowid, subject, from_name, from_addr, triage_summary)
SELECT id, subject, from_name, from_addr, triage_summary FROM emails;

CREATE TRIGGER IF NOT EXISTS emails_fts_insert AFTER INSERT ON emails BEGIN
    INSERT INTO emails_fts (rowid, subject, from_name, from_addr, triage_summary)
    VALUES (new.id, new.subject, new.from_name, new.from_addr, new.triage_summary);
END;

CREATE TRIGGER IF NOT EXISTS emails_fts_delete AFTER DELETE ON emails BEGIN
    INSERT INTO emails_fts (emails_fts, rowid, subject, from_name, from_addr, triage_summary)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.triage_summary);
END;

-- Unlike 0035's, and unlike 0050's, this trigger is the one that does the real work. A message is
-- INSERTed with no verdict at all and gains `triage_summary` by UPDATE, minutes or hours later —
-- so without this, the index would hold every subject and not one summary, and the most searchable
-- thing about a message would be the only thing missing from it.
CREATE TRIGGER IF NOT EXISTS emails_fts_update AFTER UPDATE ON emails BEGIN
    INSERT INTO emails_fts (emails_fts, rowid, subject, from_name, from_addr, triage_summary)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.triage_summary);
    INSERT INTO emails_fts (rowid, subject, from_name, from_addr, triage_summary)
    VALUES (new.id, new.subject, new.from_name, new.from_addr, new.triage_summary);
END;
